//! Public diagnostic views, separate from persistent incident authority.
use super::{DiscoverySnapshot, NodeErrorLog};
use serde::Serialize;

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Freshness {
    Missing,
    Fresh,
    Stale,
}
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Coverage {
    Unknown,
    Complete,
    Partial,
    Unavailable,
}

#[derive(Clone, Debug, Serialize)]
pub struct MonitorSnapshot {
    pub id: String,
    pub target_id: String,
    pub source_id: String,
    pub view_role: Option<String>,
    pub extension_id: String,
    pub contract: String,
    pub version: u32,
    pub method: String,
    pub freshness: Freshness,
    pub coverage: Coverage,
    pub running: bool,
    pub last_received_at_ms: Option<u64>,
    pub received_error_count: u64,
    pub last_error_log: Option<NodeErrorLog>,
    pub generation: Option<String>,
    pub cursor: Option<String>,
    pub last_error: Option<String>,
    pub interval_ms: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct MonitoringSnapshot {
    pub monitors: Vec<MonitorSnapshot>,
    pub discoveries: Vec<DiscoverySnapshot>,
    pub runtime_error: Option<String>,
    pub running: bool,
}
