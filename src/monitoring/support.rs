//! Bounded diagnostic text, shared protocol limits and clock conversion.
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub(super) const MAX_BATCH: usize = 256 * 1024;
pub(super) const MAX_SAMPLES: usize = 32;
pub(super) const DISCOVERY_INVALIDATED: &str =
    "discovered target absent or presence changed; fresh observation required";

pub(super) fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}
pub(super) fn now_ms() -> u64 {
    duration_ms(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default(),
    )
}
pub(super) fn bounded_text(value: &str) -> String {
    value.chars().take(500).collect()
}
