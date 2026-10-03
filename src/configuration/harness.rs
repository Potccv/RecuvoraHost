//! Harness configuration loading for embedded callers.
use crate::harnesses::{HarnessError, HarnessRegistryConfig};
use std::io;
use std::path::Path;

pub fn load_harness_config(path: &Path) -> Result<HarnessRegistryConfig, HarnessError> {
    let path = std::fs::canonicalize(path).map_err(|error| {
        HarnessError::InvalidConfiguration(format!("cannot resolve active configuration: {error}"))
    })?;
    let project = match std::fs::canonicalize(env!("CARGO_MANIFEST_DIR")) {
        Ok(project) => Some(project),
        // A copied executable does not require its build machine's source tree.
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(HarnessError::InvalidConfiguration(format!(
                "cannot inspect source directory: {error}"
            )));
        }
    };
    if project.is_some_and(|project| path.starts_with(project)) {
        return Err(HarnessError::InvalidConfiguration(
            "active configuration must be outside the project source tree; copy the example to an external directory and adjust workspace_roots".to_owned(),
        ));
    }
    let config = HarnessRegistryConfig::load(path)?;
    config.validate()?;
    Ok(config)
}
