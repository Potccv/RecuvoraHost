//! Pure commit proposals. The trusted caller owns durable compare-and-swap.
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Exact logical transaction binding; this is domain data, not a storage format.
/// The caller scopes `id` and revision to the correct aggregate, atomically compares
/// the prior revision and persists the complete proposal. Reusing an ID with
/// different domain/input is a conflict, never a successful retry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitRequest {
    pub id: String,
    pub expected_revision: u64,
    pub revision: u64,
    pub domain: String,
    pub input: serde_json::Value,
}
impl CommitRequest {
    pub fn new(
        id: String,
        expected_revision: u64,
        domain: String,
        input: serde_json::Value,
    ) -> Result<Self, CommitError> {
        if !crate::control::identity::valid_id(&id) || !crate::control::identity::valid_id(&domain)
        {
            return Err(CommitError::InvalidIdentity);
        }
        let revision = expected_revision
            .checked_add(1)
            .ok_or(CommitError::RevisionExhausted)?;
        Ok(Self {
            id,
            expected_revision,
            revision,
            domain,
            input,
        })
    }
}

/// Trusted assertion that this exact proposal was durably committed.
/// Not authentication; never expose this constructor to model tools. Confirming
/// the same transaction twice must not release its effects twice. The caller resolves
/// uncertain commits from protected history and restores state without effects.
#[derive(Debug)]
pub struct CommitReceipt(CommitRequest);
impl CommitReceipt {
    pub fn confirmed(request: &CommitRequest) -> Self {
        Self(request.clone())
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CommitError {
    #[error("invalid commit or domain identity")]
    InvalidIdentity,
    #[error("commit revision exhausted")]
    RevisionExhausted,
    #[error("receipt does not match the pending commit content")]
    ReceiptMismatch,
}

/// Proposed state may be exported by the caller. Effects are private until commit.
/// Dropping a proposal never changes the input state. The bound input contains
/// a versioned commitment to prior configuration/history and the complete new
/// transition input; it must not recursively embed prior commit requests.
#[derive(Debug)]
pub struct Prepared<S, E = ()> {
    request: CommitRequest,
    state: S,
    effects: Vec<E>,
}
impl<S, E> Prepared<S, E> {
    pub fn new_bound(
        id: String,
        expected_revision: u64,
        domain: String,
        input: serde_json::Value,
        state: S,
        effects: Vec<E>,
    ) -> Result<Self, CommitError> {
        Ok(Self {
            request: CommitRequest::new(id, expected_revision, domain, input)?,
            state,
            effects,
        })
    }
    pub fn request(&self) -> &CommitRequest {
        &self.request
    }
    pub fn state(&self) -> &S {
        &self.state
    }
    pub fn confirm(self, receipt: CommitReceipt) -> Result<Committed<S, E>, CommitError> {
        if receipt.0 != self.request {
            return Err(CommitError::ReceiptMismatch);
        }
        Ok(Committed {
            state: self.state,
            effects: self.effects,
        })
    }
}
/// Install after a successful commit confirmation; dispatch effects at most once.
/// Restart recovery must not reconstruct executable effects.
#[derive(Debug)]
pub struct Committed<S, E = ()> {
    pub state: S,
    pub effects: Vec<E>,
}
