//! Shared bounds for trusted configuration, domain requests and provider results.
use std::time::Duration;

pub(super) const MAX_CONFIG_BYTES: u64 = 64 * 1024;
pub(super) const MAX_HARNESSES: usize = 64;
pub(super) const MAX_WORKSPACE_ROOTS: usize = 16;
pub(super) const MAX_IDENTIFIER_BYTES: usize = 64;
pub(super) const MAX_ADDRESS_BYTES: usize = 2 * 1024;
pub(super) const MAX_MODEL_BYTES: usize = 128;
pub(super) const MAX_PROMPT_BYTES: usize = 64 * 1024;
pub(super) const MAX_PROJECT_ID_BYTES: usize = 512;
pub(super) const MAX_PROJECT_NAME_BYTES: usize = 256;
pub(super) const MAX_IDEMPOTENCY_KEY_BYTES: usize = 512;
pub(super) const MAX_DISCOVERED_PROJECTS: usize = 512;
pub(super) const MAX_TURN_DURATION: Duration = Duration::from_secs(30 * 60);
pub(super) const DEFAULT_TURN_DURATION: Duration = Duration::from_secs(3 * 60);
