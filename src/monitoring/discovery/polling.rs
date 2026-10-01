//! Discovery call deadlines, waiting for canceled calls and worker supervision.
use super::super::shared::{Shared, worker_finished};
use super::super::{MonitorError, ObservationRequest, ObservationSource};
use super::DiscoveryState;
use crate::runtime::operation::Cancellation;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

async fn run(
    mut state: DiscoveryState,
    source: Arc<dyn ObservationSource>,
    shared: Arc<Shared>,
) -> Result<(), MonitorError> {
    loop {
        if !shared.accepting.load(Ordering::Acquire) {
            break;
        }
        let cancellation = Cancellation::new();
        let request = ObservationRequest {
            extension_id: state.config.extension_id.clone(),
            contract: state.config.contract.clone(),
            version: state.config.version,
            method: state.config.method.clone(),
            params: state.config.params.clone(),
            timeout: Duration::from_millis(state.config.timeout_ms),
        };
        let call = source.discover(request, cancellation.clone());
        tokio::pin!(call);
        let result = tokio::select! {
            biased;
            _ = shared.cancellation.cancelled() => { cancellation.cancel(); let _ = call.await; break; },
            result = &mut call => result,
            _ = tokio::time::sleep(Duration::from_millis(state.config.timeout_ms)) => {
                cancellation.cancel();
                state.reject("discovery deadline elapsed", &shared)?;
                let _ = call.await;
                Err(MonitorError::Observation("discovery deadline elapsed".into()))
            }
        };
        let accepted = match result {
            Ok(batch) => state.accept(batch, source.clone(), &shared).await,
            Err(error) => Err(error),
        };
        match accepted {
            Ok(()) => {}
            Err(MonitorError::Stopped) => break,
            Err(error @ MonitorError::Incident(_)) | Err(error @ MonitorError::Runtime(_)) => {
                return Err(error);
            }
            Err(error) => state.reject(&error.to_string(), &shared)?,
        }
        tokio::select! {
            biased;
            _ = shared.cancellation.cancelled() => break,
            _ = tokio::time::sleep(Duration::from_millis(state.config.interval_ms)) => {}
        }
    }
    Ok(())
}

pub(in crate::monitoring) fn launch(
    state: DiscoveryState,
    source: Arc<dyn ObservationSource>,
    shared: Arc<Shared>,
) {
    let id = state.config.id.clone();
    let worker_shared = shared.clone();
    let worker = tokio::spawn(async move { run(state, source, worker_shared).await });
    tokio::spawn(async move {
        match worker.await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => shared.fail(error.to_string()),
            Err(error) => shared.fail(format!("discovery {id} supervisor failed: {error}")),
        }
        if let Ok(mut views) = shared.discoveries.lock()
            && let Some(view) = views.get_mut(&id)
        {
            view.running = false;
        }
        worker_finished(&shared).await;
    });
}
