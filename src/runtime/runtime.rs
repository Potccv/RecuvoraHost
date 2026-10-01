//! One-cycle startup, shutdown and cancellation-safe resource reclamation.
use super::resources::Resources;
use super::state::{InstanceState, SharedState};
use super::{
    FrameworkError, InstanceId, LifecycleOptions, Module, ModuleContext, ModuleMetadata,
    RuntimeSnapshot, ShutdownReport, StartupError,
};
use std::sync::{Arc, Mutex};
use tokio::time::timeout;

struct Entry {
    module: Box<dyn Module>,
    context: ModuleContext,
    metadata: ModuleMetadata,
    stop_finished: bool,
    cleanup: Option<Resources>,
}

pub struct Runtime {
    options: LifecycleOptions,
    state: Arc<Mutex<SharedState>>,
    entries: Vec<Entry>,
    active: Vec<usize>,
    fresh: bool,
    shutdown_issues: Vec<FrameworkError>,
}

impl Runtime {
    pub fn new(options: LifecycleOptions) -> Self {
        Self {
            options,
            state: Arc::new(Mutex::new(SharedState::default())),
            entries: Vec::new(),
            active: Vec::new(),
            fresh: true,
            shutdown_issues: Vec::new(),
        }
    }

    pub fn add(&mut self, module: Box<dyn Module>) -> Result<(), FrameworkError> {
        if !self.fresh {
            return Err(FrameworkError::RuntimeNotFresh);
        }
        if self.entries.len() >= self.options.max_modules {
            return Err(FrameworkError::CapacityExceeded("modules".into()));
        }
        let metadata = module.metadata();
        if metadata.instance.0.is_empty()
            || metadata
                .provides
                .iter()
                .chain(&metadata.requires)
                .any(|key| key.name.is_empty() || key.scope.is_empty() || key.major == 0)
        {
            return Err(FrameworkError::InvalidConfiguration(
                "empty identity or zero service major".into(),
            ));
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| FrameworkError::StatePoisoned)?;
        if state.instances.contains_key(&metadata.instance) {
            return Err(FrameworkError::DuplicateInstance(metadata.instance));
        }
        state.instances.insert(
            metadata.instance.clone(),
            InstanceState {
                metadata: metadata.clone(),
                accepting: false,
                resources: Resources::default(),
            },
        );
        let context = ModuleContext {
            instance: metadata.instance.clone(),
            state: self.state.clone(),
            options: self.options.clone(),
        };
        self.entries.push(Entry {
            module,
            context,
            metadata,
            stop_finished: false,
            cleanup: None,
        });
        Ok(())
    }

    /// Validate the entire graph before executing any module hook.
    pub fn validate(&self) -> Result<Vec<InstanceId>, FrameworkError> {
        self.order().map(|order| {
            order
                .iter()
                .map(|index| self.entries[*index].metadata.instance.clone())
                .collect()
        })
    }

    fn order(&self) -> Result<Vec<usize>, FrameworkError> {
        let metadata: Vec<_> = self.entries.iter().map(|entry| &entry.metadata).collect();
        super::dependencies::order(&self.options, &metadata)
    }

    pub async fn start(&mut self) -> Result<(), StartupError> {
        if !self.fresh {
            return Err(StartupError {
                cause: FrameworkError::RuntimeNotFresh,
                cleanup: ShutdownReport::default(),
            });
        }
        let order = self.order().map_err(|cause| StartupError {
            cause,
            cleanup: ShutdownReport::default(),
        })?;
        self.fresh = false;
        for index in order {
            let activate = self
                .state
                .lock()
                .map_err(|_| FrameworkError::StatePoisoned)
                .and_then(|mut state| {
                    let instance = &self.entries[index].metadata.instance;
                    let owner = state
                        .instances
                        .get_mut(instance)
                        .ok_or_else(|| FrameworkError::InstanceInactive(instance.clone()))?;
                    owner.accepting = true;
                    Ok(())
                });
            if let Err(cause) = activate {
                return Err(StartupError {
                    cause,
                    cleanup: self.shutdown().await,
                });
            }
            // Track before awaiting, so an interrupted start can still be shut down.
            self.active.push(index);
            let entry = &mut self.entries[index];
            let result = timeout(
                self.options.start_timeout,
                entry.module.start(&entry.context),
            )
            .await;
            let cause = match result {
                Ok(Ok(())) => self.check_publications(index).err(),
                Ok(Err(error)) => Some(FrameworkError::LifecycleFailure {
                    instance: entry.metadata.instance.clone(),
                    phase: "start",
                    message: error.message,
                }),
                Err(_) => Some(FrameworkError::LifecycleTimeout {
                    instance: entry.metadata.instance.clone(),
                    phase: "start",
                }),
            };
            if let Some(cause) = cause {
                return Err(StartupError {
                    cause,
                    cleanup: self.shutdown().await,
                });
            }
        }
        Ok(())
    }

    fn check_publications(&self, index: usize) -> Result<(), FrameworkError> {
        let state = self
            .state
            .lock()
            .map_err(|_| FrameworkError::StatePoisoned)?;
        let metadata = &self.entries[index].metadata;
        for service in &metadata.provides {
            if !state.services.contains_key(service) {
                return Err(FrameworkError::UnpublishedService {
                    instance: metadata.instance.clone(),
                    service: service.clone(),
                });
            }
        }
        Ok(())
    }

