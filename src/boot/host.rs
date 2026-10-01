//! Host lifecycle, external adapters and trusted configuration protection.
use crate::configuration;
use crate::configuration::RecoveryHostConfig;
use crate::harnesses::{
    HarnessError, HarnessRegistry, HarnessRegistryBuilder, HarnessRegistryConfig,
};
use crate::integrations::extensions::{ExtensionError, ExtensionRegistry, ExtensionsConfig};
use crate::integrations::harness::RemoteHarnessFactory;
use crate::integrations::monitoring::RegistryObservationSource;
use crate::integrations::recovery::{FileTargetOwnership, NodeRepairBackend, RecoveryScheduler};
use crate::integrations::recovery::{RecoveryConfig, RecoveryError, RecoveryService};
use crate::monitoring::{MonitorEngine, MonitorHandle, MonitorsConfig};
use crate::repair::{RepairConfig, WorkflowError};
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;

#[derive(Default)]
pub struct HostConfig {
    pub harnesses: Option<HarnessRegistryConfig>,
    pub extensions: Option<ExtensionsConfig>,
    pub monitoring: Option<MonitoringHostConfig>,
}

pub struct MonitoringHostConfig {
    pub config: MonitorsConfig,
    pub data_dir: PathBuf,
}

#[derive(Debug, Error)]
pub enum HostError {
    #[error(transparent)]
    Harness(#[from] HarnessError),
    #[error(transparent)]
    Extension(#[from] ExtensionError),
    #[error("host lifecycle: {0}")]
    Lifecycle(String),
}

/// Starts shared application services and connects external adapters to Core.
pub struct HostRuntime {
    harnesses: Option<Arc<HarnessRegistry>>,
    extensions: Option<Arc<ExtensionRegistry>>,
    monitoring: Option<MonitorEngine>,
    monitor_handle: Option<Arc<MonitorHandle>>,
    recovery: Option<Arc<RecoveryService>>,
    recovery_scheduler: Option<RecoveryScheduler>,
    idle_wait_timeout: Duration,
}

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
            recovery_scheduler: None,
            idle_wait_timeout: Duration::from_secs(30),
        })
    }

    pub fn harnesses(&self) -> Option<Arc<HarnessRegistry>> {
        self.harnesses.clone()
    }

    pub fn extensions(&self) -> Option<Arc<ExtensionRegistry>> {
        self.extensions.clone()
    }

    pub fn monitoring(&self) -> Option<Arc<MonitorHandle>> {
        self.monitor_handle.clone()
    }

    pub fn open_recovery(
        &self,
        data_dir: impl AsRef<Path>,
        config: RecoveryConfig,
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
            Arc::new(NodeRepairBackend::new(harnesses, extensions)),
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
        let recovery = RecoveryService::open_with_store_configs(
            data_dir,
            config.recovery,
            Arc::new(NodeRepairBackend::new(harnesses, extensions)),
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
        self.recovery_scheduler = Some(scheduler);
        Ok(recovery)
    }

    pub fn recovery(&self) -> Option<Arc<RecoveryService>> {
        self.recovery.clone()
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

/// Load a trusted Harness configuration outside the Host and Core source trees.
pub fn load_harness_config(path: &Path) -> Result<HarnessRegistryConfig, HarnessError> {
    let path = external_path(path).map_err(|error| {
        HarnessError::InvalidConfiguration(format!("active Harness configuration: {error}"))
    })?;
    configuration::load_harness_config(&path)
}

/// Network node configuration is data; loading never starts a node process.
pub fn load_extensions_config(path: &Path) -> Result<ExtensionsConfig, ExtensionError> {
    let path =
        external_path(path).map_err(|error| ExtensionError::Configuration(error.to_string()))?;
    ExtensionsConfig::load(path)
}

/// Protect the active extension configuration and every explicitly configured
/// TLS trust file from an approved repair target.
pub fn extension_protected_paths(
    path: &Path,
    target: &Path,
) -> Result<Vec<PathBuf>, WorkflowError> {
    let config_path = external_path(path)?;
    let config = ExtensionsConfig::load(&config_path)
        .map_err(|error| WorkflowError::Invalid(error.to_string()))?;
    let target = resolve_path(target)?;
    let mut protected = vec![config_path.canonicalize()?];
    for definition in config.extensions {
        if let Some(path) = definition.endpoint.ca_certificate {
            let path = external_path(&path)?.canonicalize()?;
            protected.push(path);
        }
    }
    if protected
        .iter()
        .any(|path| path.starts_with(&target) || target.starts_with(path))
    {
        return Err(WorkflowError::Invalid(
            "repair target overlaps extension configuration or TLS trust file".into(),
        ));
    }
    protected.sort();
    protected.dedup();
    Ok(protected)
}

/// Add the application source boundary before Core can create durable state.
/// Host prepares paths and protection; Core retains policy and approval authority.
pub fn load_repair_config(
    path: &Path,
    needs_target: bool,
) -> Result<(RepairConfig, Vec<PathBuf>), WorkflowError> {
    let path = external_path(path)?.canonicalize()?;
    let file = File::open(&path)?;
    if !file.metadata()?.is_file() {
        return Err(WorkflowError::Invalid(
            "repair configuration must be a file".into(),
        ));
    }
    let mut bytes = Vec::new();
    file.take(64 * 1024 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 64 * 1024 {
        return Err(WorkflowError::Invalid(
            "repair configuration exceeds 64 KiB".into(),
        ));
    }
    let config: RepairConfig = serde_json::from_slice(&bytes)?;
    if needs_target
        && matches!(
            &config.policy.reviewer,
            recuvora_core::recovery::approval::ReviewerConfig::HumanThenHarness { .. }
        )
    {
        return Err(WorkflowError::Invalid(
            "HumanThenHarness requires the automatic recovery service. Configure recovery_config to enable it. Local text repair supports historical inspection and human decisions.".into(),
        ));
    }
    let base = path
        .parent()
        .ok_or_else(|| invalid("configuration has no parent"))?;
    for control in [&config.harness_config, &config.data_dir]
        .into_iter()
        .chain(config.extensions_config.iter())
    {
        external_path(&relative_to(base, control))?;
    }
    let sources = source_roots()?;
    let target = resolve_path(&relative_to(base, &config.target_root))?;
    for source in &sources {
        if target.starts_with(source) || source.starts_with(&target) {
            return Err(WorkflowError::Invalid(
                "repair target must be isolated from the Host and Core source trees".into(),
            ));
        }
    }
    let (config, mut protected) =
        configuration::prepare_repair_config(config, &path, needs_target)?;
    if needs_target && let Some(extension_config) = &config.extensions_config {
        protected.extend(extension_protected_paths(
            extension_config,
            &config.target_root,
        )?);
    }
    // Recheck after Host path preparation and state creation, then retain the
    // source roots in the action backend's trusted protection set.
    external_path(&config.data_dir)?;
    protected.extend(sources);
    protected.sort();
    protected.dedup();
    Ok((config, protected))
}

/// Resolve an application control path without creating directories or following
/// links. Missing suffixes are retained for validation before the first write.
pub(crate) fn external_path(path: &Path) -> io::Result<PathBuf> {
    let resolved = resolve_path(path)?;
    if source_roots()?
        .iter()
        .any(|source| resolved.starts_with(source))
    {
        return Err(invalid(
            "runtime paths and active configuration must be outside the Host and Core source trees",
        ));
    }
    Ok(resolved)
}

pub(super) fn prepare_runtime_directory(path: &Path) -> io::Result<PathBuf> {
    let path = validate_simulation_paths(path)?;
    std::fs::create_dir_all(&path)?;
    validate_simulation_paths(&path)
}

/// Core's simulation storage is embedded here, so the application validates
/// existing journal/lock leaves as well as the directory before opening them.
pub(crate) fn validate_simulation_paths(path: &Path) -> io::Result<PathBuf> {
    let path = external_path(path)?;
    for file in ["writer.lock", "tasks.jsonl"] {
        external_path(&path.join(file))?;
    }
    Ok(path)
}

fn relative_to(base: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_owned()
    } else {
        base.join(path)
    }
}

fn source_roots() -> io::Result<Vec<PathBuf>> {
    let mut sources = Vec::new();
    // The build script resolves the actual path dependency from Cargo.toml;
    // runtime protection never guesses an adjacent directory by project name.
    for root in [env!("CARGO_MANIFEST_DIR"), env!("RECUVORA_CORE_SOURCE_DIR")] {
        match Path::new(root).canonicalize() {
            Ok(path) => sources.push(path),
            // A deployed binary must not need its build machine's source tree.
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(sources)
}

fn resolve_path(path: &Path) -> io::Result<PathBuf> {
    let absolute = std::path::absolute(path)?;
    for ancestor in absolute.ancestors() {
        match std::fs::symlink_metadata(ancestor) {
            Ok(metadata) => {
                #[cfg(windows)]
                {
                    use std::os::windows::fs::MetadataExt;
                    if metadata.file_attributes() & 0x400 != 0 {
                        return Err(invalid("linked or reparse runtime paths are forbidden"));
                    }
                }
                if metadata.file_type().is_symlink() {
                    return Err(invalid("linked runtime paths are forbidden"));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    let mut existing = absolute.as_path();
    let mut suffix = Vec::new();
    loop {
        match existing.canonicalize() {
            Ok(mut resolved) => {
                for component in suffix.iter().rev() {
                    resolved.push(component);
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let name = existing
                    .file_name()
                    .ok_or_else(|| invalid("unresolvable runtime path"))?;
                suffix.push(name.to_os_string());
                existing = existing
                    .parent()
                    .ok_or_else(|| invalid("existing path ancestor required"))?;
            }
            Err(error) => return Err(error),
        }
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
