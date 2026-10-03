//! Aggregate metadata without artifact payloads or evidence.
use super::*;

impl KnowledgeState {
    pub fn projection(&self) -> KnowledgeProjection {
        KnowledgeProjection {
            experiences: self.experiences.len(),
            artifacts: self.artifacts.len(),
            quarantined_versions: self.quarantined.len(),
            max_records: self.config.max_records,
        }
    }
}
