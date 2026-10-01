//! Trusted monitor reads, incident acknowledgement and shutdown that waits for active calls.
use super::shared::{Shared, lock};
use super::support::now_ms;
use super::{
    Coverage, Freshness, MonitorDefinition, MonitorError, MonitorSnapshot, MonitoringSnapshot,
    TargetHealth,
};
use crate::persistence::incidents::{
    IncidentError, IncidentKind, IncidentRecord, IncidentStatus, SignalCondition,
};
use std::sync::Arc;
use std::sync::atomic::Ordering;

#[derive(Clone)]
pub struct MonitorHandle {
    pub(super) shared: Arc<Shared>,
}

/// An owned gate prevents observation and acknowledgement commits from crossing
/// the authorization-to-send interval; freshness must still be rechecked.
pub struct MonitorIncidentLease {
    handle: MonitorHandle,
    incident_id: String,
    target_id: String,
    minimum_revision: u64,
    _registration: tokio::sync::OwnedMutexGuard<()>,
}
impl MonitorIncidentLease {
    pub fn current(&self) -> Result<IncidentRecord, MonitorError> {
        self.handle.repair_incident_registered(
            &self.incident_id,
            &self.target_id,
            self.minimum_revision,
        )
    }
}

impl MonitorHandle {
    pub async fn acquire_repair_incident(
        &self,
        incident_id: &str,
        target_id: &str,
        minimum_revision: u64,
    ) -> Result<MonitorIncidentLease, MonitorError> {
        let registration = self.shared.registration.clone().lock_owned().await;
        let lease = MonitorIncidentLease {
            handle: self.clone(),
            incident_id: incident_id.into(),
            target_id: target_id.into(),
            minimum_revision,
            _registration: registration,
        };
        lease.current()?;
        Ok(lease)
    }

