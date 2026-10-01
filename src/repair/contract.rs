//! Trusted repair configuration and typed workflow outcomes.
use super::*;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RepairConfig {
    pub schema_version: u32,
    pub harness_config: PathBuf,
    #[serde(default)]
    pub extensions_config: Option<PathBuf>,
    #[serde(default)]
    pub execution_workspace: Option<RemoteWorkspace>,
    #[serde(default)]
    pub reviewer_workspace: Option<RemoteWorkspace>,
    pub execution_harness: String,
    pub target_id: String,
    pub target_root: PathBuf,
    pub reviewer_directory: PathBuf,
    pub data_dir: PathBuf,
    pub allowed_files: Vec<String>,
    pub policy: ApprovalPolicy,
    pub timeout_secs: u64,
    pub max_tool_calls: usize,
}

impl RepairConfig {
    pub fn validate(&self) -> Result<(), WorkflowError> {
        self.policy.validate().map_err(ApprovalError::from)?;
        for workspace in [&self.execution_workspace, &self.reviewer_workspace]
            .into_iter()
            .flatten()
        {
            if !crate::protocol::valid_id(&workspace.node_id)
                || !crate::protocol::valid_id(&workspace.workspace_id)
            {
                return Err(WorkflowError::Invalid(
                    "invalid node or model workspace identifier".into(),
                ));
            }
        }
        if self.schema_version != 1
            || !(1..=1800).contains(&self.timeout_secs)
            || !(1..=64).contains(&self.max_tool_calls)
            || self.target_id.is_empty()
            || self.target_id.len() > 128
            || self.target_id.chars().any(char::is_control)
            || self.execution_harness.is_empty()
            || !self.policy.allowed_targets.contains(&self.target_id)
            || !self
                .policy
                .allowed_action_kinds
                .iter()
                .any(|s| s == "replace_text")
        {
            return Err(WorkflowError::Invalid(
                "invalid repair configuration or delegated target".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum WorkflowError {
    #[error("{0}")]
    Invalid(String),
    #[error("operation {request_id} may have executed: {message}")]
    OutcomeUnknown { request_id: String, message: String },
    #[error(transparent)]
    Approval(#[from] ApprovalError),
    #[error(transparent)]
    Action(#[from] ActionError),
    #[error(transparent)]
    Harness(#[from] HarnessError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

/// The bounded workflow outcome, independent of CLI or HTTP presentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepairStatus {
    Unknown,
    WaitingHuman,
    Blocked,
    Canceled,
    Failed,
    Completed,
}

#[derive(Debug)]
pub struct RepairExecutionContext {
    pub harness_id: String,
    pub thread_id: String,
    pub session_id: String,
}

/// File-workflow facts. A completed turn does not establish business health.
#[derive(Debug)]
pub struct RepairResult {
    pub status: RepairStatus,
    pub task_id: String,
    pub final_response: Option<String>,
    pub harness_error: Option<HarnessError>,
    pub operations: Vec<ApprovalRecord>,
    pub execution_context: Option<RepairExecutionContext>,
}
