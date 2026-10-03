//! Trusted persistence for Host control aggregates.
pub mod approval;
pub mod incidents;
pub mod journal;
pub mod knowledge;
mod paths;

#[cfg(test)]
#[path = "../../tests/persistence_faults.rs"]
mod faults;
