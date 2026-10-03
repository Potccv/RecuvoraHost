//! Host recovery application services, durable aggregate and supervised Core ports.
mod aggregate;
mod contract;
mod dispatch;
mod incident_gate;
mod incident_guard;
mod ownership;
mod platform;
mod scheduler;
mod service;
pub(crate) mod storage_layout;
mod storage_paths;

pub use contract::*;
pub use incident_gate::{IncidentDispatchLease, IncidentGuard, IncidentReadiness};
pub use incident_guard::MonitorIncidentGuard;
pub use ownership::{CanonicalTarget, FileTargetOwnership, TargetLease, TargetOwnership};
pub use recuvora_core::recovery::planning::{ErrorLogEvidence, ProblemOrigin};
pub use scheduler::RecoveryScheduler;
pub use service::RecoveryService;

pub use recuvora_core::recovery::workflow::{
    BusinessVerification, CheckedExecution, ExecutionResultCheck, ProblemContext, RecoveryConfig,
    RecoveryStage, RecoveryTask, RepairExecutionOutcome, RepairReceipt, ResultCheckRecord,
    TargetBinding, TargetObservation,
};

#[derive(Debug, thiserror::Error)]
pub enum RecoveryError {
    #[error("invalid recovery input: {0}")]
    Invalid(String),
    #[error("recovery service: {0}")]
    Service(String),
    #[error("recovery history corrupt: {0}")]
    Corrupt(String),
    #[error("recovery revision conflict")]
    Conflict,
    #[error("target has unfinished recovery work")]
    Busy,
    #[error("recovery service stopped")]
    Stopped,
    #[error("recovery capacity exhausted")]
    Capacity,
    #[error(transparent)]
    Approval(#[from] crate::persistence::approval::ApprovalError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
impl From<recuvora_core::recovery::workflow::RecoveryError> for RecoveryError {
    fn from(value: recuvora_core::recovery::workflow::RecoveryError) -> Self {
        use recuvora_core::recovery::workflow::RecoveryError as E;
        match value {
            E::Busy => Self::Busy,
            E::Conflict => Self::Conflict,
            E::Capacity => Self::Capacity,
            E::Invalid(s) => Self::Invalid(s),
            other => Self::Service(other.to_string()),
        }
    }
}
impl From<recuvora_core::recovery::approval::ApprovalError> for RecoveryError {
    fn from(value: recuvora_core::recovery::approval::ApprovalError) -> Self {
        Self::Approval(value.into())
    }
}
fn service(error: impl std::fmt::Display) -> RecoveryError {
    RecoveryError::Service(error.to_string())
}
fn lock<T>(value: &std::sync::Mutex<T>) -> Result<std::sync::MutexGuard<'_, T>, RecoveryError> {
    value.lock().map_err(|_| service("poisoned recovery lock"))
}
pub trait RecoveryClock: Send + Sync {
    fn now_ms(&self) -> u64;
}
pub struct SystemRecoveryClock;
impl RecoveryClock for SystemRecoveryClock {
    fn now_ms(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(u64::MAX as u128) as u64
    }
}

impl From<recuvora_core::recovery::engine::EngineError> for RecoveryError {
    fn from(value: recuvora_core::recovery::engine::EngineError) -> Self {
        use recuvora_core::recovery::engine::EngineError as E;
        match value {
            E::Conflict => Self::Conflict,
            E::Busy => Self::Busy,
            E::Stopped => Self::Stopped,
            E::Capacity => Self::Capacity,
            E::Invalid(s) => Self::Invalid(s),
            E::Recovery(e) => e.into(),
            E::Approval(e) => e.into(),
            other => service(other),
        }
    }
}
