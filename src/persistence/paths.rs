//! Local journal identity and source separation, independent of action authority.
use super::journal::JournalError;
use std::{
    fs::{File, Metadata, OpenOptions},
    path::{Component, Path},
};

pub(super) fn open(path: &Path) -> Result<(File, Vec<File>), JournalError> {
    if !path.is_absolute()
        || path.file_name().is_none()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
    {
        return Err(JournalError::Invalid(
            "journal requires an absolute file path without traversal".into(),
        ));
    }
    let parent = path
        .parent()
        .ok_or_else(|| JournalError::Invalid("journal parent missing".into()))?;
    validate_external(parent)?;
    let directories = prepare_directories(parent)?;
    reject_nonregular(path)?;
    let mut options = OpenOptions::new();
    options.read(true).append(true).create(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options
            .share_mode(3)
            .custom_flags(0x0020_0000)
            .security_qos_flags(0x0010_0000 | 0x0001_0000);
    }
    let journal = options.open(path)?;
    validate_current(path, &journal)?;
    Ok((journal, directories))
}

fn validate_external(parent: &Path) -> Result<(), JournalError> {
    for ancestor in parent.ancestors() {
        match std::fs::symlink_metadata(ancestor) {
            Ok(metadata) if linked(&metadata) || !metadata.is_dir() => {
                return Err(JournalError::Invalid(
                    "linked or non-directory journal ancestor".into(),
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    let mut existing = parent;
    while !existing.exists() {
        existing = existing
            .parent()
            .ok_or_else(|| JournalError::Invalid("existing journal ancestor missing".into()))?;
    }
    let resolved = existing.canonicalize()?;
    // Installed binaries do not require their original compiler source tree.
    for source in [env!("CARGO_MANIFEST_DIR"), env!("RECUVORA_CORE_SOURCE_DIR")] {
        match Path::new(source).canonicalize() {
            Ok(project) if resolved.starts_with(&project) => {
                return Err(JournalError::Invalid(
                    "journal must be outside Host and Core sources".into(),
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn reject_nonregular(path: &Path) -> Result<(), JournalError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if linked(&metadata) || !metadata.is_file() => Err(JournalError::Invalid(
            "journal must be a regular unlinked file".into(),
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn linked(metadata: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return true;
        }
    }
    metadata.file_type().is_symlink()
}

pub(super) fn validate_current(path: &Path, file: &File) -> Result<(), JournalError> {
    reject_nonregular(path)?;
    let metadata = file.metadata()?;
    if linked(&metadata) || !metadata.is_file() {
        return Err(JournalError::Invalid(
            "journal handle is not a regular unlinked file".into(),
        ));
    }
    #[cfg(windows)]
    {
        if winapi_util::file::information(file)?.number_of_links() != 1 {
            return Err(JournalError::Invalid(
                "hard-linked journal is forbidden".into(),
            ));
        }
        verify_handle_path(file, path)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let current = std::fs::symlink_metadata(path)?;
        if metadata.nlink() != 1
            || metadata.dev() != current.dev()
            || metadata.ino() != current.ino()
        {
            return Err(JournalError::Invalid(
                "journal hard link or replaced identity".into(),
            ));
        }
        validate_external(
            path.parent()
                .ok_or_else(|| JournalError::Invalid("journal parent missing".into()))?,
        )?;
    }
    Ok(())
}

#[cfg(windows)]
fn prepare_directories(parent: &Path) -> Result<Vec<File>, JournalError> {
    use std::os::windows::fs::OpenOptionsExt;
    // Pin each ancestor before creating/opening descendants; no FILE_SHARE_DELETE
    // keeps a concurrent rename from replacing the validated directory chain.
    windows_components(parent)?;
    let mut ancestors: Vec<_> = parent.ancestors().collect();
    ancestors.reverse();
    let mut handles = Vec::new();
    for ancestor in ancestors {
        if !ancestor.exists() {
            std::fs::create_dir(ancestor)?;
        }
        let handle = OpenOptions::new()
            .access_mode(0)
            .share_mode(3)
            .custom_flags(0x0200_0000 | 0x0020_0000)
            .security_qos_flags(0x0010_0000 | 0x0001_0000)
            .open(ancestor)?;
        let metadata = handle.metadata()?;
        if !metadata.is_dir() || linked(&metadata) {
            return Err(JournalError::Invalid("linked journal ancestor".into()));
        }
        verify_handle_path(&handle, ancestor)?;
        handles.push(handle);
    }
    Ok(handles)
}

#[cfg(not(windows))]
fn prepare_directories(parent: &Path) -> Result<Vec<File>, JournalError> {
    std::fs::create_dir_all(parent)?;
    validate_external(parent)?;
    Ok(Vec::new())
}

#[cfg(windows)]
fn verify_handle_path(file: &File, path: &Path) -> Result<(), JournalError> {
    use filepath::FilePath;
    if windows_components(&file.path()?)? != windows_components(path)? {
        return Err(JournalError::Invalid("journal object path changed".into()));
    }
    Ok(())
}

#[cfg(windows)]
fn windows_components(path: &Path) -> Result<Vec<std::ffi::OsString>, JournalError> {
    use std::path::Prefix;
    let mut parts = path.components();
    let drive = match parts.next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) => drive,
            _ => {
                return Err(JournalError::Invalid(
                    "journal requires a local DOS path".into(),
                ));
            }
        },
        _ => {
            return Err(JournalError::Invalid(
                "journal requires an absolute DOS path".into(),
            ));
        }
    };
    if parts.next() != Some(Component::RootDir) {
        return Err(JournalError::Invalid(
            "journal requires a rooted DOS path".into(),
        ));
    }
    let mut result = vec![std::ffi::OsString::from(format!(
        "{}:",
        drive.to_ascii_lowercase() as char
    ))];
    for part in parts {
        match part {
            Component::Normal(name) => result.push(name.to_ascii_lowercase()),
            _ => return Err(JournalError::Invalid("journal path alias rejected".into())),
        }
    }
    Ok(result)
}
