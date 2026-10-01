//! External extension adapter for the Core monitoring source port.

use crate::integrations::extensions::ExtensionRegistry;
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
