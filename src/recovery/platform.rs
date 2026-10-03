//! Capabilities and commits injected into the Core recovery engine.
use super::dispatch::ExecutionDispatch;
use super::incident_gate::incident;
use super::*;
use crate::runtime::operation::Cancellation;
use recuvora_core::recovery::{
    approval::{self, ApprovalRecord},
    engine::*,
    knowledge::ExperienceReport,
    workflow::{ExperienceJob, TargetAuthority},
};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

pub(super) struct Platform {
    service: Arc<RecoveryService>,
    cancellation: Cancellation,
    dispatch: Mutex<Option<Arc<ExecutionDispatch>>>,
}
impl Platform {
    pub(super) fn new(service: Arc<RecoveryService>, cancellation: Cancellation) -> Self {
        Self {
            service,
            cancellation,
            dispatch: Mutex::new(None),
        }
    }
}
fn port(error: RecoveryError) -> EngineError {
    match error {
        RecoveryError::Conflict => EngineError::Conflict,
        RecoveryError::Busy => EngineError::Busy,
        RecoveryError::Stopped => EngineError::Stopped,
        RecoveryError::Capacity => EngineError::Capacity,
        RecoveryError::Invalid(s) => EngineError::Invalid(s),
        other => EngineError::Port(other.to_string()),
    }
}
impl RecoveryPlatform for Platform {
    fn config(&self) -> &RecoveryConfig {
        &self.service.config
    }
    fn now_ms(&self) -> u64 {
        self.service.clock.now_ms()
    }
    fn cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }
    fn task(&self, id: &str) -> EngineResult<RecoveryTask> {
        self.service.owner().map_err(port)?;
        self.service.with_state(|s| s.task(id)).map_err(port)
    }
    fn approval(&self, id: &str) -> EngineResult<Option<ApprovalRecord>> {
        self.service.approval(id).map_err(port)
    }
    fn pending_experiences(&self) -> EngineResult<Vec<ExperienceJob>> {
        self.service.pending_experiences().map_err(port)
    }
    fn commit(&self, command: SessionCommand) -> EngineResult<Vec<SessionEffect>> {
        if !matches!(
            &command,
            SessionCommand::BeginSummary { .. }
                | SessionCommand::Summarized { .. }
                | SessionCommand::Deliver
        ) {
            self.service.owner().map_err(port)?;
        }
        self.service
            .with_state(|s| s.apply(command, self.now_ms()))
            .map_err(port)
    }
    fn inspect(&self, timeout: u64) -> CapabilityFuture<'_, TargetObservation> {
        Box::pin(async move {
            bounded(
                self.service
                    .backend
                    .inspect(&self.config().target, self.cancellation.clone()),
                self.cancellation.clone(),
                timeout,
            )
            .await
            .map_err(port)
        })
    }
    fn review(&self, input: ReviewInput, timeout: u64) -> CapabilityFuture<'_, ReviewOutput> {
        Box::pin(async move {
            bounded(
                self.service
                    .backend
                    .review(input, self.cancellation.clone()),
                self.cancellation.clone(),
                timeout,
            )
            .await
            .map_err(port)
        })
    }
    fn acquire_execution<'a>(
        &'a self,
        task: &'a RecoveryTask,
    ) -> CapabilityFuture<'a, ExecutionContext> {
        Box::pin(async move {
            let guard = self
                .service
                .incident_guard
                .get()
                .ok_or_else(|| EngineError::Port("incident guard not bound".into()))?;
            let lease = tokio::select! {lease=guard.acquire_dispatch(&task.problem)=>lease.map_err(port)?,_=self.cancellation.cancelled()=>return Err(EngineError::Stopped)};
            let current = incident(&task.problem, lease.current().map_err(port)?).map_err(port)?;
            self.service.owner().map_err(port)?;
            self.service
                .with_state(|s| {
                    if s.task(&task.id)?.revision != task.revision {
                        return Err(RecoveryError::Conflict);
                    }
                    s.dispatch_phase(
                        task.operation.clone().ok_or(RecoveryError::Conflict)?,
                        false,
                    )
                })
                .map_err(port)?;
            *lock(&self.dispatch).map_err(port)? = Some(Arc::new(ExecutionDispatch {
                service: self.service.clone(),
                task_id: task.id.clone(),
                lease: Mutex::new(Some(lease)),
            }));
            Ok(ExecutionContext {
                incident: current,
                authority: TargetAuthority {
                    target_id: task.problem.target_id.clone(),
                    epoch: format!("owner-{}-{}", std::process::id(), self.now_ms()),
                },
            })
        })
    }
    fn execute<'a>(
        &'a self,
        permit: &'a approval::ExecutionPermit,
        timeout_secs: u64,
    ) -> CapabilityFuture<'a, RepairReceipt> {
        Box::pin(async move {
            let dispatch = lock(&self.dispatch)
                .map_err(port)?
                .clone()
                .ok_or(EngineError::Conflict)?;
            self.service
                .with_state(|s| {
                    s.dispatch_phase(permit.operation().clone(), true)?;
                    s.dispatch_pending = true;
                    Ok(())
                })
                .map_err(port)?;
            bounded(
                self.service.backend.execute(
                    AuthorizedRepair {
                        permit,
                        timeout_secs,
                        dispatch_guard: Some(dispatch),
                    },
                    self.cancellation.clone(),
                ),
                self.cancellation.clone(),
                timeout_secs,
            )
            .await
            .map_err(port)
        })
    }
    fn release_execution(&self) {
        if let Ok(mut dispatch) = self.dispatch.lock()
            && let Some(dispatch) = dispatch.take()
        {
            crate::integrations::extensions::DispatchGuard::release(dispatch.as_ref());
        }
    }
    fn verify(
        &self,
        input: VerificationInput,
        timeout: u64,
    ) -> CapabilityFuture<'_, BusinessVerification> {
        Box::pin(async move {
            bounded(
                self.service
                    .backend
                    .verify(input, self.cancellation.clone()),
                self.cancellation.clone(),
                timeout,
            )
            .await
            .map_err(port)
        })
    }
    fn summarize(
        &self,
        job: ExperienceJob,
        timeout: u64,
    ) -> CapabilityFuture<'_, ExperienceReport> {
        Box::pin(async move {
            bounded(
                self.service.backend.summarize(
                    job,
                    self.config().clone(),
                    self.cancellation.clone(),
                ),
                self.cancellation.clone(),
                timeout,
            )
            .await
            .map_err(port)
        })
    }
}

async fn bounded<T>(
    future: RecoveryFuture<'_, T>,
    cancellation: Cancellation,
    seconds: u64,
) -> Result<T, RecoveryError> {
    tokio::pin!(future);
    tokio::select! {
        result = &mut future => result,
        _ = tokio::time::sleep(Duration::from_secs(seconds)) => { cancellation.cancel(); future.await }
    }
}
