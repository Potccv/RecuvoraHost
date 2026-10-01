//! Host implementation of the documented extension protocol v1.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;

pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionKind {
    Node,
    Plugin,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MethodDeclaration {
    pub name: String,
    pub read_only: bool,
    pub input_schema: Value,
    pub output_schema: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ContractDeclaration {
    pub id: String,
    pub version: u32,
    pub methods: Vec<MethodDeclaration>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ExtensionMetadata {
    pub protocol_version: u32,
    pub id: String,
    pub kind: ExtensionKind,
    pub contracts: Vec<ContractDeclaration>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub workspaces: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Message {
    Hello {
        protocol_version: u32,
        expected_id: String,
        kind: ExtensionKind,
    },
    Ready {
        #[serde(flatten)]
        metadata: ExtensionMetadata,
    },
    Call {
        id: String,
        contract: String,
        version: u32,
        method: String,
        params: Value,
        timeout_ms: u64,
    },
    Result {
        id: String,
        result: Value,
    },
    Error {
        id: String,
        code: String,
        message: String,
        outcome: Outcome,
    },
    Callback {
        id: String,
        parent_id: String,
        method: String,
        params: Value,
    },
    Cancel {
        id: String,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Rejected,
    Unknown,
    Cancelled,
}

#[derive(Debug, Error)]
pub enum ExtensionError {
    #[error("invalid extension configuration: {0}")]
    Configuration(String),
    #[error("extension unavailable: {0}")]
    Unavailable(String),
    #[error("extension protocol violation: {0}")]
    Protocol(String),
    #[error("extension rejected request: {0}")]
    Rejected(String),
    #[error("extension call {call_id} outcome is unknown: {message}")]
    Unknown { call_id: String, message: String },
    #[error("extension call cancelled before dispatch")]
    Cancelled,
}

pub fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

pub fn call_id() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!(
        "{}-{now}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}
