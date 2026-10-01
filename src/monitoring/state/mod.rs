//! Per-monitor evidence transitions with atomic checkpoint and incident commits.
mod checkpoint;
mod validation;

use super::shared::{Shared, lock};
use super::support::{MAX_SAMPLES, bounded_text, duration_ms, now_ms};
use super::{
    BatchCoverage, Coverage, Freshness, MonitorDefinition, MonitorError, MonitorSnapshot,
    ObservationBatch, ObservationRequest, TargetHealth,
};
use checkpoint::SourceCheckpoint;
pub(super) use checkpoint::restore_state;
use recuvora_core::recovery::incidents::{
    IncidentKind, IncidentSignal, MonitorCommit, SignalCondition,
};
use serde_json::{Value, json};
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::time::Instant;

pub(super) struct MonitorState {
    pub(super) config: MonitorDefinition,
    checkpoint: SourceCheckpoint,
    committed_checkpoint: SourceCheckpoint,
    commit_sequence: u64,
    pub(super) view: MonitorSnapshot,
    started: Instant,
    last_sample: Option<(Instant, u64)>,
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
            health: TargetHealth::Unknown,
            freshness: Freshness::Missing,
            coverage: Coverage::Unknown,
            running: true,
            last_received_at_ms: None,
            last_sample_id: None,
            last_value: None,
            sample_age_ms: None,
            consecutive_failures: 0,
            consecutive_successes: 0,
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
            started: Instant::now(),
            last_sample: None,
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

    fn signal(
        &self,
        kind: IncidentKind,
        condition: SignalCondition,
        summary: &str,
        mut evidence: Value,
    ) -> IncidentSignal {
        evidence["rule"] = json!(self.config.rule);
        IncidentSignal {
            monitor_id: self.config.id.clone(),
            target_id: self.config.target_id.clone(),
            rule_id: self.config.id.clone(),
            kind,
            condition,
            summary: summary.into(),
            evidence,
        }
    }

    fn evidence(&self, reason: &str) -> Value {
        json!({"reason":reason,"source_id":self.config.source_id,"generation":self.checkpoint.generation,
            "cursor":self.checkpoint.cursor,"sample_id":self.view.last_sample_id,"value":self.view.last_value})
    }

    fn unknown(&mut self) {
        self.view.health = TargetHealth::Unknown;
        self.view.consecutive_failures = 0;
        self.view.consecutive_successes = 0;
    }

    pub(super) fn coverage_failure(
        &mut self,
        reason: String,
        coverage: Coverage,
    ) -> Vec<IncidentSignal> {
        let reason = bounded_text(&reason);
        let target_changed = self.view.health != TargetHealth::Unknown;
        self.unknown();
        self.view.coverage = coverage;
        self.view.last_error = Some(reason.clone());
        let changed = self.coverage_problem.as_ref() != Some(&reason);
        self.coverage_problem = Some(reason.clone());
        let mut signals = Vec::new();
        if changed {
            signals.push(self.signal(
                IncidentKind::Coverage,
                SignalCondition::Active,
                &reason,
                self.evidence(&reason),
            ));
        }
        if changed || target_changed {
            signals.push(self.signal(
                IncidentKind::Target,
                SignalCondition::Unknown,
                "target lacks current complete evidence",
                self.evidence(&reason),
            ));
        }
        signals
    }

    pub(super) fn tick(&mut self) -> Vec<IncidentSignal> {
        if let Some((received, age)) = self.last_sample {
            let age = age.saturating_add(duration_ms(received.elapsed()));
            self.view.sample_age_ms = Some(age);
            if age >= self.config.stale_after_ms {
                self.view.freshness = Freshness::Stale;
                if self.view.health != TargetHealth::Unknown
                    || self.coverage_problem.as_deref() != Some("sample is stale")
                {
                    return self.coverage_failure("sample is stale".into(), self.view.coverage);
                }
            }
        } else if duration_ms(self.started.elapsed()) >= self.config.startup_grace_ms
            && self.coverage_problem.as_deref() != Some("no fresh sample after startup grace")
        {
            return self.coverage_failure(
                "no fresh sample after startup grace".into(),
                self.view.coverage,
            );
        }
        Vec::new()
    }

