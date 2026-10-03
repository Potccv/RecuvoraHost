//! Composition of application capabilities; no HTTP dependencies.
use crate::application::HostRuntime;
use crate::application::{Application, ApplicationError, ApplicationParts, journal::Journal};
use crate::configuration::{
    HostConfig, MonitoringHostConfig, load_host_harness_config, load_host_repair_config,
};
use crate::integrations::extensions::ExtensionsConfig;
use crate::monitoring::MonitorsConfig;
use crate::repair::RepairSession;
use crate::simulation::{Engine, EngineConfig};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

/// Capability inputs plus trusted transport and installation control paths.
pub struct ApplicationConfig {
    pub data_dir: PathBuf,
    pub harness_config: Option<PathBuf>,
    pub extensions_config: Option<PathBuf>,
    pub repair_config: Option<PathBuf>,
    pub monitors_config: Option<PathBuf>,
    pub recovery_config: Option<PathBuf>,
    pub control_paths: Vec<PathBuf>,
}

fn external(path: &Path) -> Result<PathBuf, ApplicationError> {
    if !path.is_absolute() {
        return Err(ApplicationError::invalid("absolute external path required"));
    }
    crate::configuration::external_path(path)
        .and_then(|path| path.canonicalize())
        .map_err(|error| ApplicationError::invalid(error.to_string()))
}

/// Opens the same capabilities and preserves the independently owned simulation engine.
pub async fn open(
    config: ApplicationConfig,
) -> Result<(Arc<Application>, Engine), ApplicationError> {
    let data = external(&config.data_dir)?;
    crate::configuration::external_path(&data.join("operations.jsonl"))?;
    crate::configuration::validate_simulation_paths(&data.join("simulation"))?;
    let recovery_config = config
        .recovery_config
        .as_ref()
        .map(|path| {
            crate::configuration::RecoveryHostConfig::load(&external(path)?)
                .map_err(ApplicationError::from)
        })
        .transpose()?;
    let recovery_storage: Vec<_> = recovery_config
        .iter()
        .flat_map(|config| [config.data_dir.clone(), config.ownership_dir.clone()])
        .collect();
    for storage in &recovery_storage {
        // A dedicated authority directory must not contain application controls.
        if storage.starts_with(&data) || data.starts_with(storage) {
            return Err(ApplicationError::invalid(
                "recovery storage must be separate from console storage",
            ));
        }
        for path in [
            config.harness_config.as_ref(),
            config.extensions_config.as_ref(),
            config.repair_config.as_ref(),
            config.monitors_config.as_ref(),
            config.recovery_config.as_ref(),
        ]
        .into_iter()
        .flatten()
        .chain(config.control_paths.iter())
        {
            let path = external(path)?;
            if path.starts_with(storage) || storage.starts_with(&path) {
                return Err(ApplicationError::invalid(
                    "recovery storage overlaps an application control path",
                ));
            }
        }
    }
    let extensions_config = config
        .extensions_config
        .as_ref()
        .map(|path| {
            ExtensionsConfig::load(external(path)?)
                .map_err(|e| ApplicationError::invalid(e.to_string()))
        })
        .transpose()?;
    let loaded_repair = config
        .repair_config
        .as_ref()
        .map(|path| load_host_repair_config(&external(path)?, true).map_err(ApplicationError::from))
        .transpose()?;
    if let (Some(recovery), Some((repair, _))) = (&recovery_config, &loaded_repair) {
        if recovery.recovery.target.target_id == repair.target_id {
            return Err(ApplicationError::invalid(
                "one target cannot use both recovery and legacy text authorities",
            ));
        }
        let harness_path = external(&repair.harness_config)?;
        if recovery_storage
            .iter()
            .any(|storage| harness_path.starts_with(storage) || storage.starts_with(&harness_path))
        {
            return Err(ApplicationError::invalid(
                "recovery storage overlaps legacy Harness configuration",
            ));
        }
    }
    if let (Some(_), Some(extensions)) = (&recovery_config, &extensions_config) {
        for path in extensions
            .extensions
            .iter()
            .filter_map(|definition| definition.endpoint.ca_certificate.as_deref())
        {
            let path = external(path)?;
            if recovery_storage
                .iter()
                .any(|storage| path.starts_with(storage) || storage.starts_with(&path))
            {
                return Err(ApplicationError::invalid(
                    "recovery storage overlaps a TLS trust file",
                ));
            }
        }
    }
    let harness_path = config
        .harness_config
        .as_ref()
        .or_else(|| loaded_repair.as_ref().map(|(c, _)| &c.harness_config));
    let harness_config = harness_path
        .map(|path| {
            load_host_harness_config(&external(path)?)
                .map_err(|e| ApplicationError::invalid(e.to_string()))
        })
        .transpose()?;
    let monitoring_config = config
        .monitors_config
        .as_ref()
        .map(|path| {
            MonitorsConfig::load(external(path)?)
                .map(|config| MonitoringHostConfig {
                    config,
                    data_dir: data.join("monitoring"),
                })
                .map_err(|error| ApplicationError::invalid(error.to_string()))
        })
        .transpose()?;
    let mut host = HostRuntime::start(HostConfig {
        harnesses: harness_config,
        extensions: extensions_config,
        monitoring: monitoring_config,
    })
    .await
    .map_err(|e| ApplicationError::unavailable(e.to_string()))?;
    let registry = host.harnesses();
    let repair_config = loaded_repair.as_ref().map(|(c, _)| c.clone());
    let setup = async {
        let repair = if let Some((cfg, mut protected)) = loaded_repair {
            if recovery_storage
                .iter()
                .any(|path| path.starts_with(&cfg.data_dir) || cfg.data_dir.starts_with(path))
            {
                return Err(ApplicationError::invalid(
                    "recovery and text repair storage must be separate",
                ));
            }
            protected.extend(config.control_paths.iter().cloned());
            protected.push(data.clone());
            if let Some(path) = &config.harness_config {
                protected.push(external(path)?);
            }
            if let Some(path) = &config.monitors_config {
                protected.push(path.clone());
            }
            if let Some(path) = &config.extensions_config {
                protected.push(path.clone());
            }
            if let Some(path) = &config.recovery_config {
                protected.push(path.clone());
            }
            protected.extend(recovery_storage.iter().cloned());
            Some(RepairSession::open(cfg, registry.clone(), &protected)?)
        } else {
            None
        };
        let journal = Journal::open(&data)?;
        let simulation = Engine::open(data.join("simulation"), EngineConfig::default())
            .await
            .map_err(|e| ApplicationError::unavailable(e.to_string()))?;
        let recovery = if let Some(cfg) = recovery_config {
            match host.start_recovery(cfg).await {
                Ok(recovery) => Some(recovery),
                Err(error) => {
                    simulation
                        .shutdown()
                        .await
                        .map_err(|e| ApplicationError::unavailable(e.to_string()))?;
                    return Err(error.into());
                }
            }
        } else {
            None
        };
        Ok::<_, ApplicationError>((repair, journal, simulation, recovery))
    }
    .await;
    let (repair, journal, simulation, recovery) = match setup {
        Ok(resources) => resources,
        Err(error) => {
            host.shutdown().await.map_err(|cleanup| {
                ApplicationError::unavailable(format!("{error}; cleanup: {cleanup}"))
            })?;
            return Err(error);
        }
    };
    let application = Application::from_parts(ApplicationParts {
        host,
        text_repair: repair,
        text_repair_config: repair_config,
        recovery,
        simulation: simulation.handle(),
        journal,
    });
    Ok((application, simulation))
}
