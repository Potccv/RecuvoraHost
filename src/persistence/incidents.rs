use super::journal::Journal;
pub use recuvora_core::recovery::incidents::*;
use serde::{Deserialize, Serialize};
use std::path::Path;
use thiserror::Error;

pub struct IncidentStore {
    journal: Journal,
    ledger: IncidentLedger,
}
impl IncidentStore {
    pub fn open(
        path: impl AsRef<Path>,
        config: IncidentStoreConfig,
    ) -> Result<Self, IncidentError> {
        config.validate()?;
        let journal = Journal::open(
            path,
            "incidents",
            serde_json::to_value(&config)?,
            config.max_journal_bytes,
        )?;
        let limits = IncidentLimits {
            max_incidents: config.max_incidents,
            max_monitors: config.max_monitors,
        };
        let mut entries = Vec::<IncidentEntry>::new();
        for record in journal.records() {
            let entry: IncidentEntry = serde_json::from_value(record.payload.clone())?;
            let expected = serde_json::json!({"config":limits,"prior_digest":entry.prior_digest,"event":entry.event});
            if record.request.input != expected
                || entry.commit_id != record.request.id
                || entry.sequence != record.request.revision
            {
                return Err(IncidentError::Corrupt(
                    "transaction payload does not match commit binding".into(),
                ));
            }
            entries.push(entry);
        }
        let ledger = IncidentLedger::restore(limits, &entries)?;
        Ok(Self { journal, ledger })
    }
    pub fn state(&self) -> &IncidentLedger {
        &self.ledger
    }
    pub fn checkpoint(&self, id: &str) -> Option<Checkpoint> {
        self.ledger.checkpoint(id)
    }
    pub fn get(&self, id: &str) -> Option<IncidentRecord> {
        self.ledger.get(id)
    }
    pub fn list(&self) -> Vec<IncidentRecord> {
        self.ledger.list()
    }
    pub fn map_records<T>(&self, projection: impl FnMut(&IncidentRecord) -> T) -> Vec<T> {
        self.ledger.map_records(projection)
    }
    pub fn commit(&mut self, commit: MonitorCommit) -> Result<(), IncidentError> {
        self.journal.available()?;
        if let Some(pending) = self
            .ledger
            .prepare_monitor(self.journal.next_id(), commit)?
        {
            let entry = pending
                .state()
                .latest_entry()
                .ok_or_else(|| IncidentError::Corrupt("missing proposed entry".into()))?;
            let receipt = self
                .journal
                .commit(pending.request(), serde_json::to_value(entry)?)?;
            self.ledger = pending.confirm(receipt)?.state;
        }
        Ok(())
    }
    pub fn acknowledge(
        &mut self,
        id: &str,
        expected_revision: u64,
        actor: &str,
        note: &str,
        now_ms: u64,
    ) -> Result<IncidentRecord, IncidentError> {
        let pending = self.ledger.prepare_acknowledge(
            self.journal.next_id(),
            id.into(),
            expected_revision,
            actor.into(),
            note.into(),
            now_ms,
        )?;
        let entry = pending
            .state()
            .latest_entry()
            .ok_or_else(|| IncidentError::Corrupt("missing proposed entry".into()))?;
        let receipt = self
            .journal
            .commit(pending.request(), serde_json::to_value(entry)?)?;
        self.ledger = pending.confirm(receipt)?.state;
        self.get(id)
            .ok_or_else(|| IncidentError::NotFound(id.into()))
    }
    pub fn close(&mut self) -> Result<(), IncidentError> {
        Ok(self.journal.close()?)
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct IncidentStoreConfig {
    pub max_incidents: usize,
    pub max_monitors: usize,
    pub max_journal_bytes: u64,
}

impl Default for IncidentStoreConfig {
    fn default() -> Self {
        Self {
            max_incidents: 10_000,
            max_monitors: 256,
            max_journal_bytes: 64 * 1024 * 1024,
        }
    }
}

impl IncidentStoreConfig {
    pub fn validate(&self) -> Result<(), IncidentError> {
        if !(1..=100_000).contains(&self.max_incidents)
            || !(1..=4096).contains(&self.max_monitors)
            || !(1..=1024 * 1024 * 1024).contains(&self.max_journal_bytes)
        {
            return Err(IncidentError::Invalid("store limits out of range".into()));
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum IncidentError {
    #[error("invalid incident input: {0}")]
    Invalid(String),
    #[error("incident state conflict: {0}")]
    Conflict(String),
    #[error("incident not found: {0}")]
    NotFound(String),
    #[error("incident capacity exhausted: {0}")]
    Capacity(String),
    #[error("corrupt incident journal: {0}")]
    Corrupt(String),
    #[error("incident storage I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("incident storage unavailable: {0}")]
    Unavailable(String),
}

impl From<recuvora_core::recovery::incidents::IncidentError> for IncidentError {
    fn from(error: recuvora_core::recovery::incidents::IncidentError) -> Self {
        use recuvora_core::recovery::incidents::IncidentError as E;
        match error {
            E::Invalid(value) => Self::Invalid(value),
            E::Conflict(value) => Self::Conflict(value),
            E::NotFound(value) => Self::NotFound(value),
            E::Capacity(value) => Self::Capacity(value),
            E::Corrupt(value) => Self::Corrupt(value),
            other => Self::Corrupt(other.to_string()),
        }
    }
}
impl From<super::journal::JournalError> for IncidentError {
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
impl From<recuvora_core::operation::CommitError> for IncidentError {
    fn from(error: recuvora_core::operation::CommitError) -> Self {
        Self::Corrupt(error.to_string())
    }
}
impl From<serde_json::Error> for IncidentError {
    fn from(error: serde_json::Error) -> Self {
        Self::Corrupt(error.to_string())
    }
}
