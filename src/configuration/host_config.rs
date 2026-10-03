//! Application entry configuration with Host and Core control-path protection.
use super::host_paths::{external_path, invalid, relative_to, resolve_path, source_roots};
use crate::harnesses::{HarnessError, HarnessRegistryConfig};
use crate::integrations::extensions::{ExtensionError, ExtensionsConfig};
use crate::repair::{RepairConfig, WorkflowError};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Load a trusted Harness configuration outside the Host and Core source trees.
pub fn load_host_harness_config(path: &Path) -> Result<HarnessRegistryConfig, HarnessError> {
    let path = external_path(path).map_err(|error| {
        HarnessError::InvalidConfiguration(format!("active Harness configuration: {error}"))
    })?;
    super::load_harness_config(&path)
}

/// Network node configuration is data; loading never starts a node process.
pub fn load_extensions_config(path: &Path) -> Result<ExtensionsConfig, ExtensionError> {
    let path =
        external_path(path).map_err(|error| ExtensionError::Configuration(error.to_string()))?;
    ExtensionsConfig::load(path)
}

/// Protect the active extension configuration and every explicitly configured
/// TLS trust file from an approved repair target.
pub fn extension_protected_paths(
    path: &Path,
    target: &Path,
) -> Result<Vec<PathBuf>, WorkflowError> {
    let config_path = external_path(path)?;
    let config = ExtensionsConfig::load(&config_path)
        .map_err(|error| WorkflowError::Invalid(error.to_string()))?;
    let target = resolve_path(target)?;
    let mut protected = vec![config_path.canonicalize()?];
    for definition in config.extensions {
        if let Some(path) = definition.endpoint.ca_certificate {
            let path = external_path(&path)?.canonicalize()?;
            protected.push(path);
        }
    }
    if protected
        .iter()
        .any(|path| path.starts_with(&target) || target.starts_with(path))
    {
        return Err(WorkflowError::Invalid(
            "repair target overlaps extension configuration or TLS trust file".into(),
        ));
    }
    protected.sort();
    protected.dedup();
    Ok(protected)
}

/// Add the application source boundary before creating durable state.
/// Host prepares paths and protection; Core retains policy and approval authority.
pub fn load_host_repair_config(
    path: &Path,
    needs_target: bool,
) -> Result<(RepairConfig, Vec<PathBuf>), WorkflowError> {
    let path = external_path(path)?.canonicalize()?;
    let file = File::open(&path)?;
    if !file.metadata()?.is_file() {
        return Err(WorkflowError::Invalid(
            "repair configuration must be a file".into(),
        ));
    }
    let mut bytes = Vec::new();
    file.take(64 * 1024 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 64 * 1024 {
        return Err(WorkflowError::Invalid(
            "repair configuration exceeds 64 KiB".into(),
        ));
    }
    let config: RepairConfig = serde_json::from_slice(&bytes)?;
    if needs_target
        && matches!(
            &config.policy.reviewer,
            recuvora_core::recovery::approval::ReviewerConfig::HumanThenHarness { .. }
        )
    {
        return Err(WorkflowError::Invalid(
            "HumanThenHarness requires the automatic recovery service. Configure recovery_config to enable it. Local text repair supports historical inspection and human decisions.".into(),
        ));
    }
    let base = path
        .parent()
        .ok_or_else(|| invalid("configuration has no parent"))?;
    for control in [&config.harness_config, &config.data_dir]
        .into_iter()
        .chain(config.extensions_config.iter())
    {
        external_path(&relative_to(base, control))?;
    }
    let sources = source_roots()?;
    let target = resolve_path(&relative_to(base, &config.target_root))?;
    for source in &sources {
        if target.starts_with(source) || source.starts_with(&target) {
            return Err(WorkflowError::Invalid(
                "repair target must be isolated from the Host and Core source trees".into(),
            ));
        }
    }
    let (config, mut protected) = super::prepare_repair_config(config, &path, needs_target)?;
    if needs_target && let Some(extension_config) = &config.extensions_config {
        protected.extend(extension_protected_paths(
            extension_config,
            &config.target_root,
        )?);
    }
    // Recheck after Host path preparation and state creation, then retain the
    // source roots in the action backend's trusted protection set.
    external_path(&config.data_dir)?;
    protected.extend(sources);
    protected.sort();
    protected.dedup();
    Ok((config, protected))
}
