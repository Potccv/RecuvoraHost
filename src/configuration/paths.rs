//! Lexical configuration path validation and link rejection.
use crate::repair::WorkflowError;
#[cfg(windows)]
use std::ffi::OsStr;
use std::ffi::OsString;
use std::io;
#[cfg(windows)]
use std::path::Prefix;
use std::path::{Path, PathBuf};

pub(super) fn absolute_normal(path: &Path) -> Result<PathBuf, WorkflowError> {
    use std::path::Component;
    let mut output = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                if !output.pop() {
                    return Err(invalid("path escapes filesystem root"));
                }
            }
            other => output.push(other.as_os_str()),
        }
    }
    if !output.is_absolute() {
        return Err(invalid("absolute path required"));
    }
    Ok(output)
}

pub(super) fn reject_links(path: &Path) -> Result<(), WorkflowError> {
    for ancestor in path.ancestors() {
        match std::fs::symlink_metadata(ancestor) {
            Ok(meta) => {
                #[cfg(windows)]
                {
                    use std::os::windows::fs::MetadataExt;
                    if meta.file_attributes() & 0x400 != 0 {
                        return Err(invalid(
                            "reparse points in configuration or runtime paths are forbidden",
                        ));
                    }
                }
                if meta.file_type().is_symlink() {
                    return Err(invalid("linked configuration/runtime paths are forbidden"));
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

/// Resolves the longest existing ancestor and then restores a normalized,
/// non-existing suffix. This gives path-placement checks one namespace even
/// before a runtime directory is created.
pub(crate) fn resolve_existing_ancestor(path: &Path) -> io::Result<PathBuf> {
    if !path.is_absolute()
        || path.components().any(|part| {
            matches!(
                part,
                std::path::Component::CurDir | std::path::Component::ParentDir
            )
        })
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "absolute normalized path required",
        ));
    }

    let mut existing = path;
    let mut suffix = Vec::<OsString>::new();
    loop {
        match std::fs::symlink_metadata(existing) {
            Ok(_) => break,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let name = existing.file_name().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::NotFound, "existing path ancestor required")
                })?;
                #[cfg(windows)]
                validate_unresolved_windows_component(name)?;
                suffix.push(name.to_os_string());
                existing = existing.parent().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::NotFound, "existing path ancestor required")
                })?;
            }
            Err(error) => return Err(error),
        }
    }

    let mut resolved = existing.canonicalize()?;
    for component in suffix.iter().rev() {
        resolved.push(component);
    }
    Ok(resolved)
}

/// Win32 folds several spellings while creating a path. An unresolved suffix
/// cannot be compared by object identity yet, so accept only an unambiguous
/// ASCII component. A deployer can pre-create a non-ASCII directory and let
/// existing-ancestor canonicalization establish its identity.
#[cfg(windows)]
fn validate_unresolved_windows_component(name: &OsStr) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    let units: Vec<_> = name.encode_wide().collect();
    if units.is_empty() || units.iter().any(|unit| *unit > 0x7f) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unresolved Windows path components must be ASCII",
        ));
    }
    let text = name.to_str().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "unresolved Windows path component is not valid text",
        )
    })?;
    if text.ends_with(['.', ' '])
        || text.bytes().any(|byte| {
            byte < 0x20
                || matches!(
                    byte,
                    b'"' | b'*' | b'/' | b':' | b'<' | b'>' | b'?' | b'|' | b'\\'
                )
        })
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unresolved Windows path component has an ambiguous or reserved spelling",
        ));
    }
    let upper = text.to_ascii_uppercase();
    let stem = upper.split('.').next().unwrap_or("");
    if matches!(stem, "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$")
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.len() == 4
            && stem.as_bytes()[3].is_ascii_digit()
            && stem.as_bytes()[3] != b'0')
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unresolved Windows path component uses a reserved device name",
        ));
    }
    Ok(())
}

/// Compares already-normalized absolute paths using Windows' DOS/verbatim and
/// case-insensitive component semantics. Other platforms use native component
/// comparison.
pub(crate) fn path_within(path: &Path, root: &Path) -> io::Result<bool> {
    #[cfg(windows)]
    {
        Ok(windows_components(path)?.starts_with(&windows_components(root)?))
    }
    #[cfg(not(windows))]
    {
        Ok(path.starts_with(root))
    }
}

#[cfg(windows)]
fn windows_components(path: &Path) -> io::Result<Vec<OsString>> {
    use std::path::Component;

    let mut parts = path.components();
    let mut result = match parts.next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) => {
                vec![
                    OsString::from("disk"),
                    OsString::from(format!("{}:", drive.to_ascii_lowercase() as char)),
                ]
            }
            Prefix::UNC(server, share) | Prefix::VerbatimUNC(server, share) => vec![
                OsString::from("unc"),
                server.to_ascii_lowercase(),
                share.to_ascii_lowercase(),
            ],
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "unsupported Windows path prefix",
                ));
            }
        },
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "absolute Windows path required",
            ));
        }
    };
    if parts.next() != Some(Component::RootDir) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "rooted Windows path required",
        ));
    }
    for part in parts {
        match part {
            Component::Normal(name) => result.push(name.to_ascii_lowercase()),
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "path traversal or alias component rejected",
                ));
            }
        }
    }
    Ok(result)
}

pub(super) fn invalid(message: impl Into<String>) -> WorkflowError {
    WorkflowError::Invalid(message.into())
}