    fn registration(&self) -> Result<tokio::sync::MutexGuard<'_, ()>, MonitorError> {
        self.shared.registration.try_lock().map_err(|_| {
            MonitorError::Runtime("monitor authority is busy with a dispatch or commit".into())
        })
    }
    pub fn snapshot(&self) -> Result<MonitoringSnapshot, MonitorError> {
        Ok(MonitoringSnapshot {
            monitors: lock(&self.shared.views)?.values().cloned().collect(),
            discoveries: lock(&self.shared.discoveries)?.values().cloned().collect(),
            runtime_error: lock(&self.shared.error)?.clone(),
            running: self.shared.accepting.load(Ordering::Acquire),
        })
    }
    pub fn monitor(&self, id: &str) -> Result<Option<MonitorSnapshot>, MonitorError> {
        Ok(lock(&self.shared.views)?.get(id).cloned())
    }
    /// The trusted enrolled definition, including fixed target parameters.
    /// This is an in-process interface; HTTP summaries do not expose configuration.
    pub fn definition(&self, id: &str) -> Result<Option<MonitorDefinition>, MonitorError> {
        Ok(lock(&self.shared.definitions)?.get(id).cloned())
    }
    pub fn incidents(&self) -> Result<Vec<IncidentRecord>, MonitorError> {
        Ok(lock(&self.shared.store)?.list())
    }
    pub fn map_incidents<T>(
        &self,
        projection: impl FnMut(&IncidentRecord) -> T,
    ) -> Result<Vec<T>, MonitorError> {
        Ok(lock(&self.shared.store)?.map_records(projection))
    }
    pub fn incident(&self, id: &str) -> Result<Option<IncidentRecord>, MonitorError> {
        Ok(lock(&self.shared.store)?.get(id))
    }
    /// Read one authoritative episode together with its current observation
    /// readiness. This neither grants action authority nor freezes future health.
    /// Resolved episodes are returned so a caller can cancel obsolete work.
    /// Active episodes require complete fresh evidence and a running source.
    /// Revision advances from acknowledgement or new observations are allowed.
    pub fn repair_incident(
        &self,
        incident_id: &str,
        target_id: &str,
        minimum_revision: u64,
    ) -> Result<IncidentRecord, MonitorError> {
        self.with_repair_incident(incident_id, target_id, minimum_revision, |record| record)
    }
    /// Runs a trusted bounded synchronous authorization commit while retaining
    /// the registration gate from the authoritative read through the callback.
    /// Observation commits and ordinary shutdown cannot cross that interval.
    /// All incident, definition, view, deadline and error locks are released
    /// before calling `callback`; only the registration gate remains held.
    /// The callback must not reenter monitor APIs or run external actions. Its
    /// scope is limited to the host's synchronous durable authorization commit;
    /// dispatch external work only after this method returns.
    pub fn with_repair_incident<R>(
        &self,
        incident_id: &str,
        target_id: &str,
        minimum_revision: u64,
        callback: impl FnOnce(IncidentRecord) -> R,
    ) -> Result<R, MonitorError> {
        let _registration = self.registration()?;
        let record = self.repair_incident_registered(incident_id, target_id, minimum_revision)?;
        Ok(callback(record))
    }
    /// The caller retains `registration`; all other guards end at this boundary.
    fn repair_incident_registered(
        &self,
        incident_id: &str,
        target_id: &str,
        minimum_revision: u64,
    ) -> Result<IncidentRecord, MonitorError> {
        let store = lock(&self.shared.store)?;
        let record = store
            .get(incident_id)
            .ok_or_else(|| MonitorError::Observation("repair incident is missing".into()))?;
        if minimum_revision == 0
            || record.revision < minimum_revision
            || record.target_id != target_id
            || record.kind != IncidentKind::Target
        {
            return Err(MonitorError::Observation(
                "repair incident identity or revision mismatch".into(),
            ));
        }
        if record.status == IncidentStatus::Resolved {
            return Ok(record);
        }
        if !self.shared.accepting.load(Ordering::Acquire) || self.shared.cancellation.is_cancelled()
        {
            return Err(MonitorError::Stopped);
        }
        if record.condition != SignalCondition::Active
            || !matches!(
                record.status,
                IncidentStatus::Open | IncidentStatus::Acknowledged
            )
        {
            return Err(MonitorError::Observation(
                "repair incident lacks current active evidence".into(),
            ));
        }
        let definitions = lock(&self.shared.definitions)?;
        let definition = definitions.get(&record.monitor_id).ok_or_else(|| {
            MonitorError::Observation("repair monitor definition is missing".into())
        })?;
        if definition.target_id != target_id || record.rule_id != definition.id {
            return Err(MonitorError::Observation(
                "repair monitor identity mismatch".into(),
            ));
        }
        let views = lock(&self.shared.views)?;
        let view = views
            .get(&record.monitor_id)
            .ok_or_else(|| MonitorError::Observation("repair monitor view is missing".into()))?;
        let deadlines = lock(&self.shared.fresh_until)?;
        if !view.running
            || view.target_id != target_id
            || view.source_id != definition.source_id
            || view.health != TargetHealth::Unhealthy
            || view.freshness != Freshness::Fresh
            || view.coverage != Coverage::Complete
            || deadlines
                .get(&record.monitor_id)
                .is_none_or(|deadline| tokio::time::Instant::now() >= *deadline)
        {
            return Err(MonitorError::Observation(
                "repair monitor is not running with fresh complete unhealthy evidence".into(),
            ));
        }
        // Runtime failure can race independently of the registration gate.
        // Recheck its irreversible cancellation after reading the complete view.
        if !self.shared.accepting.load(Ordering::Acquire) || self.shared.cancellation.is_cancelled()
        {
            return Err(MonitorError::Stopped);
        }
        if let Some(error) = lock(&self.shared.error)?.clone() {
            return Err(MonitorError::Runtime(error));
        }
        Ok(record)
    }
    pub fn acknowledge(
        &self,
        id: &str,
        expected_revision: u64,
        actor: &str,
        note: &str,
    ) -> Result<IncidentRecord, MonitorError> {
        let _registration = self.registration()?;
        let mut store = lock(&self.shared.store)?;
        if !self.shared.accepting.load(Ordering::Acquire) {
            return Err(MonitorError::Stopped);
        }
        match store.acknowledge(id, expected_revision, actor, note, now_ms()) {
            Ok(record) => Ok(record),
            Err(error) => {
                if matches!(
                    error,
                    IncidentError::Io(_)
                        | IncidentError::Unavailable(_)
                        | IncidentError::Corrupt(_)
                        | IncidentError::Capacity(_)
                ) {
                    self.shared.fail(error.to_string());
                }
                Err(error.into())
            }
        }
    }
    pub fn begin_shutdown(&self) -> Result<(), MonitorError> {
        let _registration = self.registration()?;
        self.stop_registered()
    }
    pub(crate) async fn begin_shutdown_async(&self) -> Result<(), MonitorError> {
        let _registration = self.shared.registration.lock().await;
        self.stop_registered()
    }
    pub(crate) fn request_stop(&self) {
        self.shared.accepting.store(false, Ordering::Release);
        self.shared.cancellation.cancel();
    }
    fn stop_registered(&self) -> Result<(), MonitorError> {
        self.request_stop();
        for view in lock(&self.shared.views)?.values_mut() {
            view.running = false;
            view.health = TargetHealth::Unknown;
        }
        for view in lock(&self.shared.discoveries)?.values_mut() {
            view.running = false;
        }
        if *self.shared.workers.borrow() == 0 {
            lock(&self.shared.store)?.close()?;
        }
        Ok(())
    }
    pub async fn wait_for_idle(&self) -> Result<(), MonitorError> {
        let mut workers = self.shared.workers.subscribe();
        loop {
            if *workers.borrow_and_update() == 0 {
                break;
            }
            workers
                .changed()
                .await
                .map_err(|_| MonitorError::Runtime("monitor supervisor disappeared".into()))?;
        }
        let _registration = self.shared.registration.lock().await;
        lock(&self.shared.store)?.close()?;
        if let Some(error) = lock(&self.shared.error)?.clone() {
            return Err(MonitorError::Runtime(error));
        }
        Ok(())
    }
}
