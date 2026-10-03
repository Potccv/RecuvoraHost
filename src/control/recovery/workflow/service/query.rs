//! Structured domain progress and bounded in-memory queries.
use super::*;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryNextStep {
    StartRepair,
    AssociateApproval,
    CheckApproval,
    AwaitExecution,
    VerifyBusiness,
    CheckExecutionResult,
    ExplicitResume,
    Finished,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecoveryTaskSummary {
    pub task_id: String,
    pub revision: u64,
    pub stage: RecoveryStage,
    pub next_step: RecoveryNextStep,
    pub note: Option<String>,
}
impl From<&RecoveryTask> for RecoveryTaskSummary {
    fn from(task: &RecoveryTask) -> Self {
        let next_step = match task.stage {
            RecoveryStage::Queued => RecoveryNextStep::StartRepair,
            RecoveryStage::AwaitingApproval if task.approval_id.is_none() => {
                RecoveryNextStep::AssociateApproval
            }
            RecoveryStage::AwaitingApproval => RecoveryNextStep::CheckApproval,
            RecoveryStage::Executing => RecoveryNextStep::AwaitExecution,
            RecoveryStage::Verifying => RecoveryNextStep::VerifyBusiness,
            RecoveryStage::Unknown => RecoveryNextStep::CheckExecutionResult,
            RecoveryStage::Paused => RecoveryNextStep::ExplicitResume,
            _ => RecoveryNextStep::Finished,
        };
        Self {
            task_id: task.id.clone(),
            revision: task.revision,
            stage: task.stage.clone(),
            next_step,
            note: task.note.clone(),
        }
    }
}
