//! Pure transaction proposals and strictly validated event restoration.
use super::*;

impl IncidentLedger {
    pub fn new(config: IncidentLimits) -> Result<Self, IncidentError> {
        config.validate()?;
        Ok(Self {
            digest: crate::control::binding::digest(&("incidents", &config)),
            commit_ids: im::OrdSet::new(),
            config,
            sequence: 0,
            history: im::Vector::new(),
            records: crate::control::collections::Map::new(),
            active: crate::control::collections::Map::new(),
            monitors: crate::control::collections::Map::new(),
        })
    }
    pub fn revision(&self) -> u64 {
        self.sequence
    }
    /// Copies the complete history for export; use latest_entry for incremental persistence.
    pub fn entries(&self) -> Vec<IncidentEntry> {
        self.history
            .iter()
            .map(|entry| entry.as_ref().clone())
            .collect()
    }

    /// The newest validated entry, without copying the history.
    pub fn latest_entry(&self) -> Option<&IncidentEntry> {
        self.history.back().map(AsRef::as_ref)
    }

    pub fn restore(
        config: IncidentLimits,
        entries: impl AsRef<[IncidentEntry]>,
    ) -> Result<Self, IncidentError> {
        let entries = entries.as_ref();
        let mut ledger = Self::new(config)?;
        for entry in entries {
            ledger
                .apply_entry(entry.clone())
                .map_err(|error| IncidentError::Corrupt(error.to_string()))?;
        }
        Ok(ledger)
    }

    /// Returns None for an exact retry of the latest monitor commit. The caller
    /// must ensure this aggregate is current before acknowledging that retry.
    pub fn prepare_monitor(
        &self,
        commit_id: String,
        commit: MonitorCommit,
    ) -> Result<Option<Prepared<Self>>, IncidentError> {
        self.prepare_event(commit_id, IncidentEvent::Monitor { commit })
    }

    /// Attention attribution does not resolve the condition or grant authority.
    pub fn prepare_acknowledge(
        &self,
        commit_id: String,
        id: String,
        expected_revision: u64,
        actor: String,
        note: String,
        now_ms: u64,
    ) -> Result<Prepared<Self>, IncidentError> {
        self.prepare_event(
            commit_id,
            IncidentEvent::Acknowledge {
                id,
                expected_revision,
                actor,
                note,
                now_ms,
            },
        )?
        .ok_or_else(|| IncidentError::Conflict("acknowledgement unexpectedly unchanged".into()))
    }

    fn prepare_event(
        &self,
        commit_id: String,
        event: IncidentEvent,
    ) -> Result<Option<Prepared<Self>>, IncidentError> {
        if self.commit_ids.contains(&commit_id) {
            return Err(IncidentError::Conflict("commit identity reused".into()));
        }
        let sequence = next(self.sequence)?;
        let Some(transition) = self.compute(&event, sequence)? else {
            return Ok(None);
        };
        let input =
            serde_json::json!({"config":self.config,"prior_digest":self.digest,"event":event});
        let mut proposed = self.clone();
        proposed.install(transition);
        proposed.sequence = sequence;
        let entry = IncidentEntry {
            prior_digest: self.digest.clone(),
            commit_id: commit_id.clone(),
            sequence,
            event,
        };
        proposed.digest = crate::control::binding::digest(&entry);
        proposed.commit_ids.insert(commit_id.clone());
        proposed.history.push_back(std::sync::Arc::new(entry));
        Ok(Some(Prepared::new_bound(
            commit_id,
            self.sequence,
            "incidents".into(),
            input,
            proposed,
            Vec::new(),
        )?))
    }

    fn apply_entry(&mut self, entry: IncidentEntry) -> Result<(), IncidentError> {
        if entry.prior_digest != self.digest {
            return Err(IncidentError::Corrupt("history digest mismatch".into()));
        }
        if !crate::control::identity::valid_id(&entry.commit_id) {
            return Err(IncidentError::Invalid("commit identity".into()));
        }
        if self.commit_ids.contains(&entry.commit_id) {
            return Err(IncidentError::Corrupt("duplicate commit identity".into()));
        }
        if next(self.sequence)? != entry.sequence {
            return Err(IncidentError::Corrupt("sequence gap".into()));
        }
        let transition = self
            .compute(&entry.event, entry.sequence)?
            .ok_or_else(|| IncidentError::Corrupt("duplicate committed event".into()))?;
        self.install(transition);
        self.sequence = entry.sequence;
        self.digest = crate::control::binding::digest(&entry);
        self.commit_ids.insert(entry.commit_id.clone());
        self.history.push_back(std::sync::Arc::new(entry));
        Ok(())
    }
}
