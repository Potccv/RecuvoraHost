//! Initial monitor/discovery assembly and lifetime ownership.
use super::scheduler::launch_monitor;
use super::shared::Shared;
use super::state::restore_state;
use super::{MonitorError, MonitorHandle, MonitorsConfig, ObservationSource, discovery};
use crate::persistence::incidents::{IncidentStore, IncidentStoreConfig};
use crate::runtime::operation::Cancellation;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use tokio::sync::watch;

pub struct MonitorEngine {
    handle: MonitorHandle,
}

impl MonitorEngine {
    pub fn start_with_source(
        config: MonitorsConfig,
        source: Arc<dyn ObservationSource>,
        data_dir: impl AsRef<Path>,
    ) -> Result<Self, MonitorError> {
        Self::start_with_source_and_incident_config(
            config,
            source,
            data_dir,
            IncidentStoreConfig::default(),
        )
    }

    pub fn start_with_source_and_incident_config(
        config: MonitorsConfig,
        source: Arc<dyn ObservationSource>,
        data_dir: impl AsRef<Path>,
        incident_config: IncidentStoreConfig,
    ) -> Result<Self, MonitorError> {
        config.validate()?;
        incident_config.validate()?;
        tokio::runtime::Handle::try_current().map_err(|e| MonitorError::Runtime(e.to_string()))?;
        let store =
            IncidentStore::open(data_dir.as_ref().join("incidents.jsonl"), incident_config)?;
        let mut states = Vec::new();
        let mut views = BTreeMap::new();
        let mut definitions = BTreeMap::new();
        for definition in config.monitors {
            let state = restore_state(definition, &store)?;
            definitions.insert(state.config.id.clone(), state.config.clone());
            views.insert(state.config.id.clone(), state.view.clone());
            states.push((state, source.clone()));
        }
        let mut discoveries = Vec::new();
        let mut discovery_views = BTreeMap::new();
        for definition in config.discoveries {
            let discovery = discovery::DiscoveryState::restore(definition, &store)?;
            for (state, observer) in discovery.restored_monitors(&store, source.clone())? {
                definitions.insert(state.config.id.clone(), state.config.clone());
                views.insert(state.config.id.clone(), state.view.clone());
                states.push((state, observer));
            }
            discovery_views.insert(discovery.id().to_owned(), discovery.snapshot());
            discoveries.push(discovery);
        }
        let (workers, _) = watch::channel(states.len() + discoveries.len());
        let shared = Arc::new(Shared {
            store: Mutex::new(store),
            definitions: Mutex::new(definitions),
            views: Mutex::new(views),
            fresh_until: Mutex::new(BTreeMap::new()),
            discoveries: Mutex::new(discovery_views),
            registration: Arc::new(tokio::sync::Mutex::new(())),
            error: Mutex::new(None),
            cancellation: Cancellation::new(),
            accepting: AtomicBool::new(true),
            workers,
        });
        for (state, observer) in states {
            launch_monitor(state, observer, shared.clone());
        }
        for discovery in discoveries {
            discovery::launch(discovery, source.clone(), shared.clone());
        }
        Ok(Self {
            handle: MonitorHandle { shared },
        })
    }

    pub fn handle(&self) -> MonitorHandle {
        self.handle.clone()
    }
    pub async fn shutdown(&mut self) -> Result<(), MonitorError> {
        self.handle.begin_shutdown_async().await?;
        self.handle.wait_for_idle().await
    }
}

impl Drop for MonitorEngine {
    fn drop(&mut self) {
        if self.handle.begin_shutdown().is_err() {
            self.handle.request_stop();
        }
    }
}
