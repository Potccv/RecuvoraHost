//! Serializable approval policy, evidence and durable-record contracts.
use super::{MAX_ACTION, MAX_ID, MAX_REASON, text};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReviewerConfig {
    Human,
    Harness {
        harness_id: String,
    },
    HumanThenHarness {
        harness_id: String,
        human_wait_secs: u64,
        review_timeout_secs: u64,
    },
}

/// Loaded by the caller from trusted configuration, outside AI-writable targets.
/// Target and action-kind lists are exact matches and hard limits for all reviewers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalPolicy {
    pub id: String,
    pub version: u64,
    pub reviewer: ReviewerConfig,
    pub delegation: String,
    pub allowed_targets: Vec<String>,
    pub allowed_action_kinds: Vec<String>,
    pub ttl_secs: u64,
}

impl ApprovalPolicy {
    pub fn validate(&self) -> Result<(), ApprovalError> {
        text(&self.id, MAX_ID)?;
        text(&self.delegation, MAX_REASON)?;
        if self.version == 0 || !(1..=86_400).contains(&self.ttl_secs) {
            return Err(ApprovalError::Invalid("policy version or TTL"));
        }
        match &self.reviewer {
            ReviewerConfig::Human => {}
            ReviewerConfig::Harness { harness_id } => text(harness_id, MAX_ID)?,
            ReviewerConfig::HumanThenHarness {
                harness_id,
                human_wait_secs,
                review_timeout_secs,
            } => {
                text(harness_id, MAX_ID)?;
                if *human_wait_secs == 0
                    || !(1..=1800).contains(review_timeout_secs)
                    || human_wait_secs
                        .checked_add(*review_timeout_secs)
                        .is_none_or(|sum| sum >= self.ttl_secs)
                {
                    return Err(ApprovalError::Invalid(
                        "review deadlines leave no execution validity",
                    ));
                }
            }
        }
        for values in [&self.allowed_targets, &self.allowed_action_kinds] {
            if values.is_empty() || values.len() > 128 {
                return Err(ApprovalError::Invalid(
                    "policy scope must contain 1..128 entries",
                ));
            }
            for value in values {
                text(value, MAX_ID)?;
            }
        }
        Ok(())
    }

    pub fn allows(&self, operation: &ProposedOperation) -> bool {
        self.allowed_targets.contains(&operation.target)
            && operation
                .action
                .get("kind")
                .and_then(Value::as_str)
                .is_some_and(|kind| {
                    self.allowed_action_kinds
                        .iter()
                        .any(|allowed| allowed == kind)
                })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProposedOperation {
    pub task_id: String,
    pub task_revision: u64,
    pub operation_id: String,
    pub target: String,
    pub action: Value,
}

impl ProposedOperation {
    pub fn validate(&self) -> Result<(), ApprovalError> {
        for value in [&self.task_id, &self.operation_id, &self.target] {
            text(value, MAX_ID)?;
        }
        let kind =
            self.action
                .get("kind")
                .and_then(Value::as_str)
                .ok_or(ApprovalError::Invalid(
                    "action must be an object with a string kind",
                ))?;
        text(kind, MAX_ID)?;
        if serde_json::to_vec(&self.action)?.len() > MAX_ACTION {
            return Err(ApprovalError::Invalid("action is too large"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    Approve,
    Deny,
    Escalate,
}

/// This is the entire model response. Reviewer identity is supplied by the caller.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelAssessment {
    pub request_id: String,
    pub decision: ApprovalDecision,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewerIdentity {
    pub harness_id: String,
    pub session_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum AssessmentSource {
    Harness {
        harness_id: String,
        session_id: String,
    },
    Human {
        actor: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalAssessment {
    pub decision: ApprovalDecision,
    pub reason: String,
    pub reviewer: AssessmentSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalState {
    Pending,
    WaitingHuman,
    Approved,
    Denied,
    Revoked,
    Canceled,
    Expired,
    Executing,
    Executed,
    Failed,
    Unknown,
}

/// Persisted review routing, separate from execution authorization.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewStage {
    #[default]
    ReadyHarness,
    WaitingHuman,
    ReviewingHarness,
    NeedsHuman,
    Finished,
}

/// Caller-supplied correlation for one durable, bounded review attempt.
/// Model output must never supply these fields or the reviewer identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewAttempt {
    pub request_id: String,
    pub revision: u64,
    pub attempt: u64,
    pub harness_id: String,
    pub deadline: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalRequest {
    pub request_id: String,
    pub operation: ProposedOperation,
    pub policy: ApprovalPolicy,
    pub created_at: u64,
    pub expires_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalRecord {
    pub request: ApprovalRequest,
    pub state: ApprovalState,
    pub revision: u64,
    pub updated_at: u64,
    pub assessment: Option<ApprovalAssessment>,
    pub note: Option<String>,
    #[serde(default)]
    pub review_stage: ReviewStage,
    #[serde(default)]
    pub human_deadline: Option<u64>,
    #[serde(default)]
    pub review_deadline: Option<u64>,
    #[serde(default)]
    pub review_attempt: u64,
}

impl ApprovalRecord {
    pub fn active_review_attempt(&self) -> Option<ReviewAttempt> {
        if self.review_stage != ReviewStage::ReviewingHarness
            || self.state != ApprovalState::Pending
        {
            return None;
        }
        let harness_id = match &self.request.policy.reviewer {
            ReviewerConfig::Harness { harness_id }
            | ReviewerConfig::HumanThenHarness { harness_id, .. } => harness_id.clone(),
            ReviewerConfig::Human => return None,
        };
        Some(ReviewAttempt {
            request_id: self.request.request_id.clone(),
            revision: self.revision,
            attempt: self.review_attempt,
            harness_id,
            deadline: self.review_deadline?,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionOutcome {
    Executed,
    Failed,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ApprovalLimits {
    pub max_requests: usize,
}

impl Default for ApprovalLimits {
    fn default() -> Self {
        Self {
            max_requests: 10_000,
        }
    }
}

impl ApprovalLimits {
    pub fn validate(&self) -> Result<(), ApprovalError> {
        if self.max_requests == 0 || self.max_requests > 100_000 {
            return Err(ApprovalError::Invalid("store limits"));
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum ApprovalError {
    #[error("invalid approval input: {0}")]
    Invalid(&'static str),
    #[error("restored approval state requires explicit recovery commit")]
    RecoveryRequired,
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
    #[error("invalid approval history: {0}")]
    Corrupt(String),
    #[error(transparent)]
    Commit(#[from] crate::control::operation::CommitError),
    #[error("approval JSON: {0}")]
    Json(#[from] serde_json::Error),
}
