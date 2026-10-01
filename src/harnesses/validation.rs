//! Request validation before provider dispatch and scoped local directory checks.
use super::limits::{
    MAX_IDEMPOTENCY_KEY_BYTES, MAX_MODEL_BYTES, MAX_PROJECT_ID_BYTES, MAX_PROJECT_NAME_BYTES,
    MAX_PROMPT_BYTES, MAX_TURN_DURATION,
};
use super::output_validation::{valid_project_id, valid_project_name};
use super::{
    ConversationPlacement, ConversationVisibility, HarnessCancellation, HarnessDefinition,
    HarnessError, HarnessProjectCreateRequest, HarnessProjectListRequest, HarnessRole,
    HarnessRunRequest, HarnessTool, HarnessToolHandler, REMOTE_NODE_ADAPTER, RemoteWorkspace,
};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

trait HarnessCancellationExt {
    fn ensure_active(&self, definition: &HarnessDefinition) -> Result<(), HarnessError>;
}

impl HarnessCancellationExt for HarnessCancellation {
    fn ensure_active(&self, definition: &HarnessDefinition) -> Result<(), HarnessError> {
        if self.is_cancelled() {
            return Err(HarnessError::Interrupted {
                harness: definition.id.clone(),
            });
        }
        Ok(())
    }
}

pub(super) struct ValidatedRunRequest {
    pub(super) project_directory: PathBuf,
    remote_workspace: Option<RemoteWorkspace>,
    prompt: String,
    model: Option<String>,
    timeout: Duration,
    pub(super) visibility: ConversationVisibility,
    pub(super) placement: ConversationPlacement,
    cancellation: HarnessCancellation,
    role: HarnessRole,
    tools: Vec<HarnessTool>,
    tool_handler: Option<Arc<dyn HarnessToolHandler>>,
}

impl ValidatedRunRequest {
    pub(super) fn into_request(self) -> HarnessRunRequest {
        HarnessRunRequest {
            project_directory: self.project_directory,
            remote_workspace: self.remote_workspace,
            prompt: self.prompt,
            model: self.model,
            timeout: self.timeout,
            visibility: self.visibility,
            placement: self.placement,
            cancellation: self.cancellation,
            role: self.role,
            tools: self.tools,
            tool_handler: self.tool_handler,
        }
    }
}

pub(super) fn validate_run_request(
    definition: &HarnessDefinition,
    request: HarnessRunRequest,
) -> Result<ValidatedRunRequest, HarnessError> {
    request.cancellation.ensure_active(definition)?;
    validate_run_tools(&request)?;
    if request.prompt.trim().is_empty() {
        return Err(HarnessError::InvalidRequest(
            "prompt must not be empty".to_owned(),
        ));
    }
    if request.prompt.len() > MAX_PROMPT_BYTES {
        return Err(HarnessError::InvalidRequest(format!(
            "prompt exceeds {MAX_PROMPT_BYTES} bytes"
        )));
    }
    validate_timeout(request.timeout)?;
    if let Some(model) = request.model.as_deref()
        && (model.is_empty()
            || model.len() > MAX_MODEL_BYTES
            || model.chars().any(char::is_control))
    {
        return Err(HarnessError::InvalidRequest(
            "model identifier is invalid".to_owned(),
        ));
    }
    validate_placement(definition, request.visibility, &request.placement)?;
    let project_directory = validate_directory(
        definition,
        &request.project_directory,
        request.remote_workspace.as_ref(),
        "project directory",
    )?;
    Ok(ValidatedRunRequest {
        project_directory,
        remote_workspace: request.remote_workspace,
        prompt: request.prompt,
        model: request.model,
        timeout: request.timeout,
        visibility: request.visibility,
        placement: request.placement,
        cancellation: request.cancellation,
        role: request.role,
        tools: request.tools,
        tool_handler: request.tool_handler,
    })
}

fn validate_run_tools(request: &HarnessRunRequest) -> Result<(), HarnessError> {
    if request.role == HarnessRole::Approval
        && (!request.tools.is_empty()
            || request.tool_handler.is_some()
            || request.visibility != ConversationVisibility::Hidden
            || request.placement != ConversationPlacement::NoNativeProject)
    {
        return Err(HarnessError::InvalidRequest(
            "approval runs require a fresh hidden conversation without tools or native project"
                .to_owned(),
        ));
    }
    if request.tools.len() > 32 || request.tools.is_empty() != request.tool_handler.is_none() {
        return Err(HarnessError::InvalidRequest(
            "tools require a handler and between 1 and 32 definitions".to_owned(),
        ));
    }
    let mut names = BTreeSet::new();
    let mut schema_bytes = 0_usize;
    for tool in &request.tools {
        if tool.name.is_empty()
            || tool.name.len() > 64
            || !tool
                .name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
            || !names.insert(&tool.name)
            || tool.description.trim().is_empty()
            || tool.description.len() > 4096
            || !tool.input_schema.is_object()
            || tool
                .input_schema
                .get("type")
                .and_then(serde_json::Value::as_str)
                != Some("object")
        {
            return Err(HarnessError::InvalidRequest(
                "invalid or duplicate host tool definition".to_owned(),
            ));
        }
        schema_bytes = schema_bytes.saturating_add(tool.input_schema.to_string().len());
    }
    if schema_bytes > 64 * 1024 {
        return Err(HarnessError::InvalidRequest(
            "host tool schemas exceed 64 KiB".to_owned(),
        ));
    }
    Ok(())
}

