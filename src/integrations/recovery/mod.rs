//! Node and Harness capability adapters for the Host recovery application.
mod executor;
mod harness_repair;
mod node_backend;
mod summary_context;

pub use executor::ScriptExecutorConfig;
pub use node_backend::NodeRepairBackend;

// Preserve existing public imports while application services live in recovery.
pub use crate::recovery::*;

fn service(error: impl std::fmt::Display) -> RecoveryError {
    RecoveryError::Service(error.to_string())
}
