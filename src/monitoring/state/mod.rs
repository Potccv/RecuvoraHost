//! Durable Node error receipts and source liveness, without health decisions.
mod checkpoint;
mod validation;

use super::shared::{Shared, lock};
use super::support::{MAX_SAMPLES, bounded_text, duration_ms, now_ms};
use super::{
    BatchCoverage, Coverage, Freshness, MonitorDefinition, MonitorError, MonitorSnapshot,
    NodeErrorLog, ObservationBatch, ObservationRequest,
};
use crate::control::recovery::incidents::{
    IncidentKind, IncidentRecord, IncidentSignal, MonitorCommit, SignalCondition,
};
use checkpoint::SourceCheckpoint;
pub(super) use checkpoint::restore_state;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::time::Instant;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReceiptEvidence {
    kind: String,
    extension_id: String,
    source_id: String,
    generation: String,
    log: NodeErrorLog,
}

pub(super) fn receipt_rule_id(
    config: &MonitorDefinition,
    generation: &str,
    log_id: &str,
) -> String {
    // JSON encodes tuple boundaries, including arbitrary valid ID punctuation.
    let identity = json!([
        config.id,
        config.extension_id,
        config.source_id,
        generation,
        log_id
    ]);
    format!(
        "error.{:x}",
        Sha256::digest(identity.to_string().as_bytes())
    )
}

pub(super) fn receipt_log(
    config: &MonitorDefinition,
    record: &IncidentRecord,
) -> Result<(String, NodeErrorLog), MonitorError> {
    let invalid =
        || MonitorError::Observation("error receipt identity or content is invalid".into());
    let evidence: ReceiptEvidence =
        serde_json::from_value(record.evidence.clone()).map_err(|_| invalid())?;
    if record.kind != IncidentKind::ErrorLog
        || record.monitor_id != config.id
        || record.target_id != config.target_id
        || evidence.kind != "node_error"
        || evidence.extension_id != config.extension_id
        || evidence.source_id != config.source_id
        || !crate::protocol::valid_id(&evidence.generation)
        || record.rule_id != receipt_rule_id(config, &evidence.generation, &evidence.log.id)
        || record.summary != evidence.log.message
    {
        return Err(invalid());
    }
    validation::validate_log(&evidence.log)?;
    Ok((evidence.generation, evidence.log))
}

pub(super) struct MonitorState {
    pub(super) config: MonitorDefinition,
    checkpoint: SourceCheckpoint,
    committed_checkpoint: SourceCheckpoint,
    commit_sequence: u64,
    pub(super) view: MonitorSnapshot,
    received: BTreeMap<(String, String), NodeErrorLog>,
    sequence_ids: BTreeMap<(String, u64), String>,
    started: Instant,
    last_batch: Option<Instant>,
    coverage_problem: Option<String>,
}

impl MonitorState {
    fn new(config: MonitorDefinition, checkpoint: SourceCheckpoint, commit_sequence: u64) -> Self {
        let view = MonitorSnapshot {
            id: config.id.clone(),
            target_id: config.target_id.clone(),
            source_id: config.source_id.clone(),
            view_role: config.view_role.clone(),
            extension_id: config.extension_id.clone(),
            contract: config.contract.clone(),
            version: config.version,
            method: config.method.clone(),
            freshness: Freshness::Missing,
            coverage: Coverage::Unknown,
            running: true,
            last_received_at_ms: None,
            received_error_count: 0,
            last_error_log: None,
            generation: checkpoint.generation.clone(),
            cursor: checkpoint.cursor.clone(),
            last_error: None,
            interval_ms: config.interval_ms,
        };
        Self {
            config,
            committed_checkpoint: checkpoint.clone(),
            checkpoint,
            commit_sequence,
            view,
            received: BTreeMap::new(),
            sequence_ids: BTreeMap::new(),
            started: Instant::now(),
            last_batch: None,
            coverage_problem: None,
        }
    }

    pub(super) fn request(&self) -> ObservationRequest {
        let mut params = self.config.params.clone();
        params["target_id"] = json!(self.config.target_id);
        params["source_id"] = json!(self.config.source_id);
        params["cursor"] = json!(self.checkpoint.cursor);
        params["generation"] = json!(self.checkpoint.generation);
        ObservationRequest {
            extension_id: self.config.extension_id.clone(),
            contract: self.config.contract.clone(),
            version: self.config.version,
            method: self.config.method.clone(),
            params,
            timeout: Duration::from_millis(self.config.timeout_ms),
        }
    }

    fn coverage_signal(&self, condition: SignalCondition, reason: &str) -> IncidentSignal {
        IncidentSignal {
            monitor_id: self.config.id.clone(),
            target_id: self.config.target_id.clone(),
            rule_id: self.config.id.clone(),
            kind: IncidentKind::Coverage,
            condition,
            summary: reason.into(),
            evidence: json!({"reason":reason,"extension_id":self.config.extension_id,
                "source_id":self.config.source_id,"generation":self.checkpoint.generation,"cursor":self.checkpoint.cursor}),
        }
    }

    pub(super) fn coverage_failure(
        &mut self,
        reason: String,
        coverage: Coverage,
    ) -> Vec<IncidentSignal> {
        let reason = bounded_text(&reason);
        self.view.coverage = coverage;
        self.view.last_error = Some(reason.clone());
        let changed = self.coverage_problem.as_ref() != Some(&reason);
        self.coverage_problem = Some(reason.clone());
        if changed {
            vec![self.coverage_signal(SignalCondition::Active, &reason)]
        } else {
            Vec::new()
        }
    }

