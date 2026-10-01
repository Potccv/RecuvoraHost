//! Host persistence and effect dispatch around the Core 0.2 recovery reducer.
use super::*;
use crate::persistence::{
    approval::{ApprovalStore, ApprovalStoreConfig},
    journal::Journal,
    knowledge::{KnowledgeStore, KnowledgeStoreConfig},
};
use crate::runtime::operation::{CallScope, Cancellation};
use recuvora_core::recovery::{
    approval::{
        self, ApprovalDecision, ApprovalRecord, ApprovalState, ExecutionOutcome, ReviewStage,
    },
    knowledge::{KnowledgeQuery, KnowledgeRecord, TrustedBusinessVerification},
    workflow::{
        IncidentEvidence, RecoveryCommand, RecoveryEffect, RecoveryEntry, RecoveryEvent,
        RecoveryState, TargetAuthority,
    },
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
    workflow: RecoveryState,
    journal: Journal,
    approvals: ApprovalStore,
    knowledge: KnowledgeStore,
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
        let approval = self.approval(&task)?;
        if approval.state != ApprovalState::Unknown
            && !(approval.state == ApprovalState::Failed
                && task
                    .result_check
                    .as_ref()
                    .is_some_and(|check| check.execution.outcome == CheckedExecution::NotExecuted))
        {
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
        self.event(
            RecoveryEvent::ResultChecked {
                task_id: id.into(),
                revision: task.revision,
                execution: execution.clone(),
                verification: verification.clone(),
                actor: "host-dispatch-recovery".into(),
                approval: approval.clone(),
            },
            now,
        )?;
        if approval.state == ApprovalState::Failed {
            return Ok(());
        }
        let approval = self.approvals.reconcile_unknown(
            &approval.request.request_id,
            ExecutionOutcome::Failed,
            "durable Host dispatch boundary proves original action was not dispatched".into(),
            "host-dispatch-recovery".into(),
            now / 1000,
        )?;
        #[cfg(test)]
        crash_boundary("not_dispatched_reconciled");
        let task = self.task(id)?;
        self.event(
            RecoveryEvent::ResultChecked {
                task_id: id.into(),
                revision: task.revision,
                execution,
                verification,
                actor: "host-dispatch-recovery".into(),
                approval,
            },
            now,
        )?;
        Ok(())
    }
    fn resume_result_check(&mut self, id: &str, now: u64) -> Result<(), RecoveryError> {
        let task = self.task(id)?;
        if task.stage != RecoveryStage::Unknown {
            return Ok(());
        }
        let (Some(check), Some(verification)) = (task.result_check, task.verification) else {
            return Ok(());
        };
        // Reuse the exact independently checked evidence. Restart does not make
        // old business evidence fresh; stale evidence requires a new trusted check.
        if check.execution.outcome == CheckedExecution::Unknown
            || now.saturating_sub(check.execution.checked_at_ms) > 30_000
            || now.saturating_sub(verification.verified_at_ms) > 30_000
        {
            return Ok(());
        }
        let mut approval = task
            .approval_id
            .as_ref()
            .and_then(|id| self.approvals.get(id))
            .cloned()
            .ok_or_else(|| service("missing checked approval"))?;
        if approval.state == ApprovalState::Unknown {
            let outcome = if check.execution.outcome == CheckedExecution::Executed {
                ExecutionOutcome::Executed
            } else {
                ExecutionOutcome::Failed
            };
            approval = self.approvals.reconcile_unknown(
                &approval.request.request_id,
                outcome,
                "resume independently checked evidence committed before restart".into(),
                check.actor.clone(),
                now / 1000,
            )?;
        }
        self.event(
            RecoveryEvent::ResultChecked {
                task_id: id.into(),
                revision: task.revision,
                execution: check.execution,
                verification,
                actor: check.actor,
                approval,
            },
            now,
        )?;
        Ok(())
    }
    fn apply(
        &mut self,
        command: RecoveryCommand,
        now: u64,
    ) -> Result<Vec<RecoveryEffect>, RecoveryError> {
        self.root_lock.validate()?;
        #[cfg(test)]
        let verification_boundary = matches!(
            &command,
            RecoveryCommand::Event(RecoveryEvent::VerificationRecorded { .. })
        );
        let prepared =
            self.workflow
                .prepare(self.journal.next_id(), command, now, self.knowledge.state())?;
        let entry = prepared
            .state()
            .latest_entry()
            .ok_or_else(|| service("missing recovery entry"))?;
        let receipt = self
            .journal
            .commit(prepared.request(), serde_json::to_value(entry)?)
            .map_err(service)?;
        let committed = prepared.confirm(receipt).map_err(service)?;
        self.workflow = committed.state;
        #[cfg(test)]
        {
            if verification_boundary {
                crash_boundary("verification_committed");
            }
            if committed
                .effects
                .iter()
                .any(|effect| matches!(effect, RecoveryEffect::RequestApproval { .. }))
            {
                crash_boundary("operation_committed");
            }
        }
        Ok(committed.effects)
    }
    fn event(
        &mut self,
        event: RecoveryEvent,
        now: u64,
    ) -> Result<Vec<RecoveryEffect>, RecoveryError> {
        self.apply(RecoveryCommand::Event(event), now)
    }
    fn task(&self, id: &str) -> Result<RecoveryTask, RecoveryError> {
        self.workflow
            .task(id)
            .cloned()
            .ok_or_else(|| RecoveryError::Invalid("task not found".into()))
    }
    fn approval(&self, task: &RecoveryTask) -> Result<ApprovalRecord, RecoveryError> {
        task.approval_id
            .as_ref()
            .and_then(|id| self.approvals.get(id).cloned())
            .ok_or_else(|| service("missing approval"))
    }
    fn attach(
        &mut self,
        task: &RecoveryTask,
        config: &RecoveryConfig,
        now: u64,
    ) -> Result<(), RecoveryError> {
        let op = task
            .operation
            .clone()
            .ok_or_else(|| service("missing original operation"))?;
        let policy = if task.reused_script {
            &config.script_approval
        } else {
            &config.approval
        };
        let record = self.approvals.request(op, policy.clone(), now / 1000)?;
        #[cfg(test)]
        crash_boundary("approval_requested");
        self.event(
            RecoveryEvent::ApprovalAttached {
                task_id: task.id.clone(),
                revision: task.revision,
                record,
            },
            now,
        )?;
        Ok(())
    }
    fn deliver(&mut self, now: u64) -> Result<(), RecoveryError> {
        // The reducer orders by created_revision, including accumulated failures.
        for delivery in self.workflow.pending_deliveries() {
            let candidate_id = delivery.candidate.id.clone();
            self.knowledge
                .upsert_candidate(delivery.candidate)
                .map_err(service)?;
            let proof = delivery
                .verification
                .map(|v| {
                    TrustedBusinessVerification::attest(
                        v.operation_id,
                        v.target_id,
                        v.script_id,
                        v.script_version,
                        v.verifier_id,
                        v.evidence_refs,
                        v.verified_at_ms,
                    )
                })
                .transpose()
                .map_err(service)?;
            self.knowledge
                .record_outcome(&candidate_id, delivery.case, proof)
                .map_err(service)?;
            #[cfg(test)]
            crash_boundary("knowledge_committed");
            self.event(
                RecoveryEvent::DeliveryConfirmed {
                    delivery_id: delivery.id,
                },
                now,
            )?;
        }
        Ok(())
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
        // Keep the legacy filename: its old format must be rejected, never bypassed.
        let journal = Journal::open(
            dir.join("recovery.jsonl"),
            "recovery",
            serde_json::to_value(&config)?,
            256 * 1024 * 1024,
        )
        .map_err(service)?;
        let entries: Vec<RecoveryEntry> = journal
            .records()
            .iter()
            .map(|record| {
                let entry: RecoveryEntry = serde_json::from_value(record.payload.clone())?;
                if entry.request != record.request {
                    return Err(RecoveryError::Corrupt(
                        "recovery payload and commit request differ".into(),
                    ));
                }
                Ok(entry)
            })
            .collect::<Result<_, RecoveryError>>()?;
        let workflow = if entries.is_empty() {
            RecoveryState::new(config.clone())?
        } else {
            RecoveryState::restore(config.clone(), &entries)?
        };
        let approvals = ApprovalStore::open(
            dir.join("approvals"),
            approval_config,
            clock.now_ms() / 1000,
        )?;
        let knowledge =
            KnowledgeStore::open(dir.join("knowledge.jsonl"), knowledge_config).map_err(service)?;
        let dispatch = Journal::open(
            dir.join("dispatch.jsonl"),
            "dispatch",
            serde_json::to_value(&config)?,
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
            workflow,
            journal,
            approvals,
            knowledge,
            dispatch,
            dispatch_pending: false,
        };
        if state.workflow.recovery_required() {
            state.event(RecoveryEvent::Recover, clock.now_ms())?;
            let recovered: Vec<_> = state.workflow.tasks().cloned().collect();
            for task in recovered {
                if task.stage != RecoveryStage::Paused {
                    continue;
                }
                let record = task
                    .operation
                    .as_ref()
                    .and_then(|op| {
                        state
                            .approvals
                            .find_operation(&op.task_id, &op.operation_id)
                    })
                    .cloned();
                if let Some(record) = record
                    && matches!(
                        record.state,
                        ApprovalState::Unknown
                            | ApprovalState::Executing
                            | ApprovalState::Executed
                            | ApprovalState::Failed
                    )
                {
                    // Repair the interrupted association without resuming an executable
                    // approval. Consumed authority is transferred into Unknown only.
                    state.event(
                        RecoveryEvent::Resume {
                            task_id: task.id.clone(),
                            revision: task.revision,
                        },
                        clock.now_ms(),
                    )?;
                    let current = state.task(&task.id)?;
                    let event = if current.approval_id.is_none() {
                        RecoveryEvent::ApprovalAttached {
                            task_id: current.id,
                            revision: current.revision,
                            record,
                        }
                    } else {
                        RecoveryEvent::ApprovalResolved {
                            task_id: current.id,
                            revision: current.revision,
                            record,
                        }
                    };
                    state.event(event, clock.now_ms())?;
                }
            }
            let ids: Vec<_> = state.workflow.tasks().map(|task| task.id.clone()).collect();
            for id in ids {
                state.not_dispatched(&id, &config, clock.now_ms())?;
                state.resume_result_check(&id, clock.now_ms())?;
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
        self.read_state(|s| Ok(s.workflow.task(id).cloned()))
    }
    pub fn tasks(&self) -> Result<Vec<RecoveryTask>, RecoveryError> {
        self.read_state(|s| Ok(s.workflow.tasks().cloned().collect()))
    }
    pub fn knowledge(&self, query: &KnowledgeQuery) -> Result<Vec<KnowledgeRecord>, RecoveryError> {
        self.read_state(|s| s.knowledge.search(query).map_err(service))
    }
    /// Retry committed experience delivery even when every business task ended.
    /// This never reconstructs execution effects or changes operation identity.
    pub fn deliver_pending(&self) -> Result<(), RecoveryError> {
        self.accepting()?;
        self.owner()?;
        self.with_state(|s| s.deliver(self.clock.now_ms()))
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
                s.event(
                    RecoveryEvent::Register {
                        problem: problem.clone(),
                        incident,
                    },
                    self.clock.now_ms(),
                )?;
                s.workflow
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
            let task = s.task(id)?;
            Ok(task
                .approval_id
                .as_ref()
                .and_then(|id| s.approvals.get(id).cloned()))
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
        self.with_state(|s| {
            let task = s.task(id)?;
            let record = s.approval(&task)?;
            if record.revision != revision {
                return Err(RecoveryError::Conflict);
            }
            Ok(s.approvals.decide_human(
                &record.request.request_id,
                decision,
                reason,
                actor,
                &record.request.policy,
                self.clock.now_ms() / 1000,
            )?)
        })
    }
    pub fn resume(&self, id: &str, revision: u64) -> Result<RecoveryTask, RecoveryError> {
        self.accepting()?;
        self.owner()?;
        self.with_state(|s| {
            s.event(
                RecoveryEvent::Resume {
                    task_id: id.into(),
                    revision,
                },
                self.clock.now_ms(),
            )?;
            let task = s.task(id)?;
            if task.approval_id.is_none() {
                s.attach(&task, &self.config, self.clock.now_ms())?;
            }
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
                service.run(&id, cancel).await
            })
            .await
            .map_err(super::service)?
    }
    async fn run(
        self: &Arc<Self>,
        id: &str,
        cancellation: Cancellation,
    ) -> Result<RecoveryTask, RecoveryError> {
        for _ in 0..64 {
            self.owner()?;
            self.with_state(|s| s.deliver(self.clock.now_ms()))?;
            let task = self.with_state(|s| s.task(id))?;
            if task.stage.terminal()
                || matches!(task.stage, RecoveryStage::Paused | RecoveryStage::Unknown)
            {
                return Ok(task);
            }
            if cancellation.is_cancelled()
                && !matches!(
                    task.stage,
                    RecoveryStage::Executing | RecoveryStage::Verifying
                )
            {
                return self.cancel_task(&task);
            }
            match task.stage {
                RecoveryStage::Queued | RecoveryStage::Diagnosing
                    if task.diagnosis_call.is_none() =>
                {
                    let observation = bounded(
                        self.backend
                            .inspect(&self.config.target, cancellation.clone()),
                        cancellation.clone(),
                        self.config.diagnosis_timeout_secs,
                    )
                    .await?;
                    let effects = self.with_state(|s| {
                        let event = if task.stage == RecoveryStage::Queued {
                            let mut conditions = observation.facts.clone();
                            conditions.insert(
                                "fault_fingerprint".into(),
                                task.problem.fingerprint.clone(),
                            );
                            conditions
                                .insert("platform".into(), self.config.target.platform.clone());
                            let candidate =
                                if task.episode_count >= self.config.minimum_script_occurrences {
                                    s.knowledge
                                        .search_reusable(&KnowledgeQuery {
                                            conditions,
                                            keywords: task.problem.keywords.clone(),
                                            limit: 100,
                                        })
                                        .map_err(super::service)?
                                        .into_iter()
                                        .find(|v| {
                                            !s.workflow.is_quarantined(
                                                &v.candidate.script.id,
                                                v.candidate.script.version,
                                            )
                                        })
                                } else {
                                    None
                                };
                            RecoveryEvent::SelectPlan {
                                task_id: id.into(),
                                revision: task.revision,
                                observation,
                                candidate,
                            }
                        } else {
                            RecoveryEvent::RetryDiagnosis {
                                task_id: id.into(),
                                revision: task.revision,
                                observation,
                            }
                        };
                        s.event(event, self.clock.now_ms())
                    })?;
                    for effect in effects {
                        if let RecoveryEffect::Diagnose {
                            task,
                            call_id,
                            timeout_secs,
                            ..
                        } = effect
                        {
                            let observation = task
                                .observation
                                .clone()
                                .ok_or_else(|| service("missing observation"))?;
                            let knowledge = self.with_state(|s| {
                                Ok(s.knowledge
                                    .search(&KnowledgeQuery {
                                        conditions: observation.facts.clone(),
                                        keywords: task.problem.keywords.clone(),
                                        limit: 32,
                                    })
                                    .map_err(super::service)?
                                    .into_iter()
                                    .map(|r| r.candidate)
                                    .collect())
                            })?;
                            let result = bounded(
                                self.backend.diagnose(
                                    DiagnosisInput {
                                        task: *task.clone(),
                                        config: self.config.clone(),
                                        observation,
                                        knowledge,
                                    },
                                    cancellation.clone(),
                                ),
                                cancellation.clone(),
                                timeout_secs,
                            )
                            .await;
                            self.with_state(|s| {
                                let event = match result {
                                    Ok(plan) => RecoveryEvent::DiagnosisCompleted {
                                        task_id: id.into(),
                                        revision: task.revision,
                                        call_id,
                                        plan,
                                    },
                                    Err(error) => RecoveryEvent::DiagnosisFailed {
                                        task_id: id.into(),
                                        revision: task.revision,
                                        call_id,
                                        reason: bounded_reason(&error),
                                    },
                                };
                                s.event(event, self.clock.now_ms())?;
                                Ok(())
                            })?;
                        }
                    }
                }
                RecoveryStage::AwaitingApproval => {
                    if task.approval_id.is_none() {
                        self.with_state(|s| s.attach(&task, &self.config, self.clock.now_ms()))?;
                        continue;
                    }
                    let record = self.with_state(|s| s.approval(&task))?;
                    if record.state == ApprovalState::Approved {
                        self.execute(&task, cancellation.clone()).await?;
                    } else if matches!(
                        record.state,
                        ApprovalState::Pending | ApprovalState::WaitingHuman
                    ) {
                        if !self.review(&task, &record, cancellation.clone()).await? {
                            return self.with_state(|s| s.task(id));
                        }
                    } else {
                        self.with_state(|s| {
                            s.event(
                                RecoveryEvent::ApprovalResolved {
                                    task_id: id.into(),
                                    revision: task.revision,
                                    record,
                                },
                                self.clock.now_ms(),
                            )?;
                            Ok(())
                        })?;
                    }
                }
                RecoveryStage::Verifying => {
                    let operation = task
                        .operation
                        .clone()
                        .ok_or_else(|| service("missing operation"))?;
                    let receipt = task
                        .receipt
                        .clone()
                        .ok_or_else(|| service("missing receipt"))?;
                    let result = bounded(
                        self.backend.verify(
                            VerificationInput {
                                target: self.config.target.clone(),
                                operation,
                                receipt,
                            },
                            cancellation.clone(),
                        ),
                        cancellation.clone(),
                        self.config.target.action_timeout_secs,
                    )
                    .await;
                    self.with_state(|s| {
                        let event = match result {
                            Ok(verification) => RecoveryEvent::VerificationRecorded {
                                task_id: id.into(),
                                revision: task.revision,
                                verification,
                            },
                            Err(error) => RecoveryEvent::VerificationUnavailable {
                                task_id: id.into(),
                                revision: task.revision,
                                reason: bounded_reason(&error),
                            },
                        };
                        s.event(event, self.clock.now_ms())?;
                        Ok(())
                    })?;
                }
                _ => return Err(service("task has an existing in-flight effect")),
            }
        }
        Err(RecoveryError::Capacity)
    }
    async fn review(
        &self,
        task: &RecoveryTask,
        record: &ApprovalRecord,
        cancellation: Cancellation,
    ) -> Result<bool, RecoveryError> {
        let now = self.clock.now_ms() / 1000;
        if now >= record.request.expires_at {
            self.with_state(|s| {
                s.approvals.expire(&record.request.request_id, now)?;
                Ok(())
            })?;
            return Ok(true);
        }
        if matches!(
            record.request.policy.reviewer,
            approval::ReviewerConfig::Human
        ) || record.review_stage == ReviewStage::NeedsHuman
            || (record.review_stage == ReviewStage::WaitingHuman
                && record.human_deadline.is_some_and(|deadline| now < deadline))
        {
            return Ok(false);
        }
        if record.review_stage == ReviewStage::ReviewingHarness {
            if record
                .review_deadline
                .is_some_and(|deadline| now >= deadline)
            {
                self.with_state(|s| {
                    s.approvals.expire_review(
                        &record.request.request_id,
                        record.revision,
                        &record.request.policy,
                        now,
                    )?;
                    Ok(())
                })?;
            }
            return Ok(false);
        }
        let observation = bounded(
            self.backend
                .inspect(&self.config.target, cancellation.clone()),
            cancellation.clone(),
            self.config.review_timeout_secs,
        )
        .await?;
        let timeout = match record.request.policy.reviewer {
            approval::ReviewerConfig::HumanThenHarness {
                review_timeout_secs,
                ..
            } => self.config.review_timeout_secs.min(review_timeout_secs),
            _ => self.config.review_timeout_secs,
        };
        let attempt = self.with_state(|s| {
            Ok(s.approvals.begin_harness_review(
                &record.request.request_id,
                record.revision,
                &record.request.policy,
                timeout,
                self.clock.now_ms() / 1000,
            )?)
        })?;
        let result = bounded(
            self.backend.review(
                ReviewInput {
                    request: record.request.clone(),
                    attempt: attempt.clone(),
                    observation,
                    reused_script: task.reused_script,
                },
                cancellation.clone(),
            ),
            cancellation,
            timeout,
        )
        .await;
        self.with_state(|s| {
            let saved = match result {
                Ok(value) => s.approvals.assess_attempt(
                    &attempt,
                    value.assessment,
                    value.identity,
                    &record.request.policy,
                    self.clock.now_ms() / 1000,
                ),
                Err(error) => s.approvals.fail_review_attempt(
                    &attempt,
                    bounded_reason(&error),
                    &record.request.policy,
                    self.clock.now_ms() / 1000,
                ),
            };
            if let Err(error) = saved {
                // A late callback cannot overwrite a concurrent human decision.
                if s.approvals
                    .get(&attempt.request_id)
                    .and_then(|r| r.active_review_attempt())
                    .as_ref()
                    == Some(&attempt)
                {
                    s.approvals.fail_review_attempt(
                        &attempt,
                        bounded_reason(&error),
                        &record.request.policy,
                        self.clock.now_ms() / 1000,
                    )?;
                }
            }
            Ok(())
        })?;
        Ok(true)
    }
    fn cancel_task(&self, task: &RecoveryTask) -> Result<RecoveryTask, RecoveryError> {
        self.with_state(|s| {
            let mut task = task.clone();
            if task.operation.is_some() && task.approval_id.is_none() {
                s.attach(&task, &self.config, self.clock.now_ms())?;
                task = s.task(&task.id)?;
            }
            let approval = if let Some(id) = &task.approval_id {
                Some(s.approvals.cancel(
                    id,
                    "Host canceled before dispatch".into(),
                    self.clock.now_ms() / 1000,
                )?)
            } else {
                None
            };
            s.event(
                RecoveryEvent::Cancel {
                    task_id: task.id.clone(),
                    revision: task.revision,
                    approval,
                },
                self.clock.now_ms(),
            )?;
            s.task(&task.id)
        })
    }
    async fn execute(
        self: &Arc<Self>,
        task: &RecoveryTask,
        cancellation: Cancellation,
    ) -> Result<(), RecoveryError> {
        let observation = bounded(
            self.backend
                .inspect(&self.config.target, cancellation.clone()),
            cancellation.clone(),
            self.config.target.action_timeout_secs,
        )
        .await?;
        if cancellation.is_cancelled() {
            self.cancel_task(task)?;
            return Ok(());
        }
        let guard = self
            .incident_guard
            .get()
            .ok_or_else(|| service("incident guard not bound"))?;
        let lease = tokio::select! {
            lease = guard.acquire_dispatch(&task.problem) => lease?,
            _ = cancellation.cancelled() => { self.cancel_task(task)?; return Ok(()); }
        };
        let incident = incident(&task.problem, lease.current()?)?;
        self.owner()?;
        let (permit, timeout_secs) = self.with_state(|s| {
            let current = s.task(&task.id)?;
            if current.revision != task.revision {
                return Err(RecoveryError::Conflict);
            }
            let record = s.approval(&current)?;
            let operation = current
                .operation
                .as_ref()
                .ok_or_else(|| service("missing operation"))?;
            s.dispatch_phase(operation.clone(), false)?;
            let permit = s.approvals.consume(
                &record.request.request_id,
                operation,
                &record.request.policy,
                self.clock.now_ms() / 1000,
            )?;
            #[cfg(test)]
            crash_boundary("approval_consumed");
            let approval = s
                .approvals
                .get(&record.request.request_id)
                .cloned()
                .ok_or_else(|| service("consumed approval missing"))?;
            let authority = TargetAuthority {
                target_id: task.problem.target_id.clone(),
                epoch: format!("owner-{}-{}", std::process::id(), self.clock.now_ms()),
            };
            let effects = match s.apply(
                RecoveryCommand::AuthorizeExecution {
                    task_id: task.id.clone(),
                    revision: task.revision,
                    permit,
                    approval,
                    observation,
                    incident,
                    authority,
                },
                self.clock.now_ms(),
            ) {
                Ok(effects) => effects,
                Err(error) => {
                    let approval = s
                        .approvals
                        .recover_unknown(&record.request.request_id, self.clock.now_ms() / 1000)?;
                    s.event(
                        RecoveryEvent::ApprovalResolved {
                            task_id: task.id.clone(),
                            revision: task.revision,
                            record: approval,
                        },
                        self.clock.now_ms(),
                    )?;
                    s.not_dispatched(&task.id, &self.config, self.clock.now_ms())?;
                    return Err(error);
                }
            };
            let (permit, timeout_secs) = effects
                .into_iter()
                .find_map(|effect| match effect {
                    RecoveryEffect::Execute {
                        permit,
                        timeout_secs,
                        ..
                    } => Some((permit, timeout_secs)),
                    _ => None,
                })
                .ok_or_else(|| service("committed execution effect missing"))?;
            #[cfg(test)]
            crash_boundary("execution_authorized");
            if let Err(error) = s.dispatch_phase(permit.operation().clone(), true) {
                s.approvals.complete(
                    permit,
                    ExecutionOutcome::Unknown,
                    "dispatch boundary persistence failed".into(),
                    self.clock.now_ms() / 1000,
                )?;
                return Err(error);
            }
            s.dispatch_pending = true;
            Ok((permit, timeout_secs))
        })?;
        let dispatch = Arc::new(ExecutionDispatch {
            service: self.clone(),
            task_id: task.id.clone(),
            lease: Mutex::new(Some(lease)),
        });
        let result = bounded(
            self.backend.execute(
                AuthorizedScript {
                    permit: &permit,
                    timeout_secs,
                    dispatch_guard: Some(dispatch.clone()),
                },
                cancellation.clone(),
            ),
            cancellation,
            timeout_secs,
        )
        .await;
        // Routing failures and trusted embedded backends may not enter the network
        // client. The fallback still retains the gate until their call has ended.
        crate::integrations::extensions::DispatchGuard::release(dispatch.as_ref());
        let operation = permit.operation();
        let receipt = match result {
            Ok(receipt)
                if receipt.operation_id == operation.operation_id
                    && receipt.target_id == operation.target
                    && (receipt.outcome == ScriptOutcome::Unknown || receipt.executor_stopped)
                    && !receipt.evidence_refs.is_empty()
                    && receipt.evidence_refs.len() <= 32
                    && receipt.summary.len() <= 8192 =>
            {
                receipt
            }
            result => ScriptReceipt {
                operation_id: operation.operation_id.clone(),
                target_id: operation.target.clone(),
                outcome: ScriptOutcome::Unknown,
                executor_stopped: false,
                evidence_refs: vec![format!("approval:{}", permit.request_id())],
                summary: match result {
                    Err(error) => bounded_reason(&error),
                    Ok(_) => "invalid or unbound executor receipt".into(),
                },
            },
        };
        self.with_state(|s| {
            let outcome = match receipt.outcome {
                ScriptOutcome::Executed => ExecutionOutcome::Executed,
                ScriptOutcome::Failed => ExecutionOutcome::Failed,
                ScriptOutcome::Unknown => ExecutionOutcome::Unknown,
            };
            let approval = s.approvals.complete(
                permit,
                outcome,
                receipt.summary.clone(),
                self.clock.now_ms() / 1000,
            )?;
            #[cfg(test)]
            crash_boundary("approval_completed");
            let current = s.task(&task.id)?;
            s.event(
                RecoveryEvent::ExecutionRecorded {
                    task_id: task.id.clone(),
                    revision: current.revision,
                    receipt,
                    approval,
                },
                self.clock.now_ms(),
            )?;
            Ok(())
        })
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
            let task = s.task(id)?;
            let approval = s.approval(&task)?;
            s.event(
                RecoveryEvent::ResultChecked {
                    task_id: id.into(),
                    revision,
                    execution: execution.clone(),
                    verification: verification.clone(),
                    actor: actor.clone(),
                    approval: approval.clone(),
                },
                self.clock.now_ms(),
            )?;
            if approval.state == ApprovalState::Unknown
                && execution.outcome != CheckedExecution::Unknown
            {
                #[cfg(test)]
                crash_boundary("result_check_saved");
                let outcome = if execution.outcome == CheckedExecution::Executed {
                    ExecutionOutcome::Executed
                } else {
                    ExecutionOutcome::Failed
                };
                let approval = s.approvals.reconcile_unknown(
                    &approval.request.request_id,
                    outcome,
                    "independent executor evidence committed to recovery".into(),
                    actor.clone(),
                    self.clock.now_ms() / 1000,
                )?;
                #[cfg(test)]
                crash_boundary("result_check_reconciled");
                let current = s.task(id)?;
                s.event(
                    RecoveryEvent::ResultChecked {
                        task_id: id.into(),
                        revision: current.revision,
                        execution,
                        verification,
                        actor,
                        approval,
                    },
                    self.clock.now_ms(),
                )?;
            }
            s.deliver(self.clock.now_ms())?;
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
            current.approvals.prepare_close()?;
            current.knowledge.prepare_close().map_err(service)?;
            // Keep nonterminal and Unknown durable ownership even after dropping locks.
            let terminal = current.workflow.tasks().all(|task| task.stage.terminal())
                && !current.approvals.list().iter().any(|record| {
                    matches!(
                        record.state,
                        ApprovalState::Executing | ApprovalState::Unknown
                    )
                });
            if terminal && let Some(lease) = lock(&self.ownership)?.as_mut() {
                lease.release()?;
            }
            current.approvals.finish_close();
            current.knowledge.finish_close();
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
struct ExecutionDispatch {
    service: Arc<RecoveryService>,
    task_id: String,
    lease: Mutex<Option<Box<dyn IncidentDispatchLease>>>,
}
impl crate::integrations::extensions::DispatchGuard for ExecutionDispatch {
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
            incident(&task.problem, lease.current()?)?;
            if task.stage != RecoveryStage::Executing || !state.dispatch_pending {
                return Err(RecoveryError::Conflict);
            }
            let record = state.approval(&task)?;
            if record.state != ApprovalState::Executing
                || self.service.clock.now_ms() / 1000 >= record.request.expires_at
            {
                return Err(RecoveryError::Conflict);
            }
            state.journal.available().map_err(service)?;
            state.dispatch.available().map_err(service)?;
            state.approvals.ensure_current()?;
            state.knowledge.available().map_err(service)?;
            let script = &task
                .plan
                .as_ref()
                .ok_or_else(|| service("missing execution script"))?
                .script;
            if state.workflow.is_quarantined(&script.id, script.version)
                || state
                    .knowledge
                    .state()
                    .is_quarantined(&script.id, script.version)
            {
                return Err(RecoveryError::Invalid(
                    "script quarantined before send".into(),
                ));
            }
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
fn bounded_reason(error: &impl std::fmt::Display) -> String {
    let text = error.to_string();
    let mut end = text.len().min(4096);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].into()
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
        // may repair the interrupted cross-domain commit before restart.
        std::process::exit(93);
    }
}

#[cfg(test)]
#[path = "../../../tests/recovery_commits.rs"]
mod commit_tests;
