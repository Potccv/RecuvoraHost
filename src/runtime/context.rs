//! Instance context and typed service publication/resolution.
use super::state::{ServiceEntry, SharedState};
use super::{FrameworkError, InstanceId, LifecycleOptions, ServiceKey};
use std::any::Any;
use std::sync::{Arc, Mutex, MutexGuard};

/// Clones share one instance ownership boundary and become inactive at shutdown.
#[derive(Clone)]
pub struct ModuleContext {
    pub(super) instance: InstanceId,
    pub(super) state: Arc<Mutex<SharedState>>,
    pub(super) options: LifecycleOptions,
}

impl ModuleContext {
    pub(super) fn lock(&self) -> Result<MutexGuard<'_, SharedState>, FrameworkError> {
        self.state.lock().map_err(|_| FrameworkError::StatePoisoned)
    }

    pub fn instance(&self) -> &InstanceId {
        &self.instance
    }

    pub fn is_accepting(&self) -> bool {
        self.lock()
            .is_ok_and(|state| state.active(&self.instance).is_ok())
    }

    /// Publish a sized service value; a contract may wrap an Arc<dyn Trait>.
    pub fn publish<T: Any + Send + Sync>(
        &self,
        service: ServiceKey,
        value: Arc<T>,
    ) -> Result<(), FrameworkError> {
        let mut state = self.lock()?;
        if !state
            .active(&self.instance)?
            .metadata
            .provides
            .contains(&service)
        {
            return Err(FrameworkError::UndeclaredService {
                instance: self.instance.clone(),
                service,
            });
        }
        if state.services.contains_key(&service) {
            return Err(FrameworkError::ProviderConflict(service));
        }
        state.services.insert(
            service,
            ServiceEntry {
                owner: self.instance.clone(),
                value,
            },
        );
        Ok(())
    }

    /// Resolution is restricted to declared requirements and owned services.
    /// Already acquired Arcs require the service to stop new calls and wait for active calls.
    pub fn service<T: Any + Send + Sync>(
        &self,
        service: &ServiceKey,
    ) -> Result<Arc<T>, FrameworkError> {
        let state = self.lock()?;
        let owner = state.active(&self.instance)?;
        if !owner.metadata.requires.contains(service) && !owner.metadata.provides.contains(service)
        {
            return Err(FrameworkError::UndeclaredService {
                instance: self.instance.clone(),
                service: service.clone(),
            });
        }
        let entry = state
            .services
            .get(service)
            .ok_or_else(|| FrameworkError::ServiceUnavailable(service.clone()))?;
        state.active(&entry.owner)?;
        Arc::downcast(entry.value.clone())
            .map_err(|_| FrameworkError::ServiceTypeMismatch(service.clone()))
    }
}
