//! Read-only observation contracts implemented by embedding applications.
use super::{DiscoveryFuture, MonitorError};
use recuvora_core::operation::Cancellation;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservationBatch {
    pub schema_version: u32,
    pub target_id: String,
    pub source_id: String,
    pub generation: String,
    /// Echo of the requested cursor. None on the first request.
    pub cursor: Option<String>,
    pub next_cursor: String,
    pub coverage: BatchCoverage,
    pub has_more: bool,
    pub error: Option<String>,
    pub samples: Vec<ObservationSample>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BatchCoverage {
    Complete,
    Partial,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservationSample {
    pub id: String,
    pub sequence: u64,
    /// Age measured by the source when it assembles the reply, not a wall clock timestamp.
    pub age_ms: u64,
    pub value: Value,
    pub evidence: Value,
}

#[derive(Clone, Debug)]
pub struct ObservationRequest {
    pub extension_id: String,
    pub contract: String,
    pub version: u32,
    pub method: String,
    pub params: Value,
    pub timeout: Duration,
}

pub type ObservationFuture<'a> =
    Pin<Box<dyn Future<Output = Result<ObservationBatch, MonitorError>> + Send + 'a>>;

pub trait ObservationSource: Send + Sync {
    /// Discovery-backed sources change this epoch whenever presence changes.
    /// None revokes current observation validity without discarding its cursor.
    fn observation_epoch(&self) -> Option<u64> {
        Some(0)
    }
    /// Restored discovery members await a first inventory without bypassing
    /// startup grace. Pending sources must not dispatch a real observation.
    fn observation_pending(&self) -> bool {
        false
    }
    /// Must observe cancellation and remain bounded; the engine waits for this future before stopping.
    fn poll(
        &self,
        request: ObservationRequest,
        cancellation: Cancellation,
    ) -> ObservationFuture<'_>;

    fn discover(
        &self,
        _request: ObservationRequest,
        _cancellation: Cancellation,
    ) -> DiscoveryFuture<'_> {
        Box::pin(async {
            Err(MonitorError::Observation(
                "source does not support discovery".into(),
            ))
        })
    }
}
