use super::*;

pub(super) use recuvora_core::recovery::planning::FAULT_FINGERPRINT_CONDITION;
pub use recuvora_core::recovery::planning::{ProblemContext, TargetBinding, TargetObservation};
pub(super) fn stable_conditions(
    problem: &ProblemContext,
    target: &TargetBinding,
) -> Result<BTreeMap<String, String>, RecoveryError> {
    Ok(recuvora_core::recovery::planning::stable_conditions(
        problem, target,
    )?)
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RecoveryConfig {
    pub schema_version: u32,
    /// Logical Harness identity selected by policy, independent of service routing.
    pub execution_harness: String,
    pub target: TargetBinding,
    pub approval: approval::ApprovalPolicy,
    pub summary_timeout_secs: u64,
    pub review_timeout_secs: u64,
    pub max_tool_calls: usize,
    pub max_tasks: usize,
}

impl RecoveryConfig {
    pub fn validate(&self) -> Result<(), RecoveryError> {
        self.approval.validate()?;
        for value in [
            &self.execution_harness,
            &self.target.target_id,
            &self.target.executor_id,
            &self.target.verification_profile,
        ] {
            if !crate::control::identity::valid_id(value) {
                return Err(invalid("invalid identity"));
            }
        }
        let kinds = &self.target.allowed_action_kinds;
        if self.schema_version != 2
            || !(1..=1800).contains(&self.summary_timeout_secs)
            || !(1..=1800).contains(&self.review_timeout_secs)
            || !(1..=1800).contains(&self.target.action_timeout_secs)
            || !(1..=64).contains(&self.max_tool_calls)
            || !(1..=10_000).contains(&self.max_tasks)
            || kinds.is_empty()
            || kinds.len() > 32
            || kinds
                .iter()
                .any(|kind| !crate::control::identity::valid_id(kind))
            || kinds.iter().collect::<BTreeSet<_>>().len() != kinds.len()
            || self
                .target
                .required_facts
                .contains_key(FAULT_FINGERPRINT_CONDITION)
        {
            return Err(invalid("invalid recovery limits or action scope"));
        }
        facts(&self.target.required_facts)?;
        if !self
            .approval
            .allowed_targets
            .contains(&self.target.target_id)
            || !self
                .approval
                .allowed_action_kinds
                .iter()
                .any(|kind| kind == "repair_with_harness")
        {
            return Err(invalid("target/Harness repair not explicitly delegated"));
        }
        Ok(())
    }
}

pub(super) fn facts(values: &BTreeMap<String, String>) -> Result<(), RecoveryError> {
    if values.len() > 32 {
        return Err(RecoveryError::Capacity);
    }
    for (key, value) in values {
        text(key, 128)?;
        text(value, 1024)?;
    }
    Ok(())
}
pub(super) fn evidence(values: &[String]) -> Result<(), RecoveryError> {
    if values.is_empty() || values.len() > 32 {
        return Err(RecoveryError::Invalid(
            "evidence required, maximum 32 references".into(),
        ));
    }
    let mut unique = BTreeSet::new();
    for value in values {
        text(value, 1024)?;
        if !unique.insert(value) {
            return Err(RecoveryError::Invalid(
                "duplicate evidence reference".into(),
            ));
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RepairExecutionOutcome {
    Executed,
    Failed,
    Unknown,
}

/// Domain result returned by the external executor through a trusted adapter.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepairReceipt {
    /// Actual actions supplied by the trusted backend, never by the model's final text.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub execution_trace: Vec<RepairArtifact>,
    pub operation_id: String,
    pub target_id: String,
    pub outcome: RepairExecutionOutcome,
    pub executor_stopped: bool,
    pub evidence_refs: Vec<String>,
    pub summary: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryStage {
    Queued,
    AwaitingApproval,
    Executing,
    Verifying,
    Completed,
    Failed,
    Denied,
    Canceled,
    Unknown,
    Paused,
}
impl RecoveryStage {
    pub fn terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Denied | Self::Canceled
        )
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryTask {
    pub id: String,
    pub revision: u64,
    pub problem: ProblemContext,
    pub stage: RecoveryStage,
    /// None while AwaitingApproval has a durable operation awaiting idempotent association.
    pub approval_id: Option<String>,
    pub operation: Option<approval::ProposedOperation>,
    pub observation: Option<TargetObservation>,
    pub receipt: Option<RepairReceipt>,
    pub verification: Option<BusinessVerification>,
    /// Independent execution facts and caller attribution, never inferred from health.
    #[serde(default)]
    pub result_check: Option<ResultCheckRecord>,
    pub note: Option<String>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BusinessVerification {
    pub operation_id: String,
    pub target_id: String,
    pub profile: String,
    /// None means insufficient evidence, never successful recovery.
    pub healthy: Option<bool>,
    pub executor_stopped: bool,
    pub evidence_refs: Vec<String>,
    pub verified_at_ms: u64,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CheckedExecution {
    Executed,
    Failed,
    NotExecuted,
    Unknown,
}

/// Trusted operation-status evidence from the bound executor, independent of
/// business health. A healthy target does not prove that this operation ran.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExecutionResultCheck {
    pub operation_id: String,
    pub target_id: String,
    pub executor_id: String,
    pub outcome: CheckedExecution,
    pub executor_stopped: bool,
    pub evidence_refs: Vec<String>,
    /// Unix-millisecond timestamp attached to the trusted executor evidence.
    pub checked_at_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ResultCheckRecord {
    pub execution: ExecutionResultCheck,
    /// Audit attribution supplied by the trusted caller, not authentication.
    pub actor: String,
}
