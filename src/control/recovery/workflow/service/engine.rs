//! Deterministic workflow. Time and facts are inputs; effects require commit confirmation.
use super::contract::{FAULT_FINGERPRINT_CONDITION, evidence, facts, stable_conditions};
use super::*;
use crate::control::operation::{CommitRequest, Prepared};
use approval::{ApprovalRecord, ApprovalState, ExecutionPermit};

/// The caller must hold this logical ownership epoch through commit and dispatch.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetAuthority {
    pub target_id: String,
    pub epoch: String,
}

/// Current trusted incident fact. The caller must check its revision atomically with
/// execution authorization, or hold its incident gate through that commit.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IncidentEvidence {
    pub incident_id: String,
    pub revision: u64,
    pub active: bool,
}

/// Committed history data. Loading it never dispatches an external operation.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
pub enum RecoveryEvent {
    RepairActionPrepared {
        task_id: String,
        revision: u64,
        action: RepairArtifact,
    },
    RepairRequested {
        task_id: String,
        revision: u64,
        request: HarnessRepairRequest,
    },
    BeginExperience {
        job_id: String,
    },
    ExperienceSummarized {
        job_id: String,
        call_id: String,
        report: ExperienceReport,
    },
    ExperienceFailed {
        job_id: String,
        call_id: String,
        reason: String,
    },
    ExperienceDelivered {
        job_id: String,
    },
    Register {
        problem: ProblemContext,
        incident: IncidentEvidence,
    },
    ApprovalAttached {
        task_id: String,
        revision: u64,
        record: ApprovalRecord,
    },
    ApprovalResolved {
        task_id: String,
        revision: u64,
        record: ApprovalRecord,
    },
    /// Only replay accepts this directly; live preparation requires an owned permit.
    ExecutionAuthorized {
        task_id: String,
        revision: u64,
        approval: ApprovalRecord,
        observation: TargetObservation,
        incident: IncidentEvidence,
        authority: TargetAuthority,
    },
    ExecutionRecorded {
        task_id: String,
        revision: u64,
        receipt: RepairReceipt,
        approval: ApprovalRecord,
    },
    VerificationRecorded {
        task_id: String,
        revision: u64,
        verification: BusinessVerification,
    },
    VerificationUnavailable {
        task_id: String,
        revision: u64,
        reason: String,
    },
    ResultChecked {
        task_id: String,
        revision: u64,
        execution: ExecutionResultCheck,
        verification: BusinessVerification,
        actor: String,
        approval: ApprovalRecord,
    },
    Resume {
        task_id: String,
        revision: u64,
    },
    Cancel {
        task_id: String,
        revision: u64,
        approval: Option<ApprovalRecord>,
    },
    Recover,
}

pub enum RecoveryCommand {
    StartRepair {
        task_id: String,
        revision: u64,
        observation: TargetObservation,
    },
    Event(RecoveryEvent),
    AuthorizeExecution {
        task_id: String,
        revision: u64,
        permit: ExecutionPermit,
        approval: ApprovalRecord,
        observation: TargetObservation,
        incident: IncidentEvidence,
        authority: TargetAuthority,
    },
}

