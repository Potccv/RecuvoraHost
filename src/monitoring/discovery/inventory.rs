//! Durable inventory enrollment and source-presence epochs.
use super::super::config::binding;
use super::super::scheduler::launch_monitor;
use super::super::shared::{Shared, lock};
use super::super::state::{MonitorState, restore_state};
use super::super::support::{DISCOVERY_INVALIDATED, MAX_BATCH, bounded_text, now_ms};
use super::super::{
    MonitorDefinition, MonitorError, ObservationFuture, ObservationRequest, ObservationSource,
};
use super::config::{invalid, key_valid};
use super::{DiscoveryBatch, DiscoverySnapshot, MonitorDiscovery};
use crate::persistence::incidents::{IncidentStore, MonitorCommit};
use crate::runtime::operation::Cancellation;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

type RestoredMonitor = (MonitorState, Arc<dyn ObservationSource>);

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct InventoryCheckpoint {
    schema_version: u32,
    binding: String,
    keys: Vec<String>,
}

pub(in crate::monitoring) struct DiscoveryState {
    pub(super) config: MonitorDiscovery,
    binding: String,
    sequence: u64,
    targets: BTreeMap<String, Arc<AtomicU64>>,
    view: DiscoverySnapshot,
}

fn inventory_binding(config: &MonitorDiscovery) -> Result<String, MonitorError> {
    serde_json::to_string(&(
        &config.extension_id,
        &config.contract,
        config.version,
        &config.method,
        &config.params,
        &config.parameter,
        &config.template.id,
        binding(&config.template)?,
    ))
    .map_err(|error| invalid(&error.to_string()))
}

impl DiscoveryState {
    pub(in crate::monitoring) fn id(&self) -> &str {
        &self.config.id
    }

    fn checkpoint_id(&self) -> String {
        format!("discovery.{}", self.config.id)
    }

    pub(in crate::monitoring) fn snapshot(&self) -> DiscoverySnapshot {
        self.view.clone()
    }

    pub(in crate::monitoring) fn restore(
        config: MonitorDiscovery,
        store: &IncidentStore,
    ) -> Result<Self, MonitorError> {
        let binding = inventory_binding(&config)?;
        let previous = store.checkpoint(&format!("discovery.{}", config.id));
        let keys = if let Some(previous) = &previous {
            let checkpoint: InventoryCheckpoint = serde_json::from_value(previous.value.clone())
                .map_err(|error| invalid(&format!("invalid discovery checkpoint: {error}")))?;
            if checkpoint.schema_version != 1
                || checkpoint.binding != binding
                || checkpoint.keys.len() > config.max_targets
                || checkpoint.keys.iter().any(|key| !key_valid(key))
                || checkpoint.keys.iter().collect::<BTreeSet<_>>().len() != checkpoint.keys.len()
            {
                return Err(invalid(
                    "discovery checkpoint binding or lifetime target limit changed",
                ));
            }
            checkpoint.keys.into_iter().collect::<BTreeSet<_>>()
        } else {
            BTreeSet::new()
        };
        let view = DiscoverySnapshot {
            id: config.id.clone(),
            extension_id: config.extension_id.clone(),
            contract: config.contract.clone(),
            version: config.version,
            method: config.method.clone(),
            running: true,
            last_received_at_ms: None,
            complete: false,
            known_targets: keys.len(),
            present_targets: 0,
            last_error: None,
        };
        Ok(Self {
            config,
            binding,
            sequence: previous.map_or(0, |value| value.sequence),
            view,
            targets: keys
                .into_iter()
                .map(|key| (key, Arc::new(AtomicU64::new(0))))
                .collect(),
        })
    }

    fn definition(&self, key: &str) -> MonitorDefinition {
        let mut definition = self.config.template.clone();
        definition.id = format!("{}.{}", definition.id, key);
        definition.target_id = format!("{}.{}", definition.target_id, key);
        definition.source_id = format!("{}.{}", definition.source_id, key);
        definition.params[&self.config.parameter] = json!(key);
        definition
    }

    pub(in crate::monitoring) fn restored_monitors(
        &self,
        store: &IncidentStore,
        source: Arc<dyn ObservationSource>,
    ) -> Result<Vec<RestoredMonitor>, MonitorError> {
        self.targets
            .iter()
            .map(|(key, present)| {
                let observer: Arc<dyn ObservationSource> = Arc::new(PresentSource {
                    source: source.clone(),
                    present: present.clone(),
                });
                Ok((restore_state(self.definition(key), store)?, observer))
            })
            .collect()
    }

    fn publish(&mut self, shared: &Shared) -> Result<(), MonitorError> {
        self.view.known_targets = self.targets.len();
        self.view.present_targets = self
            .targets
            .values()
            .filter(|present| present.load(Ordering::Acquire) % 2 == 1)
            .count();
        self.view.running = shared.accepting.load(Ordering::Acquire);
        lock(&shared.discoveries)?.insert(self.config.id.clone(), self.view.clone());
        Ok(())
    }

    pub(super) fn reject(&mut self, message: &str, shared: &Shared) -> Result<(), MonitorError> {
        self.view.complete = false;
        self.view.last_error = Some(bounded_text(message));
        self.publish(shared)
    }

