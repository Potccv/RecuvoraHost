//! Host-owned wire representation and validation. External implementations follow the documented JSON contract; they do not depend on this crate.

mod schema;
mod wire;

pub use schema::{validate_schema, validate_value};
pub use wire::{
    ContractDeclaration, ExtensionError, ExtensionKind, ExtensionMetadata, MAX_FRAME_BYTES,
    Message, MethodDeclaration, Outcome, PROTOCOL_VERSION, call_id, valid_id,
};
