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
    ExpandCapacity {
        expected: KnowledgeConfig,
        target: KnowledgeConfig,
    },
    UpsertCandidate(KnowledgeCandidate),
    RecordOutcome {
        record_id: String,
        case: RepairCase,
        verification: Option<BusinessVerificationRecord>,
    },
    Disable {
        record_id: String,
        expected_revision: u64,
        actor: String,
        reason: String,
    },
}
impl StoredCommand {
    fn trusted(self) -> Result<KnowledgeCommand, KnowledgeError> {
        Ok(match self {
            Self::ExpandCapacity { expected, target } => {
                KnowledgeCommand::ExpandCapacity { expected, target }
            }
            Self::UpsertCandidate(candidate) => KnowledgeCommand::UpsertCandidate(candidate),
            Self::RecordOutcome {
                record_id,
                case,
                verification,
            } => KnowledgeCommand::RecordOutcome {
                record_id,
                case,
                verification: verification
                    .map(|v| {
                        TrustedBusinessVerification::attest(
                            v.operation_id,
                            v.target_id,
                            v.script_id,
                            v.script_version,
                            v.verifier_id,
                            v.evidence_refs,
                            v.verified_at_ms,
                        )
                    })
                    .transpose()?,
            },
            Self::Disable {
                record_id,
                expected_revision,
                actor,
                reason,
            } => KnowledgeCommand::Disable {
                record_id,
                expected_revision,
                actor,
                reason,
            },
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
            max_cases_per_record: config.max_cases_per_record,
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
    pub(crate) fn prepare_close(&self) -> Result<(), KnowledgeError> {
        Ok(self.journal.prepare_close()?)
    }
    pub(crate) fn finish_close(&mut self) {
        self.journal.finish_close();
    }
    pub fn snapshot(&self) -> KnowledgeSnapshot {
        self.state.snapshot()
    }
    pub fn get(&self, id: &str) -> Option<KnowledgeRecord> {
        self.state.get(id)
    }
    pub fn validate_script(&self, script: &ScriptArtifact) -> Result<(), KnowledgeError> {
        Ok(self.state.validate_script(script)?)
    }
    pub fn is_quarantined(&self, id: &str, version: u64) -> bool {
        self.state.is_quarantined(id, version)
    }
    pub fn search(&self, query: &KnowledgeQuery) -> Result<Vec<KnowledgeRecord>, KnowledgeError> {
        Ok(self.state.search(query)?)
    }
    pub fn search_reusable(
        &self,
        query: &KnowledgeQuery,
    ) -> Result<Vec<KnowledgeRecord>, KnowledgeError> {
        Ok(self.state.search_reusable(query)?)
    }
    pub fn inspect(
        &self,
        query: &KnowledgeInspectionQuery,
    ) -> Result<Vec<KnowledgeRecordProjection>, KnowledgeError> {
        Ok(self.state.inspect(query)?)
    }
    pub fn projection(&self) -> KnowledgeProjection {
        self.state.projection()
    }
    /// Persist an explicit monotonic domain capacity migration. Reopen still
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
    pub fn upsert_candidate(
        &mut self,
        candidate: KnowledgeCandidate,
    ) -> Result<KnowledgeRecord, KnowledgeError> {
        let id = candidate.id.clone();
        self.commit(KnowledgeCommand::UpsertCandidate(candidate))?;
        self.get(&id).ok_or(KnowledgeError::NotFound(id))
    }
    pub fn record_outcome(
        &mut self,
        record_id: &str,
        case: RepairCase,
        verification: Option<TrustedBusinessVerification>,
    ) -> Result<KnowledgeRecord, KnowledgeError> {
        self.commit(KnowledgeCommand::RecordOutcome {
            record_id: record_id.into(),
            case,
            verification,
        })?;
        self.get(record_id)
            .ok_or_else(|| KnowledgeError::NotFound(record_id.into()))
    }
    pub fn disable(
        &mut self,
        record_id: &str,
        expected_revision: u64,
        actor: &str,
        reason: &str,
    ) -> Result<KnowledgeRecord, KnowledgeError> {
        self.commit(KnowledgeCommand::Disable {
            record_id: record_id.into(),
            expected_revision,
            actor: actor.into(),
            reason: reason.into(),
        })?;
        self.get(record_id)
            .ok_or_else(|| KnowledgeError::NotFound(record_id.into()))
    }
    pub fn close(&mut self) -> Result<(), KnowledgeError> {
        Ok(self.journal.close()?)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct KnowledgeStoreConfig {
    pub max_records: usize,
    pub max_cases_per_record: usize,
    pub max_journal_bytes: u64,
}

impl Default for KnowledgeStoreConfig {
    fn default() -> Self {
        Self {
            max_records: 1024,
            max_cases_per_record: 128,
            max_journal_bytes: 64 * 1024 * 1024,
        }
    }
}

impl KnowledgeStoreConfig {
    pub fn validate(&self) -> Result<(), KnowledgeError> {
        if !(1..=100_000).contains(&self.max_records)
            || !(1..=1024).contains(&self.max_cases_per_record)
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
    #[error("knowledge record not found: {0}")]
    NotFound(String),
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
            E::NotFound(value) => Self::NotFound(value),
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
