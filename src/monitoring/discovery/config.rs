//! Discovery configuration, bounded wire inventory and diagnostic contracts.
use super::super::{MonitorDefinition, MonitorError, MonitorsConfig};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::future::Future;
use std::pin::Pin;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MonitorDiscovery {
    pub id: String,
    pub extension_id: String,
    pub contract: String,
    pub version: u32,
    pub method: String,
    pub params: Value,
    pub interval_ms: u64,
    pub timeout_ms: u64,
    pub max_targets: usize,
    pub parameter: String,
    pub template: MonitorDefinition,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiscoveryTarget {
    pub key: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiscoveryBatch {
    pub schema_version: u32,
    pub complete: bool,
    pub targets: Vec<DiscoveryTarget>,
    pub error: Option<String>,
}

pub type DiscoveryFuture<'a> =
    Pin<Box<dyn Future<Output = Result<DiscoveryBatch, MonitorError>> + Send + 'a>>;

#[derive(Clone, Debug, Serialize)]
pub struct DiscoverySnapshot {
    pub id: String,
    pub extension_id: String,
    pub contract: String,
    pub version: u32,
    pub method: String,
    pub running: bool,
    pub last_received_at_ms: Option<u64>,
    pub complete: bool,
    pub known_targets: usize,
    pub present_targets: usize,
    pub last_error: Option<String>,
}

pub(super) fn key_valid(key: &str) -> bool {
    (1..=64).contains(&key.len())
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

pub(super) fn invalid(message: &str) -> MonitorError {
    MonitorError::Configuration(message.into())
}

pub(in crate::monitoring) fn validate(config: &MonitorsConfig) -> Result<(), MonitorError> {
    if config.discoveries.len() > 8 {
        return Err(invalid("at most 8 discovery sources are supported"));
    }
    let mut reserved = config.monitors.len();
    let mut ids = BTreeSet::new();
    let mut prefixes = BTreeSet::new();
    if config
        .monitors
        .iter()
        .any(|monitor| monitor.id.starts_with("discovery."))
    {
        return Err(invalid(
            "discovery. monitor IDs are reserved for inventory checkpoints",
        ));
    }
    for item in &config.discoveries {
        if [
            &item.id,
            &item.extension_id,
            &item.contract,
            &item.method,
            &item.parameter,
        ]
        .iter()
        .any(|value| !crate::protocol::valid_id(value))
            || !crate::protocol::valid_id(&format!("discovery.{}", item.id))
            || !ids.insert(&item.id)
            || !prefixes.insert(&item.template.id)
            || item.template.id.starts_with("discovery.")
            || item.version == 0
            || item.contract == "recuvora"
            || item.contract.starts_with("recuvora.")
            || !(1..=64).contains(&item.max_targets)
            || !(10..=3_600_000).contains(&item.interval_ms)
            || !(1..=30_000).contains(&item.timeout_ms)
            || ["target_id", "source_id", "cursor", "generation"].contains(&item.parameter.as_str())
            || !item.params.is_object()
            || serde_json::to_vec(&item.params).map_or(true, |bytes| bytes.len() > 16 * 1024)
        {
            return Err(invalid(
                "invalid discovery identity, duration, limit or parameters",
            ));
        }
        reserved += item.max_targets;
        if reserved > 64 {
            return Err(invalid(
                "static monitors plus discovery reservations exceed 64",
            ));
        }
        MonitorsConfig {
            schema_version: 2,
            monitors: vec![item.template.clone()],
            discoveries: vec![],
        }
        .validate()?;
        if item.template.params.get(&item.parameter).is_some()
            || [
                &item.template.id,
                &item.template.target_id,
                &item.template.source_id,
            ]
            .iter()
            .any(|prefix| !crate::protocol::valid_id(&format!("{prefix}.{}", "k".repeat(64))))
            || config.monitors.iter().any(|monitor| {
                monitor
                    .id
                    .strip_prefix(&format!("{}.", item.template.id))
                    .is_some_and(key_valid)
            })
        {
            return Err(invalid(
                "discovery template prefix conflicts or parameter already exists",
            ));
        }
        // Account for the largest supported key before any remote inventory is accepted.
        let mut expanded = item.template.clone();
        expanded.params[&item.parameter] = json!("k".repeat(64));
        MonitorsConfig {
            schema_version: 2,
            monitors: vec![expanded],
            discoveries: vec![],
        }
        .validate()?;
    }
    Ok(())
}