    /// Idempotent shutdown. One module's failure does not skip remaining cleanup.
    pub async fn shutdown(&mut self) -> ShutdownReport {
        self.fresh = false;
        while let Some(&index) = self.active.last() {
            let entry = &mut self.entries[index];
            if !entry.stop_finished {
                match self.state.lock() {
                    Ok(mut state) => {
                        if let Some(owner) = state.instances.get_mut(&entry.metadata.instance) {
                            owner.accepting = false;
                        }
                        state
                            .listeners
                            .retain(|listener| listener.owner != entry.metadata.instance);
                    }
                    Err(_) => self.shutdown_issues.push(FrameworkError::StatePoisoned),
                }
                match timeout(self.options.stop_timeout, entry.module.stop(&entry.context)).await {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => self.shutdown_issues.push(FrameworkError::LifecycleFailure {
                        instance: entry.metadata.instance.clone(),
                        phase: "stop",
                        message: error.message,
                    }),
                    Err(_) => self.shutdown_issues.push(FrameworkError::LifecycleTimeout {
                        instance: entry.metadata.instance.clone(),
                        phase: "stop",
                    }),
                }
                entry.stop_finished = true;
            }
            if entry.cleanup.is_none() {
                match self.state.lock() {
                    Ok(mut state) => {
                        entry.cleanup = Some(state.take_resources(&entry.metadata.instance))
                    }
                    Err(_) => {
                        self.shutdown_issues.push(FrameworkError::StatePoisoned);
                        break;
                    }
                }
            }
            // Keep handles in the runtime across every await. Cancelling shutdown
            // never detaches these tasks or loses their synchronous disposers.
            let Some(resources) = entry.cleanup.as_mut() else {
                break;
            };
            for task in &resources.tasks {
                task.abort();
            }
            let deadline = tokio::time::Instant::now() + self.options.cleanup_timeout;
            let mut timed_out = false;
            while let Some(task) = resources.tasks.last_mut() {
                match tokio::time::timeout_at(deadline, task).await {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) if error.is_cancelled() => {}
                    Ok(Err(error)) => self.shutdown_issues.push(FrameworkError::LifecycleFailure {
                        instance: entry.metadata.instance.clone(),
                        phase: "background task",
                        message: error.to_string(),
                    }),
                    Err(_) => {
                        self.shutdown_issues.push(FrameworkError::LifecycleTimeout {
                            instance: entry.metadata.instance.clone(),
                            phase: "cleanup",
                        });
                        timed_out = true;
                        break;
                    }
                }
                resources.tasks.pop();
            }
            if timed_out {
                // Retain this instance and its dependencies until the old task has
                // stopped. A subsequent shutdown can finish the pending cleanup.
                break;
            }
            while let Some(dispose) = resources.disposers.pop() {
                dispose();
            }
            entry.cleanup = None;
            self.active.pop();
        }
        ShutdownReport {
            issues: std::mem::take(&mut self.shutdown_issues),
        }
    }

    pub fn snapshot(&self) -> Result<RuntimeSnapshot, FrameworkError> {
        let state = self
            .state
            .lock()
            .map_err(|_| FrameworkError::StatePoisoned)?;
        Ok(RuntimeSnapshot {
            active_instances: state
                .instances
                .values()
                .filter(|owner| owner.accepting)
                .count(),
            pending_cleanup_instances: self
                .entries
                .iter()
                .filter(|entry| entry.cleanup.is_some())
                .count(),
            services: state.services.len(),
            subscriptions: state.listeners.len(),
            disposers: state
                .instances
                .values()
                .map(|owner| owner.resources.disposers.len())
                .sum::<usize>()
                + self
                    .entries
                    .iter()
                    .filter_map(|entry| entry.cleanup.as_ref())
                    .map(|resources| resources.disposers.len())
                    .sum::<usize>(),
            background_tasks: state
                .instances
                .values()
                .map(|owner| owner.resources.tasks.len())
                .sum::<usize>()
                + self
                    .entries
                    .iter()
                    .filter_map(|entry| entry.cleanup.as_ref())
                    .map(|resources| resources.tasks.len())
                    .sum::<usize>(),
        })
    }
}

impl Default for Runtime {
    fn default() -> Self {
        Self::new(LifecycleOptions::default())
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        // Async stop hooks require explicit shutdown. Drop only revokes work and
        // schedules best-effort reclamation on an existing Tokio runtime.
        let mut resources = if let Ok(mut state) = self.state.lock() {
            self.entries
                .iter()
                .map(|entry| state.take_resources(&entry.metadata.instance))
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        resources.extend(
            self.entries
                .iter_mut()
                .filter_map(|entry| entry.cleanup.take()),
        );
        for resource in &resources {
            for task in &resource.tasks {
                task.abort();
            }
        }
        let reclaim = async move {
            for resources in resources.into_iter().rev() {
                for task in resources.tasks {
                    let _ = task.await;
                }
                for dispose in resources.disposers.into_iter().rev() {
                    dispose();
                }
            }
        };
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(reclaim);
        }
    }
}
