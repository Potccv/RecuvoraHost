//! Host service assembly with compatibility exports for earlier entry points.
pub use crate::application::{HostError, HostRuntime};
pub(crate) use crate::configuration::external_path;
pub(super) use crate::configuration::prepare_runtime_directory;
pub use crate::configuration::{
    HostConfig, MonitoringHostConfig, extension_protected_paths, load_extensions_config,
    load_host_harness_config as load_harness_config, load_host_repair_config as load_repair_config,
};

use crate::configuration::RecoveryHostConfig;
use crate::harnesses::HarnessRegistryBuilder;
use crate::integrations::extensions::{ExtensionRegistry, ExtensionsConfig};
use crate::integrations::harness::RemoteHarnessFactory;
use crate::integrations::monitoring::RegistryObservationSource;
use crate::integrations::recovery::NodeRepairBackend;
use crate::monitoring::MonitorEngine;
use crate::recovery::{
    FileTargetOwnership, RecoveryConfig, RecoveryError, RecoveryScheduler, RecoveryService,
};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

impl HostRuntime {
    pub async fn start(config: HostConfig) -> Result<Self, HostError> {
        let needs_extensions = config.extensions.is_some() || config.monitoring.is_some();
        let extensions = if needs_extensions {
            let config = config.extensions.unwrap_or(ExtensionsConfig {
                schema_version: 1,
                extensions: Vec::new(),
            });
            Some(Arc::new(ExtensionRegistry::connect(config).await?))
        } else {
            None
        };

        let harnesses = if let Some(config) = config.harnesses {
            let mut builder = HarnessRegistryBuilder::new();
            if let Some(registry) = &extensions {
                builder.register(Arc::new(RemoteHarnessFactory::new(registry.clone())))?;
            }
            Some(Arc::new(builder.build(config)?))
        } else {
            None
        };

        let monitoring = if let Some(config) = config.monitoring {
            let registry = extensions
                .as_ref()
                .ok_or_else(|| HostError::Lifecycle("monitoring requires extensions".into()))?;
            Some(
                MonitorEngine::start_with_source(
                    config.config,
                    Arc::new(RegistryObservationSource::new(registry.clone())),
                    config.data_dir,
                )
                .map_err(|error| HostError::Lifecycle(error.to_string()))?,
            )
        } else {
            None
        };
        let monitor_handle = monitoring.as_ref().map(|engine| Arc::new(engine.handle()));

        Ok(Self {
            harnesses,
            extensions,
            monitoring,
            monitor_handle,
            recovery: None,
            recovery_backend: None,
            recovery_scheduler: None,
            idle_wait_timeout: Duration::from_secs(30),
        })
    }

    pub fn open_recovery(
        &self,
        data_dir: impl AsRef<Path>,
        config: RecoveryConfig,
        executor: crate::integrations::recovery::ScriptExecutorConfig,
    ) -> Result<Arc<RecoveryService>, RecoveryError> {
        let data_dir = external_path(data_dir.as_ref())?;
        let harnesses = self
            .harnesses()
            .ok_or_else(|| RecoveryError::Invalid("Harness service required".into()))?;
        let extensions = self
            .extensions()
            .ok_or_else(|| RecoveryError::Invalid("extension service required".into()))?;
        RecoveryService::open(
            data_dir,
            config,
            Arc::new(NodeRepairBackend::new(harnesses, extensions, executor)?),
        )
    }

    /// Starts the scheduler and waits for its work before closing providers.
    pub async fn start_recovery(
        &mut self,
        config: RecoveryHostConfig,
    ) -> Result<Arc<RecoveryService>, RecoveryError> {
        config.validate()?;
        if self.recovery.is_some() {
            return Err(RecoveryError::Busy);
        }
        let monitor = self
            .monitoring()
            .ok_or_else(|| RecoveryError::Invalid("monitoring service required".into()))?;
        let harnesses = self
            .harnesses()
            .ok_or_else(|| RecoveryError::Invalid("Harness service required".into()))?;
        let extensions = self
            .extensions()
            .ok_or_else(|| RecoveryError::Invalid("extension service required".into()))?;
        let [data_dir, ownership_dir] = config.storage_paths()?;
        let ownership = Arc::new(FileTargetOwnership::open(ownership_dir)?);
        let backend = Arc::new(NodeRepairBackend::new(
            harnesses,
            extensions,
            config.executor,
        )?);
        let recovery = RecoveryService::open_with_store_configs(
            data_dir,
            config.recovery,
            backend.clone(),
            config.approval_store,
            config.knowledge_store,
        )?;
        if let Err(error) = recovery.bind_target_ownership(ownership) {
            recovery.shutdown().await?;
            return Err(error);
        }
        let scheduler = match RecoveryScheduler::start(
            recovery.clone(),
            (*monitor).clone(),
            config.triggers,
            Duration::from_millis(config.interval_ms),
        ) {
            Ok(scheduler) => scheduler,
            Err(error) => {
                recovery.shutdown().await?;
                return Err(error);
            }
        };
        self.recovery = Some(recovery.clone());
        self.recovery_backend = Some(backend);
        self.recovery_scheduler = Some(scheduler);
        Ok(recovery)
    }
}
