//! Normalization of untrusted provider projects and client grouping observations.
use super::limits::{
    MAX_DISCOVERED_PROJECTS, MAX_PROJECT_ID_BYTES, MAX_PROJECT_NAME_BYTES, MAX_WORKSPACE_ROOTS,
};
use super::{
    ClientProjectGrouping, ConversationVisibility, HarnessDefinition, HarnessError, HarnessProject,
    REMOTE_NODE_ADAPTER,
};
use super::{provider_path, remote_workspace};
use std::collections::BTreeSet;

pub(super) fn normalize_project_list_result(
    definition: &HarnessDefinition,
    projects: &mut [HarnessProject],
) -> Result<(), HarnessError> {
    if projects.len() > MAX_DISCOVERED_PROJECTS {
        return Err(HarnessError::ProtocolViolation {
            harness: definition.id.clone(),
            message: format!(
                "project discovery returned more than {MAX_DISCOVERED_PROJECTS} projects"
            ),
        });
    }
    let mut ids = BTreeSet::new();
    for project in projects {
        normalize_project(definition, project)?;
        if !ids.insert(project.id.as_str()) {
            return Err(HarnessError::ProtocolViolation {
                harness: definition.id.clone(),
                message: "project discovery returned a duplicate project id".to_owned(),
            });
        }
    }
    Ok(())
}

pub(super) fn normalize_project(
    definition: &HarnessDefinition,
    project: &mut HarnessProject,
) -> Result<(), HarnessError> {
    if project.harness_id != definition.id {
        return Err(HarnessError::ProtocolViolation {
            harness: definition.id.clone(),
            message: "project result belongs to a different Harness instance".to_owned(),
        });
    }
    if !valid_project_id(&project.id) {
        return Err(HarnessError::ProtocolViolation {
            harness: definition.id.clone(),
            message: "project result contains an invalid project id".to_owned(),
        });
    }
    if !valid_project_name(&project.name) {
        return Err(HarnessError::ProtocolViolation {
            harness: definition.id.clone(),
            message: "project result contains an invalid project name".to_owned(),
        });
    }
    if project.roots.is_empty() || project.roots.len() > MAX_WORKSPACE_ROOTS {
        return Err(HarnessError::ProtocolViolation {
            harness: definition.id.clone(),
            message: "project result must contain bounded absolute roots".to_owned(),
        });
    }
    if definition.adapter == REMOTE_NODE_ADAPTER {
        if project
            .roots
            .iter()
            .any(|root| !remote_workspace::valid_remote_path(root))
        {
            return Err(HarnessError::ProtocolViolation {
                harness: definition.id.clone(),
                message: "invalid remote root metadata".into(),
            });
        }
        project.roots.sort();
        project.roots.dedup();
        return Ok(());
    }
    let mut canonical_roots = Vec::with_capacity(project.roots.len());
    for root in &project.roots {
        let canonical =
            provider_path::canonicalize_provider_directory(root, &definition.workspace_roots)
                .map_err(|error| HarnessError::ProtocolViolation {
                    harness: definition.id.clone(),
                    message: format!(
                        "project result contains an invalid or disallowed root: {error}"
                    ),
                })?;
        canonical_roots.push(canonical);
    }
    canonical_roots.sort();
    canonical_roots.dedup();
    project.roots = canonical_roots;
    Ok(())
}

pub(super) fn validate_client_project_grouping(
    definition: &HarnessDefinition,
    visibility: ConversationVisibility,
    grouping: &ClientProjectGrouping,
) -> Result<(), HarnessError> {
    let valid = match (visibility, grouping) {
        (ConversationVisibility::Hidden, ClientProjectGrouping::NotApplicable) => true,
        (
            ConversationVisibility::Client,
            ClientProjectGrouping::Unverified
            | ClientProjectGrouping::Confirmed {
                client_project_id: None,
            },
        ) => true,
        (
            ConversationVisibility::Client,
            ClientProjectGrouping::Confirmed {
                client_project_id: Some(id),
            },
        ) => valid_project_id(id),
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err(HarnessError::ProtocolViolation {
            harness: definition.id.clone(),
            message: "provider returned an invalid client project grouping state".to_owned(),
        })
    }
}

pub(super) fn valid_project_id(value: &str) -> bool {
    !value.trim().is_empty()
        && value.len() <= MAX_PROJECT_ID_BYTES
        && !value.chars().any(char::is_control)
}

pub(super) fn valid_project_name(value: &str) -> bool {
    !value.trim().is_empty()
        && value.len() <= MAX_PROJECT_NAME_BYTES
        && !value.chars().any(char::is_control)
}
