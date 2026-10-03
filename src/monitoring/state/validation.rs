//! Reject malformed or conflicting Node error batches before any cursor advance.
use super::super::support::{MAX_BATCH, MAX_SAMPLES};
use super::super::{MonitorError, NodeErrorLog, ObservationBatch};
use super::MonitorState;
use serde_json::Value;
use std::collections::BTreeMap;

pub(super) fn same_content(left: &NodeErrorLog, right: &NodeErrorLog) -> bool {
    left.id == right.id
        && left.sequence == right.sequence
        && left.fingerprint == right.fingerprint
        && left.message == right.message
        && left.evidence == right.evidence
}

pub(super) fn validate_log(log: &NodeErrorLog) -> Result<(), MonitorError> {
    if !crate::protocol::valid_id(&log.id)
        || !crate::protocol::valid_id(&log.fingerprint)
        || log.fingerprint.len() > 128
        || log.sequence == 0
        || log.sequence > 9_007_199_254_740_992
        || log.age_ms > 9_007_199_254_740_992
        || log.message.trim().is_empty()
        || log.message.len() > 8192
        || log.message.contains('\0')
        || !log.evidence.is_object()
        || !bounded_depth(&log.evidence, 24)
        || serde_json::to_vec(&log.evidence).map_or(true, |bytes| bytes.len() > 4096)
    {
        return Err(MonitorError::Observation(
            "error log identity, message or evidence limits invalid".into(),
        ));
    }
    Ok(())
}

impl MonitorState {
    pub(super) fn validate_batch(&self, batch: &ObservationBatch) -> Result<(), MonitorError> {
        let invalid = |reason: &str| MonitorError::Observation(reason.into());
        for log in &batch.errors {
            validate_log(log)?;
        }
        if serde_json::to_vec(batch).map_or(true, |bytes| bytes.len() > MAX_BATCH)
            || batch.errors.len() > MAX_SAMPLES
            || batch.schema_version != 2
            || batch.target_id != self.config.target_id
            || batch.source_id != self.config.source_id
            || batch.cursor != self.checkpoint.cursor
            || !crate::protocol::valid_id(&batch.generation)
            || batch.next_cursor.is_empty()
            || batch.next_cursor.len() > 4096
            || batch
                .source_error
                .as_ref()
                .is_some_and(|error| error.is_empty() || error.len() > 2048 || error.contains('\0'))
        {
            return Err(invalid(
                "batch identity, cursor, generation or limits invalid",
            ));
        }
        let same_generation = self.checkpoint.generation.as_ref() == Some(&batch.generation);
        if !same_generation
            && (self
                .checkpoint
                .retired_generations
                .contains(&batch.generation)
                || self
                    .received
                    .keys()
                    .any(|(generation, _)| generation == &batch.generation))
        {
            return Err(invalid("retired source generation replayed"));
        }
        if !same_generation
            && self.checkpoint.generation.is_some()
            && self.checkpoint.retired_generations.len() >= 16
        {
            return Err(invalid(
                "source generation retirement capacity exhausted; enroll a new monitor identity",
            ));
        }
        let mut identities = BTreeMap::<&str, &NodeErrorLog>::new();
        for log in &batch.errors {
            if let Some(previous) = identities.insert(&log.id, log)
                && !same_content(previous, log)
            {
                return Err(invalid("error identity reused with contradictory content"));
            }
            let previous = self
                .received
                .get(&(batch.generation.clone(), log.id.clone()));
            if previous.is_some_and(|previous| !same_content(previous, log)) {
                return Err(invalid(
                    "durable error identity reused with contradictory content",
                ));
            }
            if self
                .sequence_ids
                .get(&(batch.generation.clone(), log.sequence))
                .is_some_and(|id| id != &log.id)
            {
                return Err(invalid("error sequence reused with another identity"));
            }
            if same_generation
                && previous.is_none()
                && self
                    .checkpoint
                    .last_sequence
                    .is_some_and(|last| log.sequence <= last)
            {
                return Err(invalid("unrecognized error precedes committed sequence"));
            }
        }
        for pair in batch.errors.windows(2) {
            if pair[1].sequence < pair[0].sequence
                || (pair[1].sequence == pair[0].sequence && !same_content(&pair[0], &pair[1]))
            {
                return Err(invalid(
                    "batch errors are out of order or contradict a sequence identity",
                ));
            }
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
