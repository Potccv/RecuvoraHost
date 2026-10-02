//! Stable root ownership; retired storage generations are rejected.
use super::{RecoveryError, storage_paths as paths};
use fs2::FileExt;
use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

#[cfg(test)]
#[path = "../../../tests/recovery_layout.rs"]
mod tests;

pub(crate) struct RootStorageLock {
    path: PathBuf,
    file: File,
    _directories: Vec<File>,
}

impl RootStorageLock {
    pub(crate) fn acquire(root: &Path) -> Result<Self, RecoveryError> {
        if root.file_name().is_some_and(|name| {
            name.to_string_lossy()
                .to_ascii_lowercase()
                .starts_with(".core02-")
        }) {
            return Err(RecoveryError::Invalid(
                "retired recovery generation cannot be used as a storage root".into(),
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
        Err(RecoveryError::Corrupt(
            "retired recovery storage layout is unsupported".into(),
        ))
    }

    pub(crate) fn validate(&self) -> Result<(), RecoveryError> {
        paths::validate_current(&self.path, &self.file)?;
        if self.file.metadata()?.len() != 0 {
            return Err(RecoveryError::Corrupt(
                "recovery identity lock changed".into(),
            ));
        }
        Ok(())
    }
}
