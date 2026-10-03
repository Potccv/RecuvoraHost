use recuvora_host::control::recovery::incidents::{IncidentKind, IncidentStatus};
use recuvora_host::monitoring::*;
use recuvora_host::runtime::operation::Cancellation;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, oneshot};

static NEXT_DIR: AtomicU64 = AtomicU64::new(0);
struct TestDir {
    path: PathBuf,
    root: PathBuf,
}
impl TestDir {
    fn new() -> Self {
        let root =
            PathBuf::from(std::env::var_os("RECUVORA_TEST_TEMP").expect("external test root"));
        assert!(root.is_absolute());
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        assert!(
            !root.starts_with(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .canonicalize()
                    .unwrap()
            )
        );
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = root.join(format!(
            "monitors-{}-{stamp}-{}",
            std::process::id(),
            NEXT_DIR.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self { path, root }
    }
}
impl Drop for TestDir {
    fn drop(&mut self) {
        assert_eq!(self.path.parent(), Some(self.root.as_path()));
        if let Err(error) = std::fs::remove_dir_all(&self.path) {
            if std::thread::panicking() {
                eprintln!(
                    "retained monitor test directory {} after failure: {error}",
                    self.path.display()
                );
            } else {
                panic!(
                    "remove only recorded test directory {}: {error}",
                    self.path.display()
                );
            }
        }
    }
}

struct Pending {
    request: ObservationRequest,
    reply: oneshot::Sender<Result<ObservationBatch, MonitorError>>,
}
struct ControlledSource {
    sender: mpsc::UnboundedSender<Pending>,
}
impl ObservationSource for ControlledSource {
    fn poll(
        &self,
        request: ObservationRequest,
        cancellation: Cancellation,
    ) -> ObservationFuture<'_> {
        let sender = self.sender.clone();
        Box::pin(async move {
            let (reply, receiver) = oneshot::channel();
            sender
                .send(Pending { request, reply })
                .map_err(|_| MonitorError::Runtime("test controller closed".into()))?;
            tokio::select! {
                result = receiver => result.map_err(|_| MonitorError::Runtime("test reply closed".into()))?,
                _ = cancellation.cancelled() => Err(MonitorError::Observation("source cancelled".into())),
            }
        })
    }
}
fn source() -> (Arc<ControlledSource>, mpsc::UnboundedReceiver<Pending>) {
    let (sender, receiver) = mpsc::unbounded_channel();
    (Arc::new(ControlledSource { sender }), receiver)
}
fn config() -> MonitorsConfig {
    MonitorsConfig {
        schema_version: 2,
        discoveries: vec![],
        monitors: vec![MonitorDefinition {
            id: "service-ready".into(),
            target_id: "service".into(),
            source_id: "ready-probe".into(),
            view_role: Some("readiness".into()),
            extension_id: "probe-plugin".into(),
            contract: "example.monitor".into(),
            version: 2,
            method: "observe".into(),
            params: json!({}),
            interval_ms: 1000,
            timeout_ms: 25_000,
            stale_after_ms: 10_000,
            startup_grace_ms: 1500,
        }],
    }
}
fn batch(request: &ObservationRequest, sequence: u64, value: Value) -> ObservationBatch {
    ObservationBatch {
        schema_version: 2,
        target_id: "service".into(),
        source_id: "ready-probe".into(),
        generation: "g1".into(),
        cursor: request.params["cursor"].as_str().map(str::to_owned),
        next_cursor: format!("cursor-{sequence}"),
        coverage: BatchCoverage::Complete,
        has_more: false,
        source_error: None,
        errors: vec![ObservationSample {
            id: format!("sample-{sequence}"),
            sequence,
            age_ms: 0,
            fingerprint: "provider-error".into(),
            message: "provider reported failure".into(),
            evidence: value,
        }],
    }
}
async fn settle() {
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
}
async fn respond(pending: Pending, sequence: u64, healthy: bool) {
    let mut response = batch(&pending.request, sequence, json!({"detail":"node failure"}));
    if healthy {
        response.errors.clear();
    }
    pending.reply.send(Ok(response)).unwrap();
    settle().await;
}
async fn next(receiver: &mut mpsc::UnboundedReceiver<Pending>) -> Pending {
    tokio::time::advance(Duration::from_millis(1000)).await;
    receiver.recv().await.expect("next poll")
}
fn view(handle: &MonitorHandle) -> MonitorSnapshot {
    handle.monitor("service-ready").unwrap().unwrap()
}

struct PendingDiscovery {
    request: ObservationRequest,
    reply: oneshot::Sender<Result<DiscoveryBatch, MonitorError>>,
}
struct InventorySource {
    observations: ControlledSource,
    inventories: mpsc::UnboundedSender<PendingDiscovery>,
}

struct HeldObservationSource(Arc<InventorySource>);
impl ObservationSource for HeldObservationSource {
    fn poll(
        &self,
        request: ObservationRequest,
        _cancellation: Cancellation,
    ) -> ObservationFuture<'_> {
        let sender = self.0.observations.sender.clone();
        Box::pin(async move {
            // Explicitly released by the test after cancellation, like a source
            // returning a final reply while shutdown waits for active calls.
            let (reply, receiver) = oneshot::channel();
            sender.send(Pending { request, reply }).unwrap();
            receiver.await.unwrap()
        })
    }
    fn discover(
        &self,
        request: ObservationRequest,
        cancellation: Cancellation,
    ) -> DiscoveryFuture<'_> {
        self.0.discover(request, cancellation)
    }
}
impl ObservationSource for InventorySource {
    fn poll(
        &self,
        request: ObservationRequest,
        cancellation: Cancellation,
    ) -> ObservationFuture<'_> {
        self.observations.poll(request, cancellation)
    }
    fn discover(
        &self,
        request: ObservationRequest,
        cancellation: Cancellation,
    ) -> DiscoveryFuture<'_> {
        let sender = self.inventories.clone();
        Box::pin(async move {
            let (reply, receiver) = oneshot::channel();
            sender
                .send(PendingDiscovery { request, reply })
                .map_err(|_| MonitorError::Runtime("inventory controller closed".into()))?;
            tokio::select! {
                result = receiver => result.map_err(|_| MonitorError::Runtime("inventory reply closed".into()))?,
                _ = cancellation.cancelled() => Err(MonitorError::Observation("inventory cancelled".into())),
            }
        })
    }
}
fn inventory_source() -> (
    Arc<InventorySource>,
    mpsc::UnboundedReceiver<Pending>,
    mpsc::UnboundedReceiver<PendingDiscovery>,
) {
    let (observations, pending) = mpsc::unbounded_channel();
    let (inventories, discovery) = mpsc::unbounded_channel();
    (
        Arc::new(InventorySource {
            observations: ControlledSource {
                sender: observations,
            },
            inventories,
        }),
        pending,
        discovery,
    )
}
fn discovery_config() -> MonitorsConfig {
    let template = config().monitors.remove(0);
    MonitorsConfig {
        schema_version: 2,
        monitors: vec![],
        discoveries: vec![MonitorDiscovery {
            id: "inventory-main".into(),
            extension_id: "probe-plugin".into(),
            contract: "example.monitor".into(),
            version: 2,
            method: "inventory".into(),
            params: json!({}),
            interval_ms: 1000,
            timeout_ms: 5000,
            max_targets: 4,
            parameter: "target_key".into(),
            template,
        }],
    }
}
async fn inventory_reply(pending: PendingDiscovery, keys: &[&str], complete: bool) {
    assert_eq!(pending.request.method, "inventory");
    assert_eq!(pending.request.params, json!({}));
    pending
        .reply
        .send(Ok(DiscoveryBatch {
            schema_version: 1,
            complete,
            targets: keys
                .iter()
                .map(|key| DiscoveryTarget { key: (*key).into() })
                .collect(),
            error: (!complete).then(|| "one target is not readable".into()),
        }))
        .unwrap();
    settle().await;
}
async fn inventory_next(
    pending: &mut mpsc::UnboundedReceiver<PendingDiscovery>,
) -> PendingDiscovery {
    tokio::time::advance(Duration::from_millis(1000)).await;
    pending.recv().await.expect("next discovery")
}
async fn dynamic_response(pending: Pending, sequence: u64, healthy: bool) {
    let mut response = batch(&pending.request, sequence, json!({"detail":"node failure"}));
    if healthy {
        response.errors.clear();
    }
    response.target_id = pending.request.params["target_id"].as_str().unwrap().into();
    response.source_id = pending.request.params["source_id"].as_str().unwrap().into();
    pending.reply.send(Ok(response)).unwrap();
    settle().await;
}

