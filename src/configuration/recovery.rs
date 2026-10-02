//! Host storage and scheduling settings around the Core domain contract.
use crate::integrations::recovery::IncidentTrigger;
use crate::integrations::recovery::{CanonicalTarget, RecoveryConfig, RecoveryError};
use crate::persistence::approval::ApprovalStoreConfig;
use crate::persistence::knowledge::KnowledgeStoreConfig;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryHostConfig {
    pub schema_version: u32,
    pub data_dir: PathBuf,
    /// Stable shared authority for every recovery store protecting this target.
    pub ownership_dir: PathBuf,
    pub recovery: RecoveryConfig,
    pub executor: crate::integrations::recovery::ScriptExecutorConfig,
    pub triggers: Vec<IncidentTrigger>,
    pub interval_ms: u64,
    #[serde(default)]
    pub approval_store: ApprovalStoreConfig,
    #[serde(default)]
    pub knowledge_store: KnowledgeStoreConfig,
}

impl RecoveryHostConfig {
    pub fn validate(&self) -> Result<(), RecoveryError> {
        self.recovery.validate()?;
        self.executor.validate()?;
        if self.recovery.target.allowed_action_kinds != ["execute_script"] {
            return Err(RecoveryError::Invalid(
                "the configured Host adapter only supports execute_script".into(),
            ));
        }
        CanonicalTarget::new(self.recovery.target.target_id.clone())?;
        self.approval_store.validate()?;
        self.knowledge_store
            .validate()
            .map_err(|e| RecoveryError::Invalid(e.to_string()))?;
        if self.schema_version != 2
            || !(10..=3_600_000).contains(&self.interval_ms)
            || self.triggers.is_empty()
            || self.triggers.len() > 64
        {
            return Err(RecoveryError::Invalid(
                "invalid Host recovery schema, interval or triggers".into(),
            ));
        }
        let mut bindings = BTreeSet::new();
        for trigger in &self.triggers {
            trigger.validate()?;
            if !bindings.insert((&trigger.monitor_id, &trigger.rule_id)) {
                return Err(RecoveryError::Invalid("duplicate recovery trigger".into()));
            }
        }
        Ok(())
    }

    pub(crate) fn storage_paths(&self) -> Result<[PathBuf; 2], RecoveryError> {
        let data = crate::boot::host::external_path(&self.data_dir)?;
        let ownership = crate::boot::host::external_path(&self.ownership_dir)?;
        if data.starts_with(&ownership) || ownership.starts_with(&data) {
            return Err(RecoveryError::Invalid(
                "recovery storage and shared target ownership must be separate".into(),
            ));
        }
        Ok([data, ownership])
    }

    /// Reads trusted settings without opening a store or dispatching work.
    pub fn load(path: &Path) -> Result<Self, RecoveryError> {
        use std::io::Read;
        let path = crate::boot::host::external_path(path)?.canonicalize()?;
        let file = std::fs::File::open(&path)?;
        if !file.metadata()?.is_file() {
            return Err(RecoveryError::Invalid(
                "recovery configuration must be a file".into(),
            ));
        }
        let mut bytes = Vec::new();
        file.take(65_537).read_to_end(&mut bytes)?;
        if bytes.len() > 65_536 {
            return Err(RecoveryError::Invalid(
                "recovery configuration exceeds 64 KiB".into(),
            ));
        }
        let mut config: Self = serde_json::from_slice(&bytes)?;
        config.validate()?;
        let base = path
            .parent()
            .ok_or_else(|| RecoveryError::Invalid("configuration has no parent".into()))?;
        for directory in [&mut config.data_dir, &mut config.ownership_dir] {
            if directory.is_relative() {
                *directory = base.join(&*directory);
            }
        }
        [config.data_dir, config.ownership_dir] = config.storage_paths()?;
        if path.starts_with(&config.data_dir) || path.starts_with(&config.ownership_dir) {
            return Err(RecoveryError::Invalid(
                "recovery storage and target ownership must not contain their configuration".into(),
            ));
        }
        Ok(config)
    }
}
