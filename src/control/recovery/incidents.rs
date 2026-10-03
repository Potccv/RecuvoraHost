//! Pure monitoring facts. Acknowledgement is attribution, never repair authority.
mod contract;
mod ledger;
mod transitions;
mod validation;

use crate::control::operation::Prepared;
use serde::{Deserialize, Serialize};

const MAX_CHECKPOINT_BYTES: usize = 131_072;
const MAX_EVIDENCE_BYTES: usize = 16_384;
pub const MAX_ACK_NOTE_BYTES: usize = 4096;

pub use contract::{
    Checkpoint, IncidentAcknowledgement, IncidentError, IncidentKind, IncidentLimits,
    IncidentRecord, IncidentSignal, IncidentStatus, MonitorCommit, SignalCondition,
};

type IncidentKey = (String, String, String, IncidentKind);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum IncidentEvent {
    Monitor {
        commit: MonitorCommit,
    },
    Acknowledge {
        id: String,
        expected_revision: u64,
        actor: String,
        note: String,
        now_ms: u64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IncidentEntry {
    pub prior_digest: String,
    pub commit_id: String,
    pub sequence: u64,
    pub event: IncidentEvent,
}

struct Transition {
    records: Vec<IncidentRecord>,
    monitor: Option<MonitorState>,
}

#[derive(Debug, Clone)]
struct MonitorState {
    commit: MonitorCommit,
    updated_at_ms: u64,
}

/// Pure incident aggregate; the caller owns durable storage and atomic revision checks.
#[derive(Debug, Clone, Serialize)]
pub struct IncidentLedger {
    #[serde(skip)]
    digest: String,
    #[serde(skip)]
    commit_ids: im::OrdSet<String>,
    config: IncidentLimits,
    #[serde(skip)]
    sequence: u64,
    history: im::Vector<std::sync::Arc<IncidentEntry>>,
    #[serde(skip)]
    records: crate::control::collections::Map<String, IncidentRecord>,
    #[serde(skip)]
    active: crate::control::collections::Map<IncidentKey, String>,
    #[serde(skip)]
    monitors: crate::control::collections::Map<String, MonitorState>,
}

impl IncidentLedger {
    pub fn checkpoint(&self, monitor_id: &str) -> Option<Checkpoint> {
        self.monitors.get(monitor_id).map(|state| Checkpoint {
            sequence: state.commit.sequence,
            value: state.commit.checkpoint.clone(),
            updated_at_ms: state.updated_at_ms,
        })
    }

    pub fn get(&self, id: &str) -> Option<IncidentRecord> {
        self.records.get(id).cloned()
    }

    pub fn list(&self) -> Vec<IncidentRecord> {
        self.map_records(Clone::clone)
    }

    /// Projects summaries without cloning every stored evidence object.
    pub fn map_records<T>(&self, projection: impl FnMut(&IncidentRecord) -> T) -> Vec<T> {
        self.records.values().map(projection).collect()
    }
}

fn next(value: u64) -> Result<u64, IncidentError> {
    value
        .checked_add(1)
        .ok_or_else(|| IncidentError::Capacity("sequence overflow".into()))
}
