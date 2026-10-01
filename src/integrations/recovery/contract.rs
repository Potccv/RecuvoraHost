use super::*;
use crate::runtime::operation::Cancellation;
use recuvora_core::recovery::approval;
use recuvora_core::recovery::knowledge::KnowledgeCandidate;
use std::{future::Future, pin::Pin};
#[derive(Clone, Debug)]
pub struct DiagnosisInput {
    pub task: RecoveryTask,
    pub config: RecoveryConfig,
    pub observation: TargetObservation,
    /// Applicable verified cases are evidence for a new plan, never authority.
    pub knowledge: Vec<KnowledgeCandidate>,
}
#[derive(Clone, Debug)]
pub struct ReviewInput {
    pub request: approval::ApprovalRequest,
    pub attempt: approval::ReviewAttempt,
    pub observation: TargetObservation,
    pub reused_script: bool,
}
#[derive(Clone, Debug)]
pub struct ReviewOutput {
    pub assessment: approval::ModelAssessment,
    pub identity: approval::ReviewerIdentity,
}
#[derive(Clone, Debug)]
pub struct VerificationInput {
    pub target: TargetBinding,
    pub operation: approval::ProposedOperation,
    pub receipt: ScriptReceipt,
}

/// Only the Host workflow adapter constructs this value after confirming the
/// Core execution authorization. It cannot be cloned/deserialized and grants
/// one dispatch to a trusted backend, subject to the final dispatch gate.
pub struct AuthorizedScript<'a> {
    pub(super) permit: &'a approval::ExecutionPermit,
    pub(super) timeout_secs: u64,
    pub(super) dispatch_guard:
        Option<std::sync::Arc<dyn crate::integrations::extensions::DispatchGuard>>,
}
impl AuthorizedScript<'_> {
    pub fn operation(&self) -> &approval::ProposedOperation {
        self.permit.operation()
    }
    pub fn request_id(&self) -> &str {
        self.permit.request_id()
    }
    pub fn timeout_secs(&self) -> u64 {
        self.timeout_secs
    }
    /// Recheck current authority immediately before the actual external send,
    /// after any connection setup or local asynchronous preparation. Calling
    /// this method is not dispatch and never grants a second execution permit.
    pub fn validate_dispatch(&self) -> Result<(), RecoveryError> {
        self.dispatch_guard
            .as_ref()
            .ok_or_else(|| service("execution dispatch guard required"))?
            .validate()
            .map_err(service)
    }
    /// Release the mutation gate as soon as the send completes or fails, and on
    /// every pre-dispatch rejection. Continue supervising the original execution
    /// after release; a failed send does not establish that it never executed.
    pub fn finish_dispatch(&self) {
        if let Some(guard) = &self.dispatch_guard {
            guard.release();
        }
    }
    /// Trusted backends must validate immediately before dispatch and release
    /// after sending, including every pre-dispatch error path.
    pub fn dispatch_guard(
        &self,
    ) -> Option<std::sync::Arc<dyn crate::integrations::extensions::DispatchGuard>> {
        self.dispatch_guard.clone()
    }
}

pub type RecoveryFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, RecoveryError>> + Send + 'a>>;

/// Trusted integration boundary. Model output must never implement this interface.
/// Methods must bound work, observe cancellation, and drain before returning;
/// execute must report Unknown unless all executors are known to have stopped.
/// Custom `execute` implementations must call `script.validate_dispatch()` at
/// the actual external dispatch boundary and `script.finish_dispatch()` after
/// sending or rejecting, including error paths. Polling once or queuing work is
/// not a final validation. The Host fallback retains the gate until the backend
/// returns if it fails to release early; it cannot perform that final validation
/// on behalf of a custom backend.
pub trait RepairBackend: Send + Sync {
    fn inspect<'a>(
        &'a self,
        target: &'a TargetBinding,
        cancellation: Cancellation,
    ) -> RecoveryFuture<'a, TargetObservation>;
    fn diagnose(
        &self,
        input: DiagnosisInput,
        cancellation: Cancellation,
    ) -> RecoveryFuture<'_, RepairPlan>;
    fn review(
        &self,
        input: ReviewInput,
        cancellation: Cancellation,
    ) -> RecoveryFuture<'_, ReviewOutput>;
    fn execute<'a>(
        &'a self,
        script: AuthorizedScript<'a>,
        cancellation: Cancellation,
    ) -> RecoveryFuture<'a, ScriptReceipt>;
    fn verify(
        &self,
        input: VerificationInput,
        cancellation: Cancellation,
    ) -> RecoveryFuture<'_, BusinessVerification>;
}
