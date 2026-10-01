//! Stable multi-Harness contracts and provider selection.
//!
//! Configuration selects trusted compiled adapters; provider-specific code is external.
mod config;
mod conversation;
mod error;
mod limits;
mod output_validation;
mod projects;
mod provider;
mod provider_path;
mod registry;
mod remote_workspace;
mod tools;
mod validation;

pub use crate::runtime::operation::Cancellation as HarnessCancellation;
pub use config::{CONFIG_SCHEMA_VERSION, HarnessDefinition, HarnessRegistryConfig};
pub use conversation::{
    ClientProjectGrouping, ConversationPlacement, ConversationVisibility, HarnessRole,
    HarnessRunRequest, HarnessRunResult,
};
pub use error::{ConversationUncertainty, HarnessError, ProjectCreationUncertainty};
pub use projects::{HarnessProject, HarnessProjectCreateRequest, HarnessProjectListRequest};
pub use provider::{
    HarnessAdapterFactory, HarnessProjectCreateFuture, HarnessProjectListFuture, HarnessProvider,
    HarnessRunFuture,
};
pub use registry::{HarnessRegistry, HarnessRegistryBuilder};
pub use remote_workspace::RemoteWorkspace;
pub use tools::{
    HarnessTool, HarnessToolCall, HarnessToolFuture, HarnessToolHandler, HarnessToolResult,
};

pub const REMOTE_NODE_ADAPTER: &str = "remote-node";
