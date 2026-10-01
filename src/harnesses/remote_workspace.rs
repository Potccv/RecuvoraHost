//! Node-owned workspace references and remote path metadata validation.
use super::{HarnessDefinition, HarnessError};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RemoteWorkspace {
    pub node_id: String,
    pub workspace_id: String,
}
impl RemoteWorkspace {
    pub fn resource_path(&self) -> PathBuf {
        PathBuf::from(format!("node://{}/{}", self.node_id, self.workspace_id))
    }
    pub(super) fn validate(&self, definition: &HarnessDefinition) -> Result<(), HarnessError> {
        if !crate::protocol::valid_id(&self.node_id)
            || !crate::protocol::valid_id(&self.workspace_id)
            || definition.address != format!("node://{}", self.node_id)
            || !definition
                .workspace_roots
                .iter()
                .any(|id| id.to_str() == Some(&self.workspace_id))
        {
            return Err(HarnessError::InvalidRequest(
                "node/workspace is not in the selected Harness scope".into(),
            ));
        }
        Ok(())
    }
}

pub(super) fn validate_remote_definition(
    definition: &HarnessDefinition,
) -> Result<(), HarnessError> {
    let Some(id) = definition.address.strip_prefix("node://") else {
        return Err(HarnessError::UnsupportedAddress {
            adapter: definition.adapter.clone(),
            address: definition.address.clone(),
        });
    };
    if !crate::protocol::valid_id(id)
        || definition.workspace_roots.iter().any(|root| {
            root.to_str()
                .is_none_or(|id| !crate::protocol::valid_id(id))
        })
    {
        return Err(HarnessError::InvalidConfiguration(
            "remote address must be node://ID and workspace_roots must contain workspace IDs"
                .into(),
        ));
    }
    Ok(())
}
pub(super) fn valid_remote_path(path: &Path) -> bool {
    let Some(text) = path.to_str() else {
        return false;
    };
    !text.is_empty()
        && text.len() <= 4096
        && !text.chars().any(char::is_control)
        && (text.starts_with('/')
            || text.starts_with("\\\\")
            || (text.len() > 2
                && text.as_bytes()[0].is_ascii_alphabetic()
                && text.as_bytes()[1] == b':'
                && matches!(text.as_bytes()[2], b'\\' | b'/')))
}
