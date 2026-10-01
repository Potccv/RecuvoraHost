//! Reject malformed, conflicting or oversized batches before state transitions.
use super::super::support::{MAX_BATCH, MAX_SAMPLES};
use super::super::{MonitorError, ObservationBatch, ObservationSample};
use super::MonitorState;
use serde_json::Value;
use std::collections::BTreeMap;

impl MonitorState {
    pub(super) fn validate_batch(&self, batch: &ObservationBatch) -> Result<(), MonitorError> {
        let invalid = |reason: &str| MonitorError::Observation(reason.into());
        if batch
            .samples
            .iter()
            .any(|sample| !bounded_depth(&sample.value, 24) || !bounded_depth(&sample.evidence, 24))
        {
            return Err(invalid("sample value/evidence nesting exceeds 24 levels"));
        }
        if serde_json::to_vec(batch).map_or(true, |bytes| bytes.len() > MAX_BATCH)
            || batch.samples.len() > MAX_SAMPLES
            || batch.schema_version != 1
            || batch.target_id != self.config.target_id
            || batch.source_id != self.config.source_id
            || batch.cursor != self.checkpoint.cursor
            || !crate::protocol::valid_id(&batch.generation)
            || batch.next_cursor.is_empty()
            || batch.next_cursor.len() > 4096
            || batch
                .error
                .as_ref()
                .is_some_and(|error| error.is_empty() || error.len() > 2048)
        {
            return Err(invalid(
                "batch identity, cursor, generation or limits invalid",
            ));
        }
        let mut identities = BTreeMap::<&str, &ObservationSample>::new();
        for sample in &batch.samples {
            if let Some(previous) = identities.insert(&sample.id, sample)
                && (previous.sequence != sample.sequence
                    || previous.value != sample.value
                    || previous.evidence != sample.evidence)
            {
                return Err(invalid(
                    "sample identity is reused with contradictory content",
                ));
            }
            if self.checkpoint.generation.as_ref() == Some(&batch.generation)
                && self
                    .checkpoint
                    .last_sequence
                    .is_some_and(|last| sample.sequence > last)
                && self.checkpoint.recent_ids.contains(&sample.id)
            {
                return Err(invalid("sample identity is reused for a newer sequence"));
            }
            if !crate::protocol::valid_id(&sample.id)
                || sample.sequence == 0
                || !sample.evidence.is_object()
                || sample.sequence > 9_007_199_254_740_992
                || sample.age_ms > 9_007_199_254_740_992
                || serde_json::to_vec(&sample.evidence).map_or(true, |v| v.len() > 4096)
                || serde_json::to_vec(&sample.value).map_or(true, |v| v.len() > 4096)
            {
                return Err(invalid(
                    "sample identity, sequence or evidence/value limit invalid",
                ));
            }
        }
        for pair in batch.samples.windows(2) {
            if pair[1].sequence < pair[0].sequence
                || (pair[1].sequence == pair[0].sequence
                    && (pair[1].id != pair[0].id
                        || pair[1].value != pair[0].value
                        || pair[1].evidence != pair[0].evidence))
            {
                return Err(invalid(
                    "batch samples are out of order or contradict a sequence identity",
                ));
            }
        }
        let evidence_bytes: usize = batch
            .samples
            .iter()
            .map(|sample| {
                serde_json::to_vec(&sample.value).map_or(MAX_BATCH, |bytes| bytes.len())
                    + serde_json::to_vec(&sample.evidence).map_or(MAX_BATCH, |bytes| bytes.len())
            })
            .sum();
        if evidence_bytes > 128 * 1024 {
            return Err(invalid("batch evidence exceeds 128 KiB"));
        }
        Ok(())
    }
}

fn bounded_depth(value: &Value, maximum: usize) -> bool {
    let mut pending = vec![(value, 0usize)];
    while let Some((value, depth)) = pending.pop() {
        if depth > maximum {
            return false;
        }
        match value {
            Value::Object(object) => {
                pending.extend(object.values().map(|child| (child, depth + 1)))
            }
            Value::Array(array) => pending.extend(array.iter().map(|child| (child, depth + 1))),
            _ => {}
        }
    }
    true
}
