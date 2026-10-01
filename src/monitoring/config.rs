//! Trusted monitor definitions, bounded loading and source identity binding.
use super::{MonitorDiscovery, MonitorError, MonitorRule, discovery};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;
use std::io::Read;
use std::path::Path;

const MAX_CONFIG: usize = 256 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MonitorsConfig {
    pub schema_version: u32,
    pub monitors: Vec<MonitorDefinition>,
    #[serde(default)]
    pub discoveries: Vec<MonitorDiscovery>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MonitorDefinition {
    pub id: String,
    pub target_id: String,
    pub source_id: String,
    #[serde(default)]
    pub view_role: Option<String>,
    pub extension_id: String,
    pub contract: String,
    pub version: u32,
    pub method: String,
    pub params: Value,
    pub interval_ms: u64,
    pub timeout_ms: u64,
    pub stale_after_ms: u64,
    pub startup_grace_ms: u64,
    pub rule: MonitorRule,
}

impl MonitorsConfig {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, MonitorError> {
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .and_then(|file| file.take(MAX_CONFIG as u64 + 1).read_to_end(&mut bytes))
            .map_err(|e| MonitorError::Configuration(e.to_string()))?;
        if bytes.len() > MAX_CONFIG {
            return Err(MonitorError::Configuration(
                "configuration exceeds 256 KiB".into(),
            ));
        }
        let value: Self = serde_json::from_slice(&bytes)
            .map_err(|e| MonitorError::Configuration(e.to_string()))?;
        value.validate()?;
        Ok(value)
    }

    pub fn validate(&self) -> Result<(), MonitorError> {
        let invalid = |message: &str| MonitorError::Configuration(message.into());
        if self.schema_version != 1 || self.monitors.len() > 64 {
            return Err(invalid("expected schema_version 1 and at most 64 monitors"));
        }
        let mut ids = BTreeSet::new();
        for item in &self.monitors {
            if !ids.insert(&item.id)
                || [
                    &item.id,
                    &item.target_id,
                    &item.source_id,
                    &item.extension_id,
                    &item.contract,
                    &item.method,
                ]
                .iter()
                .any(|id| !crate::protocol::valid_id(id))
                || item
                    .view_role
                    .as_deref()
                    .is_some_and(|role| !crate::protocol::valid_id(role))
                || item.version == 0
                || item.contract == "recuvora"
                || item.contract.starts_with("recuvora.")
            {
                return Err(invalid(
                    "invalid identity, duplicate monitor or reserved contract",
                ));
            }
            let Some(params) = item.params.as_object() else {
                return Err(invalid("params must be an object"));
            };
            if ["target_id", "source_id", "cursor", "generation"]
                .iter()
                .any(|key| params.contains_key(*key))
                || serde_json::to_vec(params).map_or(true, |v| v.len() > 16 * 1024)
            {
                return Err(invalid(
                    "params exceeds limit or contains reserved observation fields",
                ));
            }
            if !(10..=3_600_000).contains(&item.interval_ms)
                || !(1..=30_000).contains(&item.timeout_ms)
                || !(10..=86_400_000).contains(&item.stale_after_ms)
                || !(10..=86_400_000).contains(&item.startup_grace_ms)
                || !(1..=1000).contains(&item.rule.failure_samples)
                || !(1..=1000).contains(&item.rule.success_samples)
            {
                return Err(invalid(
                    "monitor durations or sample thresholds outside supported limits",
                ));
            }
            item.rule.validate()?;
        }
        if serde_json::to_vec(self).map_or(true, |v| v.len() > MAX_CONFIG) {
            return Err(invalid("configuration exceeds 256 KiB"));
        }
        discovery::validate(self)?;
        Ok(())
    }
}

pub(super) fn binding(config: &MonitorDefinition) -> Result<String, MonitorError> {
    serde_json::to_string(&(
        &config.target_id,
        &config.source_id,
        &config.extension_id,
        &config.contract,
        config.version,
        &config.method,
        &config.params,
    ))
    .map_err(|e| MonitorError::Configuration(e.to_string()))
}
