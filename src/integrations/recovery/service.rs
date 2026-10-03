//! Durable storage and supervised capabilities injected into the Core engine.
use super::*;
use crate::persistence::{
    approval::ApprovalStoreConfig, journal::Journal, knowledge::KnowledgeStoreConfig,
};
use crate::runtime::operation::{CallScope, Cancellation};
use recuvora_core::recovery::{
    approval::{self, ApprovalDecision, ApprovalRecord},
    engine::*,
    knowledge::{ExperienceReport, KnowledgeConfig, KnowledgeQuery},
    workflow::{ExperienceJob, IncidentEvidence, TargetAuthority},
};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

/// Host I/O evidence only. A dispatched marker is synced before entering the
/// backend, so a prepared marker alone proves that this Host did not dispatch.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DispatchRecord {
    operation: approval::ProposedOperation,
    dispatching: bool,
}

struct State {
    root_lock: storage_layout::RootStorageLock,
    session: RecoverySession,
    journal: Journal,
    dispatch: Journal,
    dispatch_pending: bool,
}
impl State {
    fn dispatch_phase(
        &mut self,
        operation: approval::ProposedOperation,
        dispatching: bool,
    ) -> Result<(), RecoveryError> {
        let record = DispatchRecord {
            operation,
            dispatching,
        };
        let payload = serde_json::to_value(record)?;
        let request = recuvora_core::operation::CommitRequest::new(
            self.dispatch.next_id(),
            self.dispatch.revision(),
            "dispatch".into(),
            payload.clone(),
        )
        .map_err(service)?;
        self.dispatch.commit(&request, payload).map_err(service)?;
        Ok(())
    }
    fn not_dispatched(
        &mut self,
        id: &str,
        config: &RecoveryConfig,
        now: u64,
    ) -> Result<(), RecoveryError> {
        let task = self.task(id)?;
        if task.stage != RecoveryStage::Unknown {
            return Ok(());
        }
        let operation = task
            .operation
            .as_ref()
            .ok_or_else(|| service("missing original operation"))?;
        let record = self.dispatch.records().iter().rev().find_map(|entry| {
            let phase: DispatchRecord = serde_json::from_value(entry.payload.clone()).ok()?;
            (phase.operation == *operation).then_some((entry.request.id.clone(), phase))
        });
        let Some((commit_id, phase)) = record else {
            return Ok(());
        };
        if phase.dispatching {
            return Ok(());
        }
        let evidence_refs = vec![format!("host-dispatch:{commit_id}:not-dispatched")];
        let execution = ExecutionResultCheck {
            operation_id: operation.operation_id.clone(),
            target_id: operation.target.clone(),
            executor_id: config.target.executor_id.clone(),
            outcome: CheckedExecution::NotExecuted,
            executor_stopped: true,
            evidence_refs: evidence_refs.clone(),
            checked_at_ms: now,
        };
        let verification = BusinessVerification {
            operation_id: operation.operation_id.clone(),
            target_id: operation.target.clone(),
            profile: config.target.verification_profile.clone(),
            healthy: None,
            executor_stopped: true,
            evidence_refs,
            verified_at_ms: now,
        };
        self.apply(
            SessionCommand::CheckResult {
                task_id: id.into(),
                revision: task.revision,
                execution,
                verification,
                actor: "host-dispatch-recovery".into(),
            },
            now,
        )?;
        #[cfg(test)]
        crash_boundary("not_dispatched_committed");
        Ok(())
    }
    fn apply(
        &mut self,
        command: SessionCommand,
        now: u64,
    ) -> Result<Vec<SessionEffect>, RecoveryError> {
        self.root_lock.validate()?;
        #[cfg(test)]
        let boundary = match &command {
            SessionCommand::Start { .. } => "start_committed",
            SessionCommand::Authorize { .. } => "authorization_committed",
            SessionCommand::Executed { .. } => "receipt_committed",
            SessionCommand::Verified { .. } => "verification_committed",
            SessionCommand::CheckResult { .. } => "result_check_committed",
            SessionCommand::Deliver => "experience_committed",
            _ => "",
        };
        let pending = self.session.prepare(self.journal.next_id(), command, now)?;
        let entry = pending
            .state()
            .latest_entry()
            .ok_or_else(|| service("missing session entry"))?;
        let receipt = self
            .journal
            .commit(pending.request(), serde_json::to_value(entry)?)
            .map_err(service)?;
        let committed = pending.confirm(receipt).map_err(service)?;
        self.session = committed.state;
        #[cfg(test)]
        crash_boundary(boundary);
        Ok(committed.effects)
    }
    fn task(&self, id: &str) -> Result<RecoveryTask, RecoveryError> {
        self.session
            .task(id)
            .cloned()
            .ok_or_else(|| RecoveryError::Invalid("task not found".into()))
    }
}

