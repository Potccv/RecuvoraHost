//! Platform file handles, identity checks and path comparison.
use std::fs::File;
#[cfg(windows)]
use std::fs::OpenOptions;
use std::io;
#[cfg(windows)]
use std::path::Component;
use std::path::{Path, PathBuf};

#[cfg(windows)]
pub(super) fn pin_directories(path: &Path) -> io::Result<Vec<File>> {
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    use std::path::Prefix;
    if !matches!(path.components().next(), Some(Component::Prefix(p)) if matches!(p.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)))
        || path
            .components()
            .any(|p| matches!(p, Component::ParentDir | Component::CurDir))
        || !path.is_absolute()
    {
        return Err(io::Error::other(
            "only absolute local disk directories are supported",
        ));
    }
    let mut ancestors: Vec<_> = path.ancestors().collect();
    ancestors.reverse();
    let mut handles = Vec::new();
    for ancestor in ancestors {
        let handle = OpenOptions::new()
            .access_mode(0)
            .share_mode(3)
            .custom_flags(0x0200_0000 | 0x0020_0000)
            .security_qos_flags(0x0010_0000 | 0x0001_0000)
            .open(ancestor)?;
        let metadata = handle.metadata()?;
        if !metadata.is_dir() || metadata.file_attributes() & 0x400 != 0 {
            return Err(io::Error::other("directory reparse points are forbidden"));
        }
        verify_handle_path(&handle, ancestor)?;
        handles.push(handle);
    }
    Ok(handles)
}

#[cfg(windows)]
pub(super) fn open_pinned_file(path: &Path, write: bool) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    OpenOptions::new()
        .read(true)
        .write(write)
        .share_mode(1)
        .custom_flags(0x0020_0000)
        .security_qos_flags(0x0010_0000 | 0x0001_0000)
        .open(path)
}

#[cfg(windows)]
pub(super) fn verify_file(file: &File) -> io::Result<()> {
    let info = winapi_util::file::information(file)?;
    if !file.metadata()?.is_file()
        || info.file_attributes() & 0x400 != 0
        || info.number_of_links() != 1
    {
        return Err(io::Error::other(
            "only regular files without reparse points or hard links are allowed",
        ));
    }
    Ok(())
}

/// `filepath` returns the normalized path of an open Windows object and strips
/// the extended DOS prefix. Compare components without following paths again.
#[cfg(windows)]
pub(super) fn verify_handle_path(file: &File, expected: &Path) -> io::Result<PathBuf> {
    use filepath::FilePath;
    let actual = file.path()?;
    if windows_components(&actual)? != windows_components(expected)? {
        return Err(io::Error::other(
            "opened object does not match its explicitly allowed path",
        ));
    }
    Ok(actual)
}

#[cfg(windows)]
pub(super) fn path_within(path: &Path, root: &Path) -> io::Result<bool> {
    Ok(windows_components(path)?.starts_with(&windows_components(root)?))
}

#[cfg(windows)]
pub(super) fn windows_components(path: &Path) -> io::Result<Vec<std::ffi::OsString>> {
    use std::path::Prefix;
    let mut parts = path.components();
    let drive = match parts.next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) => drive,
            _ => return Err(io::Error::other("only local DOS paths are supported")),
        },
        _ => return Err(io::Error::other("absolute DOS path required")),
    };
    if parts.next() != Some(Component::RootDir) {
        return Err(io::Error::other("rooted DOS path required"));
    }
    let mut result = vec![std::ffi::OsString::from(format!(
        "{}:",
        drive.to_ascii_lowercase() as char
    ))];
    for part in parts {
        match part {
            Component::Normal(name) => result.push(name.to_ascii_lowercase()),
            _ => {
                return Err(io::Error::other(
                    "path traversal or alias component rejected",
                ));
            }
        }
    }
    Ok(result)
}

#[cfg(not(windows))]
pub(super) fn pin_directories(_: &Path) -> io::Result<Vec<File>> {
    Err(io::Error::other("text actions currently require Windows"))
}
#[cfg(not(windows))]
pub(super) fn open_pinned_file(_: &Path, _: bool) -> io::Result<File> {
    Err(io::Error::other("text actions currently require Windows"))
}
#[cfg(not(windows))]
pub(super) fn verify_file(_: &File) -> io::Result<()> {
    Err(io::Error::other("text actions currently require Windows"))
}
#[cfg(not(windows))]
pub(super) fn verify_handle_path(_: &File, _: &Path) -> io::Result<PathBuf> {
    Err(io::Error::other("text actions currently require Windows"))
}
#[cfg(not(windows))]
pub(super) fn path_within(_: &Path, _: &Path) -> io::Result<bool> {
    Err(io::Error::other("text actions currently require Windows"))
}
