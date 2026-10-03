//! Transport-independent application services, receipts and lifecycle.
mod error;
mod host;
pub(crate) mod journal;
mod operations;
pub use error::{ApplicationError, ApplicationErrorKind};
pub use host::{HostError, HostRuntime};

use crate::harnesses::{HarnessCancellation, HarnessRegistry};
use crate::integrations::extensions::ExtensionRegistry;
use crate::integrations::recovery::NodeRepairBackend;
use crate::monitoring::MonitorHandle;
use crate::recovery::RecoveryService;
use crate::repair::{RepairConfig, RepairSession};
use crate::simulation::EngineHandle;
use journal::Journal;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Operation {
    pub(crate) id: String,
    pub(crate) kind: String,
    pub(crate) status: String,
    #[serde(rename = "updatedAt")]
    pub(crate) updated_at: u64,
    pub(crate) result: Option<Value>,
    pub(crate) error: Option<String>,
    pub(crate) auto_retry: bool,
    pub(crate) context: Value,
}

/// One owner of application capabilities and accepted operation lifetimes.
pub struct Application {
    host: tokio::sync::Mutex<HostRuntime>,
    accepting: std::sync::atomic::AtomicBool,
    registry: Option<Arc<HarnessRegistry>>,
    extensions: Option<Arc<ExtensionRegistry>>,
    monitoring: Option<Arc<MonitorHandle>>,
    text_repair: Option<Arc<RepairSession>>,
    text_repair_config: Option<RepairConfig>,
    recovery: Option<Arc<RecoveryService>>,
    recovery_backend: Mutex<Option<Arc<NodeRepairBackend>>>,
    simulation: EngineHandle,
    journal: Mutex<Journal>,
    calls: Mutex<BTreeMap<String, HarnessCancellation>>,
}

pub(crate) struct ApplicationParts {
    pub host: HostRuntime,
    pub text_repair: Option<Arc<RepairSession>>,
    pub text_repair_config: Option<RepairConfig>,
    pub recovery: Option<Arc<RecoveryService>>,
    pub simulation: EngineHandle,
    pub journal: Journal,
}

/// Read-only access keeps projections under the bounded journal lock.
pub(crate) struct OperationRead<'a> {
    journal: MutexGuard<'a, Journal>,
}
impl OperationRead<'_> {
    pub fn records(&self) -> &BTreeMap<String, Operation> {
        &self.journal.records
    }
    pub fn events(&self) -> &[Operation] {
        &self.journal.events
    }
}

