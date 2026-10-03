//! Host/Core source boundaries and runtime path preparation before first writes.
use std::io;
use std::path::{Path, PathBuf};

/// Resolve an application control path without creating directories or following
/// links. Missing suffixes are retained for validation before the first write.
pub(crate) fn external_path(path: &Path) -> io::Result<PathBuf> {
    let resolved = resolve_path(path)?;
    if source_roots()?
        .iter()
        .any(|source| resolved.starts_with(source))
    {
        return Err(invalid(
            "runtime paths and active configuration must be outside the Host and Core source trees",
        ));
    }
    Ok(resolved)
}

pub(crate) fn prepare_runtime_directory(path: &Path) -> io::Result<PathBuf> {
    let path = validate_simulation_paths(path)?;
    std::fs::create_dir_all(&path)?;
    validate_simulation_paths(&path)
}

/// Simulation storage is embedded here, so the application validates
/// existing journal/lock leaves as well as the directory before opening them.
pub(crate) fn validate_simulation_paths(path: &Path) -> io::Result<PathBuf> {
    let path = external_path(path)?;
    for file in ["writer.lock", "tasks.jsonl"] {
        external_path(&path.join(file))?;
    }
    Ok(path)
}

pub(super) fn relative_to(base: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_owned()
    } else {
        base.join(path)
    }
}

pub(super) fn source_roots() -> io::Result<Vec<PathBuf>> {
    let mut sources = Vec::new();
    // The build script resolves the actual path dependency from Cargo.toml;
    // runtime protection never guesses an adjacent directory by project name.
    for root in [env!("CARGO_MANIFEST_DIR"), env!("RECUVORA_CORE_SOURCE_DIR")] {
        match Path::new(root).canonicalize() {
            Ok(path) => sources.push(path),
            // A deployed binary must not need its build machine's source tree.
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(sources)
}

pub(super) fn resolve_path(path: &Path) -> io::Result<PathBuf> {
    let absolute = std::path::absolute(path)?;
    for ancestor in absolute.ancestors() {
        match std::fs::symlink_metadata(ancestor) {
            Ok(metadata) => {
                #[cfg(windows)]
                {
                    use std::os::windows::fs::MetadataExt;
                    if metadata.file_attributes() & 0x400 != 0 {
                        return Err(invalid("linked or reparse runtime paths are forbidden"));
                    }
                }
                if metadata.file_type().is_symlink() {
                    return Err(invalid("linked runtime paths are forbidden"));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    let mut existing = absolute.as_path();
    let mut suffix = Vec::new();
    loop {
        match existing.canonicalize() {
            Ok(mut resolved) => {
                for component in suffix.iter().rev() {
                    resolved.push(component);
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let name = existing
                    .file_name()
                    .ok_or_else(|| invalid("unresolvable runtime path"))?;
                suffix.push(name.to_os_string());
                existing = existing
                    .parent()
                    .ok_or_else(|| invalid("existing path ancestor required"))?;
            }
            Err(error) => return Err(error),
        }
    }
}

pub(super) fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
