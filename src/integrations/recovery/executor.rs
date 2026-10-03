//! Script wire format and interpreter restrictions belong to the Host adapter.
use super::RecoveryError;
use crate::control::recovery::knowledge::RepairArtifact;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ScriptExecutorConfig {
    pub platform: String,
    pub allowed_languages: Vec<String>,
    pub diagnostic_queries: Vec<String>,
}
impl ScriptExecutorConfig {
    pub fn validate(&self) -> Result<(), RecoveryError> {
        if self.platform.is_empty()
            || self.platform.len() > 128
            || self.platform.chars().any(char::is_control)
            || self.allowed_languages.is_empty()
            || self.allowed_languages.len() > 3
            || self
                .allowed_languages
                .iter()
                .any(|v| !matches!(v.as_str(), "powershell" | "sh" | "python"))
            || self.diagnostic_queries.is_empty()
            || self.diagnostic_queries.len() > 32
            || self.diagnostic_queries.iter().any(|v| {
                v.is_empty()
                    || v.len() > 128
                    || !v
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
            })
        {
            return Err(RecoveryError::Invalid(
                "invalid script executor configuration".into(),
            ));
        }
        Ok(())
    }
    pub(super) fn validate_script(&self, script: &ScriptArtifact) -> Result<(), RecoveryError> {
        if script.platform != self.platform
            || !self.allowed_languages.contains(&script.language)
            || script.source.trim().is_empty()
            || script.source.len() > 32 * 1024
            || script.source.contains('\0')
        {
            return Err(RecoveryError::Invalid(
                "script outside executor scope".into(),
            ));
        }
        script.artifact()?.validate().map_err(super::service)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ScriptArtifact {
    pub id: String,
    pub version: u64,
    pub language: String,
    pub platform: String,
    pub source: String,
    pub preconditions: BTreeMap<String, String>,
    pub generated_by_harness: String,
    pub generated_in_session: String,
}
impl ScriptArtifact {
    pub fn artifact(&self) -> Result<RepairArtifact, RecoveryError> {
        let artifact = RepairArtifact {
            id: self.id.clone(),
            version: self.version,
            kind: "execute_script".into(),
            payload: serde_json::json!({"language":self.language,"platform":self.platform,"source":self.source}),
            preconditions: self.preconditions.clone(),
            generated_by_harness: self.generated_by_harness.clone(),
            generated_in_session: self.generated_in_session.clone(),
        };
        artifact.validate().map_err(super::service)?;
        Ok(artifact)
    }
}
