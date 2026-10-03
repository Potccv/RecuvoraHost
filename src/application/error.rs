//! Application failures, independent of transport status codes.
use crate::persistence::approval::ApprovalError;
use crate::recovery::RecoveryError;
use crate::repair::WorkflowError;

#[derive(Clone, Copy, Debug)]
pub enum ApplicationErrorKind {
    Invalid,
    Unavailable,
    Conflict,
    NotFound,
    Unknown,
    Capacity,
}

#[derive(Debug)]
pub struct ApplicationError {
    pub kind: ApplicationErrorKind,
    pub message: String,
}
impl ApplicationError {
    pub(crate) fn new(kind: ApplicationErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self::new(ApplicationErrorKind::Invalid, message)
    }
    pub(crate) fn unavailable(message: impl Into<String>) -> Self {
        Self::new(ApplicationErrorKind::Unavailable, message)
    }
    pub(crate) fn conflict(message: impl Into<String>) -> Self {
        Self::new(ApplicationErrorKind::Conflict, message)
    }
}
impl std::fmt::Display for ApplicationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}
impl std::error::Error for ApplicationError {}
impl From<std::io::Error> for ApplicationError {
    fn from(error: std::io::Error) -> Self {
        Self::unavailable(error.to_string())
    }
}
impl From<serde_json::Error> for ApplicationError {
    fn from(error: serde_json::Error) -> Self {
        Self::invalid(error.to_string())
    }
}
impl From<WorkflowError> for ApplicationError {
    fn from(e: WorkflowError) -> Self {
        match &e {
            WorkflowError::Approval(
                ApprovalError::Conflict
                | ApprovalError::InvalidState(_)
                | ApprovalError::Expired
                | ApprovalError::TargetBusy,
            ) => Self::conflict(e.to_string()),
            WorkflowError::Approval(ApprovalError::NotFound) => {
                Self::new(ApplicationErrorKind::NotFound, e.to_string())
            }
            WorkflowError::OutcomeUnknown { .. } => {
                Self::new(ApplicationErrorKind::Unknown, e.to_string())
            }
            _ => Self::invalid(e.to_string()),
        }
    }
}

impl From<RecoveryError> for ApplicationError {
    fn from(error: RecoveryError) -> Self {
        match &error {
            RecoveryError::Busy
            | RecoveryError::Conflict
            | RecoveryError::Approval(
                ApprovalError::Conflict
                | ApprovalError::InvalidState(_)
                | ApprovalError::Expired
                | ApprovalError::TargetBusy
                | ApprovalError::ReviewNotDue
                | ApprovalError::ReviewTimedOut,
            ) => Self::conflict(error.to_string()),
            RecoveryError::Approval(ApprovalError::NotFound) => {
                Self::new(ApplicationErrorKind::NotFound, error.to_string())
            }
            RecoveryError::Invalid(_)
            | RecoveryError::Json(_)
            | RecoveryError::Approval(ApprovalError::Invalid(_) | ApprovalError::OutOfScope) => {
                Self::invalid(error.to_string())
            }
            _ => Self::unavailable(error.to_string()),
        }
    }
}
