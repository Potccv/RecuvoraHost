//! Target-relative naming and protected control-directory policy.
use super::ActionError;
use std::path::{Component, Path, PathBuf};

pub(super) fn validate_relative(name: &str) -> Result<(), ActionError> {
    if name.is_empty()
        || name.len() > 512
        || name.contains('\\')
        || name.contains(':')
        || name.chars().any(char::is_control)
    {
        return Err(denied("use a bounded relative path with forward slashes"));
    }
    for segment in name.split('/') {
        let upper = segment.to_ascii_uppercase();
        let stem = upper.split('.').next().unwrap_or("");
        if segment.is_empty()
            || segment == "."
            || segment == ".."
            || segment.starts_with('.')
            || segment.ends_with(['.', ' '])
            || matches!(stem, "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$")
            || ((stem.starts_with("COM") || stem.starts_with("LPT")) && stem.len() == 4)
        {
            return Err(denied("invalid or protected path component"));
        }
    }
    if Path::new(name)
        .components()
        .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(denied("only ordinary relative path components are allowed"));
    }
    Ok(())
}

pub(super) fn denied(message: &str) -> ActionError {
    ActionError::Denied(message.to_owned())
}

pub(super) fn reject_control_directories(path: &Path) -> Result<(), ActionError> {
    #[cfg(test)]
    let allowed_test_root = external_test_root();
    let mut prefix = PathBuf::new();
    for part in path.components() {
        prefix.push(part.as_os_str());
        if let Component::Normal(name) = part {
            let name = name.to_string_lossy();
            if name.starts_with('.') {
                #[cfg(test)]
                if allowed_test_root.as_ref().is_some_and(|root| {
                    prefix
                        .canonicalize()
                        .is_ok_and(|candidate| candidate == *root)
                }) {
                    continue;
                }
                return Err(denied("target root cannot be inside a control directory"));
            }
        }
    }
    Ok(())
}

/// Unit tests keep all generated targets below one caller-provided external
/// directory. Permit only that directory itself to have a dot-prefixed name;
/// nested control directories remain denied. Production builds neither read
/// nor honor this environment variable.
#[cfg(test)]
fn external_test_root() -> Option<PathBuf> {
    let root = std::fs::canonicalize(std::env::var_os("RECUVORA_TEST_TEMP")?).ok()?;
    let project = std::fs::canonicalize(env!("CARGO_MANIFEST_DIR")).ok()?;
    if root.starts_with(&project) || project.starts_with(&root) {
        return None;
    }
    Some(root)
}
