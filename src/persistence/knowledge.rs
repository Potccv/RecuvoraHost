use super::journal::Journal;
use recuvora_core::operation::CommitReceipt;
pub use recuvora_core::recovery::knowledge::*;
use serde::{Deserialize, Serialize};
use std::path::Path;
use thiserror::Error;

/// Serialized commands live exclusively in the protected Host journal. Rebuild
/// attestations here, never by deserializing external/model input into authority.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
enum StoredCommand {
    RecordExperience(Box<RepairExperience>),
    ExpandCapacity {
        expected: KnowledgeConfig,
        target: KnowledgeConfig,
    },
}
impl StoredCommand {
    fn trusted(self) -> Result<KnowledgeCommand, KnowledgeError> {
        Ok(match self {
            Self::RecordExperience(item) => {
                KnowledgeCommand::RecordExperience(TrustedRepairExperience::attest(*item)?)
            }
            Self::ExpandCapacity { expected, target } => {
                KnowledgeCommand::ExpandCapacity { expected, target }
            }
        })
    }
}

pub(crate) fn decode_command(value: serde_json::Value) -> Result<KnowledgeCommand, KnowledgeError> {
    serde_json::from_value::<StoredCommand>(value)?.trusted()
}
pub(crate) fn encode_command(
    command: &KnowledgeCommand,
) -> Result<serde_json::Value, KnowledgeError> {
    Ok(serde_json::to_value(command)?)
}

pub struct KnowledgeStore {
    journal: Journal,
    state: KnowledgeState,
}
impl KnowledgeStore {
    pub fn open(
        path: impl AsRef<Path>,
        config: KnowledgeStoreConfig,
    ) -> Result<Self, KnowledgeError> {
        config.validate()?;
        let journal = Journal::open(
            path,
            "knowledge",
            serde_json::to_value(&config)?,
            config.max_journal_bytes,
        )?;
        let limits = KnowledgeConfig {
            max_records: config.max_records,
        };
        let entries = journal
            .records()
            .iter()
            .map(|record| {
                let command = decode_command(record.payload.clone())?;
                Ok(KnowledgeReplayEntry {
                    request: record.request.clone(),
                    command,
                    receipt: CommitReceipt::confirmed(&record.request),
                })
            })
            .collect::<Result<Vec<_>, KnowledgeError>>()?;
        let state = KnowledgeState::replay(limits, entries)?;
        Ok(Self { journal, state })
    }
    pub fn state(&self) -> &KnowledgeState {
        &self.state
    }
    pub fn available(&self) -> Result<(), KnowledgeError> {
        Ok(self.journal.available()?)
    }
    pub fn snapshot(&self) -> KnowledgeSnapshot {
        self.state.snapshot()
    }
    pub fn get(&self, id: &str) -> Option<RepairExperience> {
        self.state.get(id)
    }
    pub fn validate_artifact(&self, artifact: &RepairArtifact) -> Result<(), KnowledgeError> {
        Ok(self.state.validate_artifact(artifact)?)
    }
    pub fn is_quarantined(&self, id: &str, version: u64) -> bool {
        self.state.is_quarantined(id, version)
    }
    pub fn search_experiences(
        &self,
        query: &KnowledgeQuery,
    ) -> Result<Vec<RepairExperience>, KnowledgeError> {
        Ok(self.state.search_experiences(query)?)
    }
    pub fn projection(&self) -> KnowledgeProjection {
        self.state.projection()
    }
    /// Persist an explicit monotonic domain capacity expansion. Reopen still
    /// requires the original StoreConfig; replay derives the expanded limits.
    pub fn expand_capacity(
        &mut self,
        expected: KnowledgeConfig,
        target: KnowledgeConfig,
    ) -> Result<(), KnowledgeError> {
        self.commit(KnowledgeCommand::ExpandCapacity { expected, target })
    }
    fn commit(&mut self, command: KnowledgeCommand) -> Result<(), KnowledgeError> {
        let payload = encode_command(&command)?;
        let pending = self.state.propose(self.journal.next_id(), command)?;
        let receipt = self.journal.commit(pending.request(), payload)?;
        self.state = pending.confirm(receipt)?.state;
        Ok(())
    }
    pub fn record_experience(&mut self, record: RepairExperience) -> Result<(), KnowledgeError> {
        self.commit(KnowledgeCommand::RecordExperience(
            TrustedRepairExperience::attest(record)?,
        ))
    }
    pub fn close(&mut self) -> Result<(), KnowledgeError> {
        Ok(self.journal.close()?)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct KnowledgeStoreConfig {
    pub max_records: usize,
    pub max_journal_bytes: u64,
}

impl Default for KnowledgeStoreConfig {
    fn default() -> Self {
        Self {
            max_records: 1024,
            max_journal_bytes: 64 * 1024 * 1024,
        }
    }
}

impl KnowledgeStoreConfig {
    pub fn validate(&self) -> Result<(), KnowledgeError> {
        if !(1..=100_000).contains(&self.max_records)
            || !(1..=1024 * 1024 * 1024).contains(&self.max_journal_bytes)
        {
            return Err(KnowledgeError::Invalid("store limits out of range".into()));
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum KnowledgeError {
    #[error("invalid knowledge input: {0}")]
    Invalid(String),
    #[error("knowledge state conflict: {0}")]
    Conflict(String),
    #[error("knowledge capacity exhausted: {0}")]
    Capacity(String),
    #[error("corrupt knowledge journal: {0}")]
    Corrupt(String),
    #[error("knowledge store unavailable: {0}")]
    Unavailable(String),
    #[error("knowledge I/O: {0}")]
    Io(#[from] std::io::Error),
}

impl From<recuvora_core::recovery::knowledge::KnowledgeError> for KnowledgeError {
    fn from(error: recuvora_core::recovery::knowledge::KnowledgeError) -> Self {
        use recuvora_core::recovery::knowledge::KnowledgeError as E;
        match error {
            E::Invalid(value) => Self::Invalid(value),
            E::Conflict(value) => Self::Conflict(value),
            E::Capacity(value) => Self::Capacity(value),
            other => Self::Corrupt(other.to_string()),
        }
    }
}
impl From<super::journal::JournalError> for KnowledgeError {
    fn from(error: super::journal::JournalError) -> Self {
        use super::journal::JournalError as E;
        match error {
            E::Io(e) => Self::Io(e),
            E::Corrupt(s) => Self::Corrupt(s),
            E::Capacity => Self::Capacity("journal bytes".into()),
            E::Conflict(s) => Self::Conflict(s),
            E::Invalid(s) => Self::Invalid(s),
            other => Self::Unavailable(other.to_string()),
        }
    }
}
impl From<recuvora_core::operation::CommitError> for KnowledgeError {
    fn from(error: recuvora_core::operation::CommitError) -> Self {
        Self::Corrupt(error.to_string())
    }
}
impl From<serde_json::Error> for KnowledgeError {
    fn from(error: serde_json::Error) -> Self {
        Self::Corrupt(error.to_string())
    }
}
