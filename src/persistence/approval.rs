use super::journal::Journal;
use recuvora_core::operation::Prepared;
pub use recuvora_core::recovery::approval::*;
use serde::{Deserialize, Serialize};
use std::path::Path;
use thiserror::Error;

pub struct ApprovalStore {
    journal: Journal,
    ledger: ApprovalLedger,
}
impl ApprovalStore {
    pub fn open(
        data_dir: impl AsRef<Path>,
        config: ApprovalStoreConfig,
        now: u64,
    ) -> Result<Self, ApprovalError> {
        config.validate()?;
        let journal = Journal::open(
            data_dir.as_ref().join("approvals.jsonl"),
            "approval",
            serde_json::to_value(&config)?,
            config.max_journal_bytes,
        )?;
        let limits = ApprovalLimits {
            max_requests: config.max_requests,
        };
        let mut entries = Vec::<ApprovalEntry>::new();
        for record in journal.records() {
            let entry: ApprovalEntry = serde_json::from_value(record.payload.clone())?;
            let input = &record.request.input;
            if input["config"] != serde_json::to_value(&limits)?
                || input["prior_digest"] != entry.prior_digest
                || input["event"] != serde_json::to_value(&entry.event)?
                || input["now"] != entry.now
                || entry.commit_id != record.request.id
                || entry.sequence != record.request.revision
            {
                return Err(ApprovalError::Corrupt(
                    "transaction payload does not match commit binding".into(),
                ));
            }
            entries.push(entry);
        }
        let ledger = ApprovalLedger::restore(limits, &entries)?;
        let mut store = Self { journal, ledger };
        if store.ledger.recovery_required() {
            let pending = store
                .ledger
                .prepare_recovery(store.journal.next_id(), now)?;
            store.commit_prepared(pending)?;
        }
        Ok(store)
    }
    pub fn state(&self) -> &ApprovalLedger {
        &self.ledger
    }
    pub fn ensure_current(&self) -> Result<(), ApprovalError> {
        Ok(self.journal.available()?)
    }
    #[cfg(test)]
    pub(crate) fn fail_after_commits(&mut self, successful_commits: usize, after_sync: bool) {
        self.journal
            .fail_after_commits(successful_commits, after_sync);
    }
    fn commit_prepared(
        &mut self,
        pending: Prepared<ApprovalLedger, ApprovalEffect>,
    ) -> Result<Vec<ApprovalEffect>, ApprovalError> {
        let entry = pending
            .state()
            .latest_entry()
            .ok_or(ApprovalError::Conflict)?;
        let receipt = self
            .journal
            .commit(pending.request(), serde_json::to_value(entry)?)?;
        let committed = pending.confirm(receipt)?;
        self.ledger = committed.state;
        Ok(committed.effects)
    }
    fn change(
        &mut self,
        id: &str,
        change: ApprovalChange,
        policy: Option<&ApprovalPolicy>,
        now: u64,
    ) -> Result<Vec<ApprovalEffect>, ApprovalError> {
        let pending = self.ledger.prepare(
            self.journal.next_id(),
            ApprovalEvent::Changed {
                request_id: id.into(),
                change,
            },
            policy,
            now,
        )?;
        self.commit_prepared(pending)
    }
    fn changed_record(
        &mut self,
        id: &str,
        change: ApprovalChange,
        policy: Option<&ApprovalPolicy>,
        now: u64,
    ) -> Result<ApprovalRecord, ApprovalError> {
        self.change(id, change, policy, now)?;
        self.get(id).cloned().ok_or(ApprovalError::NotFound)
    }
    pub fn get(&self, id: &str) -> Option<&ApprovalRecord> {
        self.ledger.get(id)
    }
    pub fn list(&self) -> Vec<ApprovalRecord> {
        self.ledger.list()
    }
    pub fn find_operation(&self, task_id: &str, operation_id: &str) -> Option<&ApprovalRecord> {
        self.ledger.find_operation(task_id, operation_id)
    }
    pub fn request(
        &mut self,
        operation: ProposedOperation,
        policy: ApprovalPolicy,
        now: u64,
    ) -> Result<ApprovalRecord, ApprovalError> {
        self.journal.available()?;
        if let Some(old) = self
            .ledger
            .find_operation(&operation.task_id, &operation.operation_id)
        {
            return if old.request.operation == operation && old.request.policy == policy {
                Ok(old.clone())
            } else {
                Err(ApprovalError::Conflict)
            };
        }
        let pending =
            self.ledger
                .prepare_request(self.journal.next_id(), operation.clone(), policy, now)?;
        self.commit_prepared(pending)?;
        self.ledger
            .find_operation(&operation.task_id, &operation.operation_id)
            .cloned()
            .ok_or(ApprovalError::NotFound)
    }
    pub fn decide_human(
        &mut self,
        id: &str,
        decision: ApprovalDecision,
        reason: String,
        actor: String,
        policy: &ApprovalPolicy,
        now: u64,
    ) -> Result<ApprovalRecord, ApprovalError> {
        let revision = self.get(id).ok_or(ApprovalError::NotFound)?.revision;
        self.decide_human_at_revision(
            id,
            revision,
            ApprovalAssessment {
                decision,
                reason,
                reviewer: AssessmentSource::Human { actor },
            },
            policy,
            now,
        )
    }
    pub fn decide_human_at_revision(
        &mut self,
        id: &str,
        expected_revision: u64,
        assessment: ApprovalAssessment,
        policy: &ApprovalPolicy,
        now: u64,
    ) -> Result<ApprovalRecord, ApprovalError> {
        self.changed_record(
            id,
            ApprovalChange::HumanDecision {
                expected_revision,
                assessment,
            },
            Some(policy),
            now,
        )
    }
    pub fn begin_harness_review(
        &mut self,
        id: &str,
        expected_revision: u64,
        policy: &ApprovalPolicy,
        timeout_secs: u64,
        now: u64,
    ) -> Result<ReviewAttempt, ApprovalError> {
        self.change(
            id,
            ApprovalChange::BeginReview {
                expected_revision,
                timeout_secs,
            },
            Some(policy),
            now,
        )?
        .into_iter()
        .find_map(|e| {
            if let ApprovalEffect::Review(attempt) = e {
                Some(attempt)
            } else {
                None
            }
        })
        .ok_or(ApprovalError::Conflict)
    }
    pub fn assess_attempt(
        &mut self,
        attempt: &ReviewAttempt,
        assessment: ModelAssessment,
        reviewer: ReviewerIdentity,
        policy: &ApprovalPolicy,
        now: u64,
    ) -> Result<ApprovalRecord, ApprovalError> {
        if assessment.request_id != attempt.request_id {
            return Err(ApprovalError::Conflict);
        }
        self.changed_record(
            &attempt.request_id,
            ApprovalChange::AssessAttempt {
                attempt: attempt.clone(),
                assessment: ApprovalAssessment {
                    decision: assessment.decision,
                    reason: assessment.reason,
                    reviewer: AssessmentSource::Harness {
                        harness_id: reviewer.harness_id,
                        session_id: reviewer.session_id,
                    },
                },
            },
            Some(policy),
            now,
        )
    }
    pub fn fail_review_attempt(
        &mut self,
        attempt: &ReviewAttempt,
        reason: String,
        policy: &ApprovalPolicy,
        now: u64,
    ) -> Result<ApprovalRecord, ApprovalError> {
        self.changed_record(
            &attempt.request_id,
            ApprovalChange::FailReview {
                attempt: attempt.clone(),
                reason,
            },
            Some(policy),
            now,
        )
    }
    pub fn expire_review(
        &mut self,
        id: &str,
        expected_revision: u64,
        policy: &ApprovalPolicy,
        now: u64,
    ) -> Result<ApprovalRecord, ApprovalError> {
        let record = self.get(id).ok_or(ApprovalError::NotFound)?;
        if record.revision != expected_revision {
            return Err(ApprovalError::Conflict);
        }
        let attempt = record
            .active_review_attempt()
            .ok_or(ApprovalError::Conflict)?;
        if now < attempt.deadline {
            return Err(ApprovalError::ReviewNotDue);
        }
        self.fail_review_attempt(
            &attempt,
            "harness review deadline elapsed".into(),
            policy,
            now,
        )
    }
    pub fn revoke(
        &mut self,
        id: &str,
        reason: String,
        now: u64,
    ) -> Result<ApprovalRecord, ApprovalError> {
        self.changed_record(id, ApprovalChange::Revoke { reason }, None, now)
    }
    pub fn cancel(
        &mut self,
        id: &str,
        reason: String,
        now: u64,
    ) -> Result<ApprovalRecord, ApprovalError> {
        self.changed_record(id, ApprovalChange::Cancel { reason }, None, now)
    }
    pub fn mark_waiting_human(
        &mut self,
        id: &str,
        reason: String,
        now: u64,
    ) -> Result<ApprovalRecord, ApprovalError> {
        let policy = self
            .get(id)
            .ok_or(ApprovalError::NotFound)?
            .request
            .policy
            .clone();
        self.changed_record(
            id,
            ApprovalChange::WaitingHuman { reason },
            Some(&policy),
            now,
        )
    }
    pub fn consume(
        &mut self,
        id: &str,
        operation: &ProposedOperation,
        policy: &ApprovalPolicy,
        now: u64,
    ) -> Result<ExecutionPermit, ApprovalError> {
        if self
            .get(id)
            .ok_or(ApprovalError::NotFound)?
            .request
            .operation
            != *operation
        {
            return Err(ApprovalError::Conflict);
        }
        self.change(id, ApprovalChange::Consume, Some(policy), now)?
            .into_iter()
            .find_map(|e| {
                if let ApprovalEffect::Execute(permit) = e {
                    Some(permit)
                } else {
                    None
                }
            })
            .ok_or(ApprovalError::Conflict)
    }
    pub fn complete(
        &mut self,
        permit: ExecutionPermit,
        outcome: ExecutionOutcome,
        reason: String,
        now: u64,
    ) -> Result<ApprovalRecord, ApprovalError> {
        let id = permit.request_id().to_owned();
        let pending =
            self.ledger
                .prepare_complete(self.journal.next_id(), permit, outcome, reason, now)?;
        self.commit_prepared(pending)?;
        self.get(&id).cloned().ok_or(ApprovalError::NotFound)
    }
    pub fn recover_unknown(&mut self, id: &str, now: u64) -> Result<ApprovalRecord, ApprovalError> {
        self.changed_record(id, ApprovalChange::RecoverUnknown, None, now)
    }
    pub fn expire(&mut self, id: &str, now: u64) -> Result<ApprovalRecord, ApprovalError> {
        self.changed_record(id, ApprovalChange::Expire, None, now)
    }
    pub fn reconcile_unknown(
        &mut self,
        id: &str,
        outcome: ExecutionOutcome,
        reason: String,
        actor: String,
        now: u64,
    ) -> Result<ApprovalRecord, ApprovalError> {
        self.changed_record(
            id,
            ApprovalChange::Reconcile {
                outcome,
                reason,
                actor,
            },
            None,
            now,
        )
    }
    pub fn close(&mut self) -> Result<(), ApprovalError> {
        Ok(self.journal.close()?)
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ApprovalStoreConfig {
    pub max_requests: usize,
    pub max_journal_bytes: u64,
}

impl Default for ApprovalStoreConfig {
    fn default() -> Self {
        Self {
            max_requests: 10_000,
            max_journal_bytes: 32 * 1024 * 1024,
        }
    }
}

impl ApprovalStoreConfig {
    pub fn validate(&self) -> Result<(), ApprovalError> {
        if self.max_requests == 0
            || self.max_requests > 100_000
            || self.max_journal_bytes == 0
            || self.max_journal_bytes > 128 * 1024 * 1024
        {
            return Err(ApprovalError::Invalid("store limits"));
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum ApprovalError {
    #[error("invalid approval input: {0}")]
    Invalid(&'static str),
    #[error("approval request was not found")]
    NotFound,
    #[error("approval input conflicts with the durable request")]
    Conflict,
    #[error("operation falls outside the trusted policy scope")]
    OutOfScope,
    #[error("approval is expired")]
    Expired,
    #[error("human review waiting period has not elapsed")]
    ReviewNotDue,
    #[error("the harness review attempt timed out")]
    ReviewTimedOut,
    #[error("approval transition is not allowed from {0:?}")]
    InvalidState(ApprovalState),
    #[error("target has executing or unresolved work")]
    TargetBusy,
    #[error("approval storage capacity reached")]
    Capacity,
    #[error("approval journal writer is already locked: {0}")]
    Locked(std::io::Error),
    #[error("approval journal is corrupt: {0}")]
    Corrupt(String),
    #[error("approval store is unavailable after an I/O failure")]
    Unavailable,
    #[error("approval storage I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("approval JSON: {0}")]
    Json(#[from] serde_json::Error),
}

impl From<recuvora_core::recovery::approval::ApprovalError> for ApprovalError {
    fn from(error: recuvora_core::recovery::approval::ApprovalError) -> Self {
        use recuvora_core::recovery::approval::ApprovalError as E;
        match error {
            E::NotFound => Self::NotFound,
            E::Conflict => Self::Conflict,
            E::OutOfScope => Self::OutOfScope,
            E::Expired => Self::Expired,
            E::ReviewNotDue => Self::ReviewNotDue,
            E::ReviewTimedOut => Self::ReviewTimedOut,
            E::TargetBusy => Self::TargetBusy,
            E::Capacity => Self::Capacity,
            E::Invalid(value) => Self::Invalid(value),
            E::InvalidState(value) => Self::InvalidState(value),
            E::Corrupt(value) => Self::Corrupt(value),
            E::Json(value) => Self::Json(value),
            other => Self::Corrupt(other.to_string()),
        }
    }
}
impl From<super::journal::JournalError> for ApprovalError {
    fn from(error: super::journal::JournalError) -> Self {
        use super::journal::JournalError as E;
        match error {
            E::Io(e) => Self::Io(e),
            E::Corrupt(s) => Self::Corrupt(s),
            E::Capacity => Self::Capacity,
            E::Locked(e) => Self::Locked(e),
            E::Unavailable => Self::Unavailable,
            E::Conflict(_) => Self::Conflict,
            E::Json(e) => Self::Json(e),
            E::Invalid(_) => Self::Invalid("invalid journal path or configuration"),
        }
    }
}
impl From<recuvora_core::operation::CommitError> for ApprovalError {
    fn from(error: recuvora_core::operation::CommitError) -> Self {
        Self::Corrupt(error.to_string())
    }
}
