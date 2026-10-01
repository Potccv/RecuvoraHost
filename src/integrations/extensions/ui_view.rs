//! Optional data-only view registration and isolated descriptor-call capacity.
use super::ExtensionRegistry;
use super::registry::{dispatch_error, method_for};
use super::{ExtensionCall, ExtensionError, ExtensionKind};
use crate::protocol;
use crate::runtime::operation::Cancellation;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::Duration;

pub const MONITORING_VIEW_CAPABILITY: &str = "recuvora.monitoring_view.v1";
pub const MONITORING_VIEW_METHOD: &str = "describe_monitoring_view";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MonitoringViewRegistration {
    pub extension_id: String,
    pub contract: String,
    pub version: u32,
    pub method: String,
}

impl ExtensionRegistry {
    pub fn monitoring_view_registration(
        &self,
        id: &str,
    ) -> Result<Option<MonitoringViewRegistration>, ExtensionError> {
        let entry = self.entry(id)?;
        if !entry
            .metadata
            .capabilities
            .iter()
            .any(|capability| capability == MONITORING_VIEW_CAPABILITY)
        {
            return Ok(None);
        }
        if entry.definition.kind != ExtensionKind::Plugin {
            return Err(ExtensionError::Rejected(
                "monitoring view registration requires a plugin".into(),
            ));
        }
        let candidates = entry
            .metadata
            .contracts
            .iter()
            .flat_map(|contract| {
                contract
                    .methods
                    .iter()
                    .filter(|method| method.name == MONITORING_VIEW_METHOD)
                    .map(move |method| (contract, method))
            })
            .collect::<Vec<_>>();
        if candidates.len() != 1 {
            return Err(ExtensionError::Rejected(
                "monitoring view capability requires exactly one descriptor method".into(),
            ));
        }
        let (contract, method) = candidates[0];
        let owned = contract.id != "recuvora"
            && !contract.id.starts_with("recuvora.")
            && self.contract_owner(&contract.id, contract.version)
                == Some(entry.definition.id.as_str());
        if !owned || !method.read_only {
            return Err(ExtensionError::Rejected(
                "monitoring view descriptor must be a read-only method in the plugin namespace"
                    .into(),
            ));
        }
        protocol::validate_value(&method.input_schema, &json!({"schema_version":1})).map_err(
            |_| {
                ExtensionError::Rejected(
                    "monitoring view descriptor must accept the fixed schema_version 1 input"
                        .into(),
                )
            },
        )?;
        if !entry.definition.allow_calls.iter().any(|allowed| {
            allowed.contract == contract.id
                && allowed.version == contract.version
                && allowed.method == method.name
        }) {
            return Err(ExtensionError::Rejected(
                "monitoring view descriptor is not in the trusted call allowlist".into(),
            ));
        }
        Ok(Some(MonitoringViewRegistration {
            extension_id: entry.definition.id.clone(),
            contract: contract.id.clone(),
            version: contract.version,
            method: method.name.clone(),
        }))
    }

    /// Calls an optional plugin monitoring-view descriptor without granting it
    /// the node-read callback authority available to ordinary plugin calls.
    pub async fn call_monitoring_view(
        &self,
        registration: &MonitoringViewRegistration,
        params: Value,
        timeout: Duration,
        cancellation: Cancellation,
    ) -> Result<Value, ExtensionError> {
        let registry = self.clone();
        let registration = registration.clone();
        self.calls
            .run(cancellation.clone(), async move {
                registry
                    .call_monitoring_view_inner(&registration, params, timeout, cancellation)
                    .await
            })
            .await
            .map_err(dispatch_error)?
    }

    async fn call_monitoring_view_inner(
        &self,
        registration: &MonitoringViewRegistration,
        params: Value,
        timeout: Duration,
        cancellation: Cancellation,
    ) -> Result<Value, ExtensionError> {
        let current = self
            .monitoring_view_registration(&registration.extension_id)?
            .ok_or_else(|| {
                ExtensionError::Rejected("extension did not register a monitoring view".into())
            })?;
        if &current != registration {
            return Err(ExtensionError::Rejected(
                "monitoring view registration does not match the current declaration".into(),
            ));
        }
        let entry = self.entry(&registration.extension_id)?;
        let declaration = method_for(
            entry,
            &registration.contract,
            registration.version,
            &registration.method,
        )?;
        if !declaration.read_only
            || registration.contract == "recuvora"
            || registration.contract.starts_with("recuvora.")
        {
            return Err(ExtensionError::Rejected(
                "monitoring view descriptor must be a non-reserved read-only method".into(),
            ));
        }
        protocol::validate_value(&declaration.input_schema, &params)?;
        let permit = entry
            .monitoring_view
            .clone()
            .try_acquire_owned()
            .map_err(|_| ExtensionError::Rejected("monitoring view capacity exhausted".into()))?;
        let result = entry
            .client
            .call_with_settings(
                ExtensionCall {
                    contract: registration.contract.clone(),
                    version: registration.version,
                    method: registration.method.clone(),
                    params,
                    timeout,
                },
                entry.metadata.clone(),
                cancellation,
                // View declarations are data-only. In particular, they cannot
                // use the normal plugin callback router to read from nodes.
                None,
                Some(permit),
                &self.settings.protocol,
            )
            .await?;
        protocol::validate_value(&declaration.output_schema, &result).map_err(|error| {
            ExtensionError::Protocol(format!(
                "monitoring view descriptor output violates its schema: {error}"
            ))
        })?;
        Ok(result)
    }
}
