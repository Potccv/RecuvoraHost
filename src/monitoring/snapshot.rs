//! Public diagnostic views, separate from persistent incident authority.
use super::DiscoverySnapshot;
use serde::Serialize;
use serde_json::Value;

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TargetHealth {
    Unknown,
    Healthy,
    Unhealthy,
}
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
    pub health: TargetHealth,
    pub freshness: Freshness,
    pub coverage: Coverage,
    pub running: bool,
    pub last_received_at_ms: Option<u64>,
    pub last_sample_id: Option<String>,
    pub last_value: Option<Value>,
    pub sample_age_ms: Option<u64>,
    pub consecutive_failures: u32,
    pub consecutive_successes: u32,
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
