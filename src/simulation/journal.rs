//! Single-writer snapshot journal and conservative recovery validation.
use super::{EngineConfig, EngineError, TaskSnapshot, TaskState};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
    sync::{Arc, Mutex},
};

#[derive(Serialize, Deserialize)]
struct JournalRecord {
    format: u32,
    sequence: u64,
    snapshot: TaskSnapshot,
}

pub(super) struct Journal {
    file: Arc<Mutex<File>>,
    lock: Arc<File>,
    bytes: u64,
    max_bytes: u64,
    sequence: u64,
}

impl Journal {
    pub(super) fn open(
        dir: &Path,
        config: &EngineConfig,
    ) -> Result<(Self, BTreeMap<String, TaskSnapshot>), EngineError> {
        std::fs::create_dir_all(dir)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.join("writer.lock"))?;
        lock.try_lock_exclusive().map_err(EngineError::Locked)?;
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.join("tasks.jsonl"))?;
        if file.metadata()?.len() > config.max_journal_bytes {
            return Err(EngineError::JournalFull);
        }
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(config.max_journal_bytes + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > config.max_journal_bytes {
            return Err(EngineError::JournalFull);
        }
        let complete = bytes
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |index| index + 1);
        let mut records: BTreeMap<String, TaskSnapshot> = BTreeMap::new();
        let mut sequence = 0;
        for complete_line in bytes[..complete].split_inclusive(|byte| *byte == b'\n') {
            let line = &complete_line[..complete_line.len() - 1];
            let entry: JournalRecord = serde_json::from_slice(line)
                .map_err(|error| EngineError::Corrupt(error.to_string()))?;
            if entry.format != 1 || entry.sequence != sequence + 1 {
                return Err(EngineError::Corrupt(
                    "unsupported format or nonsequential record".into(),
                ));
            }
            entry
                .snapshot
                .spec
                .validate()
                .map_err(|error| EngineError::Corrupt(error.to_string()))?;
            match records.get(&entry.snapshot.spec.id) {
                None if entry.snapshot.revision == 0
                    && entry.snapshot.state
                        == if entry.snapshot.spec.simulation_authorized {
                            TaskState::Queued
                        } else {
                            TaskState::Denied
                        } => {}
                Some(previous)
                    if previous.spec == entry.snapshot.spec
                        && previous.revision.checked_add(1) == Some(entry.snapshot.revision)
                        && previous.state.allows(entry.snapshot.state) => {}
                _ => {
                    return Err(EngineError::Corrupt(
                        "invalid task revision or transition".into(),
                    ));
                }
            }
            sequence = entry.sequence;
            records.insert(entry.snapshot.spec.id.clone(), entry.snapshot);
            if records.len() > config.max_tasks {
                return Err(EngineError::Capacity);
            }
        }
        if complete != bytes.len() {
            file.set_len(complete as u64)?;
            file.sync_all()?;
        }
        file.seek(SeekFrom::End(0))?;
        Ok((
            Self {
                file: Arc::new(Mutex::new(file)),
                lock: Arc::new(lock),
                bytes: complete as u64,
                max_bytes: config.max_journal_bytes,
                sequence,
            },
            records,
        ))
    }

    pub(super) async fn append(&mut self, snapshot: &TaskSnapshot) -> Result<(), EngineError> {
        let record = JournalRecord {
            format: 1,
            sequence: self.sequence + 1,
            snapshot: snapshot.clone(),
        };
        let mut bytes =
            serde_json::to_vec(&record).map_err(|error| EngineError::Corrupt(error.to_string()))?;
        bytes.push(b'\n');
        if self.bytes.saturating_add(bytes.len() as u64) > self.max_bytes {
            return Err(EngineError::JournalFull);
        }
        let appended_bytes = bytes.len() as u64;
        let file = self.file.clone();
        let lock = self.lock.clone();
        // A canceled actor must not release the writer lock while a blocking
        // filesystem operation is still running. The I/O closure owns both Arcs.
        tokio::task::spawn_blocking(move || -> Result<(), EngineError> {
            let _write_lock = lock;
            let mut file = file
                .lock()
                .map_err(|_| EngineError::Internal("journal mutex poisoned".into()))?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            Ok(())
        })
        .await
        .map_err(|error| EngineError::Internal(error.to_string()))??;
        self.bytes += appended_bytes;
        self.sequence += 1;
        Ok(())
    }
}
