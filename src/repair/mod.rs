//! A bounded host-tool workflow with delegated, durable action approval.
use crate::actions::{ActionError, ScopedFiles, TextEdit};
use crate::harnesses::{
    ConversationVisibility, HarnessCancellation, HarnessError, HarnessRegistry, HarnessRole,
    HarnessRunRequest, HarnessTool, HarnessToolCall, HarnessToolFuture, HarnessToolHandler,
    HarnessToolResult,
};
use crate::persistence::approval::{
    ApprovalDecision, ApprovalError, ApprovalPolicy, ApprovalRecord, ApprovalState, ApprovalStore,
    ApprovalStoreConfig, ExecutionOutcome, ModelAssessment, ProposedOperation, ReviewerConfig,
    ReviewerIdentity,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use thiserror::Error;

mod contract;
mod execution;
mod review;
mod run;
mod session;
mod tools;

pub use crate::harnesses::RemoteWorkspace;
pub use contract::{
    RepairConfig, RepairExecutionContext, RepairResult, RepairStatus, WorkflowError,
};
use tools::repair_tools;

/// Trusted local service. Models only receive a restricted tool handler, never
/// this service, its policy, store access, or manual-decision methods.
pub struct RepairSession {
    config: RepairConfig,
    files: ScopedFiles,
    store: Mutex<ApprovalStore>,
    registry: Option<Arc<HarnessRegistry>>,
}

impl RepairSession {
    fn store(&self) -> Result<MutexGuard<'_, ApprovalStore>, WorkflowError> {
        self.store
            .lock()
            .map_err(|_| WorkflowError::Invalid("approval store lock poisoned".into()))
    }
}

struct RepairTools {
    session: Arc<RepairSession>,
    task_id: String,
    user_prompt: String,
    calls: AtomicUsize,
    stopped: AtomicBool,
    cancellation: HarnessCancellation,
}

fn same_root(operation: &ProposedOperation, root: &std::path::Path) -> bool {
    operation
        .action
        .get("target_root")
        .and_then(Value::as_str)
        .is_some_and(|stored| stored.eq_ignore_ascii_case(&root.to_string_lossy()))
}

fn decode_stored_edit(action: &Value) -> Result<TextEdit, WorkflowError> {
    let mut fields = action
        .as_object()
        .cloned()
        .ok_or_else(|| WorkflowError::Invalid("invalid stored action".into()))?;
    fields.remove("kind");
    fields.remove("target_root");
    fields.remove("user_request");
    fields.remove("execution_context");
    Ok(serde_json::from_value(Value::Object(fields))?)
}

pub fn now() -> Result<u64, WorkflowError> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| WorkflowError::Invalid("clock precedes epoch".into()))?
        .as_secs())
}

fn model_request(
    remote: Option<&RemoteWorkspace>,
    local: &std::path::Path,
    prompt: String,
) -> HarnessRunRequest {
    match remote {
        Some(workspace) => {
            HarnessRunRequest::remote(&workspace.node_id, &workspace.workspace_id, prompt)
        }
        None => HarnessRunRequest::new(local, prompt),
    }
}

fn check_revision(
    store: &ApprovalStore,
    id: &str,
    revision: Option<u64>,
) -> Result<(), ApprovalError> {
    let record = store.get(id).ok_or(ApprovalError::NotFound)?;
    if revision.is_some_and(|expected| expected != record.revision) {
        return Err(ApprovalError::Conflict);
    }
    Ok(())
}

#[cfg(test)]
#[path = "../../tests/workflow.rs"]
mod tests;
