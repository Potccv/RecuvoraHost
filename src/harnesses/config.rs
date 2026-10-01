//! Bounded registry configuration and explicit workspace scope preparation.
use super::limits::{
    MAX_ADDRESS_BYTES, MAX_CONFIG_BYTES, MAX_HARNESSES, MAX_IDENTIFIER_BYTES, MAX_WORKSPACE_ROOTS,
};
use super::remote_workspace;
use super::{HarnessError, REMOTE_NODE_ADAPTER};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

pub const CONFIG_SCHEMA_VERSION: u32 = 1;

/// User-editable configuration for a set of named Harness instances.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HarnessRegistryConfig {
    pub schema_version: u32,
    #[serde(default)]
    pub default_harness: Option<String>,
    pub harnesses: Vec<HarnessDefinition>,
}

impl HarnessRegistryConfig {
    /// Checks the registry schema and instance selection without constructing
    /// providers or contacting any configured Harness.
    pub fn validate(&self) -> Result<(), HarnessError> {
        validate_registry_config(self)
    }

    /// Parses a bounded JSON document. Relative workspace roots are resolved by
    /// the registry builder against the process working directory.
    pub fn from_json(bytes: &[u8]) -> Result<Self, HarnessError> {
        if bytes.len() as u64 > MAX_CONFIG_BYTES {
            return Err(HarnessError::InvalidConfiguration(format!(
                "Harness configuration exceeds {MAX_CONFIG_BYTES} bytes"
            )));
        }
        serde_json::from_slice(bytes)
            .map_err(|error| HarnessError::InvalidConfiguration(error.to_string()))
    }

    /// Loads JSON and resolves relative workspace roots against the directory
    /// containing the configuration file.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, HarnessError> {
        let path = fs::canonicalize(path.as_ref()).map_err(|error| {
            HarnessError::InvalidConfiguration(format!(
                "cannot resolve Harness configuration: {error}"
            ))
        })?;
        let metadata = fs::metadata(&path).map_err(|error| {
            HarnessError::InvalidConfiguration(format!(
                "cannot inspect Harness configuration: {error}"
            ))
        })?;
        if !metadata.is_file() {
            return Err(HarnessError::InvalidConfiguration(
                "Harness configuration must be a file".to_owned(),
            ));
        }
        if metadata.len() > MAX_CONFIG_BYTES {
            return Err(HarnessError::InvalidConfiguration(format!(
                "Harness configuration exceeds {MAX_CONFIG_BYTES} bytes"
            )));
        }
        let file = fs::File::open(&path).map_err(|error| {
            HarnessError::InvalidConfiguration(format!(
                "cannot read Harness configuration: {error}"
            ))
        })?;
        let mut bytes = Vec::new();
        file.take(MAX_CONFIG_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| {
                HarnessError::InvalidConfiguration(format!(
                    "cannot read Harness configuration: {error}"
                ))
            })?;
        let mut config = Self::from_json(&bytes)?;
        let base = path.parent().ok_or_else(|| {
            HarnessError::InvalidConfiguration(
                "Harness configuration has no parent directory".to_owned(),
            )
        })?;
        for harness in &mut config.harnesses {
            if harness.adapter == REMOTE_NODE_ADAPTER {
                continue;
            }
            for root in &mut harness.workspace_roots {
                if root.is_relative() {
                    *root = base.join(&*root);
                }
            }
        }
        Ok(config)
    }
}

/// A configured Harness instance. `adapter` names trusted code registered by
/// the host; `address` is interpreted only by that adapter.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HarnessDefinition {
    pub id: String,
    pub adapter: String,
    pub address: String,
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
    pub workspace_roots: Vec<PathBuf>,
}

impl HarnessDefinition {
    pub fn new(
        id: impl Into<String>,
        adapter: impl Into<String>,
        address: impl Into<String>,
        workspace_roots: Vec<PathBuf>,
    ) -> Self {
        Self {
            id: id.into(),
            adapter: adapter.into(),
            address: address.into(),
            enabled: true,
            workspace_roots,
        }
    }
}

fn enabled_by_default() -> bool {
    true
}

