//! Public bounded text action contracts.
use serde::{Deserialize, Serialize};
use std::io;
use thiserror::Error;

pub const MAX_FILE_BYTES: usize = 16 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TextEdit {
    pub path: String,
    pub expected: String,
    pub replacement: String,
}

#[derive(Debug, Error)]
pub enum ActionError {
    #[error("action denied: {0}")]
    Denied(String),
    #[error("target changed; obtain a new approval")]
    Changed,
    #[error("action preparation failed: {0}")]
    Io(#[from] io::Error),
    #[error("write may have occurred; verify target before any retry: {0}")]
    Unknown(String),
}

#[derive(Clone, Debug, Serialize)]
pub struct ActionReceipt {
    pub path: String,
    pub bytes_written: usize,
    pub content_verified: bool,
}