    pub(super) async fn accept(
        &mut self,
        batch: DiscoveryBatch,
        source: Arc<dyn ObservationSource>,
        shared: &Arc<Shared>,
    ) -> Result<(), MonitorError> {
        if batch.schema_version != 1
            || batch.targets.len() > 64
            || batch
                .error
                .as_ref()
                .is_some_and(|error| error.is_empty() || error.len() > 2048)
            || serde_json::to_vec(&batch).map_or(true, |bytes| bytes.len() > MAX_BATCH)
        {
            return Err(MonitorError::Observation(
                "invalid discovery response or limits".into(),
            ));
        }
        let mut present = BTreeSet::new();
        for target in batch.targets {
            if !key_valid(&target.key) || !present.insert(target.key) {
                return Err(MonitorError::Observation(
                    "invalid or duplicate discovered key".into(),
                ));
            }
        }
        let mut known: BTreeSet<String> = self.targets.keys().cloned().collect();
        known.extend(present.iter().cloned());
        if known.len() > self.config.max_targets {
            return Err(MonitorError::Observation(
                "discovery lifetime target capacity exhausted".into(),
            ));
        }
        let _registration = shared.registration.lock().await;
        if !shared.accepting.load(Ordering::Acquire) {
            return Err(MonitorError::Stopped);
        }
        let additions: Vec<String> = known
            .iter()
            .filter(|key| !self.targets.contains_key(*key))
            .cloned()
            .collect();
        let mut prepared = Vec::new();
        {
            let mut store = lock(&shared.store)?;
            for key in &additions {
                let definition = self.definition(key);
                if lock(&shared.views)?.contains_key(&definition.id) {
                    return Err(invalid(
                        "discovered monitor identity conflicts with existing monitor",
                    ));
                }
                prepared.push((key.clone(), restore_state(definition, &store)?));
            }
            if !additions.is_empty() {
                let sequence = self
                    .sequence
                    .checked_add(1)
                    .ok_or_else(|| invalid("discovery sequence exhausted"))?;
                let checkpoint = InventoryCheckpoint {
                    schema_version: 1,
                    binding: self.binding.clone(),
                    keys: known.into_iter().collect(),
                };
                store.commit(MonitorCommit {
                    monitor_id: self.checkpoint_id(),
                    sequence,
                    checkpoint: serde_json::to_value(checkpoint)
                        .map_err(|error| invalid(&error.to_string()))?,
                    signals: Vec::new(),
                    now_ms: now_ms(),
                })?;
                self.sequence = sequence;
            }
        }
        // Persist all newly accepted identities before publishing or dispatching any worker.
        for (key, state) in prepared {
            let presence = Arc::new(AtomicU64::new(1));
            let observer = Arc::new(PresentSource {
                source: source.clone(),
                present: presence.clone(),
            });
            self.targets.insert(key, presence);
            lock(&shared.definitions)?.insert(state.config.id.clone(), state.config.clone());
            lock(&shared.views)?.insert(state.config.id.clone(), state.view.clone());
            shared.workers.send_modify(|count| *count += 1);
            launch_monitor(state, observer, shared.clone());
        }
        let complete = batch.complete && batch.error.is_none();
        for (key, flag) in &self.targets {
            let previous = flag.load(Ordering::Acquire);
            let expected = if present.contains(key) {
                true
            } else if complete {
                false
            } else {
                previous % 2 == 1
            };
            if !expected {
                // Revoke repair readiness synchronously with inventory membership;
                // do not wait for the observer's next tick to publish Unknown.
                lock(&shared.fresh_until)?.remove(&self.definition(key).id);
            }
            if previous == 0 && complete && !expected {
                // Zero is unverified after restart; a complete inventory can
                // explicitly establish absence without passing through present.
                flag.store(2, Ordering::Release);
            } else if (previous % 2 == 1) != expected {
                flag.store(
                    previous.checked_add(1).ok_or_else(|| {
                        MonitorError::Runtime("discovery presence epoch exhausted".into())
                    })?,
                    Ordering::Release,
                );
            }
        }
        self.view.last_received_at_ms = Some(now_ms());
        self.view.complete = complete;
        self.view.last_error = batch
            .error
            .or_else(|| (!complete).then(|| "discovery inventory is partial".into()));
        self.publish(shared)
    }
}

struct PresentSource {
    source: Arc<dyn ObservationSource>,
    present: Arc<AtomicU64>,
}
impl ObservationSource for PresentSource {
    fn observation_pending(&self) -> bool {
        self.present.load(Ordering::Acquire) == 0
    }
    fn observation_epoch(&self) -> Option<u64> {
        let epoch = self.present.load(Ordering::Acquire);
        (epoch % 2 == 1).then_some(epoch)
    }
    fn poll(
        &self,
        request: ObservationRequest,
        cancellation: Cancellation,
    ) -> ObservationFuture<'_> {
        Box::pin(async move {
            let missing = || MonitorError::Observation(DISCOVERY_INVALIDATED.into());
            let epoch = self.observation_epoch();
            if epoch.is_none() {
                return Err(missing());
            }
            let result = self.source.poll(request, cancellation).await;
            if self.observation_epoch() != epoch {
                return Err(missing());
            }
            result
        })
    }
}
