//! Deterministic experience decisions; the caller owns storage and serialization.
use super::{validation, *};
use crate::control::collections::Map;
use crate::control::operation::Prepared;
use im::OrdSet;

/// Validated authority built only through proposals and confirmed caller commits.
#[derive(Clone, Debug)]
pub struct KnowledgeState {
    pub(super) experiences: Map<String, RepairExperience>,
    pub(super) config: KnowledgeConfig,
    revision: u64,
    digest: String,
    commit_ids: OrdSet<String>,
    pub(super) artifacts: Map<(String, u64), RepairArtifact>,
    pub(super) quarantined: OrdSet<(String, u64)>,
}

impl KnowledgeState {
    pub fn new(config: KnowledgeConfig) -> Result<Self, KnowledgeError> {
        config.validate()?;
        Ok(Self {
            digest: crate::control::binding::digest(&("knowledge", &config)),
            commit_ids: OrdSet::new(),
            config,
            revision: 0,
            experiences: Map::new(),
            artifacts: Map::new(),
            quarantined: OrdSet::new(),
        })
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn config(&self) -> &KnowledgeConfig {
        &self.config
    }

    /// Persist the command and request with an atomic revision comparison before
    /// confirming and installing this uncommitted decision.
    pub fn propose(
        &self,
        commit_id: impl Into<String>,
        command: KnowledgeCommand,
    ) -> Result<Prepared<Self>, KnowledgeError> {
        let commit_id = commit_id.into();
        if self.commit_ids.contains(&commit_id) {
            return Err(KnowledgeError::Conflict("commit identity reused".into()));
        }
        let mut state = self.clone();
        match &command {
            KnowledgeCommand::RecordExperience(proof) => {
                let item = proof.record();
                if let Some(old) = self.experiences.get(&item.id) {
                    if old != item {
                        return Err(KnowledgeError::Conflict(
                            "experience identity changed".into(),
                        ));
                    }
                } else {
                    if self.experiences.len() >= self.config.max_records {
                        return Err(KnowledgeError::Capacity("experience records".into()));
                    }
                    // Validation against the candidate state also catches conflicting
                    // action/candidate identities within this one experience.
                    for artifact in item.actions.iter().chain(candidate(item)) {
                        state.validate_artifact(artifact)?;
                        state
                            .artifacts
                            .insert((artifact.id.clone(), artifact.version), artifact.clone());
                    }
                    if matches!(item.outcome, RepairOutcome::Failed | RepairOutcome::Unknown) {
                        for action in &item.actions {
                            state
                                .quarantined
                                .insert((action.id.clone(), action.version));
                        }
                    }
                    state.experiences.insert(item.id.clone(), item.clone());
                }
            }
            KnowledgeCommand::ExpandCapacity { expected, target } => {
                target.validate()?;
                if expected != &self.config || target.max_records <= expected.max_records {
                    return Err(KnowledgeError::Conflict(
                        "capacity expansion requires current configuration and a larger limit"
                            .into(),
                    ));
                }
                state.config = target.clone();
            }
        }
        state.revision = self
            .revision
            .checked_add(1)
            .ok_or_else(|| KnowledgeError::Capacity("sequence overflow".into()))?;
        let input = serde_json::to_value((&self.digest, &self.config, &command))
            .map_err(|error| KnowledgeError::Invalid(error.to_string()))?;
        let request = crate::control::operation::CommitRequest::new(
            commit_id.clone(),
            self.revision,
            "knowledge".into(),
            input.clone(),
        )?;
        state.digest = crate::control::binding::digest(&request);
        state.commit_ids.insert(commit_id.clone());
        Ok(Prepared::new_bound(
            commit_id,
            self.revision,
            "knowledge".into(),
            input,
            state,
            Vec::new(),
        )?)
    }

    /// Repeats live validation for the caller's complete, ordered committed history.
    pub fn replay(
        config: KnowledgeConfig,
        entries: Vec<KnowledgeReplayEntry>,
    ) -> Result<Self, KnowledgeError> {
        let mut state = Self::new(config)?;
        for entry in entries {
            let pending = state.propose(entry.request.id.clone(), entry.command)?;
            if pending.request() != &entry.request {
                return Err(KnowledgeError::Conflict(
                    "replay revision or commit identity mismatch".into(),
                ));
            }
            state = pending.confirm(entry.receipt)?.state;
        }
        Ok(state)
    }

    pub fn snapshot(&self) -> KnowledgeSnapshot {
        KnowledgeSnapshot {
            revision: self.revision,
            config: self.config.clone(),
            experiences: self.experiences.values().cloned().collect(),
        }
    }

    pub fn get(&self, id: &str) -> Option<RepairExperience> {
        self.experiences.get(id).cloned()
    }

    /// Checks bounded content and known version consistency, without registering
    /// it or granting execution authority. Quarantine must be checked separately.
    pub fn validate_artifact(&self, artifact: &RepairArtifact) -> Result<(), KnowledgeError> {
        validation::artifact(artifact)?;
        if self
            .artifacts
            .get(&(artifact.id.clone(), artifact.version))
            .is_some_and(|known| known != artifact)
        {
            return Err(KnowledgeError::Conflict(
                "artifact version is immutable".into(),
            ));
        }
        Ok(())
    }

    pub fn is_quarantined(&self, id: &str, version: u64) -> bool {
        self.quarantined.contains(&(id.into(), version))
    }
}

fn candidate(item: &RepairExperience) -> Option<&RepairArtifact> {
    match &item.report.scriptability {
        Scriptability::Possible { candidate, .. } => candidate.as_ref(),
        _ => None,
    }
}
