//! External extension adapter for the Host monitoring source port.

use crate::integrations::extensions::{ExtensionKind, ExtensionRegistry};
use crate::monitoring::{
    DiscoveryFuture, MonitorError, ObservationFuture, ObservationRequest, ObservationSource,
};
use crate::runtime::operation::Cancellation;
use std::sync::Arc;

pub struct RegistryObservationSource(pub Arc<ExtensionRegistry>);

impl RegistryObservationSource {
    pub fn new(registry: Arc<ExtensionRegistry>) -> Self {
        Self(registry)
    }
}

impl ObservationSource for RegistryObservationSource {
    fn discover(
        &self,
        request: ObservationRequest,
        cancellation: Cancellation,
    ) -> DiscoveryFuture<'_> {
        Box::pin(async move {
            let value = self
                .0
                .call_read_only(
                    &request.extension_id,
                    &request.contract,
                    request.version,
                    &request.method,
                    request.params,
                    request.timeout,
                    cancellation,
                )
                .await
                .map_err(|error| MonitorError::Observation(error.to_string()))?;
            serde_json::from_value(value)
                .map_err(|error| MonitorError::Observation(error.to_string()))
        })
    }

    fn poll(
        &self,
        request: ObservationRequest,
        cancellation: Cancellation,
    ) -> ObservationFuture<'_> {
        Box::pin(async move {
            if self
                .0
                .metadata(&request.extension_id)
                .is_none_or(|metadata| metadata.kind != ExtensionKind::Node)
            {
                return Err(MonitorError::Observation(
                    "error logs require a registered Node source".into(),
                ));
            }

            let value = self
                .0
                .call_read_only(
                    &request.extension_id,
                    &request.contract,
                    request.version,
                    &request.method,
                    request.params,
                    request.timeout,
                    cancellation,
                )
                .await
                .map_err(|error| MonitorError::Observation(error.to_string()))?;
            serde_json::from_value(value)
                .map_err(|error| MonitorError::Observation(error.to_string()))
        })
    }
}
