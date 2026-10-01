//! Typed failures and persistent project/conversation uncertainty context.
use super::ConversationVisibility;
use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
#[error(
    "Harness {harness} project creation may have persisted (name={name}, root={root_directory:?}, idempotency_key={idempotency_key}, project_id={project_id:?}): {message}"
)]
pub struct ProjectCreationUncertainty {
    pub harness: String,
    pub name: String,
    pub root_directory: PathBuf,
    pub idempotency_key: String,
    pub project_id: Option<String>,
    pub message: String,
}

#[derive(Debug, Error)]
#[error(
    "Harness {harness} conversation may persist after an incomplete run (thread_id={thread_id:?}, cwd={project_directory:?}, visibility={visibility:?}, native_project_id={native_project_id:?}): {message}"
)]
pub struct ConversationUncertainty {
    pub harness: String,
    pub thread_id: Option<String>,
    pub project_directory: PathBuf,
    pub visibility: ConversationVisibility,
    pub native_project_id: Option<String>,
    pub message: String,
}

#[derive(Debug, Error)]
pub enum HarnessError {
    #[error("invalid Harness configuration: {0}")]
    InvalidConfiguration(String),
    #[error("duplicate Harness id: {0}")]
    DuplicateHarness(String),
    #[error("duplicate Harness adapter registration: {0}")]
    DuplicateAdapter(String),
    #[error("Harness adapter is not installed: {0}")]
    UnsupportedAdapter(String),
    #[error("Harness address is not supported by adapter {adapter}: {address}")]
    UnsupportedAddress { adapter: String, address: String },
    #[error("unknown Harness: {0}")]
    UnknownHarness(String),
    #[error("Harness is disabled: {0}")]
    HarnessDisabled(String),
    #[error("no default Harness is configured")]
    NoDefaultHarness,
    #[error("project directory is outside the configured workspace roots: {0}")]
    WorkspaceDenied(PathBuf),
    #[error("invalid Harness request: {0}")]
    InvalidRequest(String),
    #[error("Harness {harness} is unavailable: {message}")]
    Unavailable { harness: String, message: String },
    #[error("Harness {harness} requires authentication")]
    AuthenticationRequired { harness: String },
    #[error("Harness {harness} does not support provider-native project discovery")]
    ProjectDiscoveryUnsupported { harness: String },
    #[error("Harness {harness} does not support provider-native project creation")]
    ProjectCreationUnsupported { harness: String },
    #[error("Harness {harness} does not support provider-native project placement")]
    ProjectPlacementUnsupported { harness: String },
    #[error("Harness {harness} has no provider-native project {project_id}")]
    UnknownProject { harness: String, project_id: String },
    #[error("Harness {harness} protocol violation: {message}")]
    ProtocolViolation { harness: String, message: String },
    #[error("Harness {harness} rejected a server-initiated operation: {method}")]
    ToolRequestRejected { harness: String, method: String },
    #[error("Harness {harness} turn exceeded its {seconds}s deadline")]
    DeadlineExceeded { harness: String, seconds: u64 },
    #[error("Harness {harness} output exceeded the configured limit")]
    OutputLimitExceeded { harness: String },
    #[error("Harness {harness} turn was interrupted")]
    Interrupted { harness: String },
    #[error("Harness {harness} turn failed: {message}")]
    TurnFailed { harness: String, message: String },
    #[error(transparent)]
    ProjectCreationOutcomeUnknown(Box<ProjectCreationUncertainty>),
    #[error(transparent)]
    ConversationOutcomeUnknown(Box<ConversationUncertainty>),
}
