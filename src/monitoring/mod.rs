//! Read-only observation polling and evidence-based monitoring. No repair dispatch.

mod config;
mod discovery;
mod engine;
mod handle;
mod observation;
mod scheduler;
mod shared;
mod snapshot;
mod state;
mod support;

pub use config::{MonitorDefinition, MonitorsConfig};
pub use discovery::{
    DiscoveryBatch, DiscoveryFuture, DiscoverySnapshot, DiscoveryTarget, MonitorDiscovery,
};
pub use engine::MonitorEngine;
pub use handle::{MonitorHandle, MonitorIncidentLease};
pub use observation::{
    BatchCoverage, ErrorLogBatch, NodeErrorLog, ObservationBatch, ObservationFuture,
    ObservationRequest, ObservationSample, ObservationSource,
};
pub use snapshot::{Coverage, Freshness, MonitorSnapshot, MonitoringSnapshot};

use crate::persistence::incidents::IncidentError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum MonitorError {
    #[error("invalid monitoring configuration: {0}")]
    Configuration(String),
    #[error("invalid observation: {0}")]
    Observation(String),
    #[error("monitoring runtime: {0}")]
    Runtime(String),
    #[error("monitoring is stopped")]
    Stopped,
    #[error(transparent)]
    Incident(#[from] IncidentError),
}