    pub(super) fn tick(&mut self) -> Vec<IncidentSignal> {
        if let Some(received) = self.last_batch {
            if duration_ms(received.elapsed()) >= self.config.stale_after_ms {
                self.view.freshness = Freshness::Stale;
                return self
                    .coverage_failure("source batch is stale".into(), Coverage::Unavailable);
            }
        } else if duration_ms(self.started.elapsed()) >= self.config.startup_grace_ms {
            return self.coverage_failure(
                "no source batch after startup grace".into(),
                Coverage::Unavailable,
            );
        }
        Vec::new()
    }

    pub(super) fn accept(
        &mut self,
        batch: ObservationBatch,
        _elapsed: Duration,
    ) -> Result<Vec<IncidentSignal>, MonitorError> {
        // Full validation precedes mutations, including conflicts with all durable history.
        self.validate_batch(&batch)?;
        let changed_generation = self
            .checkpoint
            .generation
            .as_ref()
            .is_some_and(|generation| generation != &batch.generation);
        if changed_generation {
            if let Some(old) = self.checkpoint.generation.take() {
                self.checkpoint.retired_generations.push_back(old);
            }
            self.checkpoint.last_sequence = None;
            self.checkpoint.recent_ids.clear();
        }
        self.checkpoint.generation = Some(batch.generation.clone());
        let complete = batch.coverage == BatchCoverage::Complete
            && !batch.has_more
            && batch.source_error.is_none()
            && !changed_generation;
        self.view.coverage = if complete {
            Coverage::Complete
        } else {
            Coverage::Partial
        };
        self.view.last_received_at_ms = Some(now_ms());
        self.view.freshness = Freshness::Fresh;
        self.last_batch = Some(Instant::now());
        let mut signals = Vec::new();
        for log in batch.errors {
            let key = (batch.generation.clone(), log.id.clone());
            if self.received.contains_key(&key) {
                continue;
            }
            signals.push(IncidentSignal {
                monitor_id: self.config.id.clone(),
                target_id: self.config.target_id.clone(),
                rule_id: receipt_rule_id(&self.config, &batch.generation, &log.id),
                kind: IncidentKind::ErrorLog,
                condition: SignalCondition::Active,
                summary: log.message.clone(),
                evidence: json!({"kind":"node_error","extension_id":self.config.extension_id,
                    "source_id":self.config.source_id,"generation":batch.generation,"log":log}),
            });
            self.checkpoint.last_sequence = Some(log.sequence);
            self.checkpoint.recent_ids.push_back(log.id.clone());
            if self.checkpoint.recent_ids.len() > MAX_SAMPLES {
                self.checkpoint.recent_ids.pop_front();
            }
            self.sequence_ids
                .insert((batch.generation.clone(), log.sequence), log.id.clone());
            self.view.received_error_count = self.view.received_error_count.saturating_add(1);
            self.view.last_error_log = Some(log.clone());
            self.received.insert(key, log);
        }
        self.checkpoint.cursor = Some(batch.next_cursor);
        self.view.cursor = self.checkpoint.cursor.clone();
        self.view.generation = self.checkpoint.generation.clone();
        if complete {
            let had_problem = self.coverage_problem.take().is_some();
            self.view.last_error = None;
            if had_problem {
                signals.push(
                    self.coverage_signal(SignalCondition::Clear, "complete source batch received"),
                );
            }
        } else {
            let reason = if changed_generation {
                "source generation changed; continuity lost".into()
            } else {
                batch
                    .source_error
                    .unwrap_or_else(|| "source coverage is partial".into())
            };
            signals.extend(self.coverage_failure(reason, Coverage::Partial));
        }
        Ok(signals)
    }

    pub(super) fn publish(&self, shared: &Shared) -> Result<(), MonitorError> {
        let mut view = self.view.clone();
        if !shared.accepting.load(Ordering::Acquire) {
            view.running = false;
            if let Some(error) = lock(&shared.error)?.clone() {
                view.last_error = Some(error);
            }
        }
        lock(&shared.views)?.insert(self.config.id.clone(), view);
        Ok(())
    }

    pub(super) fn commit(
        &mut self,
        shared: &Shared,
        signals: Vec<IncidentSignal>,
    ) -> Result<(), MonitorError> {
        if signals.is_empty() && self.checkpoint == self.committed_checkpoint {
            return self.publish(shared);
        }
        let received_errors = signals
            .iter()
            .any(|signal| signal.kind == IncidentKind::ErrorLog);
        let sequence = self
            .commit_sequence
            .checked_add(1)
            .ok_or_else(|| MonitorError::Runtime("monitor sequence exhausted".into()))?;
        let checkpoint = serde_json::to_value(&self.checkpoint)
            .map_err(|e| MonitorError::Runtime(e.to_string()))?;
        lock(&shared.store)?.commit(MonitorCommit {
            monitor_id: self.config.id.clone(),
            sequence,
            checkpoint,
            signals,
            now_ms: now_ms(),
        })?;
        self.commit_sequence = sequence;
        self.committed_checkpoint = self.checkpoint.clone();
        self.publish(shared)?;
        if received_errors {
            shared.received.notify_one();
        }
        Ok(())
    }
}
