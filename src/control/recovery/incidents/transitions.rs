//! Pure preparation and installation shared by live commits and replay.
use super::validation::{bounded_object, bounded_text};
use super::*;

impl IncidentLedger {
    pub(super) fn compute(
        &self,
        event: &IncidentEvent,
        sequence: u64,
    ) -> Result<Option<Transition>, IncidentError> {
        match event {
            IncidentEvent::Monitor { commit } => self.compute_monitor(commit, sequence),
            IncidentEvent::Acknowledge {
                id,
                expected_revision,
                actor,
                note,
                now_ms,
            } => {
                bounded_text(id, 256, "incident id", false)?;
                bounded_text(actor, 256, "actor", false)?;
                bounded_text(note, MAX_ACK_NOTE_BYTES, "acknowledgement note", true)?;
                let mut record = self
                    .records
                    .get(id)
                    .cloned()
                    .ok_or_else(|| IncidentError::NotFound(id.clone()))?;
                if record.revision != *expected_revision || record.status != IncidentStatus::Open {
                    return Err(IncidentError::Conflict(
                        "revision changed or incident is not open".into(),
                    ));
                }
                record.revision = next(record.revision)?;
                record.status = IncidentStatus::Acknowledged;
                record.acknowledgement = Some(IncidentAcknowledgement {
                    actor: actor.clone(),
                    note: note.clone(),
                    at_ms: (*now_ms).max(record.last_seen),
                });
                Ok(Some(Transition {
                    records: vec![record],
                    monitor: None,
                }))
            }
        }
    }

    fn compute_monitor(
        &self,
        commit: &MonitorCommit,
        sequence: u64,
    ) -> Result<Option<Transition>, IncidentError> {
        bounded_text(&commit.monitor_id, 256, "monitor id", false)?;
        bounded_object(&commit.checkpoint, MAX_CHECKPOINT_BYTES, "checkpoint")?;
        if commit.signals.len() > 128 {
            return Err(IncidentError::Capacity("signals per commit".into()));
        }
        match self.monitors.get(&commit.monitor_id) {
            Some(previous) if previous.commit == *commit => return Ok(None),
            Some(previous) if commit.sequence != next(previous.commit.sequence)? => {
                return Err(IncidentError::Conflict("monitor sequence".into()));
            }
            None if commit.sequence != 1 => {
                return Err(IncidentError::Conflict(
                    "first monitor sequence must be one".into(),
                ));
            }
            None if self.monitors.len() >= self.config.max_monitors => {
                return Err(IncidentError::Capacity("monitors".into()));
            }
            _ => {}
        }
        let mut timestamp = self
            .monitors
            .get(&commit.monitor_id)
            .map_or(commit.now_ms, |previous| {
                commit.now_ms.max(previous.updated_at_ms)
            });
        let mut records: Vec<IncidentRecord> = Vec::new();
        let mut additions = 0;
        for (index, signal) in commit.signals.iter().enumerate() {
            if signal.monitor_id != commit.monitor_id {
                return Err(IncidentError::Invalid(
                    "signal monitor differs from commit".into(),
                ));
            }
            bounded_text(&signal.target_id, 256, "target id", false)?;
            bounded_text(&signal.rule_id, 256, "rule id", false)?;
            bounded_text(
                &signal.summary,
                if signal.kind == IncidentKind::ErrorLog {
                    8192
                } else {
                    2048
                },
                "summary",
                false,
            )?;
            bounded_object(
                &signal.evidence,
                if signal.kind == IncidentKind::ErrorLog {
                    64 * 1024
                } else {
                    MAX_EVIDENCE_BYTES
                },
                "evidence",
            )?;
            if signal.kind == IncidentKind::ErrorLog && signal.condition != SignalCondition::Active
            {
                return Err(IncidentError::Invalid(
                    "received error logs cannot be cleared or reclassified".into(),
                ));
            }
            let key = (
                signal.monitor_id.clone(),
                signal.target_id.clone(),
                signal.rule_id.clone(),
                signal.kind,
            );
            // Process every observation in order, including repeated keys in a
            // single batch. A later clear must not erase an earlier episode.
            let current = records
                .iter()
                .rev()
                .find(|record| {
                    record.monitor_id == signal.monitor_id
                        && record.target_id == signal.target_id
                        && record.rule_id == signal.rule_id
                        && record.kind == signal.kind
                })
                .or_else(|| self.active.get(&key).and_then(|id| self.records.get(id)))
                .filter(|record| record.status != IncidentStatus::Resolved);
            if let Some(current) = current
                && signal.kind == IncidentKind::ErrorLog
            {
                if current.summary != signal.summary || current.evidence != signal.evidence {
                    return Err(IncidentError::Conflict(
                        "error log identity was reused with different content".into(),
                    ));
                }
                continue;
            }
            let mut record = match current {
                Some(current) => {
                    timestamp = timestamp
                        .max(current.last_seen)
                        .max(current.acknowledgement.as_ref().map_or(0, |ack| ack.at_ms));
                    let mut record = current.clone();
                    record.revision = next(record.revision)?;
                    record
                }
                None if signal.condition != SignalCondition::Active => continue,
                None => {
                    additions += 1;
                    IncidentRecord {
                        id: format!("incident-{sequence:016x}-{index:04x}"),
                        revision: 1,
                        monitor_id: signal.monitor_id.clone(),
                        target_id: signal.target_id.clone(),
                        rule_id: signal.rule_id.clone(),
                        kind: signal.kind,
                        status: IncidentStatus::Open,
                        condition: signal.condition,
                        summary: signal.summary.clone(),
                        evidence: signal.evidence.clone(),
                        first_seen: timestamp,
                        last_seen: timestamp,
                        resolved_at: None,
                        acknowledgement: None,
                        occurrences: 0,
                    }
                }
            };
            record.condition = signal.condition;
            record.summary.clone_from(&signal.summary);
            record.evidence.clone_from(&signal.evidence);
            record.last_seen = timestamp;
            match signal.condition {
                SignalCondition::Active => record.occurrences = next(record.occurrences)?,
                SignalCondition::Clear => {
                    record.status = IncidentStatus::Resolved;
                    record.resolved_at = Some(timestamp);
                }
                SignalCondition::Unknown => {}
            }
            records.push(record);
        }
        if self.records.len().saturating_add(additions) > self.config.max_incidents {
            return Err(IncidentError::Capacity("incident episodes".into()));
        }
        Ok(Some(Transition {
            records,
            monitor: Some(MonitorState {
                commit: commit.clone(),
                updated_at_ms: timestamp,
            }),
        }))
    }

    pub(super) fn install(&mut self, prepared: Transition) {
        for record in prepared.records {
            let key = (
                record.monitor_id.clone(),
                record.target_id.clone(),
                record.rule_id.clone(),
                record.kind,
            );
            if record.status == IncidentStatus::Resolved {
                self.active.remove(&key);
            } else {
                self.active.insert(key, record.id.clone());
            }
            self.records.insert(record.id.clone(), record);
        }
        if let Some(state) = prepared.monitor {
            self.monitors.insert(state.commit.monitor_id.clone(), state);
        }
    }
}