impl Application {
    pub(crate) fn from_parts(parts: ApplicationParts) -> Arc<Self> {
        Arc::new(Self {
            recovery_backend: Mutex::new(parts.host.recovery_backend()),
            registry: parts.host.harnesses(),
            extensions: parts.host.extensions(),
            monitoring: parts.host.monitoring(),
            host: tokio::sync::Mutex::new(parts.host),
            accepting: std::sync::atomic::AtomicBool::new(true),
            text_repair: parts.text_repair,
            text_repair_config: parts.text_repair_config,
            recovery: parts.recovery,
            simulation: parts.simulation,
            journal: Mutex::new(parts.journal),
            calls: Mutex::new(BTreeMap::new()),
        })
    }
    pub fn harnesses(&self) -> Option<&Arc<HarnessRegistry>> {
        self.registry.as_ref()
    }
    pub fn extensions(&self) -> Option<&Arc<ExtensionRegistry>> {
        self.extensions.as_ref()
    }
    pub fn monitoring(&self) -> Option<&Arc<MonitorHandle>> {
        self.monitoring.as_ref()
    }
    pub fn text_repair(&self) -> Option<&Arc<RepairSession>> {
        self.text_repair.as_ref()
    }
    pub fn text_repair_config(&self) -> Option<&RepairConfig> {
        self.text_repair_config.as_ref()
    }
    pub fn recovery(&self) -> Option<&Arc<RecoveryService>> {
        self.recovery.as_ref()
    }
    pub fn simulation(&self) -> &EngineHandle {
        &self.simulation
    }
    pub(crate) fn ensure_accepting(&self) -> Result<(), ApplicationError> {
        if self.accepting.load(std::sync::atomic::Ordering::Acquire) {
            Ok(())
        } else {
            Err(ApplicationError::unavailable("host is shutting down"))
        }
    }
    pub(crate) fn operations(&self) -> Result<OperationRead<'_>, ApplicationError> {
        Ok(OperationRead {
            journal: lock(&self.journal)?,
        })
    }
    pub(crate) fn cancel(&self, id: &str) -> Result<(), ApplicationError> {
        let calls = lock(&self.calls)?;
        let token = calls.get(id).ok_or_else(|| {
            ApplicationError::conflict("operation is no longer active; query its recorded outcome")
        })?;
        token.cancel();
        Ok(())
    }
    pub async fn recovery_status(&self) -> Result<(bool, Option<String>), ApplicationError> {
        let host = self.host.lock().await;
        let scheduler = host
            .recovery_scheduler()
            .ok_or_else(|| ApplicationError::unavailable("recovery service is not configured"))?;
        Ok((scheduler.is_running(), scheduler.last_error()?))
    }
    fn result_backend(&self) -> Result<Arc<NodeRepairBackend>, ApplicationError> {
        lock(&self.recovery_backend)?
            .clone()
            .ok_or_else(|| ApplicationError::unavailable("recovery executor unavailable"))
    }
    pub(crate) fn start_recovery_result_check(
        self: &Arc<Self>,
        operation_id: String,
        id: String,
        revision: u64,
        actor: String,
    ) -> Result<(), ApplicationError> {
        let recovery = self
            .recovery
            .clone()
            .ok_or_else(|| ApplicationError::unavailable("recovery service is not configured"))?;
        let task = recovery.query(&id)?.ok_or_else(|| {
            ApplicationError::new(ApplicationErrorKind::NotFound, "recovery task not found")
        })?;
        if task.revision != revision || task.stage != crate::recovery::RecoveryStage::Unknown {
            return Err(ApplicationError::conflict(
                "result checking requires the current Unknown task revision",
            ));
        }
        let backend = self.result_backend()?;
        let cancellation = self.begin(
            operation_id.clone(),
            "recovery_check_result",
            json!({"taskId":id,"revision":revision}),
        )?;
        let application = self.clone();
        tokio::spawn(async move {
            let result = backend
                .check_task_result(&recovery, &id, revision, actor, cancellation)
                .await;
            application.finish(
                &operation_id,
                result.map_err(|error| error.to_string()).and_then(|task| {
                    serde_json::to_value(task)
                        .map(|task| json!({"task":task,"auto_retry":false}))
                        .map_err(|error| error.to_string())
                }),
            );
        });
        Ok(())
    }
    pub(crate) fn begin(
        &self,
        id: String,
        kind: &str,
        context: Value,
    ) -> Result<HarnessCancellation, ApplicationError> {
        if !valid_id(&id) {
            return Err(ApplicationError::invalid("invalid operation_id"));
        }
        let mut journal = lock(&self.journal)?;
        if journal.records.contains_key(&id) {
            return Err(ApplicationError::conflict(
                "operation_id already accepted; query its result",
            ));
        }
        if kind == "repair"
            && context["taskId"].as_str().is_some_and(|task_id| {
                journal.records.values().any(|operation| {
                    operation.kind == "repair" && operation.context["taskId"] == task_id
                })
            })
        {
            return Err(ApplicationError::conflict(
                "repair task_id already accepted; inspect its existing repair instead of replaying it",
            ));
        }
        if journal.records.len() >= 1000 {
            return Err(ApplicationError::unavailable("operation capacity reached"));
        }
        let mut calls = lock(&self.calls)?;
        if !self.accepting.load(std::sync::atomic::Ordering::Acquire) {
            return Err(ApplicationError::unavailable("host is shutting down"));
        }
        if calls.len() >= 8 {
            return Err(ApplicationError::new(
                ApplicationErrorKind::Capacity,
                "too many active operations",
            ));
        }
        let token = HarnessCancellation::new();
        journal.append(Operation {
            id: id.clone(),
            kind: kind.into(),
            status: "running".into(),
            updated_at: timestamp(),
            result: None,
            error: None,
            auto_retry: false,
            context,
        })?;
        calls.insert(id, token.clone());
        Ok(token)
    }
    pub(crate) fn finish(&self, id: &str, result: Result<Value, String>) {
        if let Ok(mut journal) = lock(&self.journal)
            && let Some(mut op) = journal.records.get(id).cloned()
        {
            match result {
                Ok(value) => {
                    op.status = match value.get("status").and_then(Value::as_str) {
                        Some("unknown") => "unknown",
                        Some("canceled") => "canceled",
                        Some("failed") => "failed",
                        _ => "completed",
                    }
                    .into();
                    op.result = Some(value);
                }
                Err(message) => {
                    op.status = "unknown".into();
                    op.error = Some(message);
                }
            }
            op.updated_at = timestamp();
            if journal.append(op.clone()).is_err() {
                op.status = "unknown".into();
                op.result = None;
                op.error = Some("completion could not be persisted; do not retry".into());
                journal.records.insert(id.into(), op);
            }
        }
        if let Ok(mut calls) = lock(&self.calls) {
            calls.remove(id);
        }
    }
    pub async fn shutdown(&self) -> Result<(), ApplicationError> {
        self.wait_for_idle().await;
        self.host
            .lock()
            .await
            .shutdown()
            .await
            .map_err(|error| ApplicationError::unavailable(error.to_string()))?;
        lock(&self.recovery_backend)?.take();
        if !lock(&self.calls)?.is_empty() {
            return Err(ApplicationError::unavailable(
                "operation receipts remain pending after shutdown",
            ));
        }
        Ok(())
    }
    pub async fn wait_for_idle(&self) {
        self.accepting
            .store(false, std::sync::atomic::Ordering::Release);
        self.host.lock().await.begin_shutdown();
        if let Ok(calls) = lock(&self.calls) {
            for token in calls.values() {
                token.cancel();
            }
        }
        for _ in 0..400 {
            if lock(&self.calls).is_ok_and(|c| c.is_empty()) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

pub(crate) fn timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}
pub(crate) fn valid_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.:".contains(&c))
}
fn lock<T>(m: &Mutex<T>) -> Result<MutexGuard<'_, T>, ApplicationError> {
    m.lock()
        .map_err(|_| ApplicationError::unavailable("service lock poisoned"))
}

#[cfg(test)]
#[path = "../../tests/application_support.rs"]
mod test_support;

#[cfg(test)]
#[path = "../../tests/application.rs"]
mod tests;
