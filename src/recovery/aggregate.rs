//! One locked Core aggregate and the independent durable Host dispatch evidence.
use super::*;
use crate::persistence::{
    approval::ApprovalStoreConfig, journal::Journal, knowledge::KnowledgeStoreConfig,
};
use recuvora_core::recovery::{approval, engine::*, knowledge::KnowledgeConfig};
use std::path::Path;

/// Host I/O evidence only. A dispatched marker is synced before entering the
/// backend, so a prepared marker alone proves that this Host did not dispatch.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DispatchRecord {
    operation: approval::ProposedOperation,
    dispatching: bool,
}

pub(super) struct State {
    pub(super) root_lock: storage_layout::RootStorageLock,
    pub(super) session: RecoverySession,
    pub(super) journal: Journal,
    pub(super) dispatch: Journal,
    pub(super) dispatch_pending: bool,
}
impl State {
    pub(super) fn dispatch_phase(
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
    pub(super) fn apply(
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
    pub(super) fn task(&self, id: &str) -> Result<RecoveryTask, RecoveryError> {
        self.session
            .task(id)
            .cloned()
            .ok_or_else(|| RecoveryError::Invalid("task not found".into()))
    }
}

impl State {
    pub(super) fn open(
        dir: &Path,
        config: &RecoveryConfig,
        backend_binding: serde_json::Value,
        clock: &dyn RecoveryClock,
        approval_config: ApprovalStoreConfig,
        knowledge_config: KnowledgeStoreConfig,
    ) -> Result<Self, RecoveryError> {
        config.validate()?;
        let root = dir;
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
        let storage_config = serde_json::json!({"engine":"recovery-session-v1","session":session_config,"approval_storage":approval_config,"knowledge_storage":knowledge_config,"backend":backend_binding});
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
                state.not_dispatched(&id, config, clock.now_ms())?;
            }
        }
        Ok(state)
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