/// Effects are available only after confirming the whole workflow commit.
#[derive(Debug)]
pub enum RecoveryEffect {
    ExperiencePending {
        job_id: String,
    },
    SummarizeExperience {
        job: Box<ExperienceJob>,
        call_id: String,
    },
    RequestApproval {
        operation: approval::ProposedOperation,
        policy: approval::ApprovalPolicy,
    },
    Execute {
        permit: ExecutionPermit,
        authority: TargetAuthority,
        timeout_secs: u64,
    },
    Verify {
        task_id: String,
        operation: approval::ProposedOperation,
        receipt: RepairReceipt,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryEntry {
    pub request: CommitRequest,
    pub now_ms: u64,
    pub event: RecoveryEvent,
}

/// Authority reconstructed through the same validated transitions used live.
/// No unchecked Deserialize implementation and no ambient clock or I/O.
#[derive(Clone, Debug)]
pub struct RecoveryState {
    repair_actions: crate::control::collections::Map<String, RepairArtifact>,
    pub(super) experiences: crate::control::collections::Map<String, ExperienceJob>,
    digest: String,
    commit_ids: im::OrdSet<String>,
    config: RecoveryConfig,
    revision: u64,
    updated_at_ms: u64,
    tasks: crate::control::collections::Map<String, RecoveryTask>,
    quarantined: im::OrdSet<(String, u64)>,
    artifacts: crate::control::collections::Map<(String, u64), RepairArtifact>,
    entries: im::Vector<std::sync::Arc<RecoveryEntry>>,
    recovery_required: bool,
}

impl RecoveryState {
    /// Committed action evidence only; reading it never grants execution permission.
    pub fn repair_action(&self, operation_id: &str) -> Option<&RepairArtifact> {
        self.repair_actions.get(operation_id)
    }

    pub fn new(config: RecoveryConfig) -> Result<Self, RecoveryError> {
        config.validate()?;
        Ok(Self {
            digest: crate::control::binding::digest(&("recovery", &config)),
            commit_ids: im::OrdSet::new(),
            config,
            revision: 0,
            updated_at_ms: 0,
            repair_actions: crate::control::collections::Map::new(),
            experiences: crate::control::collections::Map::new(),
            tasks: crate::control::collections::Map::new(),
            quarantined: im::OrdSet::new(),
            artifacts: crate::control::collections::Map::new(),
            entries: im::Vector::new(),
            recovery_required: false,
        })
    }
    pub fn tasks(&self) -> impl Iterator<Item = &RecoveryTask> {
        self.tasks.values()
    }
    pub fn recovery_required(&self) -> bool {
        self.recovery_required
    }
    pub fn config(&self) -> &RecoveryConfig {
        &self.config
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    /// Copies the complete history for export; use latest_entry for incremental persistence.
    pub fn entries(&self) -> Vec<RecoveryEntry> {
        self.entries
            .iter()
            .map(|entry| entry.as_ref().clone())
            .collect()
    }

    /// The newest validated entry, without copying the history.
    pub fn latest_entry(&self) -> Option<&RecoveryEntry> {
        self.entries.back().map(AsRef::as_ref)
    }
    pub fn task(&self, id: &str) -> Option<&RecoveryTask> {
        self.tasks.get(id)
    }
    pub fn is_quarantined(&self, id: &str, version: u64) -> bool {
        self.quarantined.contains(&(id.into(), version))
    }

    /// The caller persists the new entry and compares request.expected_revision, then
    /// confirms. Commit failure/uncertainty grants no effect; reload before retry.
    pub fn prepare(
        &self,
        commit_id: impl Into<String>,
        command: RecoveryCommand,
        now_ms: u64,
        knowledge: &KnowledgeState,
    ) -> Result<Prepared<Self, RecoveryEffect>, RecoveryError> {
        if self.recovery_required
            && !matches!(&command, RecoveryCommand::Event(RecoveryEvent::Recover))
        {
            return Err(invalid(
                "restored workflow requires a committed recovery transition",
            ));
        }
        let (event, permit) = match command {
            RecoveryCommand::StartRepair {
                task_id,
                revision,
                observation,
            } => {
                let task = self.current(&task_id, revision)?;
                self.conditions(&task, &observation)?;
                let request = recuvora_core::recovery::planning::prepare_repair(
                    recuvora_core::recovery::planning::RepairRequestInput {
                        problem: task.problem,
                        observation,
                        harness_id: self.config.execution_harness.clone(),
                        delegation: self.config.approval.delegation.clone(),
                        target: self.config.target.clone(),
                        max_tool_calls: self.config.max_tool_calls,
                    },
                    knowledge.experiences(),
                )?;
                (
                    RecoveryEvent::RepairRequested {
                        task_id,
                        revision,
                        request,
                    },
                    None,
                )
            }
            RecoveryCommand::Event(event) => {
                if matches!(
                    event,
                    RecoveryEvent::ExecutionAuthorized { .. }
                        | RecoveryEvent::RepairRequested { .. }
                ) {
                    return Err(invalid("execution requires an owned committed permit"));
                }
                (event, None)
            }
            RecoveryCommand::AuthorizeExecution {
                task_id,
                revision,
                permit,
                approval,
                observation,
                incident,
                authority,
            } => {
                if permit.request_id() != approval.request.request_id
                    || permit.operation() != &approval.request.operation
                    || permit.revision() != approval.revision
                {
                    return Err(invalid("permit and durable approval differ"));
                }
                (
                    RecoveryEvent::ExecutionAuthorized {
                        task_id,
                        revision,
                        approval,
                        observation,
                        incident,
                        authority,
                    },
                    Some(permit),
                )
            }
        };
        // Current knowledge authority is checked live; historical replay validates
        // the original evidence without retroactively applying newer isolation.
        match &event {
            RecoveryEvent::ExperienceSummarized { report, .. } => {
                if let Scriptability::Possible {
                    candidate: Some(script),
                    ..
                } = &report.scriptability
                {
                    knowledge.validate_artifact(script)?;
                }
            }
            RecoveryEvent::RepairActionPrepared { action, .. } => {
                knowledge.validate_artifact(action)?;
                if knowledge.is_quarantined(&action.id, action.version) {
                    return Err(invalid("repair action quarantined"));
                }
            }
            _ => {}
        }
        let id = commit_id.into();
        if self.commit_ids.contains(&id) {
            return Err(invalid(
                "commit identity already applied; read prior result",
            ));
        }
        let input = self.commit_input(&event, now_ms)?;
        let mut next = self.clone();
        let effects = next.apply(&event, now_ms, permit)?;
        next.revision = self
            .revision
            .checked_add(1)
            .ok_or(RecoveryError::Capacity)?;
        next.updated_at_ms = now_ms;
        let request =
            CommitRequest::new(id.clone(), self.revision, "recovery".into(), input.clone())?;
        next.digest = crate::control::binding::digest(&request);
        next.commit_ids.insert(id.clone());
        next.entries.push_back(std::sync::Arc::new(RecoveryEntry {
            request,
            now_ms,
            event,
        }));
        Ok(Prepared::new_bound(
            id,
            self.revision,
            "recovery".into(),
            input,
            next,
            effects,
        )?)
    }

    /// The caller supplies complete, committed history from protected storage. Replay
    /// never returns effects/permits. Persist Recover before resuming any work.
    pub fn restore(
        config: RecoveryConfig,
        entries: impl AsRef<[RecoveryEntry]>,
    ) -> Result<Self, RecoveryError> {
        let entries = entries.as_ref();
        let mut state = Self::new(config)?;
        let mut ids = BTreeSet::new();
        for entry in entries {
            if !crate::control::identity::valid_id(&entry.request.id)
                || !ids.insert(entry.request.id.clone())
                || entry.request.expected_revision != state.revision
                || Some(entry.request.revision) != state.revision.checked_add(1)
            {
                return Err(invalid("invalid recovery history order or identity"));
            }
            let expected = CommitRequest::new(
                entry.request.id.clone(),
                state.revision,
                "recovery".into(),
                state.commit_input(&entry.event, entry.now_ms)?,
            )?;
            if entry.request != expected {
                return Err(invalid("recovery commit content differs from history"));
            }
            state.apply(&entry.event, entry.now_ms, None)?;
            state.revision = entry.request.revision;
            state.updated_at_ms = entry.now_ms;
            state.digest = crate::control::binding::digest(&entry.request);
            state.commit_ids.insert(entry.request.id.clone());
            state.entries.push_back(std::sync::Arc::new(entry.clone()));
        }
        state.recovery_required = true;
        Ok(state)
    }

    fn commit_input(
        &self,
        event: &RecoveryEvent,
        now: u64,
    ) -> Result<serde_json::Value, RecoveryError> {
        serde_json::to_value((
            &self.config,
            self.revision,
            self.updated_at_ms,
            &self.digest,
            event,
            now,
        ))
        .map_err(|_| invalid("cannot encode bounded domain commit"))
    }
    fn mark_unknown(
        &mut self,
        task: &mut RecoveryTask,
        now: u64,
    ) -> Result<Option<RecoveryEffect>, RecoveryError> {
        let op = task
            .operation
            .as_ref()
            .ok_or_else(|| invalid("missing original operation"))?;
        let delivery_id = format!("{}-Unknown", op.operation_id);
        if task.receipt.is_none() {
            task.receipt = Some(RepairReceipt {
                execution_trace: self
                    .repair_actions
                    .get(&op.operation_id)
                    .cloned()
                    .into_iter()
                    .collect(),
                operation_id: op.operation_id.clone(),
                target_id: op.target.clone(),
                outcome: RepairExecutionOutcome::Unknown,
                executor_stopped: false,
                evidence_refs: vec![format!(
                    "approval:{}",
                    task.approval_id.as_deref().unwrap_or("unknown")
                )],
                summary: "interrupted or consumed execution; independent evidence required".into(),
            });
        }
        task.stage = RecoveryStage::Unknown;
        if self.experiences.contains_key(&delivery_id) {
            return Ok(None);
        }
        Ok(Some(self.finish(task, RepairOutcome::Unknown, now)?))
    }
    fn current(&self, id: &str, revision: u64) -> Result<RecoveryTask, RecoveryError> {
        let task = self
            .tasks
            .get(id)
            .ok_or_else(|| invalid("task not found"))?;
        if task.revision != revision {
            return Err(RecoveryError::Conflict);
        }
        Ok(task.clone())
    }
    fn save_task(&mut self, mut task: RecoveryTask, now: u64) -> Result<(), RecoveryError> {
        task.revision = task
            .revision
            .checked_add(1)
            .ok_or(RecoveryError::Capacity)?;
        task.updated_at_ms = now;
        self.tasks.insert(task.id.clone(), task);
        Ok(())
    }
    fn observe(&self, observed: &TargetObservation, now: u64) -> Result<(), RecoveryError> {
        facts(&observed.facts)?;
        evidence(&observed.evidence_refs)?;
        if observed.target_id != self.config.target.target_id
            || observed.observed_at_ms > now
            || now - observed.observed_at_ms > 30_000
            || self
                .config
                .target
                .required_facts
                .iter()
                .any(|(k, v)| observed.facts.get(k) != Some(v))
        {
            return Err(invalid("stale observation or target conditions changed"));
        }
        Ok(())
    }
    fn conditions(
        &self,
        task: &RecoveryTask,
        observed: &TargetObservation,
    ) -> Result<BTreeMap<String, String>, RecoveryError> {
        let values = stable_conditions(&task.problem, &self.config.target)?;
        if values
            .iter()
            .filter(|(key, _)| key.as_str() != FAULT_FINGERPRINT_CONDITION)
            .any(|(key, value)| observed.facts.get(key) != Some(value))
        {
            return Err(invalid("incident environment changed"));
        }
        Ok(values)
    }
    fn validate_action(
        &mut self,
        action: &RepairArtifact,
        observation: &TargetObservation,
    ) -> Result<(), RecoveryError> {
        action.validate()?;
        if !self
            .config
            .target
            .allowed_action_kinds
            .contains(&action.kind)
            || action
                .preconditions
                .iter()
                .any(|(key, value)| observation.facts.get(key) != Some(value))
            || self.is_quarantined(&action.id, action.version)
        {
            return Err(invalid("action conditions or version not eligible"));
        }
        let key = (action.id.clone(), action.version);
        if self.artifacts.get(&key).is_some_and(|old| old != action) {
            return Err(invalid("artifact version content changed"));
        }
        self.artifacts.insert(key, action.clone());
        Ok(())
    }
    fn bind_approval(
        &self,
        task: &RecoveryTask,
        record: &ApprovalRecord,
    ) -> Result<(), RecoveryError> {
        if task.operation.as_ref() != Some(&record.request.operation)
            || self.config.approval != record.request.policy
            || task
                .approval_id
                .as_ref()
                .is_some_and(|id| id != &record.request.request_id)
        {
            return Err(invalid(
                "approval does not bind original intent and current policy",
            ));
        }
        record.request.operation.validate()?;
        record.request.policy.validate()?;
        if !record.request.policy.allows(&record.request.operation) {
            return Err(invalid("operation outside hard policy"));
        }
        Ok(())
    }
    fn apply(
        &mut self,
        event: &RecoveryEvent,
        now: u64,
        permit: Option<ExecutionPermit>,
    ) -> Result<Vec<RecoveryEffect>, RecoveryError> {
        if now < self.updated_at_ms {
            return Err(invalid("clock moved backwards"));
        }
        if matches!(
            event,
            RecoveryEvent::RepairActionPrepared { .. }
                | RecoveryEvent::RepairRequested { .. }
                | RecoveryEvent::BeginExperience { .. }
                | RecoveryEvent::ExperienceSummarized { .. }
                | RecoveryEvent::ExperienceFailed { .. }
                | RecoveryEvent::ExperienceDelivered { .. }
        ) {
            self.apply_experience(event, now)
        } else {
            self.apply_task(event, now, permit)
        }
    }
    fn apply_experience(
        &mut self,
        event: &RecoveryEvent,
        now: u64,
    ) -> Result<Vec<RecoveryEffect>, RecoveryError> {
        let mut effects = Vec::new();
        match event {
            RecoveryEvent::RepairActionPrepared {
                task_id,
                revision,
                action,
            } => {
                let task = self.current(task_id, *revision)?;
                let op = task
                    .operation
                    .as_ref()
                    .ok_or_else(|| invalid("missing repair intent"))?;
                if task.stage != RecoveryStage::Executing
                    || op.action["kind"] != "repair_with_harness"
                    || self.repair_actions.contains_key(&op.operation_id)
                    || action.id != format!("{}-action", op.operation_id)
                    || action.version != 1
                    || action.generated_by_harness != self.config.execution_harness
                    || action.generated_in_session != op.operation_id
                {
                    return Err(invalid("invalid or repeated repair action"));
                }
                self.validate_action(
                    action,
                    task.observation
                        .as_ref()
                        .ok_or_else(|| invalid("missing observation"))?,
                )?;
                self.repair_actions
                    .insert(op.operation_id.clone(), action.clone());
                self.save_task(task, now)?;
            }
            RecoveryEvent::RepairRequested {
                task_id,
                revision,
                request,
            } => {
                let mut task = self.current(task_id, *revision)?;
                if task.stage != RecoveryStage::Queued
                    || request.problem != task.problem
                    || request.harness_id != self.config.execution_harness
                    || request.delegation != self.config.approval.delegation
                    || serde_json::to_value(&request.target).ok()
                        != serde_json::to_value(&self.config.target).ok()
                    || request.max_tool_calls != self.config.max_tool_calls
                    || request.matched_experience_count > MAX_MATCHED_EXPERIENCES
                    || request.matched_experience_count < request.experiences.len()
                    || (request.matched_experience_count == 0 && !request.experiences.is_empty())
                    || request.experiences.len() > 4
                    || !request.within_size_limit()?
                    || !request.summarize_experience
                    || !request.assess_scriptability
                {
                    return Err(invalid("invalid unified repair request"));
                }
                self.observe(&request.observation, now)?;
                let conditions = self.conditions(&task, &request.observation)?;
                let mut ids = BTreeSet::new();
                for item in &request.experiences {
                    TrustedRepairExperience::attest(item.clone())?;
                    if !ids.insert(&item.id)
                        || item
                            .conditions
                            .iter()
                            .any(|(key, value)| conditions.get(key) != Some(value))
                        || task
                            .problem
                            .keywords
                            .iter()
                            .any(|word| !item.keywords.contains(word))
                    {
                        return Err(invalid("repair experience does not match current problem"));
                    }
                }
                if request.experiences.windows(2).any(|items| {
                    items[0].recorded_at_ms < items[1].recorded_at_ms
                        || (items[0].recorded_at_ms == items[1].recorded_at_ms
                            && items[0].id > items[1].id)
                }) {
                    return Err(invalid("repair experiences are not stably ordered"));
                }
                let operation = approval::ProposedOperation {
                    task_id: task.id.clone(),
                    task_revision: task.revision,
                    operation_id: format!("{}-repair", task.id),
                    target: task.problem.target_id.clone(),
                    action: serde_json::json!({"kind":"repair_with_harness", "executor_id":self.config.target.executor_id, "request":request}),
                };
                operation.validate()?;
                if !self.config.approval.allows(&operation) {
                    return Err(invalid("Harness repair is outside current hard policy"));
                }
                task.observation = Some(request.observation.clone());
                task.operation = Some(operation.clone());
                task.stage = RecoveryStage::AwaitingApproval;
                effects.push(RecoveryEffect::RequestApproval {
                    operation,
                    policy: self.config.approval.clone(),
                });
                self.save_task(task, now)?;
            }
            RecoveryEvent::BeginExperience { job_id } => {
                let job = self
                    .experiences
                    .get_mut(job_id)
                    .ok_or_else(|| invalid("experience job missing"))?;
                if job.report.is_some() || job.call_id.is_some() || job.delivered {
                    return Err(RecoveryError::Conflict);
                }
                job.attempt = job.attempt.checked_add(1).ok_or(RecoveryError::Capacity)?;
                let call_id = format!("{}-summary-{}", job.id, job.attempt);
                job.call_id = Some(call_id.clone());
                job.last_error = None;
                effects.push(RecoveryEffect::SummarizeExperience {
                    job: Box::new(job.clone()),
                    call_id,
                });
            }
            RecoveryEvent::ExperienceSummarized {
                job_id,
                call_id,
                report,
            } => {
                report.validate()?;
                if let Scriptability::Possible {
                    candidate: Some(script),
                    ..
                } = &report.scriptability
                {
                    if script.generated_by_harness != self.config.execution_harness
                        || script.generated_in_session != *call_id
                    {
                        return Err(invalid("unexpected summary provenance"));
                    }
                    if let Some(old) = self.artifacts.get(&(script.id.clone(), script.version))
                        && old != script
                    {
                        return Err(invalid("artifact version changed"));
                    }
                }
                let job = self
                    .experiences
                    .get_mut(job_id)
                    .ok_or_else(|| invalid("experience job missing"))?;
                if job.call_id.as_ref() != Some(call_id) || job.report.is_some() {
                    return Err(RecoveryError::Conflict);
                }
                let request: HarnessRepairRequest = serde_json::from_value(
                    job.task
                        .operation
                        .as_ref()
                        .ok_or_else(|| invalid("missing repair operation"))?
                        .action["request"]
                        .clone(),
                )
                .map_err(|_| invalid("invalid repair request"))?;
                if report
                    .related_experience_ids
                    .iter()
                    .any(|id| !request.experiences.iter().any(|item| &item.id == id))
                {
                    return Err(invalid("unknown related experience"));
                }
                job.report = Some(report.clone());
                job.call_id = None;
                if let Scriptability::Possible {
                    candidate: Some(script),
                    ..
                } = &report.scriptability
                {
                    self.artifacts
                        .insert((script.id.clone(), script.version), script.clone());
                }
            }
            RecoveryEvent::ExperienceFailed {
                job_id,
                call_id,
                reason,
            } => {
                text(reason, 4096)?;
                let job = self
                    .experiences
                    .get_mut(job_id)
                    .ok_or_else(|| invalid("experience job missing"))?;
                if job.call_id.as_ref() != Some(call_id) {
                    return Err(RecoveryError::Conflict);
                }
                job.call_id = None;
                job.last_error = Some(reason.clone());
            }
            RecoveryEvent::ExperienceDelivered { job_id } => {
                let job = self
                    .experiences
                    .get_mut(job_id)
                    .ok_or_else(|| invalid("experience job missing"))?;
                if job.report.is_none() {
                    return Err(invalid("summary not completed"));
                }
                job.delivered = true;
            }
            _ => return Err(invalid("not an experience workflow event")),
        }
        Ok(effects)
    }
    fn apply_task(
        &mut self,
        event: &RecoveryEvent,
        now: u64,
        permit: Option<ExecutionPermit>,
    ) -> Result<Vec<RecoveryEffect>, RecoveryError> {
        if now < self.updated_at_ms {
            return Err(invalid("clock moved backwards"));
        }
        let mut effects = Vec::new();
        match event {
            RecoveryEvent::Register { problem, incident } => {
                problem.validate()?;
                stable_conditions(problem, &self.config.target)?;
                if problem.target_id != self.config.target.target_id {
                    return Err(invalid("wrong target"));
                }
                if let Some(old) = self
                    .tasks
                    .values()
                    .find(|t| t.problem.incident_id == problem.incident_id)
                {
                    if old.problem.target_id != problem.target_id
                        || old.problem.fingerprint != problem.fingerprint
                    {
                        return Err(invalid("incident identity changed"));
                    }
                    return Ok(effects);
                }
                if incident.incident_id != problem.incident_id
                    || incident.revision != problem.incident_revision
                    || !incident.active
                {
                    return Err(invalid("current active incident required"));
                }
                if self.tasks.values().any(|task| !task.stage.terminal()) {
                    return Err(RecoveryError::Busy);
                }
                if self.tasks.len() >= self.config.max_tasks {
                    return Err(RecoveryError::Capacity);
                }
                let mut identity = self
                    .revision
                    .checked_add(1)
                    .ok_or(RecoveryError::Capacity)?;
                while self.tasks.contains_key(&format!("task-{identity:016x}")) {
                    identity = identity.checked_add(1).ok_or(RecoveryError::Capacity)?;
                }
                let task = RecoveryTask {
                    id: format!("task-{identity:016x}"),
                    revision: 0,
                    problem: problem.clone(),
                    stage: RecoveryStage::Queued,
                    approval_id: None,
                    operation: None,
                    observation: None,
                    receipt: None,
                    verification: None,
                    result_check: None,
                    note: None,
                    created_at_ms: now,
                    updated_at_ms: now,
                };
                self.save_task(task, now)?;
            }
            RecoveryEvent::ApprovalAttached {
                task_id,
                revision,
                record,
            } => {
                let mut task = self.current(task_id, *revision)?;
                if task.stage != RecoveryStage::AwaitingApproval || task.approval_id.is_some() {
                    return Err(RecoveryError::Conflict);
                }
                self.bind_approval(&task, record)?;
                task.approval_id = Some(record.request.request_id.clone());
                if matches!(
                    record.state,
                    ApprovalState::Executing
                        | ApprovalState::Unknown
                        | ApprovalState::Executed
                        | ApprovalState::Failed
                ) && let Some(effect) = self.mark_unknown(&mut task, now)?
                {
                    effects.push(effect);
                }
                self.save_task(task, now)?;
            }
            RecoveryEvent::ApprovalResolved {
                task_id,
                revision,
                record,
            } => {
                let mut task = self.current(task_id, *revision)?;
                if task.stage != RecoveryStage::AwaitingApproval || task.approval_id.is_none() {
                    return Err(RecoveryError::Conflict);
                }
                self.bind_approval(&task, record)?;
                match record.state {
                    ApprovalState::Denied
                    | ApprovalState::Expired
                    | ApprovalState::Canceled
                    | ApprovalState::Revoked => {
                        task.stage = RecoveryStage::Denied;
                    }
                    ApprovalState::Executing
                    | ApprovalState::Unknown
                    | ApprovalState::Executed
                    | ApprovalState::Failed => {
                        if let Some(effect) = self.mark_unknown(&mut task, now)? {
                            effects.push(effect);
                        }
                    }

                    _ => {}
                }
                self.save_task(task, now)?;
            }
            RecoveryEvent::ExecutionAuthorized {
                task_id,
                revision,
                approval,
                observation,
                incident,
                authority,
            } => {
                let mut task = self.current(task_id, *revision)?;
                if task.stage != RecoveryStage::AwaitingApproval || task.approval_id.is_none() {
                    return Err(RecoveryError::Conflict);
                }
                self.bind_approval(&task, approval)?;
                if approval.state != ApprovalState::Executing
                    || now / 1000 >= approval.request.expires_at
                    || approval.updated_at > now / 1000
                {
                    return Err(invalid("current consumed approval required"));
                }
                if authority.target_id != task.problem.target_id
                    || !crate::control::identity::valid_id(&authority.epoch)
                    || incident.incident_id != task.problem.incident_id
                    || incident.revision < task.problem.incident_revision
                    || !incident.active
                {
                    return Err(invalid("current incident and target ownership required"));
                }
                self.observe(observation, now)?;
                self.conditions(&task, observation)?;
                if task
                    .operation
                    .as_ref()
                    .is_none_or(|op| op.action["kind"] != "repair_with_harness")
                {
                    return Err(invalid("missing repair intent"));
                }
                task.stage = RecoveryStage::Executing;
                task.observation = Some(observation.clone());
                self.save_task(task, now)?;
                if let Some(permit) = permit {
                    effects.push(RecoveryEffect::Execute {
                        permit,
                        authority: authority.clone(),
                        timeout_secs: self.config.target.action_timeout_secs,
                    });
                }
            }
            RecoveryEvent::ExecutionRecorded {
                task_id,
                revision,
                receipt,
                approval,
            } => {
                let mut task = self.current(task_id, *revision)?;
                if task.stage != RecoveryStage::Executing {
                    return Err(RecoveryError::Conflict);
                }
                self.bind_approval(&task, approval)?;
                let op = task
                    .operation
                    .as_ref()
                    .ok_or_else(|| invalid("missing operation"))?;
                evidence(&receipt.evidence_refs)?;
                text(&receipt.summary, 8192)?;
                if receipt.operation_id != op.operation_id
                    || receipt.target_id != op.target
                    || (receipt.outcome != RepairExecutionOutcome::Unknown
                        && !receipt.executor_stopped)
                {
                    return Err(invalid(
                        "execution receipt identity or stopping state mismatch",
                    ));
                }
                let expected = match receipt.outcome {
                    RepairExecutionOutcome::Executed => ApprovalState::Executed,
                    RepairExecutionOutcome::Failed => ApprovalState::Failed,
                    RepairExecutionOutcome::Unknown => ApprovalState::Unknown,
                };
                if approval.state != expected {
                    return Err(invalid("execution and approval facts differ"));
                }
                if op.action["kind"] == "repair_with_harness" {
                    let expected: Vec<_> = self
                        .repair_actions
                        .get(&op.operation_id)
                        .cloned()
                        .into_iter()
                        .collect();
                    if receipt.execution_trace != expected {
                        return Err(invalid("receipt differs from committed repair action"));
                    }
                }
                if receipt.execution_trace.len() > 1 {
                    return Err(invalid("repair mutation budget exceeded"));
                }
                for action in &receipt.execution_trace {
                    self.validate_action(
                        action,
                        task.observation
                            .as_ref()
                            .ok_or_else(|| invalid("missing observation"))?,
                    )?;
                }
                task.receipt = Some(receipt.clone());
                if receipt.outcome == RepairExecutionOutcome::Executed {
                    task.stage = RecoveryStage::Verifying;
                    effects.push(RecoveryEffect::Verify {
                        task_id: task.id.clone(),
                        operation: op.clone(),
                        receipt: receipt.clone(),
                    });
                } else {
                    let outcome = if receipt.outcome == RepairExecutionOutcome::Failed {
                        RepairOutcome::Failed
                    } else {
                        RepairOutcome::Unknown
                    };
                    effects.push(self.finish(&mut task, outcome, now)?);
                }
                self.save_task(task, now)?;
            }
            RecoveryEvent::VerificationRecorded {
                task_id,
                revision,
                verification,
            } => {
                let mut task = self.current(task_id, *revision)?;
                if task.stage != RecoveryStage::Verifying {
                    return Err(RecoveryError::Conflict);
                }
                self.verification(&task, verification, now)?;
                task.verification = Some(verification.clone());
                let outcome = match (verification.healthy, verification.executor_stopped) {
                    (Some(true), true) => RepairOutcome::Verified,
                    (Some(false), true) => RepairOutcome::Failed,
                    _ => RepairOutcome::Unknown,
                };
                effects.push(self.finish(&mut task, outcome, now)?);
                self.save_task(task, now)?;
            }
            RecoveryEvent::VerificationUnavailable {
                task_id,
                revision,
                reason,
            } => {
                let mut task = self.current(task_id, *revision)?;
                if task.stage != RecoveryStage::Verifying {
                    return Err(RecoveryError::Conflict);
                }
                text(reason, 8192)?;
                task.note = Some(reason.clone());
                effects.push(self.finish(&mut task, RepairOutcome::Unknown, now)?);
                self.save_task(task, now)?;
            }
            RecoveryEvent::ResultChecked {
                task_id,
                revision,
                execution,
                verification,
                actor,
                approval,
            } => {
                let mut task = self.current(task_id, *revision)?;
                if task.stage != RecoveryStage::Unknown {
                    return Err(RecoveryError::Conflict);
                }
                self.bind_approval(&task, approval)?;
                self.verification(&task, verification, now)?;
                text(actor, 128)?;
                evidence(&execution.evidence_refs)?;
                let op = task
                    .operation
                    .as_ref()
                    .ok_or_else(|| invalid("missing operation"))?;
                if execution.operation_id != op.operation_id
                    || execution.target_id != op.target
                    || execution.executor_id != self.config.target.executor_id
                    || (execution.checked_at_ms < task.updated_at_ms
                        && !task
                            .result_check
                            .as_ref()
                            .is_some_and(|r| &r.execution == execution))
                    || execution.checked_at_ms > now
                    || now - execution.checked_at_ms > 30_000
                    || execution.executor_stopped != verification.executor_stopped
                    || (execution.outcome != CheckedExecution::Unknown
                        && !execution.executor_stopped)
                {
                    return Err(invalid("invalid independent execution evidence"));
                }
                let known = task
                    .result_check
                    .as_ref()
                    .map(|r| r.execution.outcome)
                    .filter(|v| *v != CheckedExecution::Unknown)
                    .or_else(|| {
                        task.receipt.as_ref().and_then(|r| match r.outcome {
                            RepairExecutionOutcome::Executed => Some(CheckedExecution::Executed),
                            RepairExecutionOutcome::Failed => Some(CheckedExecution::Failed),
                            RepairExecutionOutcome::Unknown => None,
                        })
                    });
                if known.is_some_and(|old| old != execution.outcome) {
                    return Err(invalid("cannot overwrite confirmed execution facts"));
                }
                let allowed = match approval.state {
                    ApprovalState::Unknown => true,
                    ApprovalState::Executed => execution.outcome == CheckedExecution::Executed,
                    ApprovalState::Failed => {
                        execution.outcome == CheckedExecution::Failed
                            || (execution.outcome == CheckedExecution::NotExecuted
                                && task.result_check.as_ref().is_some_and(|r| {
                                    r.execution.outcome == CheckedExecution::NotExecuted
                                }))
                    }
                    _ => false,
                };
                if !allowed {
                    return Err(invalid("result check conflicts with approval"));
                }
                // The prepared event carries evidence. The caller must reconcile the
                // approval first or in the same atomic transaction before release.

                task.result_check = Some(ResultCheckRecord {
                    execution: execution.clone(),
                    actor: actor.clone(),
                });
                task.verification = Some(verification.clone());
                if approval.state == ApprovalState::Unknown {
                    self.save_task(task, now)?;
                    return Ok(effects);
                }
                match execution.outcome {
                    CheckedExecution::Unknown => {}
                    CheckedExecution::NotExecuted => {
                        task.stage = RecoveryStage::Canceled;
                    }
                    CheckedExecution::Executed | CheckedExecution::Failed => {
                        task.receipt = Some(RepairReceipt {
                            execution_trace: self
                                .repair_actions
                                .get(&op.operation_id)
                                .cloned()
                                .into_iter()
                                .collect(),
                            operation_id: op.operation_id.clone(),
                            target_id: op.target.clone(),
                            outcome: if execution.outcome == CheckedExecution::Executed {
                                RepairExecutionOutcome::Executed
                            } else {
                                RepairExecutionOutcome::Failed
                            },
                            executor_stopped: true,
                            evidence_refs: execution.evidence_refs.clone(),
                            summary: "independent execution result check".into(),
                        });
                        if execution.outcome == CheckedExecution::Failed {
                            effects.push(self.finish(&mut task, RepairOutcome::Failed, now)?);
                        } else if let Some(healthy) = verification.healthy {
                            effects.push(self.finish(
                                &mut task,
                                if healthy {
                                    RepairOutcome::Verified
                                } else {
                                    RepairOutcome::Failed
                                },
                                now,
                            )?);
                        }
                    }
                }
                self.save_task(task, now)?;
            }
            RecoveryEvent::Resume { task_id, revision } => {
                let mut task = self.current(task_id, *revision)?;
                if task.stage != RecoveryStage::Paused {
                    return Err(RecoveryError::Conflict);
                }
                task.stage = RecoveryStage::AwaitingApproval;
                if task.approval_id.is_none() {
                    effects.push(RecoveryEffect::RequestApproval {
                        operation: task
                            .operation
                            .clone()
                            .ok_or_else(|| invalid("missing original intent"))?,
                        policy: self.config.approval.clone(),
                    });
                }
                self.save_task(task, now)?;
            }
            RecoveryEvent::Cancel {
                task_id,
                revision,
                approval,
            } => {
                let mut task = self.current(task_id, *revision)?;
                if task.stage.terminal()
                    || matches!(
                        task.stage,
                        RecoveryStage::Executing
                            | RecoveryStage::Unknown
                            | RecoveryStage::Verifying
                    )
                {
                    return Err(invalid(
                        "dispatched work requires independent result evidence",
                    ));
                }
                if task.approval_id.is_some() {
                    let record = approval
                        .as_ref()
                        .ok_or_else(|| invalid("cancel linked approval first"))?;
                    self.bind_approval(&task, record)?;
                    if !matches!(
                        record.state,
                        ApprovalState::Canceled
                            | ApprovalState::Denied
                            | ApprovalState::Expired
                            | ApprovalState::Revoked
                    ) {
                        return Err(invalid("approval still grants authority"));
                    }
                } else if task.operation.is_some() {
                    return Err(invalid(
                        "resolve pending approval association before cancellation",
                    ));
                }
                task.stage = RecoveryStage::Canceled;
                self.save_task(task, now)?;
            }
            RecoveryEvent::Recover => {
                self.recovery_required = false;
                let ids: Vec<_> = self
                    .experiences
                    .values()
                    .filter(|job| job.call_id.is_some())
                    .map(|job| job.id.clone())
                    .collect();
                for id in ids {
                    let job = self.experiences.get_mut(&id).unwrap();
                    job.call_id = None;
                    job.last_error = Some("summary interrupted; retry independently".into());
                }
                let tasks: Vec<_> = self.tasks.values().cloned().collect();
                for mut task in tasks {
                    match task.stage {
                        RecoveryStage::Executing => {
                            let op = task
                                .operation
                                .as_ref()
                                .ok_or_else(|| invalid("missing execution intent"))?;
                            task.receipt = Some(RepairReceipt {
                                execution_trace: self
                                    .repair_actions
                                    .get(&op.operation_id)
                                    .cloned()
                                    .into_iter()
                                    .collect(),
                                operation_id: op.operation_id.clone(),
                                target_id: op.target.clone(),
                                outcome: RepairExecutionOutcome::Unknown,
                                executor_stopped: false,
                                evidence_refs: vec![format!(
                                    "approval:{}",
                                    task.approval_id.as_deref().unwrap_or("unknown")
                                )],
                                summary: "interrupted execution; independent evidence required"
                                    .into(),
                            });
                            effects.push(self.finish(&mut task, RepairOutcome::Unknown, now)?);
                            task.note =
                                Some("interrupted execution; explicit evidence required".into());
                        }
                        RecoveryStage::AwaitingApproval => {
                            task.stage = RecoveryStage::Paused;
                        }
                        _ => continue,
                    }
                    self.save_task(task, now)?;
                }
            }
            _ => return Err(invalid("not a recovery workflow event")),
        }
        Ok(effects)
    }

    fn verification(
        &self,
        task: &RecoveryTask,
        v: &BusinessVerification,
        now: u64,
    ) -> Result<(), RecoveryError> {
        evidence(&v.evidence_refs)?;
        let op = task
            .operation
            .as_ref()
            .ok_or_else(|| invalid("missing operation"))?;
        if v.operation_id != op.operation_id
            || v.target_id != op.target
            || v.profile != self.config.target.verification_profile
            || (v.verified_at_ms < task.updated_at_ms && task.verification.as_ref() != Some(v))
            || v.verified_at_ms > now
            || now - v.verified_at_ms > 30_000
        {
            return Err(invalid(
                "verification identity, profile or freshness mismatch",
            ));
        }
        Ok(())
    }
    fn finish(
        &mut self,
        task: &mut RecoveryTask,
        outcome: RepairOutcome,
        now: u64,
    ) -> Result<RecoveryEffect, RecoveryError> {
        let operation = task
            .operation
            .as_ref()
            .ok_or_else(|| invalid("missing operation"))?;
        if operation.action["kind"] != "repair_with_harness" {
            return Err(invalid("missing repair intent"));
        }
        let receipt = task
            .receipt
            .as_ref()
            .ok_or_else(|| invalid("missing receipt"))?;
        if outcome != RepairOutcome::Verified {
            for script in &receipt.execution_trace {
                self.quarantined.insert((script.id.clone(), script.version));
            }
        }
        let id = format!("{}-{outcome:?}", operation.operation_id);
        task.stage = match outcome {
            RepairOutcome::Verified => RecoveryStage::Completed,
            RepairOutcome::Failed => RecoveryStage::Failed,
            RepairOutcome::Unknown => RecoveryStage::Unknown,
        };
        if !self.experiences.contains_key(&id) {
            self.experiences.insert(
                id.clone(),
                ExperienceJob {
                    id: id.clone(),
                    task: task.clone(),
                    outcome,
                    recorded_at_ms: now,
                    attempt: 0,
                    call_id: None,
                    report: None,
                    last_error: None,
                    delivered: false,
                },
            );
        }
        Ok(RecoveryEffect::ExperiencePending { job_id: id })
    }
}
