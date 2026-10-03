use super::*;
pub use recuvora_core::recovery::knowledge::{ExperienceReport, RepairExperience, Scriptability};
use serde::Serialize;
/// Only trusted caller code may attest a persisted result; model JSON cannot do so.
#[derive(Clone, Debug, Serialize)]
pub struct TrustedRepairExperience(Box<RepairExperience>);
impl TrustedRepairExperience {
    pub fn attest(experience: RepairExperience) -> Result<Self, KnowledgeError> {
        experience.report.validate()?;
        if experience.actions.len() > 1 {
            return Err(KnowledgeError::Invalid(
                "at most one repair action is allowed".into(),
            ));
        }
        for action in &experience.actions {
            action.validate()?;
        }
        for id in [
            &experience.id,
            &experience.operation_id,
            &experience.target_id,
        ] {
            validation::text(id, 256, "repair identity")?;
        }
        validation::query(&KnowledgeQuery {
            conditions: experience.conditions.clone(),
            keywords: experience.keywords.clone(),
            limit: 1,
        })?;
        validation::evidence(&experience.evidence_refs)?;
        Ok(Self(Box::new(experience)))
    }
    pub fn record(&self) -> &RepairExperience {
        &self.0
    }
}

impl KnowledgeState {
    pub(crate) fn experiences(&self) -> impl Iterator<Item = &RepairExperience> {
        self.experiences.values()
    }

    pub(crate) fn matching_experiences<'a>(
        &'a self,
        query: &KnowledgeQuery,
    ) -> Result<Vec<&'a RepairExperience>, KnowledgeError> {
        Ok(recuvora_core::recovery::knowledge::matching_experiences(
            query,
            self.experiences.values(),
        )?)
    }

    /// Exact applicable experience, including failures as explicitly labelled evidence.
    /// No returned item grants permission to execute a candidate script.
    pub fn search_experiences(
        &self,
        query: &KnowledgeQuery,
    ) -> Result<Vec<RepairExperience>, KnowledgeError> {
        Ok(self
            .matching_experiences(query)?
            .into_iter()
            .take(query.limit)
            .cloned()
            .collect())
    }
}
