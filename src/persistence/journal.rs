//! Locked, bounded, append-only transactions; confirmation follows durable sync.
use fs2::FileExt;
use recuvora_core::operation::{CommitReceipt, CommitRequest};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeSet,
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
};

#[derive(Debug, thiserror::Error)]
pub enum JournalError {
    #[error("invalid journal: {0}")]
    Invalid(String),
    #[error("journal conflict: {0}")]
    Conflict(String),
    #[error("corrupt journal: {0}")]
    Corrupt(String),
    #[error("journal capacity exhausted")]
    Capacity,
    #[error("journal writer is locked: {0}")]
    Locked(std::io::Error),
    #[error("journal is closed or its last commit is uncertain")]
    Unavailable,
    #[error("journal I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("journal JSON: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    format: u32,
    domain: String,
    config: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JournalRecord {
    pub request: CommitRequest,
    pub payload: Value,
}

pub struct Journal {
    file: Option<File>,
    path: PathBuf,
    directories: Vec<File>,
    domain: String,
    records: Vec<JournalRecord>,
    bytes: u64,
    max_bytes: u64,
    poisoned: bool,
    #[cfg(test)]
    fault: Option<(usize, bool)>,
}

impl Journal {
    pub fn open(
        path: impl AsRef<Path>,
        domain: &str,
        config: Value,
        max_bytes: u64,
    ) -> Result<Self, JournalError> {
        if max_bytes == 0 || max_bytes > 1024 * 1024 * 1024 {
            return Err(JournalError::Invalid("byte limit out of range".into()));
        }
        let path = path.as_ref().to_path_buf();
        let (mut file, directories) = super::paths::open(&path)?;
        file.try_lock_exclusive().map_err(JournalError::Locked)?;
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(max_bytes + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > max_bytes {
            return Err(JournalError::Capacity);
        }
        let mut records = Vec::new();
        if bytes.is_empty() {
            bytes = serde_json::to_vec(&Header {
                format: 2,
                domain: domain.into(),
                config,
            })?;
            bytes.push(b'\n');
            if bytes.len() as u64 > max_bytes {
                return Err(JournalError::Capacity);
            }
            file.write_all(&bytes)?;
            file.sync_all()?;
            #[cfg(unix)]
            File::open(
                path.parent()
                    .ok_or_else(|| JournalError::Invalid("missing parent".into()))?,
            )?
            .sync_all()?;
        } else {
            if bytes.last() != Some(&b'\n') {
                return Err(JournalError::Corrupt(
                    "incomplete final transaction; original data retained".into(),
                ));
            }
            let mut lines = bytes[..bytes.len() - 1].split(|byte| *byte == b'\n');
            let header: Header =
                serde_json::from_slice(lines.next().unwrap_or_default()).map_err(|e| {
                    JournalError::Corrupt(format!(
                        "missing v2 header (legacy journals require explicit import): {e}"
                    ))
                })?;
            if header.format != 2 || header.domain != domain || header.config != config {
                return Err(JournalError::Conflict(
                    "stored domain/configuration differs from trusted configuration".into(),
                ));
            }
            let mut ids = BTreeSet::new();
            for line in lines {
                let record: JournalRecord = serde_json::from_slice(line)
                    .map_err(|e| JournalError::Corrupt(e.to_string()))?;
                let request = &record.request;
                let canonical = CommitRequest::new(
                    request.id.clone(),
                    records.len() as u64,
                    domain.into(),
                    request.input.clone(),
                )
                .map_err(|e| JournalError::Corrupt(e.to_string()))?;
                if request != &canonical || !ids.insert(request.id.clone()) {
                    return Err(JournalError::Corrupt(
                        "revision, domain or commit identity conflict".into(),
                    ));
                }
                records.push(record);
            }
        }
        Ok(Self {
            file: Some(file),
            path,
            directories,
            domain: domain.into(),
            records,
            bytes: bytes.len() as u64,
            max_bytes,
            poisoned: false,
            #[cfg(test)]
            fault: None,
        })
    }

    pub fn records(&self) -> &[JournalRecord] {
        &self.records
    }
    pub fn revision(&self) -> u64 {
        self.records.len() as u64
    }
    pub fn next_id(&self) -> String {
        format!("{}-{:016x}", self.domain, self.revision() + 1)
    }
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
    pub fn available(&self) -> Result<(), JournalError> {
        if self.poisoned {
            return Err(JournalError::Unavailable);
        }
        let file = self.file.as_ref().ok_or(JournalError::Unavailable)?;
        super::paths::validate_current(&self.path, file)?;
        if file.metadata()?.len() != self.bytes {
            return Err(JournalError::Conflict(
                "journal changed outside writer".into(),
            ));
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn fail_after_commits(&mut self, successful_commits: usize, after_sync: bool) {
        self.fault = Some((successful_commits, after_sync));
    }

    /// Already committed requests are resolved through `records` and restoration,
    /// never by issuing a second receipt that could release effects twice.
    pub fn commit(
        &mut self,
        request: &CommitRequest,
        payload: Value,
    ) -> Result<CommitReceipt, JournalError> {
        self.available()?;
        if let Some(previous) = self.records.iter().find(|r| r.request.id == request.id) {
            return if previous.request == *request && previous.payload == payload {
                Err(JournalError::Conflict(
                    "transaction already committed; restore without redispatch".into(),
                ))
            } else {
                Err(JournalError::Conflict(
                    "commit ID reused with different content".into(),
                ))
            };
        }
        let canonical = CommitRequest::new(
            request.id.clone(),
            self.revision(),
            self.domain.clone(),
            request.input.clone(),
        )
        .map_err(|e| JournalError::Invalid(e.to_string()))?;
        if request != &canonical {
            return Err(JournalError::Conflict(
                "compare-and-swap revision/domain mismatch".into(),
            ));
        }
        let record = JournalRecord {
            request: request.clone(),
            payload,
        };
        let mut bytes = serde_json::to_vec(&record)?;
        bytes.push(b'\n');
        if self
            .bytes
            .checked_add(bytes.len() as u64)
            .is_none_or(|size| size > self.max_bytes)
        {
            return Err(JournalError::Capacity);
        }
        self.poisoned = true;
        #[cfg(test)]
        let lose_receipt = match self.fault.as_mut() {
            Some((0, false)) => {
                return Err(JournalError::Io(std::io::Error::other(
                    "injected transaction write failure",
                )));
            }
            Some((0, true)) => true,
            Some((remaining, _)) => {
                *remaining -= 1;
                false
            }
            None => false,
        };
        let file = self.file.as_mut().ok_or(JournalError::Unavailable)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        #[cfg(test)]
        if lose_receipt {
            return Err(JournalError::Io(std::io::Error::other(
                "injected acknowledgement loss after durable sync",
            )));
        }
        self.bytes += bytes.len() as u64;
        self.records.push(record);
        self.poisoned = false;
        Ok(CommitReceipt::confirmed(request))
    }

    pub fn close(&mut self) -> Result<(), JournalError> {
        if let Some(file) = self.file.as_ref() {
            self.available()?;
            file.sync_all()?;
        }
        self.file.take();
        self.directories.clear();
        Ok(())
    }
    /// Validate and flush while retaining the writer and its path guards.
    pub(crate) fn prepare_close(&self) -> Result<(), JournalError> {
        self.available()?;
        self.file
            .as_ref()
            .ok_or(JournalError::Unavailable)?
            .sync_all()?;
        Ok(())
    }
    /// Only after every participant prepared and ownership release succeeded.
    pub(crate) fn finish_close(&mut self) {
        self.file.take();
        self.directories.clear();
    }
}
