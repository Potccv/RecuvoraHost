//! Host-owned authorization and durable recovery transitions.
//! Storage and runtime adapters confirm proposals before releasing effects.
mod binding;
mod collections;
mod identity;
pub mod operation;
pub mod recovery;
