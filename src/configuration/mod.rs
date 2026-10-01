//! Trusted application configuration loading and cross-domain path preparation.
mod paths;
mod recovery;

pub use recovery::RecoveryHostConfig;

use self::paths::{absolute_normal, invalid, path_within, reject_links, resolve_existing_ancestor};
use crate::harnesses::{HarnessError, HarnessRegistryConfig};
use crate::repair::{RepairConfig, WorkflowError};
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

pub fn load_repair_config(
    path: &Path,
    needs_target: bool,
) -> Result<(RepairConfig, Vec<PathBuf>), WorkflowError> {
    reject_links(path)?;
    let path = path.canonicalize()?;
    let file = File::open(&path)?;
    if !file.metadata()?.is_file() {
        return Err(invalid("repair configuration must be a file"));
    }
    let mut bytes = Vec::new();
    file.take(64 * 1024 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 64 * 1024 {
        return Err(invalid("repair configuration exceeds 64 KiB"));
    }
    let config: RepairConfig = serde_json::from_slice(&bytes)?;
    prepare_repair_config(config, &path, needs_target)
}

/// Applies the same trusted path preparation and protection derivation as the
/// standalone repair loader to an already-deserialized configuration. This is
/// used by Host assembly at the explicit workflow-open boundary.
pub fn prepare_repair_config(
    mut config: RepairConfig,
    active_config: &Path,
    needs_target: bool,
) -> Result<(RepairConfig, Vec<PathBuf>), WorkflowError> {
    reject_links(active_config)?;
    let path = active_config.canonicalize()?;
    if !path.is_file() {
        return Err(invalid("active repair configuration must be a file"));
    }
    config.validate()?;
    let base = path
        .parent()
        .ok_or_else(|| invalid("configuration has no parent"))?;
    if let Some(extension) = &mut config.extensions_config {
        if extension.is_relative() {
            *extension = base.join(&*extension);
        }
        reject_links(extension)?;
        *extension = extension.canonicalize()?;
    }
    for value in [
        &mut config.harness_config,
        &mut config.target_root,
        &mut config.reviewer_directory,
        &mut config.data_dir,
    ] {
        if value.is_relative() {
            *value = base.join(&*value);
        }
    }
    // Validate lexical ancestors before resolving or creating trusted state;
    // never create runtime state inside a target or source tree through links.
    config.target_root = absolute_normal(&config.target_root)?;
    config.reviewer_directory = absolute_normal(&config.reviewer_directory)?;
    config.harness_config = absolute_normal(&config.harness_config)?;
    if needs_target {
        reject_links(&config.target_root)?;
        config.target_root = config.target_root.canonicalize()?;
        reject_links(&config.harness_config)?;
        config.harness_config = config.harness_config.canonicalize()?;
        if config.reviewer_workspace.is_none() {
            reject_links(&config.reviewer_directory)?;
            config.reviewer_directory = config.reviewer_directory.canonicalize()?;
        }
    }
    let target = resolve_existing_ancestor(&config.target_root)?;
    let reviewer = if config.reviewer_workspace.is_none() {
        Some(resolve_existing_ancestor(&config.reviewer_directory)?)
    } else {
        None
    };
    let harness = resolve_existing_ancestor(&config.harness_config)?;
    let extension = config
        .extensions_config
        .as_deref()
        .map(resolve_existing_ancestor)
        .transpose()?;
    let source = match Path::new(env!("CARGO_MANIFEST_DIR")).canonicalize() {
        Ok(p) => Some(p),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
    };
    let installation = std::env::current_exe()?
        .canonicalize()?
        .parent()
        .ok_or_else(|| invalid("executable has no parent"))?
        .to_path_buf();
    if let Some(source) = &source {
        let extension_in_source = match &extension {
            Some(value) => path_within(value, source)?,
            None => false,
        };
        if path_within(&path, source)? || path_within(&harness, source)? || extension_in_source {
            return Err(invalid(
                "active configurations must be outside the project source tree",
            ));
        }
    }
    let extension_in_target = match &extension {
        Some(value) => path_within(value, &target)?,
        None => false,
    };
    let reviewer_in_target = match &reviewer {
        Some(value) => path_within(value, &target)?,
        None => false,
    };
    let target_in_reviewer = match &reviewer {
        Some(value) => path_within(&target, value)?,
        None => false,
    };
    let target_overlaps_source = match &source {
        Some(value) => path_within(&target, value)? || path_within(value, &target)?,
        None => false,
    };
    if path_within(&path, &target)?
        || path_within(&harness, &target)?
        || reviewer_in_target
        || target_in_reviewer
        || extension_in_target
        || target_overlaps_source
        || path_within(&target, &installation)?
        || path_within(&installation, &target)?
    {
        return Err(invalid(
            "repair target must be isolated from configuration, reviewer, source and installation paths",
        ));
    }
    let state = absolute_normal(&config.data_dir)?;
    reject_links(&state)?;
    let planned_state = resolve_existing_ancestor(&state)?;
    let planned_state_in_source = match &source {
        Some(value) => path_within(&planned_state, value)?,
        None => false,
    };
    if path_within(&planned_state, &target)?
        || planned_state_in_source
        || path_within(&path, &planned_state)?
    {
        return Err(invalid(
            "state directory must be outside target, source and active configuration",
        ));
    }
    std::fs::create_dir_all(&state)?;
    let state = state.canonicalize()?;
    let state_in_source = match &source {
        Some(value) => path_within(&state, value)?,
        None => false,
    };
    if path_within(&state, &target)? || state_in_source || path_within(&path, &state)? {
        return Err(invalid(
            "state directory must be outside target, source and active configuration",
        ));
    }
    config.data_dir = state;
    let mut protected = vec![
        path,
        config.harness_config.clone(),
        config.data_dir.clone(),
        installation,
    ];
    if config.reviewer_workspace.is_none() {
        protected.push(config.reviewer_directory.clone());
    }
    if let Some(source) = source {
        protected.push(source);
    }
    if let Some(extension) = &config.extensions_config {
        protected.push(extension.clone());
    }
    Ok((config, protected))
}

pub fn load_harness_config(path: &Path) -> Result<HarnessRegistryConfig, HarnessError> {
    let path = std::fs::canonicalize(path).map_err(|error| {
        HarnessError::InvalidConfiguration(format!("cannot resolve active configuration: {error}"))
    })?;
    let project = match std::fs::canonicalize(env!("CARGO_MANIFEST_DIR")) {
        Ok(project) => Some(project),
        // A copied executable does not require its build machine's source tree.
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(HarnessError::InvalidConfiguration(format!(
                "cannot inspect source directory: {error}"
            )));
        }
    };
    if project.is_some_and(|project| path.starts_with(project)) {
        return Err(HarnessError::InvalidConfiguration(
            "active configuration must be outside the project source tree; copy the example to an external directory and adjust workspace_roots".to_owned(),
        ));
    }
    let config = HarnessRegistryConfig::load(path)?;
    config.validate()?;
    Ok(config)
}
