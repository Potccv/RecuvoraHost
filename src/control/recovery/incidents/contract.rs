//! Monitoring signals, checkpoints, incident records and store limits.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IncidentKind {
    Target,
    Coverage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalCondition {
    Active,
    Clear,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IncidentStatus {
    Open,
    Acknowledged,
    Resolved,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IncidentSignal {
    pub monitor_id: String,
    pub target_id: String,
    pub rule_id: String,
    pub kind: IncidentKind,
    pub condition: SignalCondition,
    pub summary: String,
    pub evidence: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IncidentAcknowledgement {
    pub actor: String,
    pub note: String,
    pub at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IncidentRecord {
    pub id: String,
    pub revision: u64,
    pub monitor_id: String,
    pub target_id: String,
    pub rule_id: String,
    pub kind: IncidentKind,
    pub status: IncidentStatus,
    pub condition: SignalCondition,
    pub summary: String,
    pub evidence: Value,
    pub first_seen: u64,
    pub last_seen: u64,
    pub resolved_at: Option<u64>,
    pub acknowledgement: Option<IncidentAcknowledgement>,
    pub occurrences: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Checkpoint {
    pub sequence: u64,
    pub value: Value,
    pub updated_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MonitorCommit {
    pub monitor_id: String,
    pub sequence: u64,
    pub checkpoint: Value,
    pub signals: Vec<IncidentSignal>,
    pub now_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct IncidentLimits {
    pub max_incidents: usize,
    pub max_monitors: usize,
}

impl Default for IncidentLimits {
    fn default() -> Self {
        Self {
            max_incidents: 10_000,
            max_monitors: 256,
        }
    }
}

impl IncidentLimits {
    pub fn validate(&self) -> Result<(), IncidentError> {
        if !(1..=100_000).contains(&self.max_incidents) || !(1..=4096).contains(&self.max_monitors)
        {
            return Err(IncidentError::Invalid("store limits out of range".into()));
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum IncidentError {
    #[error("invalid incident input: {0}")]
    Invalid(String),
    #[error("incident state conflict: {0}")]
    Conflict(String),
    #[error("incident not found: {0}")]
    NotFound(String),
    #[error("incident capacity exhausted: {0}")]
    Capacity(String),
    #[error("corrupt incident journal: {0}")]
    Corrupt(String),
    #[error(transparent)]
    Commit(#[from] crate::control::operation::CommitError),
}
