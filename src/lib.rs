//! Application startup, CLI adaptation and authenticated HTTP APIs over Recuvora Core.
pub mod actions;
pub mod boot;
mod cli;
pub mod configuration;
pub mod harnesses;
pub mod integrations;
pub mod monitoring;
mod presentation;
pub mod protocol;
pub mod repair;
pub mod runtime;
pub mod server;
pub mod simulation;

#[cfg(test)]
#[path = "../tests/workflow_support.rs"]
pub(crate) mod workflow_test_support;
