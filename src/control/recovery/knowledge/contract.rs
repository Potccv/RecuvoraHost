//! Neutral repair artifacts, exact queries and trusted caller commands.
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub use recuvora_core::recovery::knowledge::{
    KnowledgeQuery, MAX_ARTIFACT_BYTES, RepairArtifact, RepairOutcome,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct KnowledgeProjection {
    pub experiences: usize,
    pub artifacts: usize,
    pub quarantined_versions: usize,
    pub max_records: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeConfig {
    pub max_records: usize,
}

impl Default for KnowledgeConfig {
    fn default() -> Self {
        Self { max_records: 1024 }
    }
}

impl KnowledgeConfig {
    pub fn validate(&self) -> Result<(), KnowledgeError> {
        if !(1..=100_000).contains(&self.max_records) {
            return Err(KnowledgeError::Invalid("record limit out of range".into()));
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum KnowledgeError {
    #[error("invalid knowledge input: {0}")]
    Invalid(String),
    #[error("knowledge state conflict: {0}")]
    Conflict(String),
    #[error("knowledge capacity exhausted: {0}")]
    Capacity(String),
    #[error(transparent)]
    Commit(#[from] crate::control::operation::CommitError),
}

/// Trusted caller inputs; this enum cannot be deserialized into authority.
#[derive(Clone, Debug, Serialize)]
pub enum KnowledgeCommand {
    RecordExperience(super::TrustedRepairExperience),
    /// Monotonic capacity expansion retains all domain facts.
    ExpandCapacity {
        expected: KnowledgeConfig,
        target: KnowledgeConfig,
    },
}

/// Reconstructed only from the caller's protected, complete committed history.
pub struct KnowledgeReplayEntry {
    pub request: crate::control::operation::CommitRequest,
    pub command: KnowledgeCommand,
    pub receipt: crate::control::operation::CommitReceipt,
}

/// Complete data export for transport and queries, not installable authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct KnowledgeSnapshot {
    pub revision: u64,
    pub config: KnowledgeConfig,
    pub experiences: Vec<super::RepairExperience>,
}

impl From<recuvora_core::recovery::BusinessError> for KnowledgeError {
    fn from(error: recuvora_core::recovery::BusinessError) -> Self {
        match error {
            recuvora_core::recovery::BusinessError::Invalid(reason) => Self::Invalid(reason),
            recuvora_core::recovery::BusinessError::Capacity => {
                Self::Capacity("business data".into())
            }
        }
    }
}
