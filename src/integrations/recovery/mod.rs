//! Host adapters that bind the Core recovery state machine to external nodes.

mod incident_guard;
mod node_backend;
mod scheduler;
mod service;

pub use incident_guard::MonitorIncidentGuard;
pub use node_backend::NodeRepairBackend;
pub use scheduler::{IncidentTrigger, RecoveryScheduler};
pub use service::RecoveryService;

pub use recuvora_core::recovery::workflow::{
    AuthorizedScript, BusinessVerification, CanonicalTarget, CheckedExecution, DiagnosisInput,
    ExecutionResultCheck, FileTargetOwnership, IncidentGuard, IncidentReadiness, ProblemContext,
    RecoveryClock, RecoveryConfig, RecoveryError, RecoveryFuture, RecoveryStage, RecoveryTask,
    RepairBackend, RepairPlan, ResultCheckRecord, ReviewInput, ReviewOutput, ScriptOutcome,
    ScriptReceipt, SystemRecoveryClock, TargetBinding, TargetLease, TargetObservation,
    TargetOwnership, VerificationInput,
};
