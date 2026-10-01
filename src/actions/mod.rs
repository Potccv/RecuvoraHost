//! Bounded local text actions. Mutation is private to the trusted host.
mod contract;
mod files;
mod path_policy;
mod platform;

pub use contract::{ActionError, ActionReceipt, MAX_FILE_BYTES, TextEdit};
pub use files::ScopedFiles;

#[cfg(test)]
#[path = "../../tests/actions.rs"]
mod tests;
