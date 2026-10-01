//! Stable root ownership with an explicitly installed, complete storage generation.
use super::{RecoveryError, storage_paths as paths};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::{Read, Seek},
    path::{Path, PathBuf},
};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MigrationMarker {
    pub host_recovery_layout: u32,
    pub generation: String,
}

#[cfg(test)]
#[path = "../../../tests/recovery_layout.rs"]
mod tests;

impl MigrationMarker {
    pub(crate) fn validate(&self) -> Result<(), RecoveryError> {
        let suffix = self.generation.strip_prefix(".core02-").unwrap_or_default();
        if self.host_recovery_layout != 1
            || suffix.is_empty()
            || suffix.len() > 100
            || !suffix.bytes().all(|c| c.is_ascii_digit() || c == b'-')
        {
            return Err(RecoveryError::Invalid(
                "invalid recovery migration marker".into(),
            ));
        }
        Ok(())
    }
}

pub(crate) struct RootStorageLock {
    path: PathBuf,
    file: File,
    _directories: Vec<File>,
    marker: Option<(PathBuf, File, Vec<u8>)>,
}

impl RootStorageLock {
    pub(crate) fn acquire(root: &Path) -> Result<Self, RecoveryError> {
        if root.file_name().is_some_and(|name| {
            name.to_string_lossy()
                .to_ascii_lowercase()
                .starts_with(".core02-")
        }) {
            return Err(RecoveryError::Invalid(
                "open a migrated recovery through its original root".into(),
            ));
        }
        let path = root.join("recovery.lock");
        let (file, directories) = paths::open(&path)?;
        file.try_lock_exclusive()?;
        if file.metadata()?.len() != 0 {
            return Err(RecoveryError::Corrupt(
                "recovery identity lock is not empty".into(),
            ));
        }
        Ok(Self {
            path,
            file,
            _directories: directories,
            marker: None,
        })
    }

    pub(crate) fn resolve(&mut self, root: &Path) -> Result<PathBuf, RecoveryError> {
        self.validate()?;
        let path = root.join("recovery.jsonl");
        let (mut file, _) = paths::open(&path)?;
        let mut bytes = Vec::new();
        Read::by_ref(&mut file).take(1025).read_to_end(&mut bytes)?;
        let first = bytes
            .split_inclusive(|byte| *byte == b'\n')
            .next()
            .unwrap_or_default();
        let value = serde_json::from_slice::<serde_json::Value>(first).ok();
        if !value
            .as_ref()
            .is_some_and(|v| v.get("host_recovery_layout").is_some())
        {
            return Ok(root.to_path_buf());
        }
        if bytes.len() > 1024
            || file.metadata()?.len() != bytes.len() as u64
            || bytes.last() != Some(&b'\n')
        {
            return Err(RecoveryError::Corrupt(
                "invalid migration marker size or tail".into(),
            ));
        }
        let marker: MigrationMarker = serde_json::from_slice(&bytes)?;
        marker.validate()?;
        let directory = root.join(&marker.generation);
        for name in [
            "recovery.jsonl",
            "dispatch.jsonl",
            "approvals/approvals.jsonl",
            "knowledge.jsonl",
            "legacy-recovery.jsonl",
        ] {
            if !directory.join(name).is_file() {
                return Err(RecoveryError::Corrupt(
                    "incomplete migrated recovery generation".into(),
                ));
            }
        }
        self.marker = Some((path, file, bytes));
        Ok(directory)
    }

    pub(crate) fn validate(&self) -> Result<(), RecoveryError> {
        paths::validate_current(&self.path, &self.file)?;
        if self.file.metadata()?.len() != 0 {
            return Err(RecoveryError::Corrupt(
                "recovery identity lock changed".into(),
            ));
        }
        if let Some((path, file, original)) = &self.marker {
            paths::validate_current(path, file)?;
            let mut reader = file.try_clone()?;
            reader.rewind()?;
            let mut current = Vec::new();
            reader.take(1025).read_to_end(&mut current)?;
            if current != *original {
                return Err(RecoveryError::Corrupt("migration marker changed".into()));
            }
        }
        Ok(())
    }
}
