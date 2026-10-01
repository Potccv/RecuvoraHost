//! Single-in-flight observation polling with independent freshness checks.
use super::shared::{Shared, worker_finished};
use super::state::MonitorState;
use super::support::DISCOVERY_INVALIDATED;
use super::{Coverage, MonitorError, ObservationSource, TargetHealth};
use crate::runtime::operation::Cancellation;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::time::Instant;

// Registration reserves ownership before spawning; shutdown cannot observe zero
// while a discovery supervisor is still able to add an observation worker.
pub(super) fn launch_monitor(
    state: MonitorState,
    source: Arc<dyn ObservationSource>,
    shared: Arc<Shared>,
) {
    let id = state.config.id.clone();
    let worker_shared = shared.clone();
    let worker = tokio::spawn(async move { run_monitor(state, source, worker_shared).await });
    tokio::spawn(async move {
        match worker.await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => shared.fail(error.to_string()),
            Err(error) => shared.fail(format!("monitor {id} supervisor failed: {error}")),
        }
        if let Ok(mut views) = shared.views.lock()
            && let Some(view) = views.get_mut(&id)
        {
            view.running = false;
            view.health = TargetHealth::Unknown;
        }
        worker_finished(&shared).await;
    });
}

async fn run_monitor(
    mut state: MonitorState,
    source: Arc<dyn ObservationSource>,
    shared: Arc<Shared>,
) -> Result<(), MonitorError> {
    let mut next_poll = Instant::now();
    let mut observed_epoch = source.observation_epoch();
    let tick_ms = state
        .config
        .interval_ms
        .min(state.config.stale_after_ms)
        .min(state.config.startup_grace_ms)
        .min(250);
    let mut ticks = tokio::time::interval(Duration::from_millis(tick_ms));
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            biased;
            _ = shared.cancellation.cancelled() => break,
            _ = ticks.tick() => {
                let _registration = shared.registration.lock().await;
                let current_epoch = source.observation_epoch();
                let signals = if (current_epoch.is_none() && !source.observation_pending())
                    || (observed_epoch.is_some() && current_epoch != observed_epoch) {
                    state.coverage_failure(DISCOVERY_INVALIDATED.into(), Coverage::Unavailable)
                } else { state.tick() };
                observed_epoch = current_epoch;
                if !signals.is_empty() { state.commit(&shared, signals)?; } else { state.publish(&shared)?; }
            },
            _ = tokio::time::sleep_until(next_poll) => {
                if !shared.accepting.load(Ordering::Acquire) { break; }
                if source.observation_pending() {
                    // Keep the independent freshness/grace clock alive while
                    // waiting for inventory; no observation future is created.
                    next_poll = Instant::now() + Duration::from_millis(tick_ms);
                    continue;
                }
                let start = Instant::now();
                let epoch = source.observation_epoch();
                let request = state.request();
                let cancellation = Cancellation::new();
                let poll = source.poll(request, cancellation.clone());
                tokio::pin!(poll);
                let deadline = tokio::time::sleep(Duration::from_millis(state.config.timeout_ms));
                tokio::pin!(deadline);
                let mut timed_out = false;
                let result = loop {
                    tokio::select! {
                        biased;
                        _ = shared.cancellation.cancelled() => { cancellation.cancel(); let _ = poll.await; break None; },
                        result = &mut poll => break Some(result),
                        _ = &mut deadline, if !timed_out => {
                            timed_out = true;
                            cancellation.cancel();
                            let result = {
                                let _registration = shared.registration.lock().await;
                                let signals = state.coverage_failure("observation deadline elapsed".into(), Coverage::Unavailable);
                                state.commit(&shared, signals)
                            };
                            if let Err(error) = result {
                                shared.fail(error.to_string());
                                let _ = poll.await;
                                return Err(error);
                            }
                        },
                        _ = ticks.tick() => {
                            let result = {
                                let _registration = shared.registration.lock().await;
                                let signals = if source.observation_epoch() != epoch || epoch.is_none() {
                                    cancellation.cancel();
                                    state.coverage_failure(DISCOVERY_INVALIDATED.into(), Coverage::Unavailable)
                                } else { state.tick() };
                                if !signals.is_empty() { state.commit(&shared, signals) } else { state.publish(&shared) }
                            };
                            if let Err(error) = result {
                                shared.fail(error.to_string());
                                cancellation.cancel();
                                let _ = poll.await;
                                return Err(error);
                            }
                        }
                    }
                };
                let Some(result) = result else { break; };
                let _registration = shared.registration.lock().await;
                let current_epoch = source.observation_epoch();
                let mut signals = if observed_epoch.is_some() && current_epoch != observed_epoch {
                    state.coverage_failure(DISCOVERY_INVALIDATED.into(), Coverage::Unavailable)
                } else { Vec::new() };
                observed_epoch = current_epoch;
                let result = if current_epoch != epoch || epoch.is_none() {
                    Err(MonitorError::Observation(DISCOVERY_INVALIDATED.into()))
                } else if timed_out { Err(MonitorError::Observation("observation deadline elapsed".into())) } else { result };
                signals.extend(match result.and_then(|batch| state.accept(batch, start.elapsed())) {
                    Ok(signals) => signals,
                    Err(error) => state.coverage_failure(error.to_string(), Coverage::Unavailable),
                });
                state.commit(&shared, signals)?;
                next_poll = Instant::now() + Duration::from_millis(state.config.interval_ms);
            }
        }
    }
    Ok(())
}
