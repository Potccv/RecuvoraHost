//! Trusted extension definitions, namespace ownership and call allowlists.
use super::{ExtensionError, ExtensionKind, NetworkEndpoint, UiLinksConfig};
use crate::protocol;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::io::Read;
use std::path::Path;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AllowedMethod {
    pub contract: String,
    pub version: u32,
    pub method: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionDefinition {
    pub id: String,
    pub kind: ExtensionKind,
    #[serde(default = "enabled")]
    pub enabled: bool,
    pub endpoint: NetworkEndpoint,
    #[serde(default)]
    pub namespaces: Vec<String>,
    #[serde(default)]
    pub allow_calls: Vec<AllowedMethod>,
    #[serde(default)]
    pub allow_nodes: Vec<String>,
    #[serde(default)]
    pub ui_links: UiLinksConfig,
}
fn enabled() -> bool {
    true
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionsConfig {
    pub schema_version: u32,
    pub extensions: Vec<ExtensionDefinition>,
}
impl ExtensionsConfig {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ExtensionError> {
        let path = std::fs::canonicalize(path)
            .map_err(|e| ExtensionError::Configuration(e.to_string()))?;
        let base = path
            .parent()
            .ok_or_else(|| ExtensionError::Configuration("configuration has no parent".into()))?;
        let mut data = Vec::new();
        std::fs::File::open(&path)
            .map_err(|e| ExtensionError::Configuration(e.to_string()))?
            .take(256 * 1024 + 1)
            .read_to_end(&mut data)
            .map_err(|e| ExtensionError::Configuration(e.to_string()))?;
        if data.len() > 256 * 1024 {
            return Err(ExtensionError::Configuration(
                "configuration exceeds 256 KiB".into(),
            ));
        }
        let mut config: Self = serde_json::from_slice(&data)
            .map_err(|e| ExtensionError::Configuration(e.to_string()))?;
        for definition in &mut config.extensions {
            if let Some(path) = &mut definition.endpoint.ca_certificate
                && path.is_relative()
            {
                *path = base.join(&*path);
            }
        }
        config.validate()?;
        Ok(config)
    }
    pub fn validate(&self) -> Result<(), ExtensionError> {
        if self.schema_version != 1 || self.extensions.len() > 64 {
            return Err(ExtensionError::Configuration(
                "expected schema_version 1 and at most 64 extensions".into(),
            ));
        }
        let mut ids = BTreeSet::new();
        let mut namespaces = Vec::<String>::new();
        for definition in &self.extensions {
            if !protocol::valid_id(&definition.id)
                || !ids.insert(&definition.id)
                || definition.namespaces.len() > 16
                || definition.allow_calls.len() > 128
                || definition.allow_nodes.len() > 64
            {
                return Err(ExtensionError::Configuration(
                    "invalid or duplicate extension identity/limits".into(),
                ));
            }
            definition.endpoint.validate()?;
            definition.ui_links.validate(definition.kind)?;
            for namespace in &definition.namespaces {
                if definition.kind != ExtensionKind::Plugin
                    || !protocol::valid_id(namespace)
                    || namespace == "recuvora"
                    || namespace.starts_with("recuvora.")
                    || namespaces.iter().any(|n| {
                        n == namespace
                            || n.starts_with(&format!("{namespace}."))
                            || namespace.starts_with(&format!("{n}."))
                    })
                {
                    return Err(ExtensionError::Configuration(
                        "invalid, reserved or conflicting namespace".into(),
                    ));
                }
                namespaces.push(namespace.clone());
            }
            for allowed in &definition.allow_calls {
                if !protocol::valid_id(&allowed.contract)
                    || !protocol::valid_id(&allowed.method)
                    || allowed.version == 0
                {
                    return Err(ExtensionError::Configuration(
                        "invalid call allowlist".into(),
                    ));
                }
            }
            if definition
                .allow_nodes
                .iter()
                .any(|id| !protocol::valid_id(id))
            {
                return Err(ExtensionError::Configuration("invalid allowed node".into()));
            }
        }
        Ok(())
    }
}
