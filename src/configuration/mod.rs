//! Trusted application configuration loading and cross-domain path preparation.
mod harness;
mod host;
mod host_config;
mod host_paths;
mod paths;
mod recovery;
mod repair;

pub use harness::load_harness_config;
pub use host::{HostConfig, MonitoringHostConfig};
pub use host_config::{
    extension_protected_paths, load_extensions_config, load_host_harness_config,
    load_host_repair_config,
};
pub(crate) use host_paths::{external_path, prepare_runtime_directory, validate_simulation_paths};
pub use recovery::RecoveryHostConfig;
pub use repair::{load_repair_config, prepare_repair_config};
