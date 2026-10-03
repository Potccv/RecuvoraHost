//! Host-owned incident bindings and serial supervised recovery scheduling.
use super::MonitorIncidentGuard;
use super::incident_guard::problem_from_record;
use crate::control::recovery::incidents::IncidentKind;
use crate::monitoring::MonitorHandle;
use crate::recovery::{RecoveryError, RecoveryService, RecoveryStage};
use crate::runtime::operation::Cancellation;
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

/// Owns one bounded serial scheduler. Dropping requests cancellation; explicit
/// Shutdown waits for the original operation and recovery storage to finish active work.
/// The supplied monitor belongs to its caller and is never shut down here.
pub struct RecoveryScheduler {
    recovery: Arc<RecoveryService>,
    cancellation: Cancellation,
    running: Arc<AtomicBool>,
    last_error: Arc<Mutex<Option<String>>>,
    worker: tokio::sync::Mutex<Option<tokio::task::JoinHandle<Result<(), RecoveryError>>>>,
}

impl RecoveryScheduler {
    pub fn start(
        recovery: Arc<RecoveryService>,
        monitor: MonitorHandle,
        interval: Duration,
    ) -> Result<Self, RecoveryError> {
        if interval < Duration::from_millis(10) || interval > Duration::from_secs(3600) {
            return Err(RecoveryError::Invalid(
                "scheduler requires 10ms..1h interval".into(),
            ));
        }
        tokio::runtime::Handle::try_current().map_err(service)?;
        recovery.bind_incident_guard(Arc::new(MonitorIncidentGuard::new(monitor.clone())))?;
        let cancellation = Cancellation::new();
        let running = Arc::new(AtomicBool::new(true));
        let last_error = Arc::new(Mutex::new(None));
        let worker_recovery = recovery.clone();
        let worker_cancellation = cancellation.clone();
        let worker_running = running.clone();
        let worker_error = last_error.clone();
        let worker = tokio::spawn(async move {
            let _running = RunningGuard(worker_running);
            run(
                &worker_recovery,
                &monitor,
                interval,
                &worker_cancellation,
                &worker_error,
            )
            .await;
            let result = worker_recovery.shutdown().await;
            if let Err(error) = &result {
                remember(&worker_error, error);
            }
            result
        });
        Ok(Self {
            recovery,
            cancellation,
            running,
            last_error,
            worker: tokio::sync::Mutex::new(Some(worker)),
        })
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }

    /// Most recent per-item or scheduler error; retained until a later error replaces
    /// it so successful work does not hide another item's failure.
    pub fn last_error(&self) -> Result<Option<String>, RecoveryError> {
        Ok(lock(&self.last_error)?.clone())
    }

    pub fn begin_shutdown(&self) {
        self.cancellation.cancel();
    }

    /// Safe to cancel and await again: the owned JoinHandle remains stored until
    /// it actually completes, and shutdown never drops an in-flight repair future.
    pub async fn shutdown(&self) -> Result<(), RecoveryError> {
        self.begin_shutdown();
        let mut worker = self.worker.lock().await;
        if let Some(task) = worker.as_mut() {
            let result = task.await;
            worker.take();
            if let Err(error) = result {
                self.recovery.shutdown().await?;
                return Err(service(format!("scheduler supervisor failed: {error}")));
            }
            result.map_err(service)??;
        }
        Ok(())
    }
}

impl Drop for RecoveryScheduler {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

struct RunningGuard(Arc<AtomicBool>);
impl Drop for RunningGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

fn remember(slot: &Mutex<Option<String>>, error: impl std::fmt::Display) {
    let mut message = error.to_string();
    if message.len() > 4096 {
        let mut end = 4096;
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        message.truncate(end);
    }
    if let Ok(mut value) = slot.lock() {
        *value = Some(message);
    }
}

fn lock<T>(value: &Mutex<T>) -> Result<MutexGuard<'_, T>, RecoveryError> {
    value
        .lock()
        .map_err(|_| service("recovery scheduler lock poisoned"))
}

fn service(error: impl std::fmt::Display) -> RecoveryError {
    RecoveryError::Service(error.to_string())
}

async fn run(
    recovery: &Arc<RecoveryService>,
    monitor: &MonitorHandle,
    interval: Duration,
    cancellation: &Cancellation,
    last_error: &Mutex<Option<String>>,
) {
    let notifications = monitor.error_notifications();
    loop {
        let notified = notifications.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if cancellation.is_cancelled() {
            break;
        }
        let tasks = match recovery.tasks() {
            Ok(tasks) => tasks,
            Err(error) => {
                remember(last_error, error);
                Vec::new()
            }
        };
        let mut seen: BTreeSet<_> = tasks
            .iter()
            .map(|task| task.problem.incident_id.clone())
            .collect();
        let mut waiting_for_target = false;
        match monitor.incidents() {
            Ok(incidents) => {
                for incident in incidents {
                    if cancellation.is_cancelled() {
                        break;
                    }
                    if incident.kind != IncidentKind::ErrorLog
                        || seen.contains(&incident.id)
                        || incident.target_id != recovery.config().target.target_id
                    {
                        continue;
                    }
                    let problem = match problem_from_record(
                        &incident,
                        recovery.config().target.required_facts.clone(),
                    ) {
                        Ok(problem) => problem,
                        Err(error) => {
                            remember(last_error, error);
                            continue;
                        }
                    };
                    match recovery.submit(problem) {
                        Ok(_) => {
                            seen.insert(incident.id);
                        }
                        Err(RecoveryError::Busy) => waiting_for_target = true,
                        Err(error) => remember(last_error, error),
                    }
                }
            }
            Err(error) => remember(last_error, error),
        }
        match recovery.tasks() {
            Ok(tasks) => {
                for task in tasks {
                    if cancellation.is_cancelled() {
                        break;
                    }
                    if task.stage.terminal()
                        || matches!(task.stage, RecoveryStage::Paused | RecoveryStage::Unknown)
                    {
                        continue;
                    }
                    let operation_cancel = Cancellation::new();
                    let advance = recovery.advance(&task.id, operation_cancel.clone());
                    tokio::pin!(advance);
                    let result = tokio::select! {
                        result = &mut advance => result,
                        _ = cancellation.cancelled() => {
                            operation_cancel.cancel();
                            advance.await
                        }
                    };
                    match result {
                        Ok(task) if task.stage.terminal() && waiting_for_target => {
                            notifications.notify_one();
                        }
                        Err(error) => remember(last_error, error),
                        _ => {}
                    }
                }
            }
            Err(error) => remember(last_error, error),
        }
        if let Err(error) = recovery.summarize_pending(cancellation.clone()).await {
            remember(last_error, error);
        }
        if let Err(error) = recovery.deliver_pending() {
            remember(last_error, error);
        }
        tokio::select! {
            _ = cancellation.cancelled() => break,
            _ = notified => {},
            _ = tokio::time::sleep(interval) => {}
        }
    }
}
