//! Owned Host services, queries and coordinated shutdown.
use crate::harnesses::{HarnessError, HarnessRegistry};
use crate::integrations::extensions::{ExtensionError, ExtensionRegistry};
use crate::integrations::recovery::NodeRepairBackend;
use crate::monitoring::{MonitorEngine, MonitorHandle};
use crate::recovery::{RecoveryScheduler, RecoveryService};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum HostError {
    #[error(transparent)]
    Harness(#[from] HarnessError),
    #[error(transparent)]
    Extension(#[from] ExtensionError),
    #[error("host lifecycle: {0}")]
    Lifecycle(String),
}

/// Owns shared Host services and drains them before releasing their dependencies.
pub struct HostRuntime {
    pub(crate) harnesses: Option<Arc<HarnessRegistry>>,
    pub(crate) extensions: Option<Arc<ExtensionRegistry>>,
    pub(crate) monitoring: Option<MonitorEngine>,
    pub(crate) monitor_handle: Option<Arc<MonitorHandle>>,
    pub(crate) recovery: Option<Arc<RecoveryService>>,
    pub(crate) recovery_backend: Option<Arc<NodeRepairBackend>>,
    pub(crate) recovery_scheduler: Option<RecoveryScheduler>,
    pub(crate) idle_wait_timeout: Duration,
}

impl HostRuntime {
    pub fn harnesses(&self) -> Option<Arc<HarnessRegistry>> {
        self.harnesses.clone()
    }

    pub fn extensions(&self) -> Option<Arc<ExtensionRegistry>> {
        self.extensions.clone()
    }

    pub fn monitoring(&self) -> Option<Arc<MonitorHandle>> {
        self.monitor_handle.clone()
    }

    pub fn recovery(&self) -> Option<Arc<RecoveryService>> {
        self.recovery.clone()
    }

    pub(crate) fn recovery_backend(&self) -> Option<Arc<NodeRepairBackend>> {
        self.recovery_backend.clone()
    }

    pub fn recovery_scheduler(&self) -> Option<&RecoveryScheduler> {
        self.recovery_scheduler.as_ref()
    }

    pub fn begin_shutdown(&self) {
        if let Some(scheduler) = &self.recovery_scheduler {
            scheduler.begin_shutdown();
        }
    }

    pub async fn shutdown(&mut self) -> Result<(), HostError> {
        self.begin_shutdown();
        if let Some(scheduler) = &self.recovery_scheduler {
            tokio::time::timeout(self.idle_wait_timeout, scheduler.shutdown())
                .await
                .map_err(|_| HostError::Lifecycle("waiting for recovery tasks timed out".into()))?
                .map_err(|error| HostError::Lifecycle(error.to_string()))?;
        }
        self.recovery_scheduler = None;
        self.recovery = None;
        self.recovery_backend = None;
        if let Some(handle) = &self.monitor_handle {
            handle
                .begin_shutdown_async()
                .await
                .map_err(|error| HostError::Lifecycle(error.to_string()))?;
        }
        if let Some(registry) = &self.harnesses {
            registry.begin_shutdown()?;
        }
        if let Some(registry) = &self.extensions {
            registry.begin_shutdown()?;
        }
        tokio::time::timeout(self.idle_wait_timeout, async {
            if let Some(engine) = &mut self.monitoring {
                engine
                    .shutdown()
                    .await
                    .map_err(|error| HostError::Lifecycle(error.to_string()))?;
            }
            if let Some(registry) = &self.harnesses {
                registry.shutdown().await?;
            }
            if let Some(registry) = &self.extensions {
                registry.shutdown().await?;
            }
            Ok::<(), HostError>(())
        })
        .await
        .map_err(|_| HostError::Lifecycle("waiting for service tasks timed out".into()))??;
        self.monitor_handle = None;
        self.monitoring = None;
        self.harnesses = None;
        self.extensions = None;
        Ok(())
    }
}

impl Drop for HostRuntime {
    fn drop(&mut self) {
        self.begin_shutdown();
        if let Some(handle) = &self.monitor_handle {
            let _ = handle.begin_shutdown();
        }
        if let Some(registry) = &self.harnesses {
            let _ = registry.begin_shutdown();
        }
        if let Some(registry) = &self.extensions {
            let _ = registry.begin_shutdown();
        }
    }
}