/// Owns protected storage, trusted ports and supervised calls; Core owns transitions.
pub struct RecoveryService {
    config: RecoveryConfig,
    backend: Arc<dyn RepairBackend>,
    clock: Arc<dyn RecoveryClock>,
    state: Mutex<Option<State>>,
    active: Mutex<BTreeSet<String>>,
    calls: CallScope,
    accepting: AtomicBool,
    incident_guard: OnceLock<Arc<dyn IncidentGuard>>,
    state_directory: PathBuf,
    ownership: Mutex<Option<Box<dyn TargetLease>>>,
}
impl RecoveryService {
    pub fn open(
        dir: impl AsRef<Path>,
        config: RecoveryConfig,
        backend: Arc<dyn RepairBackend>,
    ) -> Result<Arc<Self>, RecoveryError> {
        Self::open_with_clock_and_store_configs(
            dir,
            config,
            backend,
            Arc::new(SystemRecoveryClock),
            ApprovalStoreConfig::default(),
            KnowledgeStoreConfig::default(),
        )
    }
    pub fn open_with_store_configs(
        dir: impl AsRef<Path>,
        config: RecoveryConfig,
        backend: Arc<dyn RepairBackend>,
        approvals: ApprovalStoreConfig,
        knowledge: KnowledgeStoreConfig,
    ) -> Result<Arc<Self>, RecoveryError> {
        Self::open_with_clock_and_store_configs(
            dir,
            config,
            backend,
            Arc::new(SystemRecoveryClock),
            approvals,
            knowledge,
        )
    }
    pub fn open_with_clock(
        dir: impl AsRef<Path>,
        config: RecoveryConfig,
        backend: Arc<dyn RepairBackend>,
        clock: Arc<dyn RecoveryClock>,
    ) -> Result<Arc<Self>, RecoveryError> {
        Self::open_with_clock_and_store_configs(
            dir,
            config,
            backend,
            clock,
            ApprovalStoreConfig::default(),
            KnowledgeStoreConfig::default(),
        )
    }
    pub fn open_with_clock_and_store_configs(
        dir: impl AsRef<Path>,
        config: RecoveryConfig,
        backend: Arc<dyn RepairBackend>,
        clock: Arc<dyn RecoveryClock>,
        approval_config: ApprovalStoreConfig,
        knowledge_config: KnowledgeStoreConfig,
    ) -> Result<Arc<Self>, RecoveryError> {
        config.validate()?;
        let root = dir.as_ref();
        let mut root_lock = storage_layout::RootStorageLock::acquire(root)?;
        let directory = root_lock.resolve(root)?;
        let dir = directory.as_path();
        approval_config.validate()?;
        knowledge_config.validate().map_err(service)?;
        let session_config = SessionConfig {
            recovery: config.clone(),
            approvals: approval::ApprovalLimits {
                max_requests: approval_config.max_requests,
            },
            knowledge: KnowledgeConfig {
                max_records: knowledge_config.max_records,
            },
        };
        // The aggregate honors the strictest configured journal ceiling.
        let max_bytes = (256 * 1024 * 1024)
            .min(approval_config.max_journal_bytes)
            .min(knowledge_config.max_journal_bytes);
        let storage_config = serde_json::json!({"engine":"recovery-session-v1","session":session_config,"approval_storage":approval_config,"knowledge_storage":knowledge_config,"backend":backend.persistence_binding()});
        let journal = Journal::open(
            dir.join("recovery.jsonl"),
            "recovery-session",
            storage_config.clone(),
            max_bytes,
        )
        .map_err(service)?;
        let entries: Vec<SessionEntry> = journal
            .records()
            .iter()
            .map(|record| {
                let entry: SessionEntry = serde_json::from_value(record.payload.clone())?;
                if entry.request != record.request {
                    return Err(RecoveryError::Corrupt(
                        "session payload and commit request differ".into(),
                    ));
                }
                Ok(entry)
            })
            .collect::<Result<_, RecoveryError>>()?;
        let session = RecoverySession::restore(session_config, &entries)?;
        let dispatch = Journal::open(
            dir.join("dispatch.jsonl"),
            "dispatch",
            storage_config,
            64 * 1024 * 1024,
        )
        .map_err(service)?;
        for entry in dispatch.records() {
            let phase: DispatchRecord = serde_json::from_value(entry.payload.clone())?;
            if entry.request.input != entry.payload {
                return Err(RecoveryError::Corrupt(
                    "dispatch payload differs from commit".into(),
                ));
            }
            phase.operation.validate()?;
        }
        let mut state = State {
            root_lock,
            session,
            journal,
            dispatch,
            dispatch_pending: false,
        };
        if state.session.recovery_required() {
            state.apply(SessionCommand::Recover, clock.now_ms())?;
            let ids: Vec<_> = state.session.tasks().map(|task| task.id.clone()).collect();
            for id in ids {
                state.not_dispatched(&id, &config, clock.now_ms())?;
            }
        }
        Ok(Arc::new(Self {
            config,
            backend,
            clock,
            state: Mutex::new(Some(state)),
            active: Mutex::new(BTreeSet::new()),
            calls: CallScope::default(),
            accepting: AtomicBool::new(true),
            incident_guard: OnceLock::new(),
            state_directory: root.to_path_buf(),
            ownership: Mutex::new(None),
        }))
    }
    pub fn config(&self) -> &RecoveryConfig {
        &self.config
    }
    pub fn bind_target_ownership(
        &self,
        authority: Arc<dyn TargetOwnership>,
    ) -> Result<(), RecoveryError> {
        self.accepting()?;
        let mut owner = lock(&self.ownership)?;
        if owner.is_some() {
            return Err(RecoveryError::Invalid(
                "target ownership already bound".into(),
            ));
        }
        let target = CanonicalTarget::new(self.config.target.target_id.clone())?;
        let lease = authority.acquire(&target, &self.state_directory)?;
        if lease.target() != &target
            || lease.recovery_directory() != self.state_directory.canonicalize()?
        {
            return Err(RecoveryError::Invalid(
                "ownership target or storage mismatch".into(),
            ));
        }
        lease.validate()?;
        *owner = Some(lease);
        Ok(())
    }
    pub fn bind_incident_guard(&self, guard: Arc<dyn IncidentGuard>) -> Result<(), RecoveryError> {
        self.accepting()?;
        self.incident_guard
            .set(guard)
            .map_err(|_| RecoveryError::Invalid("incident guard already bound".into()))
    }
    fn accepting(&self) -> Result<(), RecoveryError> {
        if self.accepting.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err(RecoveryError::Stopped)
        }
    }
    fn owner(&self) -> Result<(), RecoveryError> {
        lock(&self.ownership)?
            .as_ref()
            .ok_or_else(|| service("target ownership not bound"))?
            .validate()
    }
    fn with_state<T>(
        &self,
        f: impl FnOnce(&mut State) -> Result<T, RecoveryError>,
    ) -> Result<T, RecoveryError> {
        let mut state = lock(&self.state)?;
        let state = state.as_mut().ok_or(RecoveryError::Stopped)?;
        state.root_lock.validate()?;
        if state.dispatch_pending {
            return Err(RecoveryError::Busy);
        }
        f(state)
    }
    fn read_state<T>(
        &self,
        f: impl FnOnce(&State) -> Result<T, RecoveryError>,
    ) -> Result<T, RecoveryError> {
        let state = lock(&self.state)?;
        f(state.as_ref().ok_or(RecoveryError::Stopped)?)
    }
    pub fn query(&self, id: &str) -> Result<Option<RecoveryTask>, RecoveryError> {
        self.read_state(|s| Ok(s.session.task(id).cloned()))
    }
    pub fn tasks(&self) -> Result<Vec<RecoveryTask>, RecoveryError> {
        self.read_state(|s| Ok(s.session.tasks().cloned().collect()))
    }
    pub fn experiences(
        &self,
        query: &KnowledgeQuery,
    ) -> Result<Vec<recuvora_core::recovery::knowledge::RepairExperience>, RecoveryError> {
        self.read_state(|s| Ok(s.session.experiences(query)?))
    }

    /// Retry committed experience delivery even when every business task ended.
    /// This never reconstructs execution effects or changes operation identity.
    pub fn deliver_pending(&self) -> Result<(), RecoveryError> {
        self.accepting()?;
        self.owner()?;
        self.with_state(|s| {
            s.apply(SessionCommand::Deliver, self.clock.now_ms())?;
            Ok(())
        })
    }
    pub fn submit(&self, problem: ProblemContext) -> Result<RecoveryTask, RecoveryError> {
        self.accepting()?;
        self.owner()?;
        let guard = self
            .incident_guard
            .get()
            .ok_or_else(|| service("incident guard not bound"))?;
        let mut result = None;
        guard.with_current(&problem, &mut |current| {
            if result.is_some() {
                return Err(service("incident guard repeated callback"));
            }
            let incident = incident(&problem, current)?;
            result = Some(self.with_state(|s| {
                s.apply(
                    SessionCommand::Register {
                        problem: problem.clone(),
                        incident,
                    },
                    self.clock.now_ms(),
                )?;
                s.session
                    .tasks()
                    .find(|t| t.problem.incident_id == problem.incident_id)
                    .cloned()
                    .ok_or_else(|| service("registered task missing"))
            })?);
            Ok(())
        })?;
        result.ok_or_else(|| service("incident guard did not check facts"))
    }
    pub fn approval(&self, id: &str) -> Result<Option<ApprovalRecord>, RecoveryError> {
        self.read_state(|s| {
            s.task(id)?;
            Ok(s.session.approval(id).cloned())
        })
    }
    pub fn decide_human(
        &self,
        id: &str,
        revision: u64,
        decision: ApprovalDecision,
        actor: String,
        reason: String,
    ) -> Result<ApprovalRecord, RecoveryError> {
        self.accepting()?;
        self.owner()?;
        self.with_state(|s| {
            s.apply(
                SessionCommand::HumanDecision {
                    task_id: id.into(),
                    revision,
                    decision,
                    actor,
                    reason,
                },
                self.clock.now_ms(),
            )?;
            s.session
                .approval(id)
                .cloned()
                .ok_or_else(|| service("missing approval"))
        })
    }
    pub fn resume(&self, id: &str, revision: u64) -> Result<RecoveryTask, RecoveryError> {
        self.accepting()?;
        self.owner()?;
        self.with_state(|s| {
            s.apply(
                SessionCommand::Resume {
                    task_id: id.into(),
                    revision,
                },
                self.clock.now_ms(),
            )?;
            s.task(id)
        })
    }
    pub async fn advance(
        self: &Arc<Self>,
        id: &str,
        cancellation: Cancellation,
    ) -> Result<RecoveryTask, RecoveryError> {
        self.accepting()?;
        self.owner()?;
        let service = self.clone();
        let id = id.to_owned();
        let cancel = cancellation.clone();
        self.calls
            .run(cancellation, async move {
                if !lock(&service.active)?.insert(id.clone()) {
                    return Err(RecoveryError::Busy);
                }
                let _active = Active {
                    service: service.clone(),
                    id: id.clone(),
                };
                RecoveryEngine::advance(&Platform::new(service.clone(), cancel), &id)
                    .await
                    .map_err(Into::into)
            })
            .await
            .map_err(super::service)?
    }
    /// Retry only post-repair summaries and delivery; never redispatch repair work.
    pub fn summarize_pending(
        self: &Arc<Self>,
        cancellation: Cancellation,
    ) -> RecoveryFuture<'_, ()> {
        Box::pin(self.summarize_pending_with_retry(cancellation, false))
    }
    /// Explicit operator retry after the automatic three-attempt summary budget.
    pub fn retry_experiences(
        self: &Arc<Self>,
        cancellation: Cancellation,
    ) -> RecoveryFuture<'_, ()> {
        Box::pin(self.summarize_pending_with_retry(cancellation, true))
    }
    async fn summarize_pending_with_retry(
        self: &Arc<Self>,
        cancellation: Cancellation,
        explicit: bool,
    ) -> Result<(), RecoveryError> {
        self.accepting()?;
        let service = self.clone();
        let cancel = cancellation.clone();
        self.calls
            .run(cancellation, async move {
                RecoveryEngine::summarize_pending(&Platform::new(service, cancel), explicit)
                    .await
                    .map_err(RecoveryError::from)
            })
            .await
            .map_err(super::service)?
    }
    pub fn pending_experiences(
        &self,
    ) -> Result<Vec<recuvora_core::recovery::workflow::ExperienceJob>, RecoveryError> {
        self.read_state(|s| Ok(s.session.pending_experiences()))
    }
    pub fn check_result(
        &self,
        id: &str,
        revision: u64,
        execution: ExecutionResultCheck,
        verification: BusinessVerification,
        actor: String,
    ) -> Result<RecoveryTask, RecoveryError> {
        self.accepting()?;
        self.owner()?;
        self.with_state(|s| {
            s.apply(
                SessionCommand::CheckResult {
                    task_id: id.into(),
                    revision,
                    execution,
                    verification,
                    actor,
                },
                self.clock.now_ms(),
            )?;
            s.task(id)
        })
    }
    pub async fn shutdown(&self) -> Result<(), RecoveryError> {
        self.accepting.store(false, Ordering::Release);
        self.calls.shutdown().await.map_err(service)?;
        let mut state = lock(&self.state)?;
        if let Some(current) = state.as_mut() {
            current.root_lock.validate()?;
            // A cached terminal state cannot release ownership after an uncertain
            // write or external file change in any participating aggregate.
            current.journal.prepare_close().map_err(service)?;
            current.dispatch.prepare_close().map_err(service)?;
            let terminal = current.session.releasable();
            if terminal && let Some(lease) = lock(&self.ownership)?.as_mut() {
                lease.release()?;
            }
            current.journal.finish_close();
            current.dispatch.finish_close();
            state.take();
            lock(&self.ownership)?.take();
        }
        Ok(())
    }
}
impl Drop for RecoveryService {
    fn drop(&mut self) {
        self.accepting.store(false, Ordering::Release);
        let _ = self.calls.close();
    }
}

