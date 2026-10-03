//! Pure approval decisions and commit-gated execution capabilities.
//!
//! Callers are trusted integration code: this is not an authentication boundary against
//! arbitrary code in the calling process. Model output is evidence, never a permit.

mod contract;
mod ledger;
mod transitions;

use crate::control::operation::Prepared;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

const MAX_ID: usize = 512;
const MAX_REASON: usize = 8192;
const MAX_ACTION: usize = 131_072;

pub use contract::{
    ApprovalAssessment, ApprovalDecision, ApprovalError, ApprovalLimits, ApprovalPolicy,
    ApprovalRecord, ApprovalRequest, ApprovalState, AssessmentSource, ExecutionOutcome,
    ModelAssessment, ProposedOperation, ReviewAttempt, ReviewStage, ReviewerConfig,
    ReviewerIdentity,
};

/// A one-use, non-cloneable capability produced only after the execution intent
/// was confirmed by the trusted caller. It does not implement Deserialize and its fields are private.
#[derive(Debug)]
pub struct ExecutionPermit {
    request_id: String,
    revision: u64,
    operation: ProposedOperation,
    store_identity: Arc<()>,
}

impl ExecutionPermit {
    pub fn request_id(&self) -> &str {
        &self.request_id
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn operation(&self) -> &ProposedOperation {
        &self.operation
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalEntry {
    pub prior_digest: String,
    pub commit_id: String,
    pub sequence: u64,
    pub now: u64,
    pub event: ApprovalEvent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
pub enum ApprovalEvent {
    /// Explicit caller restart recovery; never dispatches a stored intent.
    Recover,
    Requested {
        request: ApprovalRequest,
    },
    Changed {
        request_id: String,
        change: ApprovalChange,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "change", rename_all = "snake_case", deny_unknown_fields)]
pub enum ApprovalChange {
    HumanDecision {
        expected_revision: u64,
        assessment: ApprovalAssessment,
    },
    BeginReview {
        expected_revision: u64,
        timeout_secs: u64,
    },
    AssessAttempt {
        attempt: ReviewAttempt,
        assessment: ApprovalAssessment,
    },
    FailReview {
        attempt: ReviewAttempt,
        reason: String,
    },
    WaitingHuman {
        reason: String,
    },
    Revoke {
        reason: String,
    },
    Cancel {
        reason: String,
    },
    Expire,
    Consume,
    Complete {
        outcome: ExecutionOutcome,
        reason: String,
    },
    RecoverUnknown,
    Reconcile {
        outcome: ExecutionOutcome,
        reason: String,
        actor: String,
    },
}

/// Pure approval aggregate. The caller atomically commits its history and aggregate revision.
/// Restoring history produces no executable effects.
#[derive(Debug, Clone, Serialize)]
pub struct ApprovalLedger {
    #[serde(skip)]
    digest: String,
    #[serde(skip)]
    commit_ids: im::OrdSet<String>,
    config: ApprovalLimits,
    #[serde(skip)]
    recovery_required: bool,
    #[serde(skip)]
    sequence: u64,
    #[serde(skip)]
    records: crate::control::collections::Map<String, ApprovalRecord>,
    history: im::Vector<std::sync::Arc<ApprovalEntry>>,
    #[serde(skip)]
    identity: Arc<()>,
}

/// Released only after the caller confirms a successful durable commit.
#[derive(Debug)]
pub enum ApprovalEffect {
    Execute(ExecutionPermit),
    Review(ReviewAttempt),
}

fn text(value: &str, maximum: usize) -> Result<(), ApprovalError> {
    if value.trim().is_empty() || value.len() > maximum || value.contains('\0') {
        Err(ApprovalError::Invalid(
            "empty, oversized or NUL-containing text",
        ))
    } else {
        Ok(())
    }
}
