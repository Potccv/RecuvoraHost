//! Trusted persistence for Core domain proposals and Host incident facts.
pub mod approval;
pub mod incidents;
pub mod journal;
pub mod knowledge;
mod paths;

#[cfg(test)]
#[path = "../../tests/persistence_faults.rs"]
mod faults;
