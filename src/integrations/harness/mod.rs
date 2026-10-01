//! Remote Harness adapter backed by the Host extension registry.

mod callbacks;
mod remote;
mod wire;

pub use remote::{REMOTE_NODE_ADAPTER, RemoteHarnessFactory};
