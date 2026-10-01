//! Restricted plugin callbacks into explicitly allowed read-only node services.
use super::ExtensionRegistry;
use super::{CallbackFuture, CallbackHandler, ExtensionError, ExtensionKind};
use recuvora_core::operation::Cancellation;
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadRequest {
    node_id: String,
    contract: String,
    version: u32,
    method: String,
    params: Value,
}
pub(super) struct ReadRouter {
    pub(super) registry: ExtensionRegistry,
    pub(super) allowed: Vec<String>,
}
impl CallbackHandler for ReadRouter {
    fn call<'a>(
        &'a self,
        method: String,
        params: Value,
        cancellation: Cancellation,
    ) -> CallbackFuture<'a> {
        Box::pin(async move {
            if method != "service.call" {
                return Err(ExtensionError::Rejected(
                    "unsupported plugin callback".into(),
                ));
            }
            let request: ReadRequest = serde_json::from_value(params)
                .map_err(|e| ExtensionError::Rejected(e.to_string()))?;
            if !self.allowed.contains(&request.node_id)
                || self
                    .registry
                    .metadata(&request.node_id)
                    .is_none_or(|m| m.kind != ExtensionKind::Node)
            {
                return Err(ExtensionError::Rejected(
                    "plugin cannot access this node".into(),
                ));
            }
            self.registry
                .call_read_only(
                    &request.node_id,
                    &request.contract,
                    request.version,
                    &request.method,
                    request.params,
                    self.registry.settings.plugin_callback_timeout(),
                    cancellation,
                )
                .await
        })
    }
}
