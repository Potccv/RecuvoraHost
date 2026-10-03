//! Versioned, deterministic commitment to configuration and ordered transitions.
use serde::Serialize;
use sha2::{Digest, Sha256};

pub(crate) fn digest(value: &impl Serialize) -> String {
    // Private callers provide serializable domain data with string-keyed maps.
    // Sort recursively even when a caller enables serde_json's preserve_order.
    let mut value = serde_json::to_value(value).expect("serializable domain binding");
    value.sort_all_objects();
    let mut hash = Sha256::new();
    hash.update(b"recuvora-chain-v1:");
    hash.update(serde_json::to_vec(&value).expect("JSON domain binding"));
    format!("{:x}", hash.finalize())
}
