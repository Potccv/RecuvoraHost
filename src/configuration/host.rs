//! Shared application service configuration, without service startup.
use crate::harnesses::HarnessRegistryConfig;
use crate::integrations::extensions::ExtensionsConfig;
use crate::monitoring::MonitorsConfig;
use std::path::PathBuf;

#[derive(Default)]
pub struct HostConfig {
    pub harnesses: Option<HarnessRegistryConfig>,
    pub extensions: Option<ExtensionsConfig>,
    pub monitoring: Option<MonitoringHostConfig>,
}

pub struct MonitoringHostConfig {
    pub config: MonitorsConfig,
    pub data_dir: PathBuf,
}