    pub(super) fn accept(
        &mut self,
        batch: ObservationBatch,
        elapsed: Duration,
    ) -> Result<Vec<IncidentSignal>, MonitorError> {
        self.validate_batch(&batch)?;
        self.view.last_received_at_ms = Some(now_ms());
        let changed_generation = self
            .checkpoint
            .generation
            .as_ref()
            .is_some_and(|generation| generation != &batch.generation);
        if changed_generation {
            if self
                .checkpoint
                .retired_generations
                .contains(&batch.generation)
            {
                return Err(MonitorError::Observation(
                    "retired source generation replayed".into(),
                ));
            }
            if let Some(old) = self.checkpoint.generation.take() {
                self.checkpoint.retired_generations.push_back(old);
                if self.checkpoint.retired_generations.len() > 16 {
                    self.checkpoint.retired_generations.pop_front();
                }
            }
            self.checkpoint.last_sequence = None;
            self.checkpoint.recent_ids.clear();
            self.last_sample = None;
            self.view.freshness = Freshness::Missing;
            self.unknown();
        }
        self.checkpoint.generation = Some(batch.generation.clone());
        let complete = batch.coverage == BatchCoverage::Complete
            && !batch.has_more
            && batch.error.is_none()
            && !changed_generation;
        self.view.coverage = if complete {
            Coverage::Complete
        } else {
            Coverage::Partial
        };
        let reason = if changed_generation {
            "source generation changed; continuity lost".to_string()
        } else {
            batch
                .error
                .clone()
                .unwrap_or_else(|| "observation coverage is partial".into())
        };
        let mut signals = if complete {
            Vec::new()
        } else {
            self.coverage_failure(reason, Coverage::Partial)
        };
        let mut fresh_accepted = false;
        let mut sample_problem = None;
        let empty = batch.samples.is_empty();
        for sample in batch.samples {
            if self
                .checkpoint
                .last_sequence
                .is_some_and(|last| sample.sequence <= last)
                || self.checkpoint.recent_ids.contains(&sample.id)
            {
                continue;
            }
            self.checkpoint.last_sequence = Some(sample.sequence);
            self.checkpoint.recent_ids.push_back(sample.id.clone());
            if self.checkpoint.recent_ids.len() > MAX_SAMPLES {
                self.checkpoint.recent_ids.pop_front();
            }
            let age = sample.age_ms.saturating_add(duration_ms(elapsed));
            if age >= self.config.stale_after_ms {
                self.unknown();
                revoke_pending_clears(&mut signals);
                sample_problem = Some("batch contains stale samples".into());
                // Historical data can be retained in evidence, never refresh a current sample.
                if self.last_sample.is_none() {
                    self.last_sample = Some((Instant::now(), age));
                    self.view.freshness = Freshness::Stale;
                    self.view.sample_age_ms = Some(age);
                    self.view.last_sample_id = Some(sample.id);
                    self.view.last_value = Some(sample.value.clone());
                }
                continue;
            }
            let healthy = match self.config.rule.evaluate(&sample.value) {
                Ok(healthy) => healthy,
                Err(error) => {
                    sample_problem = Some(error.to_string());
                    self.unknown();
                    revoke_pending_clears(&mut signals);
                    continue;
                }
            };
            fresh_accepted = true;
            self.last_sample = Some((Instant::now(), age));
            self.view.freshness = Freshness::Fresh;
            self.view.sample_age_ms = Some(age);
            self.view.last_sample_id = Some(sample.id.clone());
            self.view.last_value = Some(sample.value.clone());
            let evidence = json!({"source_id":self.config.source_id,"generation":batch.generation,
                "sample_id":sample.id,"sequence":sample.sequence,"age_ms":age,"value":sample.value,"source_evidence":sample.evidence});
            if healthy && complete {
                self.view.consecutive_failures = 0;
                self.view.consecutive_successes = self
                    .view
                    .consecutive_successes
                    .saturating_add(1)
                    .min(self.config.rule.success_samples);
                if self.view.consecutive_successes >= self.config.rule.success_samples {
                    self.view.health = TargetHealth::Healthy;
                    signals.push(self.signal(
                        IncidentKind::Target,
                        SignalCondition::Clear,
                        "fresh samples satisfy healthy rule",
                        evidence,
                    ));
                }
            } else if !healthy {
                self.view.consecutive_successes = 0;
                self.view.consecutive_failures = self
                    .view
                    .consecutive_failures
                    .saturating_add(1)
                    .min(self.config.rule.failure_samples);
                if self.view.consecutive_failures >= self.config.rule.failure_samples {
                    self.view.health = TargetHealth::Unhealthy;
                    signals.push(self.signal(
                        IncidentKind::Target,
                        SignalCondition::Active,
                        "fresh samples violate healthy rule",
                        evidence,
                    ));
                } else {
                    revoke_pending_clears(&mut signals);
                    if self.view.health == TargetHealth::Healthy {
                        self.view.health = TargetHealth::Unknown;
                    }
                    signals.push(self.signal(
                        IncidentKind::Target,
                        SignalCondition::Unknown,
                        "unhealthy samples below fault threshold",
                        evidence,
                    ));
                }
            } else {
                self.view.consecutive_successes = 0;
            }
        }
        if empty {
            self.view.consecutive_failures = 0;
            self.view.consecutive_successes = 0;
        }
        self.checkpoint.cursor = Some(batch.next_cursor);
        self.view.cursor = self.checkpoint.cursor.clone();
        self.view.generation = self.checkpoint.generation.clone();
        if let Some(problem) = sample_problem {
            revoke_pending_clears(&mut signals);
            signals.extend(self.coverage_failure(problem, Coverage::Partial));
        } else if complete && fresh_accepted {
            self.coverage_problem = None;
            self.view.last_error = None;
            signals.retain(|signal| !matches!(signal.kind, IncidentKind::Coverage));
            signals.push(self.signal(
                IncidentKind::Coverage,
                SignalCondition::Clear,
                "fresh complete observation restored coverage",
                self.evidence("fresh complete observation"),
            ));
        }
        Ok(signals)
    }

    pub(super) fn publish(&self, shared: &Shared) -> Result<(), MonitorError> {
        let mut view = self.view.clone();
        if !shared.accepting.load(Ordering::Acquire) {
            view.running = false;
            view.health = TargetHealth::Unknown;
            if let Some(error) = lock(&shared.error)?.clone() {
                view.last_error = Some(error);
            }
        }
        let mut views = lock(&shared.views)?;
        let mut fresh_until = lock(&shared.fresh_until)?;
        if let Some((received, age)) = self.last_sample {
            let remaining = self.config.stale_after_ms.saturating_sub(age);
            fresh_until.insert(
                self.config.id.clone(),
                received + Duration::from_millis(remaining),
            );
        } else {
            fresh_until.remove(&self.config.id);
        }
        views.insert(self.config.id.clone(), view);
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
        self.publish(shared)
    }
}

fn revoke_pending_clears(signals: &mut Vec<IncidentSignal>) {
    while let Some(index) = signals
        .iter()
        .rposition(|signal| matches!(signal.kind, IncidentKind::Target))
    {
        if !matches!(signals[index].condition, SignalCondition::Clear) {
            break;
        }
        signals.remove(index);
    }
}
