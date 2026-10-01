//! Durable ownership of a canonical domain target, independent of state-directory routing.
use super::RecoveryError;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};

use super::storage_paths as paths;

const MAX_RECORD_BYTES: usize = 16 * 1024;
const MAX_JOURNAL_BYTES: u64 = 4 * 1024 * 1024;

/// Host-resolved, stable logical identity of the protected domain target.
/// Aliases must resolve to the same value before acquiring ownership.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct CanonicalTarget(String);

impl CanonicalTarget {
    pub fn new(id: impl Into<String>) -> Result<Self, RecoveryError> {
        let id = id.into();
        if !crate::protocol::valid_id(&id)
            || !id.bytes().all(|byte| !byte.is_ascii_uppercase())
            || !id.as_bytes()[0].is_ascii_alphanumeric()
            || id.ends_with('.')
        {
            return Err(RecoveryError::Invalid(
                "canonical target requires a lowercase logical identity without path aliases"
                    .into(),
            ));
        }
        Ok(Self(id))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Trusted Host integration. All owners of one target must share this authority.
/// The directory is the recovery service's storage identity, never a node workspace.
pub trait TargetOwnership: Send + Sync {
    fn acquire(
        &self,
        target: &CanonicalTarget,
        recovery_directory: &Path,
    ) -> Result<Box<dyn TargetLease>, RecoveryError>;
}

/// A non-cloneable ownership capability held for the entire recovery lifetime.
/// Dropping a lease must retain its durable claim. Host may explicitly release
/// only after draining calls and confirming no unfinished or Unknown authority.
pub trait TargetLease: Send + Sync {
    fn target(&self) -> &CanonicalTarget;
    /// The canonical storage directory bound to this ownership capability.
    fn recovery_directory(&self) -> &Path;
    fn validate(&self) -> Result<(), RecoveryError>;
    fn release(&mut self) -> Result<(), RecoveryError>;
}

/// Local durable authority shared by every recovery protecting the same target.
/// Process exit unlocks the journal but does not release its persisted owner.
pub struct FileTargetOwnership {
    directory: PathBuf,
    authority_path: PathBuf,
    authority: File,
    _directories: Vec<File>,
}

impl FileTargetOwnership {
    pub fn open(directory: impl AsRef<Path>) -> Result<Self, RecoveryError> {
        let directory = directory.as_ref().to_path_buf();

        let authority_path = directory.join("ownership-authority.lock");
        let (authority, directories) = paths::open(&authority_path)?;
        if authority.metadata()?.len() != 0 {
            return Err(RecoveryError::Corrupt(
                "ownership authority marker contains unexpected data".into(),
            ));
        }
        Ok(Self {
            directory,
            authority_path,
            authority,
            _directories: directories,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StorageOwner {
    directory: String,
    device: u64,
    file: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    format: u32,
    sequence: u64,
    target: String,
    owner: StorageOwner,
    released: bool,
}

struct FileTargetLease {
    target: CanonicalTarget,
    recovery_directory: PathBuf,
    recovery_lock_path: PathBuf,
    recovery_lock: File,
    owner: StorageOwner,
    authority_path: PathBuf,
    authority: File,
    path: PathBuf,
    journal: File,
    _directories: Vec<File>,
    sequence: u64,
    bytes: u64,
    released: bool,
    poisoned: bool,
}

impl TargetOwnership for FileTargetOwnership {
    fn acquire(
        &self,
        target: &CanonicalTarget,
        recovery_directory: &Path,
    ) -> Result<Box<dyn TargetLease>, RecoveryError> {
        paths::validate_current(&self.authority_path, &self.authority)?;
        if self.authority.metadata()?.len() != 0 {
            return Err(RecoveryError::Corrupt(
                "ownership authority marker changed".into(),
            ));
        }
        let path = self
            .directory
            .join(format!("target-{}.jsonl", target.as_str()));
        let (journal, mut directories) = paths::open(&path)?;
        match journal.try_lock_exclusive() {
            Ok(()) => {}
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.raw_os_error() == fs2::lock_contended_error().raw_os_error() =>
            {
                return Err(RecoveryError::Busy);
            }
            Err(error) => return Err(error.into()),
        }
        let (sequence, previous) = replay(&journal, target)?;
        let recovery_lock_path = recovery_directory.join("recovery.lock");
        let (recovery_lock, recovery_directories) = paths::open(&recovery_lock_path)?;
        directories.extend(recovery_directories);
        if recovery_lock.metadata()?.len() != 0 {
            return Err(RecoveryError::Corrupt(
                "recovery identity lock contains unexpected data".into(),
            ));
        }
        let canonical_directory = recovery_directory.canonicalize()?;
        let directory = canonical_directory.to_str().ok_or_else(|| {
            RecoveryError::Invalid("recovery identity must have a UTF-8 directory".into())
        })?;
        #[cfg(windows)]
        let directory = directory.to_ascii_lowercase();
        #[cfg(not(windows))]
        let directory = directory.to_owned();
        let (device, file) = file_identity(&recovery_lock)?;
        let owner = StorageOwner {
            directory,
            device,
            file,
        };
        validate_owner(&owner)?;
        if previous.as_ref().is_some_and(|previous| previous != &owner) {
            return Err(RecoveryError::Busy);
        }
        let bytes = journal.metadata()?.len();
        let mut lease = FileTargetLease {
            target: target.clone(),
            recovery_directory: canonical_directory,
            recovery_lock_path,
            recovery_lock,
            owner,
            authority_path: self.authority_path.clone(),
            authority: self.authority.try_clone()?,
            path,
            journal,
            _directories: directories,
            sequence,
            bytes,
            released: false,
            poisoned: false,
        };
        lease.validate()?;
        if previous.is_none() {
            lease.append(false)?;
        }
        Ok(Box::new(lease))
    }
}

fn validate_owner(owner: &StorageOwner) -> Result<(), RecoveryError> {
    let path = Path::new(&owner.directory);
    if owner.directory.len() > 8192
        || owner.directory.contains('\0')
        || !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
    {
        return Err(RecoveryError::Invalid(
            "invalid recovery storage identity".into(),
        ));
    }
    Ok(())
}

fn replay(
    file: &File,
    target: &CanonicalTarget,
) -> Result<(u64, Option<StorageOwner>), RecoveryError> {
    let length = file.metadata()?.len();
    if length > MAX_JOURNAL_BYTES {
        return Err(RecoveryError::Capacity);
    }
    let mut copy = file.try_clone()?;
    copy.seek(SeekFrom::Start(0))?;
    let mut reader = BufReader::new(copy);
    let mut owner = None;
    let mut sequence = 0u64;
    let mut bytes = 0u64;
    loop {
        let mut line = Vec::new();
        let count = reader
            .by_ref()
            .take((MAX_RECORD_BYTES + 1) as u64)
            .read_until(b'\n', &mut line)?;
        if count == 0 {
            break;
        }
        if count > MAX_RECORD_BYTES || line.last() != Some(&b'\n') {
            return Err(RecoveryError::Corrupt("incomplete ownership event".into()));
        }
        let entry: Entry = serde_json::from_slice(&line)
            .map_err(|error| RecoveryError::Corrupt(error.to_string()))?;
        validate_owner(&entry.owner).map_err(|error| RecoveryError::Corrupt(error.to_string()))?;
        if entry.format != 1
            || entry.sequence != sequence.checked_add(1).ok_or(RecoveryError::Capacity)?
            || entry.target != target.as_str()
            || (entry.released && owner.as_ref() != Some(&entry.owner))
            || (!entry.released && owner.is_some())
        {
            return Err(RecoveryError::Corrupt(
                "invalid ownership transition or identity".into(),
            ));
        }
        owner = if entry.released {
            None
        } else {
            Some(entry.owner)
        };
        sequence = entry.sequence;
        bytes = bytes
            .checked_add(count as u64)
            .ok_or(RecoveryError::Capacity)?;
    }
    if bytes != length || file.metadata()?.len() != length {
        return Err(RecoveryError::Corrupt(
            "ownership journal changed during replay".into(),
        ));
    }
    Ok((sequence, owner))
}

impl FileTargetLease {
    fn append(&mut self, released: bool) -> Result<(), RecoveryError> {
        self.validate()?;
        let sequence = self
            .sequence
            .checked_add(1)
            .ok_or(RecoveryError::Capacity)?;
        let mut bytes = serde_json::to_vec(&Entry {
            format: 1,
            sequence,
            target: self.target.as_str().into(),
            owner: self.owner.clone(),
            released,
        })?;
        bytes.push(b'\n');
        let total = self
            .bytes
            .checked_add(bytes.len() as u64)
            .filter(|total| *total <= MAX_JOURNAL_BYTES)
            .ok_or(RecoveryError::Capacity)?;
        if bytes.len() > MAX_RECORD_BYTES {
            return Err(RecoveryError::Capacity);
        }
        self.poisoned = true;
        self.journal.write_all(&bytes)?;
        self.journal.sync_data()?;
        self.sequence = sequence;
        self.bytes = total;
        self.poisoned = false;
        Ok(())
    }
}

impl TargetLease for FileTargetLease {
    fn target(&self) -> &CanonicalTarget {
        &self.target
    }

    fn recovery_directory(&self) -> &Path {
        &self.recovery_directory
    }

    fn validate(&self) -> Result<(), RecoveryError> {
        if self.released || self.poisoned {
            return Err(RecoveryError::Service(
                "target ownership is released or unavailable".into(),
            ));
        }
        paths::validate_current(&self.path, &self.journal)?;
        paths::validate_current(&self.authority_path, &self.authority)?;
        paths::validate_current(&self.recovery_lock_path, &self.recovery_lock)?;
        let (device, file) = file_identity(&self.recovery_lock)?;
        if self.journal.metadata()?.len() != self.bytes
            || self.authority.metadata()?.len() != 0
            || self.recovery_lock.metadata()?.len() != 0
            || (device, file) != (self.owner.device, self.owner.file)
        {
            return Err(RecoveryError::Corrupt(
                "target ownership file identity or length changed".into(),
            ));
        }
        Ok(())
    }

    fn release(&mut self) -> Result<(), RecoveryError> {
        self.append(true)?;
        self.released = true;
        FileExt::unlock(&self.journal)?;
        Ok(())
    }
}

#[cfg(windows)]
fn file_identity(file: &File) -> Result<(u64, u64), RecoveryError> {
    let info = winapi_util::file::information(file)?;
    Ok((info.volume_serial_number(), info.file_index()))
}

#[cfg(unix)]
fn file_identity(file: &File) -> Result<(u64, u64), RecoveryError> {
    use std::os::unix::fs::MetadataExt;
    let metadata = file.metadata()?;
    Ok((metadata.dev(), metadata.ino()))
}

#[cfg(not(any(windows, unix)))]
fn file_identity(_: &File) -> Result<(u64, u64), RecoveryError> {
    Err(RecoveryError::Invalid(
        "durable target ownership unsupported on this platform".into(),
    ))
}
