//! Tool correlation, replay rejection and bounded trusted callback dispatch.
use crate::harnesses::{HarnessCancellation, HarnessTool, HarnessToolCall, HarnessToolHandler};
use crate::integrations::extensions::{CallbackFuture, CallbackHandler, ExtensionError};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireTool {
    harness_id: String,
    thread_id: String,
    turn_id: String,
    call_id: String,
    tool: String,
    arguments: Value,
}

fn valid_project_id(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 512 && !value.chars().any(char::is_control)
}
#[derive(Default)]
pub(super) struct ToolScope {
    pub(super) thread: Option<String>,
    turn: Option<String>,
    calls: BTreeSet<String>,
}
pub(super) struct ToolRouter {
    pub(super) harness_id: String,
    pub(super) tools: Vec<HarnessTool>,
    pub(super) handler: Arc<dyn HarnessToolHandler>,
    pub(super) scope: Arc<Mutex<ToolScope>>,
}
impl CallbackHandler for ToolRouter {
    fn call<'a>(
        &'a self,
        method: String,
        params: Value,
        cancellation: HarnessCancellation,
    ) -> CallbackFuture<'a> {
        Box::pin(async move {
            if method != "tool" {
                return Err(ExtensionError::Rejected(
                    "only declared host tools are supported".into(),
                ));
            }
            let tool: WireTool = serde_json::from_value(params)
                .map_err(|e| ExtensionError::Rejected(e.to_string()))?;
            if tool.harness_id != self.harness_id
                || !valid_project_id(&tool.thread_id)
                || !valid_project_id(&tool.turn_id)
                || !valid_project_id(&tool.call_id)
                || tool.arguments.to_string().len() > 64 * 1024
                || !tool.arguments.is_object()
                || !self.tools.iter().any(|t| t.name == tool.tool)
            {
                return Err(ExtensionError::Rejected(
                    "invalid tool scope, arguments or tool name".into(),
                ));
            }
            {
                let mut scope = self
                    .scope
                    .lock()
                    .map_err(|_| ExtensionError::Rejected("tool scope unavailable".into()))?;
                if scope
                    .thread
                    .as_ref()
                    .is_some_and(|id| id != &tool.thread_id)
                    || scope.turn.as_ref().is_some_and(|id| id != &tool.turn_id)
                    || scope.calls.len() >= 64
                    || !scope.calls.insert(tool.call_id.clone())
                {
                    return Err(ExtensionError::Rejected(
                        "duplicate tool or changed conversation scope".into(),
                    ));
                }
                scope.thread = Some(tool.thread_id.clone());
                scope.turn = Some(tool.turn_id.clone());
            }
            if cancellation.is_cancelled() {
                return Err(ExtensionError::Cancelled);
            }
            let result = self
                .handler
                .call(HarnessToolCall {
                    harness_id: tool.harness_id,
                    thread_id: tool.thread_id,
                    turn_id: tool.turn_id,
                    call_id: tool.call_id,
                    tool: tool.tool,
                    arguments: tool.arguments,
                    cancellation,
                })
                .await
                .map_err(|e| ExtensionError::Rejected(e.to_string()))?;
            if result.content.len() > 256 * 1024 {
                return Err(ExtensionError::Rejected(
                    "tool output exceeds 256 KiB".into(),
                ));
            }
            Ok(json!({"content":result.content,"success":result.success}))
        })
    }
}
