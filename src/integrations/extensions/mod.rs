//! Network extension runtime owned by the application host.

mod callbacks;
mod client;
mod config;
mod endpoint;
mod network;
mod node_settings;
mod protocol_settings;
mod registry;
mod routing;
mod transport;
mod ui_view;
mod validation;

pub use crate::protocol::{
    ContractDeclaration, ExtensionError, ExtensionKind, ExtensionMetadata, MAX_FRAME_BYTES,
    Message, MethodDeclaration, Outcome, PROTOCOL_VERSION, call_id, valid_id, validate_schema,
    validate_value,
};
pub use client::{CallbackFuture, CallbackHandler, ExtensionCall, ExtensionClient};
pub use config::{AllowedMethod, ExtensionDefinition, ExtensionsConfig};
pub use endpoint::NetworkEndpoint;
pub use node_settings::NodeSettings;
pub use protocol_settings::ProtocolSettings;
pub use registry::{ExtensionRegistry, ExtensionStatus};
pub use ui_view::{MONITORING_VIEW_CAPABILITY, MONITORING_VIEW_METHOD, MonitoringViewRegistration};
