use std::io;
use std::path::{Path, PathBuf};

/// Resolve a provider-returned directory without allowing it to select an
/// unrelated volume or network share before the configured roots are checked.
/// On Windows every lexical component is opened without following reparse
/// points and held until canonicalization and the final scope check complete.
#[cfg(target_os = "windows")]
pub(super) fn canonicalize_provider_directory(
    directory: &Path,
    allowed_roots: &[PathBuf],
) -> io::Result<PathBuf> {
    if !directory.is_absolute() || !lexically_allowed_windows(directory, allowed_roots) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "provider directory is not lexically within an allowed workspace root",
        ));
    }
    let handles = open_directory_chain(directory)?;
    let canonical = std::fs::canonicalize(directory).map_err(|error| {
        with_path_context("cannot canonicalize provider directory", directory, error)
    })?;
    if !canonical.is_dir() || !lexically_allowed_windows(&canonical, allowed_roots) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "provider directory resolves outside the allowed workspace roots",
        ));
    }
    drop(handles);
    Ok(canonical)
}

#[cfg(target_os = "windows")]
fn lexically_allowed_windows(directory: &Path, allowed_roots: &[PathBuf]) -> bool {
    let Some(directory) = windows_component_key(directory) else {
        return false;
    };
    allowed_roots.iter().any(|root| {
        windows_component_key(root)
            .is_some_and(|root| directory.len() >= root.len() && directory[..root.len()] == root)
    })
}

#[cfg(target_os = "windows")]
fn windows_component_key(path: &Path) -> Option<Vec<String>> {
    use std::path::{Component, Prefix};

    let mut components = path.components();
    let prefix = match components.next()? {
        Component::Prefix(prefix) => match prefix.kind() {
            Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) => {
                format!("disk:{}", (drive as char).to_ascii_lowercase())
            }
            Prefix::UNC(server, share) | Prefix::VerbatimUNC(server, share) => format!(
                "unc:{}:{}",
                server.to_string_lossy().to_lowercase(),
                share.to_string_lossy().to_lowercase()
            ),
            _ => return None,
        },
        _ => return None,
    };
    if !matches!(components.next(), Some(Component::RootDir)) {
        return None;
    }
    let mut key = vec![prefix];
    for component in components {
        match component {
            Component::Normal(value) => key.push(value.to_string_lossy().to_lowercase()),
            _ => return None,
        }
    }
    Some(key)
}

#[cfg(target_os = "windows")]
fn open_directory_chain(path: &Path) -> io::Result<Vec<std::fs::File>> {
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};

    if !path.is_absolute() {
        return Err(invalid_input("directory lease requires an absolute path"));
    }
    let mut components = path.ancestors().collect::<Vec<_>>();
    components.reverse();
    let mut handles = Vec::with_capacity(components.len());
    for component in components {
        if component.as_os_str().is_empty() {
            continue;
        }
        let file = std::fs::OpenOptions::new()
            .access_mode(0)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .security_qos_flags(SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION)
            .open(component)
            .map_err(|error| {
                with_path_context("cannot pin directory component", component, error)
            })?;
        let metadata = file.metadata().map_err(|error| {
            with_path_context("cannot inspect directory component", component, error)
        })?;
        if !metadata.is_dir() {
            return Err(invalid_input(
                "a pinned filesystem component is not a directory",
            ));
        }
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(invalid_input(
                "reparse points are not allowed in a pinned directory path",
            ));
        }
        handles.push(file);
    }
    Ok(handles)
}

#[cfg(target_os = "windows")]
fn invalid_input(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

#[cfg(target_os = "windows")]
fn with_path_context(action: &str, path: &Path, error: io::Error) -> io::Error {
    io::Error::new(
        error.kind(),
        format!("{action} ({}): {error}", path.display()),
    )
}

#[cfg(target_os = "windows")]
const FILE_SHARE_READ: u32 = 0x0000_0001;
#[cfg(target_os = "windows")]
const FILE_SHARE_WRITE: u32 = 0x0000_0002;
#[cfg(target_os = "windows")]
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
#[cfg(target_os = "windows")]
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
#[cfg(target_os = "windows")]
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
#[cfg(target_os = "windows")]
const SECURITY_IDENTIFICATION: u32 = 0x0001_0000;
#[cfg(target_os = "windows")]
const SECURITY_SQOS_PRESENT: u32 = 0x0010_0000;

#[cfg(not(target_os = "windows"))]
pub(super) fn canonicalize_provider_directory(
    directory: &Path,
    allowed_roots: &[PathBuf],
) -> io::Result<PathBuf> {
    use std::path::Component;

    if !directory.is_absolute()
        || directory
            .components()
            .any(|component| matches!(component, Component::ParentDir))
        || !allowed_roots.iter().any(|root| directory.starts_with(root))
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "provider directory is not lexically within an allowed workspace root",
        ));
    }
    let canonical = std::fs::canonicalize(directory)?;
    if !canonical.is_dir() || !allowed_roots.iter().any(|root| canonical.starts_with(root)) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "provider directory resolves outside the allowed workspace roots",
        ));
    }
    Ok(canonical)
}
