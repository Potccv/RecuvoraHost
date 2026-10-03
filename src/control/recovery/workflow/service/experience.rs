use super::*;
pub use recuvora_core::recovery::planning::{HarnessRepairRequest, MAX_REPAIR_REQUEST_BYTES};
pub(super) const MAX_MATCHED_EXPERIENCES: usize = 100_000;

/// Persisted independently of business completion, with a stable delivery identity.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExperienceJob {
    pub id: String,
    pub task: RecoveryTask,
    pub outcome: RepairOutcome,
    pub recorded_at_ms: u64,
    pub attempt: u64,
    pub call_id: Option<String>,
    pub report: Option<ExperienceReport>,
    pub last_error: Option<String>,
    pub delivered: bool,
}
impl ExperienceJob {
    pub fn record(&self) -> Result<RepairExperience, RecoveryError> {
        let operation = self
            .task
            .operation
            .as_ref()
            .ok_or_else(|| invalid("missing repair operation"))?;
        let receipt = self
            .task
            .receipt
            .as_ref()
            .ok_or_else(|| invalid("missing repair receipt"))?;
        let observation = self
            .task
            .observation
            .as_ref()
            .ok_or_else(|| invalid("missing observation"))?;
        let request: HarnessRepairRequest = serde_json::from_value(
            operation
                .action
                .get("request")
                .cloned()
                .ok_or_else(|| invalid("missing repair request"))?,
        )
        .map_err(|_| invalid("invalid repair request"))?;
        let conditions = super::contract::stable_conditions(&self.task.problem, &request.target)?;
        if conditions
            .iter()
            .filter(|(key, _)| key.as_str() != super::contract::FAULT_FINGERPRINT_CONDITION)
            .any(|(key, value)| observation.facts.get(key) != Some(value))
        {
            return Err(invalid("incident environment changed"));
        }
        Ok(recuvora_core::recovery::build_experience(
            recuvora_core::recovery::ExperienceInput {
                id: &self.id,
                operation_id: &operation.operation_id,
                problem: &self.task.problem,
                target: &request.target,
                outcome: self.outcome,
                actions: &receipt.execution_trace,
                evidence_refs: self
                    .task
                    .verification
                    .as_ref()
                    .map_or(&receipt.evidence_refs, |v| &v.evidence_refs),
                recorded_at_ms: self.recorded_at_ms,
                report: self
                    .report
                    .as_ref()
                    .ok_or_else(|| invalid("summary not completed"))?,
            },
        )?)
    }
}

impl RecoveryState {
    pub fn pending_experiences(&self) -> Vec<ExperienceJob> {
        let mut jobs: Vec<_> = self
            .experiences
            .values()
            .filter(|job| !job.delivered)
            .cloned()
            .collect();
        jobs.sort_by(|a, b| {
            a.recorded_at_ms
                .cmp(&b.recorded_at_ms)
                .then_with(|| a.id.cmp(&b.id))
        });
        jobs
    }
}
