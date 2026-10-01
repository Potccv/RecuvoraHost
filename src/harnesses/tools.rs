//! Explicit host tool definitions and trusted handler contracts.
use super::{HarnessCancellation, HarnessError};
use std::future::Future;
use std::pin::Pin;

#[derive(Clone, Debug)]
pub struct HarnessTool {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

#[derive(Clone, Debug)]
pub struct HarnessToolCall {
    pub harness_id: String,
    pub thread_id: String,
    pub turn_id: String,
    pub call_id: String,
    pub tool: String,
    pub arguments: serde_json::Value,
    pub cancellation: HarnessCancellation,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HarnessToolResult {
    pub content: String,
    pub success: bool,
}

pub type HarnessToolFuture<'a> =
    Pin<Box<dyn Future<Output = Result<HarnessToolResult, HarnessError>> + Send + 'a>>;

/// Trusted handlers must validate parameters and authority before effects,
/// enforce their own finite deadline, observe cancellation before dispatch,
/// and durably record effects before returning. Cancellation never rolls back
/// an effect. Providers await every dispatched handler, including on failure.
pub trait HarnessToolHandler: Send + Sync {
    fn call<'a>(&'a self, call: HarnessToolCall) -> HarnessToolFuture<'a>;
}
