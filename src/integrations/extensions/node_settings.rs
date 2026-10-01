//! Bounded runtime tuning for host-side extension routing.
use super::{ExtensionError, ProtocolSettings};
use serde::{Deserialize, Serialize};
use std::time::Duration;

const MAX_CONCURRENCY: usize = 256;
const MAX_CALL_TIMEOUT_MS: u64 = 1_800_000;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct NodeSettings {
    pub protocol: ProtocolSettings,
    pub ordinary_concurrency: usize,
    pub monitoring_view_concurrency: usize,
    pub approval_concurrency: usize,
    pub plugin_callback_timeout_ms: u64,
}

impl Default for NodeSettings {
    fn default() -> Self {
        Self {
            protocol: ProtocolSettings::default(),
            ordinary_concurrency: 4,
            monitoring_view_concurrency: 1,
            approval_concurrency: 2,
            plugin_callback_timeout_ms: 30_000,
        }
    }
}

impl NodeSettings {
    pub fn validate(&self) -> Result<(), ExtensionError> {
        self.protocol.validate()?;
        if !(1..=MAX_CONCURRENCY).contains(&self.ordinary_concurrency)
            || !(1..=MAX_CONCURRENCY).contains(&self.monitoring_view_concurrency)
            || !(1..=MAX_CONCURRENCY).contains(&self.approval_concurrency)
        {
            return Err(ExtensionError::Configuration(
                "node concurrency limits must each be 1..256".into(),
            ));
        }
        if !(1..=MAX_CALL_TIMEOUT_MS).contains(&self.plugin_callback_timeout_ms) {
            return Err(ExtensionError::Configuration(
                "plugin callback timeout must be 1..1800000ms".into(),
            ));
        }
        Ok(())
    }

    pub(super) fn plugin_callback_timeout(&self) -> Duration {
        Duration::from_millis(self.plugin_callback_timeout_ms)
    }
}
