//! Instance ownership and shared service/event registrations.
use super::events::Listener;
use super::resources::Resources;
use super::{FrameworkError, InstanceId, ModuleMetadata, ServiceKey};
use std::any::Any;
use std::collections::BTreeMap;
use std::sync::Arc;

pub(super) struct ServiceEntry {
    pub(super) owner: InstanceId,
    pub(super) value: Arc<dyn Any + Send + Sync>,
}

pub(super) struct InstanceState {
    pub(super) metadata: ModuleMetadata,
    pub(super) accepting: bool,
    pub(super) resources: Resources,
}

#[derive(Default)]
pub(super) struct SharedState {
    pub(super) instances: BTreeMap<InstanceId, InstanceState>,
    pub(super) services: BTreeMap<ServiceKey, ServiceEntry>,
    pub(super) listeners: Vec<Listener>,
}

impl SharedState {
    pub(super) fn active(&self, instance: &InstanceId) -> Result<&InstanceState, FrameworkError> {
        self.instances
            .get(instance)
            .filter(|state| state.accepting)
            .ok_or_else(|| FrameworkError::InstanceInactive(instance.clone()))
    }

    pub(super) fn take_resources(&mut self, instance: &InstanceId) -> Resources {
        self.services
            .retain(|_, service| &service.owner != instance);
        self.listeners
            .retain(|listener| &listener.owner != instance);
        if let Some(state) = self.instances.get_mut(instance) {
            state.accepting = false;
            std::mem::take(&mut state.resources)
        } else {
            Resources::default()
        }
    }
}
