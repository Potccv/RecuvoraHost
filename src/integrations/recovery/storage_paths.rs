//! Local journal identity and source separation, independent of action authority.
use super::RecoveryError;
use std::{
    fs::{File, Metadata, OpenOptions},
    path::{Component, Path},
};

pub(super) fn open(path: &Path) -> Result<(File, Vec<File>), RecoveryError> {
    if !path.is_absolute()
        || path.file_name().is_none()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
    {
        return Err(RecoveryError::Invalid(
            "journal requires an absolute file path without traversal".into(),
        ));
    }
    let parent = path
        .parent()
        .ok_or_else(|| RecoveryError::Invalid("journal parent missing".into()))?;
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

fn validate_external(parent: &Path) -> Result<(), RecoveryError> {
    for ancestor in parent.ancestors() {
        match std::fs::symlink_metadata(ancestor) {
            Ok(metadata) if linked(&metadata) || !metadata.is_dir() => {
                return Err(RecoveryError::Invalid(
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
            .ok_or_else(|| RecoveryError::Invalid("existing journal ancestor missing".into()))?;
    }
    let resolved = existing.canonicalize()?;
    // Installed binaries do not require their original compiler source tree.
    match Path::new(env!("CARGO_MANIFEST_DIR")).canonicalize() {
        Ok(project) if resolved.starts_with(&project) => Err(RecoveryError::Invalid(
            "journal must be outside project source".into(),
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn reject_nonregular(path: &Path) -> Result<(), RecoveryError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if linked(&metadata) || !metadata.is_file() => Err(RecoveryError::Invalid(
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

pub(super) fn validate_current(path: &Path, file: &File) -> Result<(), RecoveryError> {
    reject_nonregular(path)?;
    let metadata = file.metadata()?;
    if linked(&metadata) || !metadata.is_file() {
        return Err(RecoveryError::Invalid(
            "journal handle is not a regular unlinked file".into(),
        ));
    }
    #[cfg(windows)]
    {
        if winapi_util::file::information(file)?.number_of_links() != 1 {
            return Err(RecoveryError::Invalid(
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
            return Err(RecoveryError::Invalid(
                "journal hard link or replaced identity".into(),
            ));
        }
        validate_external(
            path.parent()
                .ok_or_else(|| RecoveryError::Invalid("journal parent missing".into()))?,
        )?;
    }
    Ok(())
}

#[cfg(windows)]
fn prepare_directories(parent: &Path) -> Result<Vec<File>, RecoveryError> {
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
            return Err(RecoveryError::Invalid("linked journal ancestor".into()));
        }
        verify_handle_path(&handle, ancestor)?;
        handles.push(handle);
    }
    Ok(handles)
}

#[cfg(not(windows))]
fn prepare_directories(parent: &Path) -> Result<Vec<File>, RecoveryError> {
    std::fs::create_dir_all(parent)?;
    validate_external(parent)?;
    Ok(Vec::new())
}

#[cfg(windows)]
fn verify_handle_path(file: &File, path: &Path) -> Result<(), RecoveryError> {
    use filepath::FilePath;
    if windows_components(&file.path()?)? != windows_components(path)? {
        return Err(RecoveryError::Invalid("journal object path changed".into()));
    }
    Ok(())
}

#[cfg(windows)]
fn windows_components(path: &Path) -> Result<Vec<std::ffi::OsString>, RecoveryError> {
    use std::path::Prefix;
    let mut parts = path.components();
    let drive = match parts.next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) => drive,
            _ => {
                return Err(RecoveryError::Invalid(
                    "journal requires a local DOS path".into(),
                ));
            }
        },
        _ => {
            return Err(RecoveryError::Invalid(
                "journal requires an absolute DOS path".into(),
            ));
        }
    };
    if parts.next() != Some(Component::RootDir) {
        return Err(RecoveryError::Invalid(
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
            _ => {
                return Err(RecoveryError::Invalid("journal path alias rejected".into()));
            }
        }
    }
    Ok(result)
}
