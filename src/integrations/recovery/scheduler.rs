//! Host-owned incident bindings and serial supervised recovery scheduling.
use super::MonitorIncidentGuard;
use crate::control::recovery::incidents::{IncidentKind, IncidentStatus, SignalCondition};
use crate::integrations::recovery::{
    ProblemContext, RecoveryError, RecoveryService, RecoveryStage,
};
use crate::monitoring::MonitorHandle;
use crate::runtime::operation::Cancellation;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

/// Static host policy mapping one observed condition into a repair context.
/// Neither provider observations nor model output may choose these fields.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IncidentTrigger {
    pub monitor_id: String,
    pub rule_id: String,
    pub fingerprint: String,
    pub keywords: Vec<String>,
    pub conditions: BTreeMap<String, String>,
}

impl IncidentTrigger {
    pub fn validate(&self) -> Result<(), RecoveryError> {
        for value in [&self.monitor_id, &self.rule_id, &self.fingerprint] {
            text(value, 128)?;
        }
        if self.keywords.len() > 32 || self.conditions.is_empty() {
            return Err(RecoveryError::Invalid(
                "trigger needs exact conditions and at most 32 keywords".into(),
            ));
        }
        let mut words = BTreeSet::new();
        for keyword in &self.keywords {
            text(keyword, 128)?;
            if !words.insert(keyword) {
                return Err(RecoveryError::Invalid("duplicate trigger keyword".into()));
            }
        }
        validate_facts(&self.conditions)
    }
}

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
        triggers: Vec<IncidentTrigger>,
        interval: Duration,
    ) -> Result<Self, RecoveryError> {
        if triggers.is_empty()
            || triggers.len() > 64
            || interval < Duration::from_millis(10)
            || interval > Duration::from_secs(3600)
        {
            return Err(RecoveryError::Invalid(
                "scheduler requires 1..64 triggers and 10ms..1h interval".into(),
            ));
        }
        let mut bindings = BTreeSet::new();
        for trigger in &triggers {
            trigger.validate()?;
            if !bindings.insert((&trigger.monitor_id, &trigger.rule_id)) {
                return Err(RecoveryError::Invalid("duplicate incident trigger".into()));
            }
            let definition = monitor
                .definition(&trigger.monitor_id)
                .map_err(service)?
                .ok_or_else(|| {
                    RecoveryError::Invalid("trigger monitor is not registered".into())
                })?;
            if trigger.rule_id != definition.id {
                return Err(RecoveryError::Invalid(
                    "trigger rule must match the registered monitor rule identity".into(),
                ));
            }
            if definition.target_id != recovery.config().target.target_id {
                return Err(RecoveryError::Invalid(
                    "trigger monitor belongs to another target".into(),
                ));
            }
        }
        tokio::runtime::Handle::try_current().map_err(service)?;
        recovery.bind_incident_guard(Arc::new(MonitorIncidentGuard::new(
            monitor.clone(),
            triggers.clone(),
        )?))?;
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
                &triggers,
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

fn text(value: &str, maximum: usize) -> Result<(), RecoveryError> {
    if value.trim().is_empty() || value.len() > maximum || value.contains('\0') {
        Err(RecoveryError::Invalid(
            "empty, oversized or NUL-containing trigger value".into(),
        ))
    } else {
        Ok(())
    }
}

fn validate_facts(values: &BTreeMap<String, String>) -> Result<(), RecoveryError> {
    if values.len() > 32 {
        return Err(RecoveryError::Capacity);
    }
    for (key, value) in values {
        text(key, 128)?;
        text(value, 1024)?;
    }
    Ok(())
}

async fn run(
    recovery: &Arc<RecoveryService>,
    monitor: &MonitorHandle,
    triggers: &[IncidentTrigger],
    interval: Duration,
    cancellation: &Cancellation,
    last_error: &Mutex<Option<String>>,
) {
    loop {
        if cancellation.is_cancelled() {
            break;
        }
        if let Err(error) = recovery.summarize_pending(cancellation.clone()).await {
            remember(last_error, error);
        }
        if let Err(error) = recovery.deliver_pending() {
            remember(last_error, error);
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
        match monitor.incidents() {
            Ok(incidents) => {
                for incident in incidents {
                    if cancellation.is_cancelled() {
                        break;
                    }
                    if incident.kind != IncidentKind::Target
                        || incident.condition != SignalCondition::Active
                        || incident.status == IncidentStatus::Resolved
                        || seen.contains(&incident.id)
                        || incident.target_id != recovery.config().target.target_id
                    {
                        continue;
                    }
                    let Some(trigger) = triggers.iter().find(|trigger| {
                        trigger.monitor_id == incident.monitor_id
                            && trigger.rule_id == incident.rule_id
                    }) else {
                        continue;
                    };
                    let problem = ProblemContext {
                        incident_id: incident.id.clone(),
                        incident_revision: incident.revision,
                        target_id: incident.target_id,
                        fingerprint: trigger.fingerprint.clone(),
                        summary: incident.summary,
                        occurrences: incident.occurrences,
                        keywords: trigger.keywords.clone(),
                        conditions: trigger.conditions.clone(),
                        evidence_refs: vec![format!(
                            "incident:{}:revision:{}",
                            incident.id, incident.revision
                        )],
                    };
                    match recovery.submit(problem) {
                        Ok(_) => {
                            seen.insert(incident.id);
                        }
                        Err(RecoveryError::Busy) => {}
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
                    if let Err(error) = result {
                        remember(last_error, error);
                    }
                }
            }
            Err(error) => remember(last_error, error),
        }
        tokio::select! {
            _ = cancellation.cancelled() => break,
            _ = tokio::time::sleep(interval) => {}
        }
    }
}
