//! Configuration-authorized inventory polling and durable target enrollment.
mod config;
mod inventory;
mod polling;

pub(super) use config::validate;
pub use config::{
    DiscoveryBatch, DiscoveryFuture, DiscoverySnapshot, DiscoveryTarget, MonitorDiscovery,
};
pub(super) use inventory::DiscoveryState;
pub(super) use polling::launch;
