//! Allowlisted read-only dispatch and the internal reserved Harness route.
use super::ExtensionRegistry;
use super::callbacks::ReadRouter;
use super::registry::{dispatch_error, method_for};
use super::{CallbackHandler, ExtensionCall, ExtensionError, ExtensionKind};
use crate::protocol;
use recuvora_core::operation::Cancellation;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;

impl ExtensionRegistry {
    /// Reserved repair route, reachable only from trusted crate consumers.
    /// execute_script is dispatched by actions after durable permit consumption.
    pub(crate) async fn call_repair(
        &self,
        id: &str,
        method: &str,
        params: Value,
        timeout: Duration,
        cancellation: Cancellation,
    ) -> Result<Value, ExtensionError> {
        let registry = self.clone();
        let id = id.to_owned();
        let method = method.to_owned();
        self.calls
            .run(cancellation.clone(), async move {
                let entry = registry.entry(&id)?;
                if entry.definition.kind != ExtensionKind::Node
                    || !matches!(
                        method.as_str(),
                        "inspect" | "verify" | "reconcile" | "execute_script"
                    )
                {
                    return Err(ExtensionError::Rejected("invalid repair node route".into()));
                }
                let declaration = method_for(entry, "recuvora.repair", 1, &method)?;
                if declaration.read_only != (method != "execute_script") {
                    return Err(ExtensionError::Rejected(
                        "repair method effect mismatch".into(),
                    ));
                }
                protocol::validate_value(&declaration.input_schema, &params)?;
                let operation_id = params
                    .get("operation")
                    .and_then(|operation| operation.get("operation_id"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let capacity = entry.ordinary.clone().try_acquire_owned().map_err(|_| {
                    ExtensionError::Rejected("repair node capacity exhausted".into())
                })?;
                let result = entry
                    .client
                    .call_with_settings(
                        ExtensionCall {
                            contract: "recuvora.repair".into(),
                            version: 1,
                            method: method.clone(),
                            params,
                            timeout,
                        },
                        entry.metadata.clone(),
                        cancellation,
                        None,
                        Some(capacity),
                        &registry.settings.protocol,
                    )
                    .await?;
                protocol::validate_value(&declaration.output_schema, &result).map_err(|error| {
                    if method == "execute_script" {
                        ExtensionError::Unknown {
                            call_id: operation_id,
                            message: error.to_string(),
                        }
                    } else {
                        error
                    }
                })?;
                Ok(result)
            })
            .await
            .map_err(dispatch_error)?
    }

    // The explicit contract/version/method triplet is the wire routing identity;
    // keeping it separate from cancellation and payload makes authority visible.
    #[allow(clippy::too_many_arguments)]
    pub async fn call_read_only(
        &self,
        id: &str,
        contract: &str,
        version: u32,
        method: &str,
        params: Value,
        timeout: Duration,
        cancellation: Cancellation,
    ) -> Result<Value, ExtensionError> {
        let registry = self.clone();
        let (id, contract, method) = (id.to_owned(), contract.to_owned(), method.to_owned());
        self.calls
            .run(cancellation.clone(), async move {
                registry
                    .call_read_only_inner(
                        &id,
                        &contract,
                        version,
                        &method,
                        params,
                        timeout,
                        cancellation,
                    )
                    .await
            })
            .await
            .map_err(dispatch_error)?
    }

    #[allow(clippy::too_many_arguments)]
    async fn call_read_only_inner(
        &self,
        id: &str,
        contract: &str,
        version: u32,
        method: &str,
        params: Value,
        timeout: Duration,
        cancellation: Cancellation,
    ) -> Result<Value, ExtensionError> {
        let entry = self.entry(id)?;
        let declaration = method_for(entry, contract, version, method)?;
        if !declaration.read_only || contract.starts_with("recuvora.") {
            return Err(ExtensionError::Rejected(
                "generic extension calls permit only non-reserved read-only contracts".into(),
            ));
        }
        protocol::validate_value(&declaration.input_schema, &params)?;
        let handler = if entry.definition.kind == ExtensionKind::Plugin {
            Some(Arc::new(ReadRouter {
                registry: self.clone(),
                allowed: entry.definition.allow_nodes.clone(),
            }) as Arc<dyn CallbackHandler>)
        } else {
            None
        };
        let permit = entry
            .ordinary
            .clone()
            .try_acquire_owned()
            .map_err(|_| ExtensionError::Rejected("extension capacity exhausted".into()))?;
        let result = entry
            .client
            .call_with_settings(
                ExtensionCall {
                    contract: contract.into(),
                    version,
                    method: method.into(),
                    params,
                    timeout,
                },
                entry.metadata.clone(),
                cancellation,
                handler,
                Some(permit),
                &self.settings.protocol,
            )
            .await;
        let result = result?;
        protocol::validate_value(&declaration.output_schema, &result)?;
        Ok(result)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn call_harness(
        &self,
        id: &str,
        method: &str,
        params: Value,
        timeout: Duration,
        cancellation: Cancellation,
        handler: Option<Arc<dyn CallbackHandler>>,
        approval: bool,
    ) -> Result<Value, ExtensionError> {
        let registry = self.clone();
        let (id, method) = (id.to_owned(), method.to_owned());
        self.calls
            .run(cancellation.clone(), async move {
                registry
                    .call_harness_inner(
                        &id,
                        &method,
                        params,
                        timeout,
                        cancellation,
                        handler,
                        approval,
                    )
                    .await
            })
            .await
            .map_err(dispatch_error)?
    }

    #[allow(clippy::too_many_arguments)]
    async fn call_harness_inner(
        &self,
        id: &str,
        method: &str,
        params: Value,
        timeout: Duration,
        cancellation: Cancellation,
        handler: Option<Arc<dyn CallbackHandler>>,
        approval: bool,
    ) -> Result<Value, ExtensionError> {
        let entry = self.entry(id)?;
        if entry.definition.kind != ExtensionKind::Node {
            return Err(ExtensionError::Rejected(
                "Harness provider must be a node".into(),
            ));
        }
        let _ = method_for(entry, "recuvora.harness", 1, method)?;
        let permit = if approval {
            entry.approval.clone()
        } else {
            entry.ordinary.clone()
        }
        .try_acquire_owned()
        .map_err(|_| ExtensionError::Rejected("Harness role capacity exhausted".into()))?;
        entry
            .client
            .call_with_settings(
                ExtensionCall {
                    contract: "recuvora.harness".into(),
                    version: 1,
                    method: method.into(),
                    params,
                    timeout,
                },
                entry.metadata.clone(),
                cancellation,
                handler,
                Some(permit),
                &self.settings.protocol,
            )
            .await
    }
}
