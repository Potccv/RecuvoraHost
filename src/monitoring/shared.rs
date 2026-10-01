//! Shared runtime ownership, registration gate and active worker tracking.
use super::{DiscoverySnapshot, MonitorDefinition, MonitorError, MonitorSnapshot, TargetHealth};
use crate::persistence::incidents::IncidentStore;
use crate::runtime::operation::Cancellation;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use tokio::sync::watch;

pub(super) struct Shared {
    pub(super) store: Mutex<IncidentStore>,
    pub(super) definitions: Mutex<BTreeMap<String, MonitorDefinition>>,
    pub(super) views: Mutex<BTreeMap<String, MonitorSnapshot>>,
    pub(super) fresh_until: Mutex<BTreeMap<String, tokio::time::Instant>>,
    pub(super) discoveries: Mutex<BTreeMap<String, DiscoverySnapshot>>,
    pub(super) registration: Arc<tokio::sync::Mutex<()>>,
    pub(super) error: Mutex<Option<String>>,
    pub(super) cancellation: Cancellation,
    pub(super) accepting: AtomicBool,
    pub(super) workers: watch::Sender<usize>,
}

pub(super) fn lock<T>(mutex: &Mutex<T>) -> Result<MutexGuard<'_, T>, MonitorError> {
    mutex
        .lock()
        .map_err(|_| MonitorError::Runtime("monitor state lock poisoned".into()))
}

impl Shared {
    pub(super) fn fail(&self, message: String) {
        if let Ok(mut error) = self.error.lock() {
            *error = Some(message.clone());
        }
        if let Ok(mut views) = self.views.lock() {
            for view in views.values_mut() {
                view.running = false;
                view.health = TargetHealth::Unknown;
                view.last_error = Some(message.clone());
            }
        }
        self.accepting.store(false, Ordering::Release);
        self.cancellation.cancel();
        if let Ok(mut discoveries) = self.discoveries.lock() {
            for view in discoveries.values_mut() {
                view.running = false;
            }
        }
    }
}

pub(super) async fn worker_finished(shared: &Shared) {
    let _registration = shared.registration.lock().await;
    let result = (|| -> Result<(), MonitorError> {
        shared
            .workers
            .send_modify(|count| *count = count.saturating_sub(1));
        if *shared.workers.borrow() == 0 && !shared.accepting.load(Ordering::Acquire) {
            lock(&shared.store)?.close()?;
        }
        Ok(())
    })();
    if let Err(error) = result {
        shared.fail(error.to_string());
    }
}
