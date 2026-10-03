//! Pure, shared CLI and HTTP projections of Host control facts.
use crate::harnesses::{
    ClientProjectGrouping, ConversationVisibility, HarnessError, HarnessProject,
};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
pub(crate) fn project_json(project: &HarnessProject) -> Value {
    json!({ "harness_id": project.harness_id, "id": project.id, "name": project.name, "roots": paths_json(&project.roots) })
}

pub(crate) fn path_json(path: &Path) -> Value {
    // JSON strings require Unicode; retain exact OS paths in all requests.
    json!(path.to_string_lossy())
}

pub(crate) fn paths_json(paths: &[PathBuf]) -> Vec<Value> {
    paths.iter().map(|path| path_json(path)).collect()
}

pub(crate) fn visibility_name(visibility: ConversationVisibility) -> &'static str {
    match visibility {
        ConversationVisibility::Hidden => "hidden",
        ConversationVisibility::Client => "client",
    }
}

pub(crate) fn grouping_json(grouping: &ClientProjectGrouping) -> Value {
    match grouping {
        ClientProjectGrouping::NotApplicable => json!({ "status": "not_applicable" }),
        ClientProjectGrouping::Unverified => json!({ "status": "unverified" }),
        ClientProjectGrouping::Confirmed { client_project_id } => {
            json!({ "status": "confirmed", "client_project_id": client_project_id })
        }
    }
}

pub(crate) fn error_json(error: &HarnessError) -> Value {
    let category = match error {
        HarnessError::InvalidConfiguration(_)
        | HarnessError::DuplicateHarness(_)
        | HarnessError::DuplicateAdapter(_) => "configuration",
        HarnessError::UnsupportedAdapter(_) | HarnessError::UnsupportedAddress { .. } => {
            "unsupported_adapter"
        }
        HarnessError::UnknownHarness(_)
        | HarnessError::HarnessDisabled(_)
        | HarnessError::NoDefaultHarness => "selection",
        HarnessError::WorkspaceDenied(_) => "workspace_denied",
        HarnessError::InvalidRequest(_) => "invalid_request",
        HarnessError::Unavailable { .. } => "unavailable",
        HarnessError::AuthenticationRequired { .. } => "authentication_required",
        HarnessError::ProjectDiscoveryUnsupported { .. } => "project_discovery_unsupported",
        HarnessError::ProjectCreationUnsupported { .. } => "project_creation_unsupported",
        HarnessError::ProjectPlacementUnsupported { .. } => "project_placement_unsupported",
        HarnessError::UnknownProject { .. } => "unknown_project",
        HarnessError::ProtocolViolation { .. } => "protocol_violation",
        HarnessError::ToolRequestRejected { .. } => "tool_request_rejected",
        HarnessError::DeadlineExceeded { .. } => "deadline_exceeded",
        HarnessError::OutputLimitExceeded { .. } => "output_limit_exceeded",
        HarnessError::Interrupted { .. } => "interrupted",
        HarnessError::TurnFailed { .. } => "turn_failed",
        HarnessError::ProjectCreationOutcomeUnknown(_) => "project_creation_outcome_unknown",
        HarnessError::ConversationOutcomeUnknown(_) => "conversation_outcome_unknown",
    };
    let mut result = json!({ "status": if matches!(error, HarnessError::Interrupted { .. }) { "canceled" } else { "failed" }, "code": category, "category": category, "message": error.to_string(), "auto_retry": false });
    match error {
        HarnessError::ProjectCreationOutcomeUnknown(unknown) => {
            result["status"] = json!("unknown");
            result["harness_id"] = json!(unknown.harness);
            result["name"] = json!(unknown.name);
            result["cwd"] = path_json(&unknown.root_directory);
            result["idempotency_key"] = json!(unknown.idempotency_key);
            result["native_project_id"] = json!(unknown.project_id);
            result["reason"] = json!(unknown.message);
            result["client_project_grouping"] = json!({ "status": "unverified" });
        }
        HarnessError::ConversationOutcomeUnknown(unknown) => {
            result["status"] = json!("unknown");
            result["harness_id"] = json!(unknown.harness);
            result["thread_id"] = json!(unknown.thread_id);
            result["cwd"] = path_json(&unknown.project_directory);
            result["visibility"] = json!(visibility_name(unknown.visibility));
            result["native_project_id"] = json!(unknown.native_project_id);
            result["reason"] = json!(unknown.message);
            result["client_project_grouping"] =
                grouping_json(if unknown.visibility == ConversationVisibility::Hidden {
                    &ClientProjectGrouping::NotApplicable
                } else {
                    &ClientProjectGrouping::Unverified
                });
        }
        HarnessError::WorkspaceDenied(path) => {
            result["cwd"] = path_json(path);
        }
        HarnessError::UnknownHarness(harness)
        | HarnessError::HarnessDisabled(harness)
        | HarnessError::Unavailable { harness, .. }
        | HarnessError::AuthenticationRequired { harness }
        | HarnessError::ProjectDiscoveryUnsupported { harness }
        | HarnessError::ProjectCreationUnsupported { harness }
        | HarnessError::ProjectPlacementUnsupported { harness }
        | HarnessError::ProtocolViolation { harness, .. }
        | HarnessError::ToolRequestRejected { harness, .. }
        | HarnessError::OutputLimitExceeded { harness }
        | HarnessError::Interrupted { harness }
        | HarnessError::TurnFailed { harness, .. } => {
            result["harness_id"] = json!(harness);
        }
        HarnessError::UnknownProject {
            harness,
            project_id,
        } => {
            result["harness_id"] = json!(harness);
            result["native_project_id"] = json!(project_id);
        }
        HarnessError::DeadlineExceeded { harness, seconds } => {
            result["harness_id"] = json!(harness);
            result["timeout_seconds"] = json!(seconds);
        }
        _ => {}
    }
    result
}

/// The shared CLI/HTTP representation of typed workflow facts.
pub(crate) fn repair_result_json(result: &crate::repair::RepairResult) -> serde_json::Value {
    use crate::repair::RepairStatus;
    use serde_json::json;
    let status = match result.status {
        RepairStatus::Unknown => "unknown",
        RepairStatus::WaitingHuman => "waiting_human",
        RepairStatus::Blocked => "blocked",
        RepairStatus::Canceled => "canceled",
        RepairStatus::Failed => "failed",
        RepairStatus::Completed => "completed",
    };
    let context = result.execution_context.as_ref().map(|context| {
        json!({"harness_id":context.harness_id,"thread_id":context.thread_id,"session_id":context.session_id})
    });
    json!({
        "status":status,"task_id":result.task_id,"final_response":result.final_response,
        "harness_error":result.harness_error.as_ref().map(error_json),
        "operations":result.operations,"execution_context":context,
        "business_verified":false,"auto_retry":false
    })
}
