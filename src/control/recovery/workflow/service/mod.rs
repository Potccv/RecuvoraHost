//! Pure recovery transitions and caller-owned commit/execution contracts.
mod contract;
mod engine;
mod experience;
pub use experience::*;
mod query;
use crate::control::recovery::{approval, knowledge::*};
pub use contract::*;
pub use engine::*;
pub use query::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;
#[derive(Debug, Error)]
pub enum RecoveryError {
    #[error("invalid recovery input: {0}")]
    Invalid(String),
    #[error("task or target revision conflict")]
    Conflict,
    #[error("target has unfinished recovery work")]
    Busy,
    #[error("domain capacity exhausted")]
    Capacity,
    #[error(transparent)]
    Approval(#[from] approval::ApprovalError),
    #[error(transparent)]
    Knowledge(#[from] KnowledgeError),
    #[error(transparent)]
    Commit(#[from] crate::control::operation::CommitError),
}
fn text(value: &str, max: usize) -> Result<(), RecoveryError> {
    if value.trim().is_empty() || value.len() > max || value.contains('\0') {
        Err(RecoveryError::Invalid(
            "empty, oversized or NUL-containing value".into(),
        ))
    } else {
        Ok(())
    }
}
fn invalid(reason: &str) -> RecoveryError {
    RecoveryError::Invalid(reason.into())
}

impl From<recuvora_core::recovery::BusinessError> for RecoveryError {
    fn from(error: recuvora_core::recovery::BusinessError) -> Self {
        match error {
            recuvora_core::recovery::BusinessError::Invalid(reason) => Self::Invalid(reason),
            recuvora_core::recovery::BusinessError::Capacity => Self::Capacity,
        }
    }
}