pub(super) fn validate_registry_config(config: &HarnessRegistryConfig) -> Result<(), HarnessError> {
    if config.schema_version != CONFIG_SCHEMA_VERSION {
        return Err(HarnessError::InvalidConfiguration(format!(
            "unsupported schema_version {}; expected {CONFIG_SCHEMA_VERSION}",
            config.schema_version
        )));
    }
    if config.harnesses.is_empty() || config.harnesses.len() > MAX_HARNESSES {
        return Err(HarnessError::InvalidConfiguration(format!(
            "configuration must contain between 1 and {MAX_HARNESSES} Harnesses"
        )));
    }
    let mut ids = BTreeSet::new();
    for harness in &config.harnesses {
        validate_identifier("Harness", &harness.id)?;
        validate_identifier("adapter", &harness.adapter)?;
        validate_address(&harness.address)?;
        if harness.workspace_roots.is_empty() || harness.workspace_roots.len() > MAX_WORKSPACE_ROOTS
        {
            return Err(HarnessError::InvalidConfiguration(format!(
                "Harness {} must contain between 1 and {MAX_WORKSPACE_ROOTS} workspace roots",
                harness.id
            )));
        }
        if !ids.insert(harness.id.clone()) {
            return Err(HarnessError::DuplicateHarness(harness.id.clone()));
        }
    }
    if let Some(default) = config.default_harness.as_deref() {
        validate_identifier("default Harness", default)?;
        let harness = config
            .harnesses
            .iter()
            .find(|harness| harness.id == default)
            .ok_or_else(|| {
                HarnessError::InvalidConfiguration(format!(
                    "default Harness {default} is not configured"
                ))
            })?;
        if !harness.enabled {
            return Err(HarnessError::InvalidConfiguration(format!(
                "default Harness {default} is disabled"
            )));
        }
    }
    Ok(())
}

pub(super) fn canonicalize_workspace_roots(
    definition: &mut HarnessDefinition,
) -> Result<(), HarnessError> {
    if definition.adapter == REMOTE_NODE_ADAPTER {
        remote_workspace::validate_remote_definition(definition)?;
        return Ok(());
    }
    for root in &mut definition.workspace_roots {
        let absolute = if root.is_absolute() {
            root.clone()
        } else {
            std::env::current_dir()
                .map_err(|error| HarnessError::InvalidConfiguration(error.to_string()))?
                .join(&*root)
        };
        let canonical = fs::canonicalize(&absolute).map_err(|error| {
            HarnessError::InvalidConfiguration(format!(
                "cannot resolve workspace root for Harness {}: {error}",
                definition.id
            ))
        })?;
        if !canonical.is_dir() {
            return Err(HarnessError::InvalidConfiguration(format!(
                "workspace root for Harness {} is not a directory",
                definition.id
            )));
        }
        *root = canonical;
    }
    definition.workspace_roots.sort();
    definition.workspace_roots.dedup();
    Ok(())
}

pub(super) fn validate_identifier(kind: &str, value: &str) -> Result<(), HarnessError> {
    if value.is_empty()
        || value.len() > MAX_IDENTIFIER_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(HarnessError::InvalidConfiguration(format!(
            "{kind} identifier must use 1..={MAX_IDENTIFIER_BYTES} ASCII letters, digits, '.', '_' or '-'"
        )));
    }
    Ok(())
}

fn validate_address(value: &str) -> Result<(), HarnessError> {
    if value.is_empty() || value.len() > MAX_ADDRESS_BYTES || value.chars().any(char::is_control) {
        return Err(HarnessError::InvalidConfiguration(
            "Harness address is empty, too long, or contains control characters".to_owned(),
        ));
    }
    let Some((scheme, _rest)) = value.split_once("://") else {
        return Err(HarnessError::InvalidConfiguration(
            "Harness address must include a URI scheme".to_owned(),
        ));
    };
    let mut bytes = scheme.bytes();
    if !bytes.next().is_some_and(|byte| byte.is_ascii_alphabetic())
        || !bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.'))
    {
        return Err(HarnessError::InvalidConfiguration(
            "Harness address has an invalid URI scheme".to_owned(),
        ));
    }
    Ok(())
}