struct Platform {
    service: Arc<RecoveryService>,
    cancellation: Cancellation,
    dispatch: Mutex<Option<Arc<ExecutionDispatch>>>,
}
impl Platform {
    fn new(service: Arc<RecoveryService>, cancellation: Cancellation) -> Self {
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
struct ExecutionDispatch {
    service: Arc<RecoveryService>,
    task_id: String,
    lease: Mutex<Option<Box<dyn IncidentDispatchLease>>>,
}
impl crate::integrations::extensions::DispatchGuard for ExecutionDispatch {
    fn prepare_repair_action(
        &self,
        action: &recuvora_core::recovery::knowledge::RepairArtifact,
    ) -> Result<(), crate::integrations::extensions::ExtensionError> {
        use crate::integrations::extensions::ExtensionError;
        self.validate()?;
        let mut state =
            lock(&self.service.state).map_err(|e| ExtensionError::Rejected(e.to_string()))?;
        let state = state
            .as_mut()
            .ok_or_else(|| ExtensionError::Rejected("service stopped".into()))?;
        let task = state
            .task(&self.task_id)
            .map_err(|e| ExtensionError::Rejected(e.to_string()))?;
        state
            .apply(
                SessionCommand::PrepareAction {
                    task_id: task.id,
                    action: action.clone(),
                },
                self.service.clock.now_ms(),
            )
            .map_err(|e| ExtensionError::Rejected(e.to_string()))?;
        Ok(())
    }
    fn validate(&self) -> Result<(), crate::integrations::extensions::ExtensionError> {
        let check = || -> Result<(), RecoveryError> {
            let lease = lock(&self.lease)?;
            let lease = lease
                .as_ref()
                .ok_or_else(|| service("dispatch gate released"))?;
            self.service.owner()?;
            let mut state = lock(&self.service.state)?;
            let state = state.as_mut().ok_or(RecoveryError::Stopped)?;
            state.root_lock.validate()?;
            let task = state.task(&self.task_id)?;
            let incident = incident(&task.problem, lease.current()?)?;
            if !state.dispatch_pending {
                return Err(RecoveryError::Conflict);
            }
            state.journal.available().map_err(service)?;
            state.dispatch.available().map_err(service)?;
            state.session.validate_dispatch(
                &self.task_id,
                &incident,
                self.service.clock.now_ms(),
            )?;
            Ok(())
        };
        check().map_err(|error| {
            crate::integrations::extensions::ExtensionError::Rejected(error.to_string())
        })
    }
    fn release(&self) {
        if let Ok(mut lease) = self.lease.lock()
            && lease.is_some()
        {
            if let Ok(mut state) = self.service.state.lock()
                && let Some(state) = state.as_mut()
            {
                state.dispatch_pending = false;
            }
            lease.take();
        }
    }
}
impl Drop for ExecutionDispatch {
    fn drop(&mut self) {
        crate::integrations::extensions::DispatchGuard::release(self);
    }
}
struct Active {
    service: Arc<RecoveryService>,
    id: String,
}
impl Drop for Active {
    fn drop(&mut self) {
        if let Ok(mut active) = self.service.active.lock() {
            active.remove(&self.id);
        }
    }
}
fn incident(
    problem: &ProblemContext,
    current: IncidentReadiness,
) -> Result<IncidentEvidence, RecoveryError> {
    match current {
        IncidentReadiness::Active { revision } if revision >= problem.incident_revision => {
            Ok(IncidentEvidence {
                incident_id: problem.incident_id.clone(),
                revision,
                active: true,
            })
        }
        IncidentReadiness::Resolved { .. } => Err(RecoveryError::Invalid(
            "incident resolved before dispatch".into(),
        )),
        IncidentReadiness::Unavailable { reason } => Err(service(reason)),
        _ => Err(RecoveryError::Conflict),
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

#[cfg(test)]
fn crash_boundary(name: &str) {
    if std::env::var("RECUVORA_RECOVERY_CRASH_BOUNDARY").as_deref() == Ok(name) {
        // Used only by a dedicated test subprocess. No destructor or shutdown
        // may repair the interrupted aggregate commit before restart.
        std::process::exit(93);
    }
}

#[cfg(test)]
#[path = "../../../tests/recovery_commits.rs"]
mod commit_tests;
