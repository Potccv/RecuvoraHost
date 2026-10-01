//! Bounded model-visible read and proposal tools; no human decision endpoint.
use super::*;

impl HarnessToolHandler for RepairTools {
    fn call<'a>(&'a self, call: HarnessToolCall) -> HarnessToolFuture<'a> {
        Box::pin(async move {
            match self.handle(call).await {
                Ok(value) => Ok(HarnessToolResult {
                    content: value.to_string(),
                    success: true,
                }),
                Err(error) => {
                    self.stopped.store(true, Ordering::Release);
                    Ok(HarnessToolResult { content: json!({"status":"failed","message":error.to_string(),"auto_retry":false}).to_string(), success:false })
                }
            }
        })
    }
}

impl RepairTools {
    async fn handle(&self, call: HarnessToolCall) -> Result<Value, WorkflowError> {
        if self.stopped.load(Ordering::Acquire)
            || self.cancellation.is_cancelled()
            || call.cancellation.is_cancelled()
        {
            return Err(WorkflowError::Invalid(
                "task stopped; no further tool calls allowed".into(),
            ));
        }
        if call.harness_id != self.session.config.execution_harness
            || self.calls.fetch_add(1, Ordering::AcqRel) >= self.session.config.max_tool_calls
        {
            return Err(WorkflowError::Invalid(
                "tool identity mismatch or budget exhausted".into(),
            ));
        }
        match call.tool.as_str() {
            "recuvora_read_text" => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct ReadArgs {
                    path: String,
                }
                let args: ReadArgs = serde_json::from_value(call.arguments)?;
                Ok(
                    json!({"status":"read","path":args.path,"content":self.session.files.read(&args.path)?}),
                )
            }
            "recuvora_replace_text" => {
                let edit: TextEdit = serde_json::from_value(call.arguments)?;
                // Establish a real, reviewable operation before asking a reviewer.
                drop(self.session.files.prepare(&edit)?);
                let operation = ProposedOperation {
                    task_id: self.task_id.clone(),
                    task_revision: 1,
                    operation_id: format!("{}:{}", self.task_id, call.call_id),
                    target: self.session.config.target_id.clone(),
                    action: json!({"kind":"replace_text","target_root":self.session.files.root(),"path":edit.path,
                        "expected":edit.expected,"replacement":edit.replacement,"user_request":self.user_prompt,
                        "execution_context":{"harness_id":call.harness_id,"thread_id":call.thread_id,"turn_id":call.turn_id,"call_id":call.call_id}}),
                };
                let record = self.session.store()?.request(
                    operation,
                    self.session.config.policy.clone(),
                    now()?,
                )?;
                let id = record.request.request_id.clone();
                if record.state == ApprovalState::Pending {
                    self.review(&record, &call.cancellation).await?;
                }
                if self.cancellation.is_cancelled() || call.cancellation.is_cancelled() {
                    let current = self.session.record(&id)?;
                    if matches!(
                        current.state,
                        ApprovalState::Pending
                            | ApprovalState::WaitingHuman
                            | ApprovalState::Approved
                    ) {
                        self.session.store()?.cancel(
                            &id,
                            "task canceled before execution".into(),
                            now()?,
                        )?;
                    }
                    self.stopped.store(true, Ordering::Release);
                    return Ok(json!({"status":"canceled","request_id":id}));
                }
                let mut record = self.session.record(&id)?;
                if record.state == ApprovalState::Approved {
                    record = self.session.apply(&id, &call.cancellation)?;
                }
                if record.state != ApprovalState::Executed {
                    self.stopped.store(true, Ordering::Release);
                }
                Ok(
                    json!({"status":record.state,"request_id":id,"note":record.note,"auto_retry":false}),
                )
            }
            _ => Err(WorkflowError::Invalid("unknown host tool".into())),
        }
    }
}

pub(super) fn repair_tools() -> Vec<HarnessTool> {
    vec![HarnessTool { name:"recuvora_read_text".into(),description:"Read one explicitly allowed UTF-8 file (at most 16 KiB).".into(),
        input_schema:json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"],"additionalProperties":false}) },
        HarnessTool { name:"recuvora_replace_text".into(),description:"Request approval and replace one allowed file only if its entire content equals expected. Never retries. Stop if approval is denied, waiting, expired or result is unknown.".into(),
        input_schema:json!({"type":"object","properties":{"path":{"type":"string"},"expected":{"type":"string"},"replacement":{"type":"string"}},"required":["path","expected","replacement"],"additionalProperties":false}) }]
}
