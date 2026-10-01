//! Model turn orchestration and classification of durable operation outcomes.
use super::*;

impl RepairSession {
    pub async fn run(
        self: &Arc<Self>,
        task_id: String,
        prompt: String,
        cancellation: HarnessCancellation,
    ) -> Result<RepairResult, WorkflowError> {
        if task_id.is_empty()
            || task_id.len() > 128
            || task_id.chars().any(char::is_control)
            || prompt.trim().is_empty()
            || prompt.len() > 8192
        {
            return Err(WorkflowError::Invalid(
                "task ID must be 1..128 printable bytes; prompt 1..8192 bytes".into(),
            ));
        }
        let previous = self.records()?;
        if previous
            .iter()
            .any(|r| r.request.operation.task_id == task_id)
        {
            return Err(WorkflowError::Invalid("task already has durable operations; inspect/apply existing requests instead of replaying it".into()));
        }
        if previous.iter().any(|r| {
            same_root(&r.request.operation, self.files.root())
                && matches!(r.state, ApprovalState::Unknown | ApprovalState::Executing)
        }) {
            return Err(ApprovalError::TargetBusy.into());
        }
        let registry = self
            .registry
            .as_ref()
            .ok_or_else(|| WorkflowError::Invalid("execution registry is unavailable".into()))?;
        let handler = Arc::new(RepairTools {
            session: self.clone(),
            task_id: task_id.clone(),
            user_prompt: prompt.clone(),
            calls: AtomicUsize::new(0),
            stopped: AtomicBool::new(false),
            cancellation: cancellation.clone(),
        });
        let instructions = format!(
            "Work on this user request using only recuvora_read_text and recuvora_replace_text. Allowed files: {}. Each replacement is a complete UTF-8 file and must include its exact current content as expected. Writes are reviewed by the host. If a tool reports waiting_human, denied, expired, unknown, failed or canceled, stop and describe the pending request; do not try a workaround. After applied changes, report them accurately. Content verification is not evidence of business repair: do not claim tests or business recovery without evidence. User request:\n{}",
            serde_json::to_string(&self.config.allowed_files)?,
            prompt
        );
        let request = model_request(
            self.config.execution_workspace.as_ref(),
            self.files.root(),
            instructions,
        )
        .with_visibility(ConversationVisibility::Hidden)
        .with_tools(repair_tools(), handler.clone())
        .with_cancellation(cancellation.clone())
        .with_timeout(Duration::from_secs(self.config.timeout_secs));
        let result = registry
            .run(Some(&self.config.execution_harness), request)
            .await;
        let records: Vec<_> = self
            .records()?
            .into_iter()
            .filter(|r| r.request.operation.task_id == task_id)
            .collect();
        let status = if records
            .iter()
            .any(|r| matches!(r.state, ApprovalState::Executing | ApprovalState::Unknown))
        {
            RepairStatus::Unknown
        } else if records.iter().any(|r| {
            matches!(
                r.state,
                ApprovalState::Pending | ApprovalState::WaitingHuman | ApprovalState::Approved
            )
        }) {
            RepairStatus::WaitingHuman
        } else if records.iter().any(|r| {
            matches!(
                r.state,
                ApprovalState::Denied
                    | ApprovalState::Revoked
                    | ApprovalState::Expired
                    | ApprovalState::Failed
            )
        }) {
            RepairStatus::Blocked
        } else if cancellation.is_cancelled() {
            RepairStatus::Canceled
        } else if result.is_err() || handler.stopped.load(Ordering::Acquire) {
            RepairStatus::Failed
        } else {
            RepairStatus::Completed
        };
        let (response, harness_error, execution_context) = match result {
            Ok(result) => (
                Some(result.final_response),
                None,
                Some(RepairExecutionContext {
                    harness_id: result.harness_id,
                    thread_id: result.thread_id,
                    session_id: result.session_id,
                }),
            ),
            Err(error) => (None, Some(error), None),
        };
        Ok(RepairResult {
            status,
            task_id,
            final_response: response,
            harness_error,
            operations: records,
            execution_context,
        })
    }
}