pub(super) fn validate_project_list_request(
    definition: &HarnessDefinition,
    mut request: HarnessProjectListRequest,
) -> Result<HarnessProjectListRequest, HarnessError> {
    request.cancellation.ensure_active(definition)?;
    validate_timeout(request.timeout)?;
    request.context_directory = validate_directory(
        definition,
        &request.context_directory,
        request.remote_workspace.as_ref(),
        "project discovery context directory",
    )?;
    Ok(request)
}

pub(super) fn validate_project_create_request(
    definition: &HarnessDefinition,
    mut request: HarnessProjectCreateRequest,
) -> Result<HarnessProjectCreateRequest, HarnessError> {
    request.cancellation.ensure_active(definition)?;
    validate_timeout(request.timeout)?;
    request.name = request.name.trim().to_owned();
    if !valid_project_name(&request.name) {
        return Err(HarnessError::InvalidRequest(format!(
            "project name must contain 1..={MAX_PROJECT_NAME_BYTES} bytes and no control characters"
        )));
    }
    if request.idempotency_key.trim().is_empty()
        || request.idempotency_key.len() > MAX_IDEMPOTENCY_KEY_BYTES
        || request.idempotency_key.chars().any(char::is_control)
    {
        return Err(HarnessError::InvalidRequest(format!(
            "project idempotency key must contain 1..={MAX_IDEMPOTENCY_KEY_BYTES} bytes and no control characters"
        )));
    }
    request.root_directory = validate_directory(
        definition,
        &request.root_directory,
        request.remote_workspace.as_ref(),
        "project root directory",
    )?;
    Ok(request)
}

fn validate_placement(
    definition: &HarnessDefinition,
    visibility: ConversationVisibility,
    placement: &ConversationPlacement,
) -> Result<(), HarnessError> {
    let ConversationPlacement::ExistingProject {
        harness_id,
        project_id,
    } = placement
    else {
        return Ok(());
    };
    if visibility == ConversationVisibility::Hidden {
        return Err(HarnessError::InvalidRequest(
            "a hidden conversation cannot request a provider-native project".to_owned(),
        ));
    }
    if harness_id != &definition.id {
        return Err(HarnessError::InvalidRequest(format!(
            "provider-native project belongs to Harness {harness_id}, not {}",
            definition.id
        )));
    }
    if !valid_project_id(project_id) {
        return Err(HarnessError::InvalidRequest(format!(
            "project id must contain 1..={MAX_PROJECT_ID_BYTES} bytes and no control characters"
        )));
    }
    Ok(())
}

fn validate_timeout(timeout: Duration) -> Result<(), HarnessError> {
    if timeout.is_zero() || timeout > MAX_TURN_DURATION {
        return Err(HarnessError::InvalidRequest(format!(
            "timeout must be between 1ns and {}s",
            MAX_TURN_DURATION.as_secs()
        )));
    }
    Ok(())
}

fn validate_directory(
    definition: &HarnessDefinition,
    directory: &Path,
    remote: Option<&RemoteWorkspace>,
    kind: &str,
) -> Result<PathBuf, HarnessError> {
    if definition.adapter == REMOTE_NODE_ADAPTER {
        let workspace = remote.ok_or_else(|| {
            HarnessError::InvalidRequest(
                "remote Harness requires an explicit node-owned workspace".into(),
            )
        })?;
        workspace.validate(definition)?;
        Ok(workspace.resource_path())
    } else if remote.is_some() {
        Err(HarnessError::InvalidRequest(
            "local Harness does not accept remote workspaces".into(),
        ))
    } else {
        canonicalize_allowed_directory(definition, directory, kind)
    }
}

fn canonicalize_allowed_directory(
    definition: &HarnessDefinition,
    directory: &Path,
    kind: &str,
) -> Result<PathBuf, HarnessError> {
    let canonical = fs::canonicalize(directory)
        .map_err(|error| HarnessError::InvalidRequest(format!("cannot resolve {kind}: {error}")))?;
    if !canonical.is_dir() {
        return Err(HarnessError::InvalidRequest(format!(
            "{kind} must be an existing directory"
        )));
    }
    if !definition
        .workspace_roots
        .iter()
        .any(|root| canonical.starts_with(root))
    {
        return Err(HarnessError::WorkspaceDenied(canonical));
    }
    Ok(canonical)
}
