use recuvora_core::recovery::incidents::{IncidentKind, IncidentStatus};
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
        std::fs::remove_dir_all(&self.path).expect("remove only this recorded test directory");
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
        schema_version: 1,
        discoveries: vec![],
        monitors: vec![MonitorDefinition {
            id: "service-ready".into(),
            target_id: "service".into(),
            source_id: "ready-probe".into(),
            view_role: Some("readiness".into()),
            extension_id: "probe-plugin".into(),
            contract: "example.monitor".into(),
            version: 1,
            method: "observe".into(),
            params: json!({}),
            interval_ms: 1000,
            timeout_ms: 25_000,
            stale_after_ms: 10_000,
            startup_grace_ms: 1500,
            rule: MonitorRule {
                pointer: "/ready".into(),
                operator: RuleOperator::Eq,
                value: json!(true),
                failure_samples: 2,
                success_samples: 2,
            },
        }],
    }
}
fn batch(request: &ObservationRequest, sequence: u64, value: Value) -> ObservationBatch {
    ObservationBatch {
        schema_version: 1,
        target_id: "service".into(),
        source_id: "ready-probe".into(),
        generation: "g1".into(),
        cursor: request.params["cursor"].as_str().map(str::to_owned),
        next_cursor: format!("cursor-{sequence}"),
        coverage: BatchCoverage::Complete,
        has_more: false,
        error: None,
        samples: vec![ObservationSample {
            id: format!("sample-{sequence}"),
            sequence,
            age_ms: 0,
            value,
            evidence: json!({"origin":"controlled-test"}),
        }],
    }
}
async fn settle() {
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
}
async fn respond(pending: Pending, sequence: u64, healthy: bool) {
    let response = batch(&pending.request, sequence, json!({"ready":healthy}));
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
    let mut template = config().monitors.remove(0);
    template.rule.failure_samples = 1;
    template.rule.success_samples = 1;
    MonitorsConfig {
        schema_version: 1,
        monitors: vec![],
        discoveries: vec![MonitorDiscovery {
            id: "inventory-main".into(),
            extension_id: "probe-plugin".into(),
            contract: "example.monitor".into(),
            version: 1,
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
    let mut response = batch(&pending.request, sequence, json!({"ready":healthy}));
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
            .health,
        TargetHealth::Unhealthy
    );
    assert_eq!(
        handle
            .monitor("service-ready.beta")
            .unwrap()
            .unwrap()
            .health,
        TargetHealth::Healthy
    );
    let monitor = handle.monitor("service-ready.beta").unwrap().unwrap();
    let monitor_json = serde_json::to_value(&monitor).unwrap();
    assert_eq!(monitor_json["view_role"], "readiness");
    assert_eq!(monitor_json["contract"], "example.monitor");
    assert_eq!(monitor_json["version"], 1);
    assert_eq!(monitor_json["method"], "observe");
    let discovery_json = serde_json::to_value(&handle.snapshot().unwrap().discoveries[0]).unwrap();
    assert_eq!(discovery_json["contract"], "example.monitor");
    assert_eq!(discovery_json["version"], 1);
    assert_eq!(discovery_json["method"], "inventory");
    let incident = handle
        .incidents()
        .unwrap()
        .into_iter()
        .find(|i| i.kind == IncidentKind::Target)
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
        .find(|i| i.kind == IncidentKind::Target)
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
    assert_eq!(missing.health, TargetHealth::Unknown);
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
            .health,
        TargetHealth::Unknown
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
    assert_eq!(view.health, TargetHealth::Unknown);
    assert_eq!(view.consecutive_successes, 0);
    assert!(view.cursor.is_none());
    engine.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn discovery_delete_and_return_between_polls_invalidates_old_health_and_counts() {
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
            .health,
        TargetHealth::Healthy
    );
    tokio::time::advance(Duration::from_millis(10)).await;
    inventory_reply(inventories.recv().await.unwrap(), &[], true).await;
    tokio::time::advance(Duration::from_millis(10)).await;
    inventory_reply(inventories.recv().await.unwrap(), &["alpha"], true).await;
    tokio::time::advance(Duration::from_millis(250)).await;
    settle().await;
    let view = handle.monitor("service-ready.alpha").unwrap().unwrap();
    assert_eq!(view.health, TargetHealth::Unknown);
    assert_eq!(view.consecutive_successes, 0);
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

#[tokio::test(start_paused = true)]
async fn stable_samples_ack_and_evidence_resolution_with_duplicate_suppression() {
    let dir = TestDir::new();
    let (source, mut pending) = source();
    let mut engine = MonitorEngine::start_with_source(config(), source, &dir.path).unwrap();
    let handle = engine.handle();
    respond(pending.recv().await.unwrap(), 1, false).await;
    assert_eq!(view(&handle).consecutive_failures, 1);
    assert!(handle.incidents().unwrap().is_empty());
    let before = std::fs::metadata(dir.path.join("incidents.jsonl"))
        .unwrap()
        .len();
    respond(next(&mut pending).await, 1, false).await;
    assert_eq!(view(&handle).consecutive_failures, 1);
    assert_eq!(
        std::fs::metadata(dir.path.join("incidents.jsonl"))
            .unwrap()
            .len(),
        before
    );
    respond(next(&mut pending).await, 2, false).await;
    assert_eq!(view(&handle).health, TargetHealth::Unhealthy);
    let incident = handle
        .incidents()
        .unwrap()
        .into_iter()
        .find(|i| i.kind == IncidentKind::Target)
        .unwrap();
    let acknowledged = handle
        .acknowledge(&incident.id, incident.revision, "operator", "investigating")
        .unwrap();
    assert_eq!(acknowledged.status, IncidentStatus::Acknowledged);
    assert_eq!(view(&handle).health, TargetHealth::Unhealthy);
    respond(next(&mut pending).await, 3, true).await;
    assert_eq!(view(&handle).health, TargetHealth::Unhealthy);
    respond(next(&mut pending).await, 4, true).await;
    assert_eq!(view(&handle).health, TargetHealth::Healthy);
    assert_eq!(
        handle.incident(&incident.id).unwrap().unwrap().status,
        IncidentStatus::Resolved
    );
    engine.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn missing_empty_partial_and_wrong_types_do_not_create_health() {
    let dir = TestDir::new();
    let (source, mut pending) = source();
    let mut cfg = config();
    cfg.monitors[0].rule.success_samples = 1;
    let mut engine = MonitorEngine::start_with_source(cfg, source, &dir.path).unwrap();
    let handle = engine.handle();
    let first = pending.recv().await.unwrap();
    tokio::time::advance(Duration::from_millis(1600)).await;
    settle().await;
    assert_eq!(view(&handle).freshness, Freshness::Missing);
    let coverage = handle
        .incidents()
        .unwrap()
        .into_iter()
        .find(|i| i.kind == IncidentKind::Coverage)
        .unwrap();
    let mut empty = batch(&first.request, 0, json!({}));
    empty.samples.clear();
    first.reply.send(Ok(empty)).unwrap();
    settle().await;
    assert_eq!(view(&handle).coverage, Coverage::Complete);
    assert_eq!(view(&handle).health, TargetHealth::Unknown);
    assert_ne!(
        handle.incident(&coverage.id).unwrap().unwrap().status,
        IncidentStatus::Resolved
    );
    let request = next(&mut pending).await;
    let mut partial = batch(&request.request, 1, json!({"ready":true}));
    partial.has_more = true;
    request.reply.send(Ok(partial)).unwrap();
    settle().await;
    assert_eq!(view(&handle).health, TargetHealth::Unknown);
    assert_eq!(view(&handle).coverage, Coverage::Partial);
    let request = next(&mut pending).await;
    let invalid = batch(&request.request, 2, json!({"ready":"true"}));
    request.reply.send(Ok(invalid)).unwrap();
    settle().await;
    assert_eq!(view(&handle).health, TargetHealth::Unknown);
    assert!(view(&handle).last_error.unwrap().contains("invalid type"));
    respond(next(&mut pending).await, 3, true).await;
    assert_eq!(view(&handle).health, TargetHealth::Healthy);
    assert_eq!(
        handle.incident(&coverage.id).unwrap().unwrap().status,
        IncidentStatus::Resolved
    );
    engine.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn expiration_runs_during_inflight_poll_and_late_history_does_not_refresh() {
    let dir = TestDir::new();
    let (source, mut pending) = source();
    let mut cfg = config();
    cfg.monitors[0].rule.success_samples = 1;
    cfg.monitors[0].stale_after_ms = 3000;
    let mut engine = MonitorEngine::start_with_source(cfg, source, &dir.path).unwrap();
    let handle = engine.handle();
    respond(pending.recv().await.unwrap(), 1, true).await;
    assert_eq!(view(&handle).health, TargetHealth::Healthy);
    let request = next(&mut pending).await;
    tokio::time::advance(Duration::from_millis(2100)).await;
    settle().await;
    assert_eq!(view(&handle).freshness, Freshness::Stale);
    assert_eq!(view(&handle).health, TargetHealth::Unknown);
    let mut history = batch(&request.request, 2, json!({"ready":true}));
    history.samples[0].age_ms = 10_000;
    request.reply.send(Ok(history)).unwrap();
    settle().await;
    assert_eq!(view(&handle).freshness, Freshness::Stale);
    assert_eq!(view(&handle).last_sample_id.as_deref(), Some("sample-1"));
    assert_eq!(view(&handle).health, TargetHealth::Unknown);
    let _inflight = next(&mut pending).await;
    engine.shutdown().await.unwrap();
    assert!(!handle.snapshot().unwrap().running);
}

#[tokio::test(start_paused = true)]
async fn restart_keeps_cursor_but_requires_new_samples_and_source_reset_reports_gap() {
    let dir = TestDir::new();
    let mut cfg = config();
    cfg.monitors[0].rule.success_samples = 1;
    cfg.monitors[0].rule.failure_samples = 1;
    let (first_source, mut first_pending) = source();
    let mut first = MonitorEngine::start_with_source(cfg.clone(), first_source, &dir.path).unwrap();
    let old_handle = first.handle();
    respond(first_pending.recv().await.unwrap(), 1, false).await;
    let incident = old_handle
        .incidents()
        .unwrap()
        .into_iter()
        .find(|i| i.kind == IncidentKind::Target)
        .unwrap();
    first.shutdown().await.unwrap();
    let (second_source, mut pending) = source();
    let mut second = MonitorEngine::start_with_source(cfg, second_source, &dir.path).unwrap();
    let handle = second.handle();
    assert_eq!(view(&handle).health, TargetHealth::Unknown);
    let request = pending.recv().await.unwrap();
    assert_eq!(request.request.params["cursor"], "cursor-1");
    respond(request, 1, true).await;
    assert_eq!(view(&handle).health, TargetHealth::Unknown);
    assert_eq!(
        handle.incident(&incident.id).unwrap().unwrap().status,
        IncidentStatus::Open
    );
    let request = next(&mut pending).await;
    let mut reset = batch(&request.request, 1, json!({"ready":true}));
    reset.generation = "g2".into();
    request.reply.send(Ok(reset)).unwrap();
    settle().await;
    assert_eq!(view(&handle).health, TargetHealth::Unknown);
    assert_eq!(view(&handle).coverage, Coverage::Partial);
    let request = next(&mut pending).await;
    let mut fresh = batch(&request.request, 2, json!({"ready":true}));
    fresh.generation = "g2".into();
    request.reply.send(Ok(fresh)).unwrap();
    settle().await;
    assert_eq!(view(&handle).health, TargetHealth::Healthy);
    assert_eq!(
        handle.incident(&incident.id).unwrap().unwrap().status,
        IncidentStatus::Resolved
    );
    second.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn identity_failure_does_not_advance_checkpoint_and_shutdown_waits_for_active_calls() {
    let dir = TestDir::new();
    let (source, mut pending) = source();
    let mut engine = MonitorEngine::start_with_source(config(), source, &dir.path).unwrap();
    let handle = engine.handle();
    let request = pending.recv().await.unwrap();
    let mut invalid = batch(&request.request, 1, json!({"ready":true}));
    invalid.target_id = "other-target".into();
    request.reply.send(Ok(invalid)).unwrap();
    settle().await;
    assert_eq!(view(&handle).coverage, Coverage::Unavailable);
    assert_eq!(view(&handle).cursor, None);
    let inflight = next(&mut pending).await;
    assert!(inflight.request.params["cursor"].is_null());
    engine.shutdown().await.unwrap();
    assert!(!view(&handle).running);
    assert!(matches!(
        handle.acknowledge("missing", 1, "operator", "note"),
        Err(MonitorError::Stopped)
    ));
}

#[tokio::test(start_paused = true)]
async fn numeric_and_string_rules_are_configurable_without_domain_code() {
    for (operator, expected, observed) in [
        (RuleOperator::Lt, json!(5), json!(3)),
        (RuleOperator::Eq, json!("ready"), json!("ready")),
    ] {
        let dir = TestDir::new();
        let (source, mut pending) = source();
        let mut cfg = config();
        cfg.monitors[0].rule = MonitorRule {
            pointer: "/metrics/result".into(),
            operator,
            value: expected,
            failure_samples: 1,
            success_samples: 1,
        };
        let mut engine = MonitorEngine::start_with_source(cfg, source, &dir.path).unwrap();
        let request = pending.recv().await.unwrap();
        let sample_value = json!({"metrics":{"result":observed}});
        let response = batch(&request.request, 1, sample_value.clone());
        request.reply.send(Ok(response)).unwrap();
        settle().await;
        let snapshot = view(&engine.handle());
        assert_eq!(snapshot.health, TargetHealth::Healthy);
        assert_eq!(snapshot.last_value, Some(sample_value));
        engine.shutdown().await.unwrap();
    }
    let mut cfg = config();
    cfg.monitors[0].params = json!({"cursor":"forged"});
    assert!(cfg.validate().is_err());
    let mut cfg = config();
    cfg.monitors[0].rule.pointer = "/bad~2pointer".into();
    assert!(cfg.validate().is_err());
}

#[tokio::test(start_paused = true)]
async fn batch_transitions_are_durable_and_final_uncertainty_cannot_clear_old_fault() {
    let dir = TestDir::new();
    let (source, mut pending) = source();
    let mut cfg = config();
    cfg.monitors[0].rule.success_samples = 1;
    let mut engine = MonitorEngine::start_with_source(cfg, source, &dir.path).unwrap();
    let handle = engine.handle();
    respond(pending.recv().await.unwrap(), 1, false).await;
    respond(next(&mut pending).await, 2, false).await;
    let original = handle
        .incidents()
        .unwrap()
        .into_iter()
        .find(|i| i.kind == IncidentKind::Target)
        .unwrap();
    let request = next(&mut pending).await;
    let mut mixed = batch(&request.request, 3, json!({"ready":true}));
    mixed
        .samples
        .extend(batch(&request.request, 4, json!({"ready":false})).samples);
    mixed.next_cursor = "cursor-4".into();
    request.reply.send(Ok(mixed)).unwrap();
    settle().await;
    assert_eq!(view(&handle).health, TargetHealth::Unknown);
    assert_eq!(
        handle.incident(&original.id).unwrap().unwrap().status,
        IncidentStatus::Open
    );
    let request = next(&mut pending).await;
    let mut transitions = batch(&request.request, 5, json!({"ready":false}));
    transitions
        .samples
        .extend(batch(&request.request, 6, json!({"ready":true})).samples);
    transitions
        .samples
        .extend(batch(&request.request, 7, json!({"ready":false})).samples);
    transitions
        .samples
        .extend(batch(&request.request, 8, json!({"ready":false})).samples);
    transitions.next_cursor = "cursor-8".into();
    request.reply.send(Ok(transitions)).unwrap();
    settle().await;
    assert_eq!(view(&handle).health, TargetHealth::Unhealthy);
    assert!(
        handle
            .incidents()
            .unwrap()
            .iter()
            .any(|i| i.kind == IncidentKind::Target && i.status == IncidentStatus::Open)
    );
    engine.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn empty_and_out_of_order_batches_cannot_accumulate_a_false_threshold() {
    let dir = TestDir::new();
    let (source, mut pending) = source();
    let mut engine = MonitorEngine::start_with_source(config(), source, &dir.path).unwrap();
    let handle = engine.handle();
    respond(pending.recv().await.unwrap(), 2, false).await;
    let request = next(&mut pending).await;
    let mut older = batch(&request.request, 1, json!({"ready":false}));
    older.next_cursor = "cursor-2".into();
    request.reply.send(Ok(older)).unwrap();
    settle().await;
    assert_eq!(view(&handle).consecutive_failures, 1);
    let request = next(&mut pending).await;
    let mut empty = batch(&request.request, 2, json!({}));
    empty.samples.clear();
    request.reply.send(Ok(empty)).unwrap();
    settle().await;
    assert_eq!(view(&handle).consecutive_failures, 0);
    respond(next(&mut pending).await, 3, false).await;
    assert_eq!(view(&handle).consecutive_failures, 1);
    assert_eq!(view(&handle).health, TargetHealth::Unknown);
    assert!(
        !handle
            .incidents()
            .unwrap()
            .iter()
            .any(|i| i.kind == IncidentKind::Target)
    );
    engine.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn maximum_alternating_batch_fits_atomic_journal_and_oversized_batch_is_rejected_safely() {
    let dir = TestDir::new();
    let (source, mut pending) = source();
    let mut cfg = config();
    let expected = "x".repeat(3000);
    cfg.monitors[0].rule = MonitorRule {
        pointer: "/value".into(),
        operator: RuleOperator::Eq,
        value: json!(expected),
        failure_samples: 1,
        success_samples: 1,
    };
    let mut engine = MonitorEngine::start_with_source(cfg, source, &dir.path).unwrap();
    let handle = engine.handle();
    let request = pending.recv().await.unwrap();
    let mut response = batch(&request.request, 1, json!({}));
    response.samples.clear();
    for sequence in 1..=32 {
        let text = if sequence % 2 == 0 {
            expected.clone()
        } else {
            "y".repeat(3000)
        };
        let mut sample = batch(&request.request, sequence, json!({"value":text}))
            .samples
            .remove(0);
        sample.evidence = json!({"details":"e".repeat(500)});
        response.samples.push(sample);
    }
    response.next_cursor = "cursor-32".into();
    request.reply.send(Ok(response)).unwrap();
    settle().await;
    assert_eq!(view(&handle).health, TargetHealth::Healthy);
    assert_eq!(
        handle
            .incidents()
            .unwrap()
            .iter()
            .filter(|record| record.kind == IncidentKind::Target
                && record.status == IncidentStatus::Resolved)
            .count(),
        16
    );
    assert!(handle.snapshot().unwrap().runtime_error.is_none());
    let request = next(&mut pending).await;
    let mut oversized = batch(&request.request, 33, json!({"value":expected}));
    oversized.samples = (33..=65)
        .map(|sequence| {
            batch(&request.request, sequence, json!({"value":"x"}))
                .samples
                .remove(0)
        })
        .collect();
    request.reply.send(Ok(oversized)).unwrap();
    settle().await;
    assert_eq!(view(&handle).coverage, Coverage::Unavailable);
    assert_eq!(view(&handle).cursor.as_deref(), Some("cursor-32"));
    assert!(handle.snapshot().unwrap().runtime_error.is_none());
    let request = next(&mut pending).await;
    let mut disordered = batch(&request.request, 34, json!({"value":"x"}));
    disordered
        .samples
        .extend(batch(&request.request, 33, json!({"value":"x"})).samples);
    request.reply.send(Ok(disordered)).unwrap();
    settle().await;
    assert_eq!(view(&handle).cursor.as_deref(), Some("cursor-32"));
    assert!(view(&handle).last_error.unwrap().contains("out of order"));
    let request = next(&mut pending).await;
    let mut reused = batch(&request.request, 33, json!({"value":"x"}));
    let mut contradictory = batch(&request.request, 34, json!({"value":"y"}))
        .samples
        .remove(0);
    contradictory.id = reused.samples[0].id.clone();
    reused.samples.push(contradictory);
    request.reply.send(Ok(reused)).unwrap();
    settle().await;
    assert_eq!(view(&handle).cursor.as_deref(), Some("cursor-32"));
    assert!(view(&handle).last_error.unwrap().contains("identity"));
    engine.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn empty_engine_drop_releases_store_even_when_handle_is_retained() {
    let dir = TestDir::new();
    let cfg = MonitorsConfig {
        schema_version: 1,
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

#[tokio::test(start_paused = true)]
async fn unsafe_integer_precision_and_deep_evidence_are_observation_failures() {
    let dir = TestDir::new();
    let (source, mut pending) = source();
    let mut cfg = config();
    cfg.monitors[0].rule.value = json!(9_007_199_254_740_993_u64);
    assert!(cfg.validate().is_err());
    cfg.monitors[0].rule.value = json!(9_007_199_254_740_992_u64);
    cfg.monitors[0].rule.success_samples = 1;
    let mut engine = MonitorEngine::start_with_source(cfg, source, &dir.path).unwrap();
    let handle = engine.handle();
    let request = pending.recv().await.unwrap();
    let oversized_integer = batch(
        &request.request,
        1,
        json!({"ready":9_007_199_254_740_993_u64}),
    );
    request.reply.send(Ok(oversized_integer)).unwrap();
    settle().await;
    assert_eq!(view(&handle).health, TargetHealth::Unknown);
    assert_eq!(view(&handle).coverage, Coverage::Partial);
    let request = next(&mut pending).await;
    let mut deep = json!({});
    for _ in 0..40 {
        deep = json!({"nested":deep});
    }
    let mut response = batch(
        &request.request,
        2,
        json!({"ready":9_007_199_254_740_992_u64}),
    );
    response.samples[0].evidence = deep;
    request.reply.send(Ok(response)).unwrap();
    settle().await;
    assert_eq!(view(&handle).coverage, Coverage::Unavailable);
    assert!(handle.snapshot().unwrap().runtime_error.is_none());
    engine.shutdown().await.unwrap();
}