#[tokio::test(start_paused = true)]
async fn discovery_enrols_after_empty_start_and_isolates_simultaneous_targets() {
    let dir = TestDir::new();
    let (source, mut polls, mut inventories) = inventory_source();
    let mut engine =
        MonitorEngine::start_with_source(discovery_config(), source, &dir.path).unwrap();
    let handle = engine.handle();
    inventory_reply(inventories.recv().await.unwrap(), &[], true).await;
    assert!(handle.snapshot().unwrap().monitors.is_empty());
    assert!(handle.snapshot().unwrap().running);
    inventory_reply(
        inventory_next(&mut inventories).await,
        &["alpha", "beta"],
        true,
    )
    .await;
    let a = polls.recv().await.unwrap();
    let b = polls.recv().await.unwrap();
    let mut names = vec![
        a.request.params["target_key"].as_str().unwrap().to_owned(),
        b.request.params["target_key"].as_str().unwrap().to_owned(),
    ];
    names.sort();
    assert_eq!(names, ["alpha", "beta"]);
    assert_ne!(a.request.params["target_id"], b.request.params["target_id"]);
    assert_ne!(a.request.params["source_id"], b.request.params["source_id"]);
    for item in [a, b] {
        let healthy = item.request.params["target_key"] == "beta";
        dynamic_response(item, 1, healthy).await;
    }
    assert_eq!(
        handle
            .monitor("service-ready.alpha")
            .unwrap()
            .unwrap()
            .received_error_count,
        1
    );
    assert_eq!(
        handle
            .monitor("service-ready.beta")
            .unwrap()
            .unwrap()
            .received_error_count,
        0
    );
    let monitor = handle.monitor("service-ready.beta").unwrap().unwrap();
    let monitor_json = serde_json::to_value(&monitor).unwrap();
    assert_eq!(monitor_json["view_role"], "readiness");
    assert_eq!(monitor_json["contract"], "example.monitor");
    assert_eq!(monitor_json["version"], 2);
    assert_eq!(monitor_json["method"], "observe");
    let discovery_json = serde_json::to_value(&handle.snapshot().unwrap().discoveries[0]).unwrap();
    assert_eq!(discovery_json["contract"], "example.monitor");
    assert_eq!(discovery_json["version"], 2);
    assert_eq!(discovery_json["method"], "inventory");
    let incident = handle
        .incidents()
        .unwrap()
        .into_iter()
        .find(|i| i.kind == IncidentKind::ErrorLog)
        .unwrap();
    assert_eq!(incident.monitor_id, "service-ready.alpha");
    engine.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn discovery_partial_duplicate_and_capacity_do_not_remove_existing_targets() {
    let dir = TestDir::new();
    let (source, _polls, mut inventories) = inventory_source();
    let mut engine =
        MonitorEngine::start_with_source(discovery_config(), source, &dir.path).unwrap();
    let handle = engine.handle();
    inventory_reply(inventories.recv().await.unwrap(), &["a", "b"], true).await;
    inventory_reply(inventory_next(&mut inventories).await, &["c"], false).await;
    assert_eq!(handle.snapshot().unwrap().monitors.len(), 3);
    assert_eq!(handle.snapshot().unwrap().discoveries[0].present_targets, 3);
    inventory_reply(inventory_next(&mut inventories).await, &["a", "a"], true).await;
    let status = handle.snapshot().unwrap();
    assert_eq!(status.monitors.len(), 3);
    assert!(status.discoveries[0].last_error.is_some());
    assert_eq!(status.discoveries[0].present_targets, 3);
    inventory_reply(
        inventory_next(&mut inventories).await,
        &["a", "b", "c", "d", "e"],
        true,
    )
    .await;
    let status = handle.snapshot().unwrap();
    assert_eq!(status.monitors.len(), 3);
    assert!(status.discoveries[0].last_error.is_some());
    assert!(status.runtime_error.is_none());
    engine.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn discovery_removal_and_restart_retain_identity_cursor_and_acknowledged_fault() {
    let dir = TestDir::new();
    let (source, mut polls, mut inventories) = inventory_source();
    let cfg = discovery_config();
    let mut engine = MonitorEngine::start_with_source(cfg.clone(), source, &dir.path).unwrap();
    let handle = engine.handle();
    inventory_reply(inventories.recv().await.unwrap(), &["alpha"], true).await;
    dynamic_response(polls.recv().await.unwrap(), 1, false).await;
    let incident = handle
        .incidents()
        .unwrap()
        .into_iter()
        .find(|i| i.kind == IncidentKind::ErrorLog)
        .unwrap();
    handle
        .acknowledge(&incident.id, incident.revision, "operator", "received")
        .unwrap();
    let cursor = handle
        .monitor("service-ready.alpha")
        .unwrap()
        .unwrap()
        .cursor;
    inventory_reply(inventory_next(&mut inventories).await, &[], true).await;
    tokio::time::advance(Duration::from_millis(2000)).await;
    settle().await;
    assert_eq!(handle.snapshot().unwrap().discoveries[0].present_targets, 0);
    let missing = handle.monitor("service-ready.alpha").unwrap().unwrap();
    assert_eq!(missing.coverage, Coverage::Unavailable);
    assert_eq!(missing.cursor, cursor);
    assert_ne!(
        handle.incident(&incident.id).unwrap().unwrap().status,
        IncidentStatus::Resolved
    );
    engine.shutdown().await.unwrap();
    let (source, mut polls, mut inventories) = inventory_source();
    let mut restarted_config = cfg;
    restarted_config.discoveries[0].template.view_role = Some("diagnostic".into());
    let mut engine = MonitorEngine::start_with_source(restarted_config, source, &dir.path).unwrap();
    let handle = engine.handle();
    assert_eq!(
        handle
            .monitor("service-ready.alpha")
            .unwrap()
            .unwrap()
            .freshness,
        Freshness::Missing
    );
    assert_eq!(
        handle
            .monitor("service-ready.alpha")
            .unwrap()
            .unwrap()
            .view_role
            .as_deref(),
        Some("diagnostic"),
        "display-only roles must not invalidate persisted source checkpoints"
    );
    assert_eq!(handle.snapshot().unwrap().discoveries[0].known_targets, 1);
    assert_eq!(handle.snapshot().unwrap().discoveries[0].present_targets, 0);
    assert!(polls.try_recv().is_err());
    assert_eq!(
        handle.incident(&incident.id).unwrap().unwrap().status,
        IncidentStatus::Acknowledged
    );
    inventory_reply(
        inventories.recv().await.unwrap(),
        &["alpha", "renamed"],
        true,
    )
    .await;
    tokio::time::advance(Duration::from_millis(1000)).await;
    settle().await;
    for _ in 0..2 {
        let item = polls.recv().await.unwrap();
        if item.request.params["target_key"] == "alpha" {
            assert_eq!(item.request.params["cursor"], json!(cursor));
        } else {
            assert_eq!(item.request.params["target_key"], "renamed");
            assert!(item.request.params["cursor"].is_null());
        }
    }
    assert_eq!(handle.snapshot().unwrap().monitors.len(), 2);
    engine.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn discovery_timeout_and_shutdown_with_no_monitors_wait_for_source_and_release_store() {
    let dir = TestDir::new();
    let (source, _polls, mut inventories) = inventory_source();
    let mut engine =
        MonitorEngine::start_with_source(discovery_config(), source, &dir.path).unwrap();
    let held = inventories.recv().await.unwrap();
    tokio::time::advance(Duration::from_millis(5001)).await;
    settle().await;
    assert!(
        engine.handle().snapshot().unwrap().discoveries[0]
            .last_error
            .is_some()
    );
    assert!(held.reply.is_closed());
    engine.shutdown().await.unwrap();
    let (source, _, _) = inventory_source();
    let mut restarted =
        MonitorEngine::start_with_source(discovery_config(), source, &dir.path).unwrap();
    restarted.shutdown().await.unwrap();
}

#[test]
fn discovery_templates_cannot_override_identity_scope_or_exceed_capacity() {
    let valid = discovery_config();
    valid.validate().unwrap();
    for key in ["cursor", "generation", "source_id", "target_id"] {
        let mut cfg = valid.clone();
        cfg.discoveries[0].parameter = key.into();
        assert!(cfg.validate().is_err());
    }
    let mut cfg = valid.clone();
    cfg.discoveries[0].template.params = json!({"target_key":"fixed"});
    assert!(cfg.validate().is_err());
    let mut cfg = valid.clone();
    cfg.discoveries[0].max_targets = 65;
    assert!(cfg.validate().is_err());
    let mut cfg = valid.clone();
    cfg.monitors = config().monitors;
    cfg.discoveries[0].max_targets = 64;
    assert!(cfg.validate().is_err());
    let mut cfg = valid.clone();
    cfg.discoveries[0].template.contract = "recuvora.actions".into();
    assert!(cfg.validate().is_err());
    let mut cfg = valid;
    cfg.discoveries.push(cfg.discoveries[0].clone());
    assert!(cfg.validate().is_err());

    let mut cfg = config();
    cfg.monitors[0].view_role = Some("invalid role".into());
    assert!(cfg.validate().is_err());
}

#[tokio::test(start_paused = true)]
async fn discovery_delete_and_return_rejects_inflight_reply_from_old_membership() {
    let dir = TestDir::new();
    let (source, mut polls, mut inventories) = inventory_source();
    let mut engine = MonitorEngine::start_with_source(
        discovery_config(),
        Arc::new(HeldObservationSource(source)),
        &dir.path,
    )
    .unwrap();
    let handle = engine.handle();
    inventory_reply(inventories.recv().await.unwrap(), &["alpha"], true).await;
    let held = polls.recv().await.unwrap();
    inventory_reply(inventory_next(&mut inventories).await, &[], true).await;
    inventory_reply(inventory_next(&mut inventories).await, &["alpha"], true).await;
    dynamic_response(held, 1, true).await;
    let view = handle.monitor("service-ready.alpha").unwrap().unwrap();
    assert_eq!(view.received_error_count, 0);
    assert_eq!(view.coverage, Coverage::Unavailable);
    assert!(view.cursor.is_none());
    engine.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn discovery_delete_and_return_between_polls_invalidates_coverage() {
    let dir = TestDir::new();
    let (source, mut polls, mut inventories) = inventory_source();
    let mut cfg = discovery_config();
    cfg.discoveries[0].interval_ms = 10;
    cfg.discoveries[0].template.interval_ms = 3_600_000;
    let mut engine = MonitorEngine::start_with_source(cfg, source, &dir.path).unwrap();
    let handle = engine.handle();
    inventory_reply(inventories.recv().await.unwrap(), &["alpha"], true).await;
    dynamic_response(polls.recv().await.unwrap(), 1, true).await;
    assert_eq!(
        handle
            .monitor("service-ready.alpha")
            .unwrap()
            .unwrap()
            .received_error_count,
        0
    );
    tokio::time::advance(Duration::from_millis(10)).await;
    inventory_reply(inventories.recv().await.unwrap(), &[], true).await;
    tokio::time::advance(Duration::from_millis(10)).await;
    inventory_reply(inventories.recv().await.unwrap(), &["alpha"], true).await;
    tokio::time::advance(Duration::from_millis(250)).await;
    settle().await;
    let view = handle.monitor("service-ready.alpha").unwrap().unwrap();
    assert_eq!(view.received_error_count, 0);
    assert_eq!(view.coverage, Coverage::Unavailable);
    assert_eq!(view.cursor.as_deref(), Some("cursor-1"));
    assert!(polls.try_recv().is_err());
    engine.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn discovery_restart_waits_startup_grace_without_false_incident_on_first_validation() {
    let dir = TestDir::new();
    let cfg = discovery_config();
    let (source, mut polls, mut inventories) = inventory_source();
    let mut first = MonitorEngine::start_with_source(cfg.clone(), source, &dir.path).unwrap();
    inventory_reply(inventories.recv().await.unwrap(), &["alpha"], true).await;
    dynamic_response(polls.recv().await.unwrap(), 1, true).await;
    assert!(first.handle().incidents().unwrap().is_empty());
    first.shutdown().await.unwrap();
    let (source, mut polls, mut inventories) = inventory_source();
    let mut restarted = MonitorEngine::start_with_source(cfg.clone(), source, &dir.path).unwrap();
    let handle = restarted.handle();
    let pending = inventories.recv().await.unwrap();
    tokio::time::advance(Duration::from_millis(1000)).await;
    settle().await;
    assert!(polls.try_recv().is_err());
    assert!(handle.incidents().unwrap().is_empty());
    inventory_reply(pending, &["alpha"], true).await;
    dynamic_response(polls.recv().await.unwrap(), 2, true).await;
    assert!(handle.incidents().unwrap().is_empty());
    restarted.shutdown().await.unwrap();
    let (source, mut polls, mut inventories) = inventory_source();
    let mut missing = MonitorEngine::start_with_source(cfg, source, &dir.path).unwrap();
    let held = inventories.recv().await.unwrap();
    tokio::time::advance(Duration::from_millis(1750)).await;
    settle().await;
    assert!(polls.try_recv().is_err());
    assert!(
        missing
            .handle()
            .incidents()
            .unwrap()
            .iter()
            .any(|i| i.kind == IncidentKind::Coverage && i.status == IncidentStatus::Open)
    );
    assert!(!held.reply.is_closed());
    missing.shutdown().await.unwrap();
}

fn receipts(
    handle: &MonitorHandle,
) -> Vec<recuvora_host::control::recovery::incidents::IncidentRecord> {
    handle
        .incidents()
        .unwrap()
        .into_iter()
        .filter(|record| record.kind == IncidentKind::ErrorLog)
        .collect()
}

#[tokio::test(start_paused = true)]
async fn every_new_node_error_is_received_immediately_and_notifies_after_durable_commit() {
    let dir = TestDir::new();
    let (source, mut pending) = source();
    let mut engine = MonitorEngine::start_with_source(config(), source, &dir.path).unwrap();
    let handle = engine.handle();
    let notifications = handle.error_notifications();
    let request = pending.recv().await.unwrap();
    let mut response = batch(&request.request, 1, json!({"original":"evidence"}));
    response.errors[0].message = "node error\nwith original context".into();
    response.errors[0].age_ms = 500_000;
    request.reply.send(Ok(response)).unwrap();
    notifications.notified().await;
    let first = receipts(&handle).remove(0);
    assert_eq!(first.summary, "node error\nwith original context");
    assert_eq!(
        first.evidence["log"]["evidence"],
        json!({"original":"evidence"})
    );
    assert_eq!(first.evidence["log"]["age_ms"], 500_000);
    assert_eq!(first.evidence["kind"], "node_error");
    assert_eq!(first.occurrences, 1);
    assert_eq!(view(&handle).freshness, Freshness::Fresh);
    respond(next(&mut pending).await, 2, false).await;
    assert_eq!(receipts(&handle).len(), 2);
    assert_eq!(view(&handle).received_error_count, 2);
    assert_eq!(view(&handle).last_error_log.unwrap().id, "sample-2");
    assert_eq!(handle.incident(&first.id).unwrap().unwrap(), first);
    assert!(
        receipts(&handle)
            .iter()
            .all(|record| record.status == IncidentStatus::Open)
    );
    engine.shutdown().await.unwrap();
    let (source, _) = self::source();
    let mut reopened = MonitorEngine::start_with_source(config(), source, &dir.path).unwrap();
    assert_eq!(receipts(&reopened.handle()).len(), 2);
    reopened.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn empty_partial_failed_and_stale_sources_never_clear_received_errors() {
    let dir = TestDir::new();
    let (source, mut pending) = source();
    let mut engine = MonitorEngine::start_with_source(config(), source, &dir.path).unwrap();
    let handle = engine.handle();
    respond(pending.recv().await.unwrap(), 1, false).await;
    let record = receipts(&handle).remove(0);
    let acknowledged = handle
        .acknowledge(&record.id, record.revision, "operator", "received")
        .unwrap();
    respond(next(&mut pending).await, 2, true).await;
    assert_eq!(view(&handle).freshness, Freshness::Fresh);
    let request = next(&mut pending).await;
    let mut response = batch(&request.request, 3, json!({}));
    response.errors.clear();
    response.coverage = BatchCoverage::Partial;
    request.reply.send(Ok(response)).unwrap();
    settle().await;
    assert_eq!(view(&handle).coverage, Coverage::Partial);
    let request = next(&mut pending).await;
    request
        .reply
        .send(Err(MonitorError::Observation("source unavailable".into())))
        .unwrap();
    settle().await;
    let held = next(&mut pending).await;
    tokio::time::advance(Duration::from_millis(10_000)).await;
    settle().await;
    assert_eq!(view(&handle).freshness, Freshness::Stale);
    assert_eq!(handle.incident(&record.id).unwrap().unwrap(), acknowledged);
    assert!(
        handle
            .repair_incident(&record.id, "service", record.revision)
            .is_ok()
    );
    assert!(
        handle
            .repair_incident(&record.id, "other", record.revision)
            .is_err()
    );
    assert!(
        handle
            .repair_incident(&record.id, "service", acknowledged.revision + 1)
            .is_err()
    );
    engine.shutdown().await.unwrap();
    assert!(held.reply.is_closed());
    assert!(
        handle
            .repair_incident(&record.id, "service", record.revision)
            .is_err()
    );
}

#[tokio::test(start_paused = true)]
async fn duplicate_age_is_ignored_but_immutable_conflicts_reject_the_entire_batch() {
    let dir = TestDir::new();
    let (source, mut pending) = source();
    let mut engine = MonitorEngine::start_with_source(config(), source, &dir.path).unwrap();
    let handle = engine.handle();
    respond(pending.recv().await.unwrap(), 1, false).await;
    let original = receipts(&handle).remove(0);
    handle.error_notifications().notified().await;
    let request = next(&mut pending).await;
    let mut duplicate = batch(&request.request, 1, json!({"detail":"node failure"}));
    duplicate.errors[0].age_ms = 100_000;
    request.reply.send(Ok(duplicate)).unwrap();
    settle().await;
    assert_eq!(handle.incident(&original.id).unwrap().unwrap(), original);
    assert!(
        tokio::time::timeout(
            Duration::from_millis(1),
            handle.error_notifications().notified()
        )
        .await
        .is_err()
    );
    for conflict in ["message", "fingerprint", "evidence", "sequence"] {
        let request = next(&mut pending).await;
        let mut response = batch(&request.request, 1, json!({"detail":"node failure"}));
        match conflict {
            "message" => response.errors[0].message = "changed".into(),
            "fingerprint" => response.errors[0].fingerprint = "changed".into(),
            "evidence" => response.errors[0].evidence = json!({"changed":true}),
            _ => response.errors[0].sequence = 2,
        }
        let fresh = batch(&request.request, 3, json!({})).errors.remove(0);
        response.errors.push(fresh);
        response.next_cursor = "must-not-commit".into();
        request.reply.send(Ok(response)).unwrap();
        settle().await;
        assert_eq!(view(&handle).cursor.as_deref(), Some("cursor-1"));
        assert_eq!(receipts(&handle), vec![original.clone()]);
    }
    engine.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn all_persisted_receipts_rebuild_dedup_beyond_the_recent_window() {
    let dir = TestDir::new();
    let (source, mut pending) = source();
    let mut engine = MonitorEngine::start_with_source(config(), source, &dir.path).unwrap();
    respond(pending.recv().await.unwrap(), 1, false).await;
    for sequence in 2..=40 {
        respond(next(&mut pending).await, sequence, false).await;
    }
    let original = receipts(&engine.handle());
    engine.shutdown().await.unwrap();
    let (source, mut pending) = self::source();
    let mut restarted = MonitorEngine::start_with_source(config(), source, &dir.path).unwrap();
    let handle = restarted.handle();
    assert_eq!(view(&handle).received_error_count, 40);
    let request = pending.recv().await.unwrap();
    assert_eq!(request.request.params["cursor"], "cursor-40");
    let mut duplicate = batch(&request.request, 1, json!({"detail":"node failure"}));
    duplicate.next_cursor = "cursor-40".into();
    request.reply.send(Ok(duplicate)).unwrap();
    settle().await;
    assert_eq!(receipts(&handle), original);
    let request = next(&mut pending).await;
    let mut conflict = batch(&request.request, 1, json!({"changed":true}));
    conflict.next_cursor = "wrong".into();
    request.reply.send(Ok(conflict)).unwrap();
    settle().await;
    assert_eq!(view(&handle).cursor.as_deref(), Some("cursor-40"));
    assert_eq!(receipts(&handle), original);
    restarted.shutdown().await.unwrap();
    let (source, _) = self::source();
    let mut changed = config();
    changed.monitors[0].params = json!({"different":true});
    assert!(MonitorEngine::start_with_source(changed, source, &dir.path).is_err());
}

#[tokio::test(start_paused = true)]
async fn generation_change_creates_new_receipts_and_retired_generation_is_rejected() {
    let dir = TestDir::new();
    let (source, mut pending) = source();
    let mut engine = MonitorEngine::start_with_source(config(), source, &dir.path).unwrap();
    let handle = engine.handle();
    respond(pending.recv().await.unwrap(), 1, false).await;
    let request = next(&mut pending).await;
    let mut response = batch(&request.request, 1, json!({"detail":"node failure"}));
    response.generation = "g2".into();
    response.next_cursor = "g2-position".into();
    request.reply.send(Ok(response)).unwrap();
    settle().await;
    assert_eq!(receipts(&handle).len(), 2);
    assert_eq!(view(&handle).coverage, Coverage::Partial);
    let request = next(&mut pending).await;
    let mut retired = batch(&request.request, 2, json!({}));
    retired.next_cursor = "retired".into();
    request.reply.send(Ok(retired)).unwrap();
    settle().await;
    assert_eq!(view(&handle).cursor.as_deref(), Some("g2-position"));
    assert_eq!(receipts(&handle).len(), 2);
    engine.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn malformed_batches_preserve_cursor_and_original_receipts() {
    let dir = TestDir::new();
    let (source, mut pending) = source();
    let mut engine = MonitorEngine::start_with_source(config(), source, &dir.path).unwrap();
    let handle = engine.handle();
    respond(pending.recv().await.unwrap(), 1, false).await;
    for case in 0..12 {
        let request = next(&mut pending).await;
        let mut response = batch(&request.request, 2, json!({}));
        match case {
            0 => response.target_id = "other".into(),
            1 => response.cursor = None,
            2 => response.schema_version = 1,
            3 => response.errors[0].message = "x".repeat(8193),
            4 => response.errors[0].message = "bad\0message".into(),
            5 => response.errors[0].fingerprint = "bad fingerprint".into(),
            6 => response.errors[0].evidence = json!([1, 2]),
            7 => response.errors[0].sequence = 9_007_199_254_740_993,
            8 => response.errors[0].age_ms = 9_007_199_254_740_993,
            9 => {
                let mut nested = json!({});
                for _ in 0..26 {
                    nested = json!({"child":nested});
                }
                response.errors[0].evidence = nested;
            }
            10 => response
                .errors
                .push(batch(&request.request, 1, json!({})).errors.remove(0)),
            _ => {
                response.errors[0].sequence = 1;
                response.errors[0].id = "another-id".into();
            }
        }
        request.reply.send(Ok(response)).unwrap();
        settle().await;
        assert_eq!(
            view(&handle).cursor.as_deref(),
            Some("cursor-1"),
            "case {case}"
        );
        assert_eq!(receipts(&handle).len(), 1, "case {case}");
    }
    engine.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn journal_capacity_failure_stops_instance_without_partial_receipts_or_cursor() {
    use recuvora_host::persistence::incidents::IncidentStoreConfig;
    let dir = TestDir::new();
    let (source, mut pending) = source();
    let limits = IncidentStoreConfig {
        max_incidents: 1,
        ..IncidentStoreConfig::default()
    };
    let mut engine = MonitorEngine::start_with_source_and_incident_config(
        config(),
        source,
        &dir.path,
        limits.clone(),
    )
    .unwrap();
    let handle = engine.handle();
    let request = pending.recv().await.unwrap();
    let mut response = batch(&request.request, 1, json!({}));
    response
        .errors
        .push(batch(&request.request, 2, json!({})).errors.remove(0));
    request.reply.send(Ok(response)).unwrap();
    settle().await;
    assert!(!handle.snapshot().unwrap().running);
    assert!(handle.snapshot().unwrap().runtime_error.is_some());
    assert!(receipts(&handle).is_empty());
    assert_eq!(view(&handle).cursor, None);
    assert!(engine.shutdown().await.is_err());
    let (source, _) = self::source();
    let mut reopened =
        MonitorEngine::start_with_source_and_incident_config(config(), source, &dir.path, limits)
            .unwrap();
    assert_eq!(view(&reopened.handle()).cursor, None);
    assert!(receipts(&reopened.handle()).is_empty());
    reopened.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn maximum_error_batch_is_atomic_and_oversized_batch_is_rejected() {
    let dir = TestDir::new();
    let (source, mut pending) = source();
    let mut engine = MonitorEngine::start_with_source(config(), source, &dir.path).unwrap();
    let handle = engine.handle();
    let request = pending.recv().await.unwrap();
    let mut response = batch(&request.request, 1, json!({}));
    response.errors = (1..=32)
        .map(|sequence| {
            let mut log = batch(
                &request.request,
                sequence,
                json!({"padding":"e".repeat(3000)}),
            )
            .errors
            .remove(0);
            log.message = "m".repeat(4700);
            log
        })
        .collect();
    assert!(serde_json::to_vec(&response).unwrap().len() < 256 * 1024);
    request.reply.send(Ok(response)).unwrap();
    settle().await;
    assert!(handle.snapshot().unwrap().runtime_error.is_none());
    assert_eq!(receipts(&handle).len(), 32);
    let request = next(&mut pending).await;
    let mut response = batch(&request.request, 33, json!({}));
    response.errors = (33..=64)
        .map(|sequence| {
            let mut log = batch(
                &request.request,
                sequence,
                json!({"padding":"e".repeat(4000)}),
            )
            .errors
            .remove(0);
            log.message = "m".repeat(8192);
            log
        })
        .collect();
    request.reply.send(Ok(response)).unwrap();
    settle().await;
    assert_eq!(receipts(&handle).len(), 32);
    assert_eq!(view(&handle).cursor.as_deref(), Some("cursor-1"));
    engine.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn shutdown_waits_for_original_source_future_after_cancellation() {
    let dir = TestDir::new();
    let (source, mut pending, _) = inventory_source();
    let mut engine = MonitorEngine::start_with_source(
        config(),
        Arc::new(HeldObservationSource(source)),
        &dir.path,
    )
    .unwrap();
    let handle = engine.handle();
    let held = pending.recv().await.unwrap();
    let shutdown = tokio::spawn(async move { engine.shutdown().await });
    settle().await;
    assert!(!shutdown.is_finished());
    assert!(!handle.snapshot().unwrap().running);
    let response = batch(&held.request, 1, json!({}));
    held.reply.send(Ok(response)).unwrap();
    shutdown.await.unwrap().unwrap();
    assert!(receipts(&handle).is_empty());
}

#[tokio::test(start_paused = true)]
async fn empty_engine_drop_releases_store_even_when_handle_is_retained() {
    let dir = TestDir::new();
    let cfg = MonitorsConfig {
        schema_version: 2,
        discoveries: vec![],
        monitors: vec![],
    };
    let (first_source, _) = source();
    let first = MonitorEngine::start_with_source(cfg.clone(), first_source, &dir.path).unwrap();
    let handle = first.handle();
    drop(first);
    assert!(!handle.snapshot().unwrap().running);
    let (second_source, _) = source();
    let mut second = MonitorEngine::start_with_source(cfg, second_source, &dir.path).unwrap();
    second.shutdown().await.unwrap();
}

#[test]
fn old_rules_and_observation_wire_fields_are_rejected() {
    let mut cfg = serde_json::to_value(config()).unwrap();
    cfg["monitors"][0]["rule"] = json!({"pointer":"/ready"});
    assert!(serde_json::from_value::<MonitorsConfig>(cfg).is_err());
    assert!(serde_json::from_value::<ObservationBatch>(json!({
        "schema_version":1,"target_id":"service","source_id":"ready-probe","generation":"g1",
        "cursor":null,"next_cursor":"1","coverage":"complete","has_more":false,"error":null,"samples":[]
    })).is_err());
}

#[tokio::test(start_paused = true)]
async fn published_error_log_example_is_accepted_without_health_fields() {
    let example: ErrorLogBatch =
        serde_json::from_str(include_str!("../docs/extensions/examples/error-logs.json")).unwrap();
    let dir = TestDir::new();
    let (source, mut pending) = source();
    let mut cfg = config();
    cfg.monitors[0].target_id = example.target_id.clone();
    cfg.monitors[0].source_id = example.source_id.clone();
    let mut engine = MonitorEngine::start_with_source(cfg, source, &dir.path).unwrap();
    let original = serde_json::to_value(&example.errors[0]).unwrap();
    pending
        .recv()
        .await
        .unwrap()
        .reply
        .send(Ok(example))
        .unwrap();
    settle().await;
    let handle = engine.handle();
    assert_eq!(receipts(&handle).remove(0).evidence["log"], original);
    let snapshot = serde_json::to_value(view(&handle)).unwrap();
    assert!(snapshot.get("health").is_none());
    assert!(snapshot.get("consecutive_failures").is_none());
    engine.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn unchanged_empty_batches_refresh_liveness_without_appending_journal() {
    let dir = TestDir::new();
    let (source, mut pending) = source();
    let mut engine = MonitorEngine::start_with_source(config(), source, &dir.path).unwrap();
    respond(pending.recv().await.unwrap(), 1, true).await;
    let committed_bytes = std::fs::metadata(dir.path.join("incidents.jsonl"))
        .unwrap()
        .len();
    for _ in 0..3 {
        respond(next(&mut pending).await, 1, true).await;
    }
    assert_eq!(
        std::fs::metadata(dir.path.join("incidents.jsonl"))
            .unwrap()
            .len(),
        committed_bytes
    );
    assert_eq!(view(&engine.handle()).freshness, Freshness::Fresh);
    engine.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn empty_retired_generations_cannot_be_forgotten_and_replayed() {
    let dir = TestDir::new();
    let (source, mut pending) = source();
    let mut engine = MonitorEngine::start_with_source(config(), source, &dir.path).unwrap();
    for generation in 0..=16 {
        let request = if generation == 0 {
            pending.recv().await.unwrap()
        } else {
            next(&mut pending).await
        };
        let mut response = batch(&request.request, generation + 1, json!({}));
        response.errors.clear();
        response.generation = format!("g{generation}");
        request.reply.send(Ok(response)).unwrap();
        settle().await;
    }
    let handle = engine.handle();
    assert_eq!(view(&handle).generation.as_deref(), Some("g16"));
    for generation in ["g17", "g0"] {
        let request = next(&mut pending).await;
        let mut response = batch(&request.request, 100, json!({}));
        response.errors.clear();
        response.generation = generation.into();
        request.reply.send(Ok(response)).unwrap();
        settle().await;
        assert_eq!(view(&handle).generation.as_deref(), Some("g16"));
        assert_eq!(view(&handle).cursor.as_deref(), Some("cursor-17"));
    }
    engine.shutdown().await.unwrap();
}
