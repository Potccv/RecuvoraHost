//! Hold current incident and target authority through the actual network send.
use super::incident_gate::incident;
use super::*;
use recuvora_core::recovery::engine::SessionCommand;
use std::sync::{Arc, Mutex};

pub(super) struct ExecutionDispatch {
    pub(super) service: Arc<RecoveryService>,
    pub(super) task_id: String,
    pub(super) lease: Mutex<Option<Box<dyn IncidentDispatchLease>>>,
}
impl RepairActionGuard for ExecutionDispatch {
    fn prepare_repair_action(
        &self,
        action: &recuvora_core::recovery::knowledge::RepairArtifact,
    ) -> Result<(), crate::integrations::extensions::ExtensionError> {
        use crate::integrations::extensions::ExtensionError;
        crate::integrations::extensions::DispatchGuard::validate(self)?;
        let mut state =
            lock(&self.service.state).map_err(|e| ExtensionError::Rejected(e.to_string()))?;
        let state = state
            .as_mut()
            .ok_or_else(|| ExtensionError::Rejected("service stopped".into()))?;
        let task = state
            .task(&self.task_id)
            .map_err(|e| ExtensionError::Rejected(e.to_string()))?;
        state
            .apply(
                SessionCommand::PrepareAction {
                    task_id: task.id,
                    action: action.clone(),
                },
                self.service.clock.now_ms(),
            )
            .map_err(|e| ExtensionError::Rejected(e.to_string()))?;
        Ok(())
    }
}
impl crate::integrations::extensions::DispatchGuard for ExecutionDispatch {
    fn validate(&self) -> Result<(), crate::integrations::extensions::ExtensionError> {
        let check = || -> Result<(), RecoveryError> {
            let lease = lock(&self.lease)?;
            let lease = lease
                .as_ref()
                .ok_or_else(|| service("dispatch gate released"))?;
            self.service.owner()?;
            let mut state = lock(&self.service.state)?;
            let state = state.as_mut().ok_or(RecoveryError::Stopped)?;
            state.root_lock.validate()?;
            let task = state.task(&self.task_id)?;
            let incident = incident(&task.problem, lease.current()?)?;
            if !state.dispatch_pending {
                return Err(RecoveryError::Conflict);
            }
            state.journal.available().map_err(service)?;
            state.dispatch.available().map_err(service)?;
            state.session.validate_dispatch(
                &self.task_id,
                &incident,
                self.service.clock.now_ms(),
            )?;
            Ok(())
        };
        check().map_err(|error| {
            crate::integrations::extensions::ExtensionError::Rejected(error.to_string())
        })
    }
    fn release(&self) {
        if let Ok(mut lease) = self.lease.lock()
            && lease.is_some()
        {
            if let Ok(mut state) = self.service.state.lock()
                && let Some(state) = state.as_mut()
            {
                state.dispatch_pending = false;
            }
            lease.take();
        }
    }
}
impl Drop for ExecutionDispatch {
    fn drop(&mut self) {
        crate::integrations::extensions::DispatchGuard::release(self);
    }
}
