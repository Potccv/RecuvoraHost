//! Per-instance background tasks and synchronous disposal registrations.
use super::state::SharedState;
use super::{FrameworkError, ModuleContext};
use std::future::Future;
use tokio::task::JoinHandle;

#[derive(Default)]
pub(super) struct Resources {
    pub(super) disposers: Vec<Box<dyn FnOnce() + Send>>,
    pub(super) tasks: Vec<JoinHandle<()>>,
}

impl ModuleContext {
    /// Callbacks are quick synchronous disposers, invoked in reverse registration order.
    pub fn on_dispose(
        &self,
        dispose: impl FnOnce() + Send + 'static,
    ) -> Result<(), FrameworkError> {
        let mut state = self.lock()?;
        self.check_resource_capacity(&state)?;
        let owner = state
            .instances
            .get_mut(&self.instance)
            .ok_or_else(|| FrameworkError::InstanceInactive(self.instance.clone()))?;
        owner.resources.disposers.push(Box::new(dispose));
        Ok(())
    }

    /// Owned async tasks are aborted and joined at cleanup; futures must cooperate.
    pub fn spawn(
        &self,
        task: impl Future<Output = ()> + Send + 'static,
    ) -> Result<(), FrameworkError> {
        let mut state = self.lock()?;
        self.check_resource_capacity(&state)?;
        let owner = state
            .instances
            .get_mut(&self.instance)
            .ok_or_else(|| FrameworkError::InstanceInactive(self.instance.clone()))?;
        let runtime = tokio::runtime::Handle::try_current().map_err(|_| {
            FrameworkError::InvalidConfiguration("background task requires a Tokio runtime".into())
        })?;
        owner.resources.tasks.push(runtime.spawn(task));
        Ok(())
    }

    fn check_resource_capacity(&self, state: &SharedState) -> Result<(), FrameworkError> {
        let owner = state.active(&self.instance)?;
        if owner.resources.disposers.len() + owner.resources.tasks.len()
            >= self.options.max_resources_per_module
        {
            return Err(FrameworkError::CapacityExceeded(
                "instance resources".into(),
            ));
        }
        Ok(())
    }
}
