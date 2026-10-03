use recuvora_core::recovery::approval::{
    ApprovalDecision, ApprovalPolicy, ApprovalState, ReviewerConfig,
};
use recuvora_core::recovery::workflow::ErrorLogEvidence;
use recuvora_host::control::recovery::incidents::{IncidentKind, IncidentRecord, IncidentStatus};
use recuvora_host::integrations::recovery::*;
use recuvora_host::monitoring::*;
use recuvora_host::runtime::operation::Cancellation;
use serde_json::json;
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

#[allow(dead_code)]
mod network_fixture;
#[path = "workflow_support.rs"]
mod support;
use support::TestDir;

#[tokio::test]
async fn shared_target_ownership_blocks_other_stores_and_survives_pending_shutdown() {
    let dir = TestDir::new("target-ownership-pending");
    let authority_dir = dir.path.join("ownership");
    let backend = Arc::new(Backend::default());
    let first = RecoveryService::open(dir.path.join("first"), config(), backend.clone()).unwrap();
    let mut monitor = MonitorEngine::start_with_source(
        monitor_config(),
        Arc::new(Source::new()),
        dir.path.join("monitor"),
    )
    .unwrap();
    let incident = received_error(&monitor.handle()).await;
    let context = problem(&incident);
    assert!(matches!(
        first.submit(context.clone()),
        Err(RecoveryError::Service(_))
    ));
    assert!(first.tasks().unwrap().is_empty());
    first
        .bind_target_ownership(Arc::new(FileTargetOwnership::open(&authority_dir).unwrap()))
        .unwrap();
    assert!(matches!(
        first.bind_target_ownership(Arc::new(FileTargetOwnership::open(&authority_dir).unwrap())),
        Err(RecoveryError::Invalid(_))
    ));
    bind(&first, monitor.handle());
    let task = first.submit(context).unwrap();
    let second = RecoveryService::open(dir.path.join("second"), config(), backend.clone()).unwrap();
    assert!(matches!(
        second.bind_target_ownership(Arc::new(FileTargetOwnership::open(&authority_dir).unwrap())),
        Err(RecoveryError::Busy)
    ));
    first.shutdown().await.unwrap();
    drop(first);
    assert!(
        matches!(
            second.bind_target_ownership(Arc::new(
                FileTargetOwnership::open(&authority_dir).unwrap()
            )),
            Err(RecoveryError::Busy)
        ),
        "pending work retains durable ownership after shutdown"
    );
    let reopened =
        RecoveryService::open(dir.path.join("first"), config(), backend.clone()).unwrap();
    reopened
        .bind_target_ownership(Arc::new(FileTargetOwnership::open(&authority_dir).unwrap()))
        .unwrap();
    assert_eq!(reopened.query(&task.id).unwrap().unwrap().id, task.id);
    assert_eq!(backend.executions.load(Ordering::SeqCst), 0);
    reopened.shutdown().await.unwrap();
    second.shutdown().await.unwrap();
    monitor.shutdown().await.unwrap();
}

#[tokio::test]
async fn drained_target_ownership_can_transfer_to_another_store() {
    let dir = TestDir::new("target-ownership-transfer");
    let authority = Arc::new(FileTargetOwnership::open(dir.path.join("ownership")).unwrap());
    let first = RecoveryService::open(
        dir.path.join("first"),
        config(),
        Arc::new(Backend::default()),
    )
    .unwrap();
    let second = RecoveryService::open(
        dir.path.join("second"),
        config(),
        Arc::new(Backend::default()),
    )
    .unwrap();
    first.bind_target_ownership(authority.clone()).unwrap();
    assert!(matches!(
        second.bind_target_ownership(authority.clone()),
        Err(RecoveryError::Busy)
    ));
    first.shutdown().await.unwrap();
    second.bind_target_ownership(authority.clone()).unwrap();
    second.shutdown().await.unwrap();
    drop(authority);
}

struct Source {
    unavailable: AtomicBool,
}
impl Source {
    fn new() -> Self {
        Self {
            unavailable: AtomicBool::new(false),
        }
    }
}
impl ObservationSource for Source {
    fn poll(&self, request: ObservationRequest, _: Cancellation) -> ObservationFuture<'_> {
        let unavailable = self.unavailable.load(Ordering::SeqCst);
        Box::pin(async move {
            if unavailable {
                return Err(MonitorError::Observation("source disconnected".into()));
            }
            Ok(ErrorLogBatch {
                schema_version: 2,
                target_id: "target-a".into(),
                source_id: "source-a".into(),
                generation: "generation-a".into(),
                cursor: request.params["cursor"].as_str().map(str::to_owned),
                next_cursor: "cursor-1".into(),
                coverage: BatchCoverage::Complete,
                has_more: false,
                source_error: None,
                errors: vec![NodeErrorLog {
                    id: "error-1".into(),
                    sequence: 1,
                    age_ms: 86_400_000,
                    fingerprint: "workload-error".into(),
                    message: "ERROR original message\n  stack: 执行失败".into(),
                    evidence: json!({"exit_code":17,"detail":{"worker":"worker-1"}}),
                }],
            })
        })
    }
}
fn monitor_config() -> MonitorsConfig {
    MonitorsConfig {
        schema_version: 2,
        discoveries: Vec::new(),
        monitors: vec![MonitorDefinition {
            id: "monitor-a".into(),
            target_id: "target-a".into(),
            source_id: "source-a".into(),
            view_role: None,
            extension_id: "provider-a".into(),
            contract: "example.logs".into(),
            version: 1,
            method: "errors".into(),
            params: json!({}),
            interval_ms: 10,
            timeout_ms: 100,
            stale_after_ms: 1000,
            startup_grace_ms: 10,
        }],
    }
}
fn config() -> RecoveryConfig {
    let policy = |id: &str, reviewer| ApprovalPolicy {
        id: id.into(),
        version: 1,
        reviewer,
        delegation: "review bounded repair for the received error report".into(),
        allowed_targets: vec!["target-a".into()],
        allowed_action_kinds: vec!["repair_with_harness".into()],
        ttl_secs: 600,
    };
    RecoveryConfig {
        schema_version: 2,
        execution_harness: "executor".into(),
        target: TargetBinding {
            target_id: "target-a".into(),
            executor_id: "target-node".into(),
            allowed_action_kinds: vec!["execute_script".into()],
            verification_profile: "readiness".into(),
            required_facts: BTreeMap::from([("workload_version".into(), "1".into())]),
            action_timeout_secs: 10,
        },
        approval: policy("human-repair", ReviewerConfig::Human),
        summary_timeout_secs: 10,
        review_timeout_secs: 10,
        max_tool_calls: 4,
        max_tasks: 16,
    }
}

#[derive(Default)]
struct Backend {
    executions: AtomicUsize,
    block_next_inspect: AtomicBool,
    inspect_entered: tokio::sync::Notify,
    inspect_release: tokio::sync::Notify,
}
impl RepairBackend for Backend {
    fn inspect<'a>(
        &'a self,
        target: &'a TargetBinding,
        _: Cancellation,
    ) -> RecoveryFuture<'a, TargetObservation> {
        Box::pin(async move {
            if self.block_next_inspect.swap(false, Ordering::SeqCst) {
                self.inspect_entered.notify_one();
                self.inspect_release.notified().await;
            }
            Ok(TargetObservation {
                target_id: target.target_id.clone(),
                facts: target.required_facts.clone(),
                evidence_refs: vec!["provider-observation:current-environment".into()],
                observed_at_ms: SystemRecoveryClock.now_ms(),
            })
        })
    }
    fn review(&self, _: ReviewInput, _: Cancellation) -> RecoveryFuture<'_, ReviewOutput> {
        Box::pin(async { panic!("human-review fixture must not invoke a model reviewer") })
    }
    fn execute<'a>(
        &'a self,
        script: AuthorizedRepair<'a>,
        _: Cancellation,
    ) -> RecoveryFuture<'a, RepairReceipt> {
        Box::pin(async move {
            script.validate_dispatch()?;
            self.executions.fetch_add(1, Ordering::SeqCst);
            script.finish_dispatch();
            Ok(RepairReceipt {
                execution_trace: Vec::new(),
                operation_id: script.operation().operation_id.clone(),
                target_id: script.operation().target.clone(),
                outcome: RepairExecutionOutcome::Executed,
                executor_stopped: true,
                evidence_refs: vec!["external-action:receipt".into()],
                summary: "test receipt".into(),
            })
        })
    }
    fn verify(
        &self,
        input: VerificationInput,
        _: Cancellation,
    ) -> RecoveryFuture<'_, BusinessVerification> {
        Box::pin(async move {
            Ok(BusinessVerification {
                operation_id: input.operation.operation_id,
                target_id: input.target.target_id,
                profile: input.target.verification_profile,
                healthy: Some(true),
                executor_stopped: true,
                evidence_refs: vec!["provider-observation:business-check".into()],
                verified_at_ms: SystemRecoveryClock.now_ms(),
            })
        })
    }
}

async fn wait_for(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("bounded fixture condition");
}

async fn received_error(handle: &MonitorHandle) -> IncidentRecord {
    wait_for(|| {
        handle
            .incidents()
            .unwrap()
            .iter()
            .any(|record| record.kind == IncidentKind::ErrorLog)
    })
    .await;
    handle
        .incidents()
        .unwrap()
        .into_iter()
        .find(|record| record.kind == IncidentKind::ErrorLog)
        .unwrap()
}
fn problem(record: &IncidentRecord) -> ProblemContext {
    let stored = &record.evidence;
    let log: NodeErrorLog = serde_json::from_value(stored["log"].clone()).unwrap();
    ProblemContext {
        origin: ProblemOrigin::ErrorLog,
        report: Some(ErrorLogEvidence {
            source_id: stored["source_id"].as_str().unwrap().into(),
            generation: stored["generation"].as_str().unwrap().into(),
            record_id: log.id,
            sequence: log.sequence,
            age_ms: log.age_ms,
            evidence: log.evidence,
        }),
        incident_id: record.id.clone(),
        incident_revision: record.revision,
        target_id: record.target_id.clone(),
        fingerprint: log.fingerprint,
        summary: log.message,
        occurrences: record.occurrences,
        keywords: Vec::new(),
        conditions: config().target.required_facts,
        evidence_refs: vec![
            format!("node-error:{}", stored["extension_id"].as_str().unwrap()),
            format!("error-receipt:{}", record.id),
        ],
    }
}
fn bind(recovery: &RecoveryService, handle: MonitorHandle) {
    recovery
        .bind_incident_guard(Arc::new(guard(handle)))
        .unwrap();
}
fn guard(handle: MonitorHandle) -> MonitorIncidentGuard {
    MonitorIncidentGuard::new(handle)
}
fn owned_recovery(dir: &TestDir, backend: Arc<Backend>) -> Arc<RecoveryService> {
    let recovery = RecoveryService::open(dir.path.join("recovery"), config(), backend).unwrap();
    recovery
        .bind_target_ownership(Arc::new(
            FileTargetOwnership::open(dir.path.join("ownership")).unwrap(),
        ))
        .unwrap();
    recovery
}
async fn pending(recovery: &Arc<RecoveryService>, incident: &IncidentRecord) -> RecoveryTask {
    let task = recovery.submit(problem(incident)).unwrap();
    let task = recovery
        .advance(&task.id, Cancellation::new())
        .await
        .unwrap();
    assert_eq!(task.stage, RecoveryStage::AwaitingApproval);
    task
}

fn approve(recovery: &RecoveryService, task: &RecoveryTask) {
    let approval = recovery.approval(&task.id).unwrap().unwrap();
    recovery
        .decide_human(
            &task.id,
            approval.revision,
            ApprovalDecision::Approve,
            "trusted-operator".into(),
            "reviewed exact operation".into(),
        )
        .unwrap();
}

async fn assert_blocked(recovery: &Arc<RecoveryService>, backend: &Backend, task: &RecoveryTask) {
    let _ = recovery.advance(&task.id, Cancellation::new()).await;
    assert_eq!(backend.executions.load(Ordering::SeqCst), 0);
    assert!(!matches!(
        recovery.approval(&task.id).unwrap().unwrap().state,
        ApprovalState::Executing | ApprovalState::Executed
    ));
}

#[tokio::test]
async fn immutable_receipt_rejects_forged_text_evidence_origin_and_source() {
    let dir = TestDir::new("receipt-binding");
    let mut monitor = MonitorEngine::start_with_source(
        monitor_config(),
        Arc::new(Source::new()),
        dir.path.join("monitor"),
    )
    .unwrap();
    let record = received_error(&monitor.handle()).await;
    let backend = Arc::new(Backend::default());
    let recovery = owned_recovery(&dir, backend.clone());
    bind(&recovery, monitor.handle());
    let original = problem(&record);
    for kind in 0..7 {
        let mut altered = original.clone();
        match kind {
            0 => altered.summary.push_str(" forged"),
            1 => altered.report.as_mut().unwrap().evidence["exit_code"] = 0.into(),
            2 => altered.report.as_mut().unwrap().source_id = "another-source".into(),
            3 => altered.report.as_mut().unwrap().age_ms = 0,
            4 => altered.evidence_refs = vec!["forged:source".into()],
            5 => {
                altered.origin = ProblemOrigin::Incident;
                altered.report = None;
            }
            _ => altered.target_id = "another-target".into(),
        }
        assert!(
            recovery.submit(altered).is_err(),
            "reject changed field {kind}"
        );
    }
    assert!(recovery.tasks().unwrap().is_empty());
    // Acknowledgement between the scheduler's read and guarded registration
    // advances receipt revision without replacing the accepted original report.
    monitor
        .handle()
        .acknowledge(
            &record.id,
            record.revision,
            "operator",
            "received before intake",
        )
        .unwrap();
    let task = recovery.submit(original.clone()).unwrap();
    assert_eq!(task.problem, original);
    assert_eq!(backend.executions.load(Ordering::SeqCst), 0);
    recovery.shutdown().await.unwrap();
    monitor.shutdown().await.unwrap();
}

#[tokio::test]
async fn acknowledged_historical_error_remains_a_receipt_and_can_be_independently_verified() {
    let dir = TestDir::new("receipt-acknowledged");
    let source = Arc::new(Source::new());
    let mut monitor = MonitorEngine::start_with_source(
        monitor_config(),
        source.clone(),
        dir.path.join("monitor"),
    )
    .unwrap();
    let handle = monitor.handle();
    let record = received_error(&handle).await;
    let backend = Arc::new(Backend::default());
    let recovery = owned_recovery(&dir, backend.clone());
    bind(&recovery, handle.clone());
    let task = pending(&recovery, &record).await;
    let acknowledged = handle
        .acknowledge(
            &record.id,
            record.revision,
            "operator",
            "receipt acknowledged",
        )
        .unwrap();
    assert_eq!(acknowledged.status, IncidentStatus::Acknowledged);
    assert!(acknowledged.revision > record.revision);
    assert!(matches!(
        guard(handle.clone()).check(&task.problem).unwrap(),
        IncidentReadiness::Received { .. }
    ));
    source.unavailable.store(true, Ordering::SeqCst);
    wait_for(|| {
        handle
            .monitor("monitor-a")
            .unwrap()
            .unwrap()
            .last_error
            .is_some()
    })
    .await;
    approve(&recovery, &task);
    let completed = recovery
        .advance(&task.id, Cancellation::new())
        .await
        .unwrap();
    assert_eq!(completed.stage, RecoveryStage::Completed);
    assert_eq!(completed.problem.summary, record.summary);
    assert_eq!(
        completed.problem.report.as_ref().unwrap().age_ms,
        86_400_000
    );
    assert_eq!(backend.executions.load(Ordering::SeqCst), 1);
    recovery.shutdown().await.unwrap();
    monitor.shutdown().await.unwrap();
}

#[tokio::test]
async fn stopped_source_instance_cannot_authorize_a_persisted_error_receipt() {
    let dir = TestDir::new("receipt-stopped");
    let mut monitor = MonitorEngine::start_with_source(
        monitor_config(),
        Arc::new(Source::new()),
        dir.path.join("monitor"),
    )
    .unwrap();
    let record = received_error(&monitor.handle()).await;
    let backend = Arc::new(Backend::default());
    let recovery = owned_recovery(&dir, backend.clone());
    bind(&recovery, monitor.handle());
    let task = pending(&recovery, &record).await;
    approve(&recovery, &task);
    monitor.shutdown().await.unwrap();
    assert_blocked(&recovery, &backend, &task).await;
    recovery.shutdown().await.unwrap();
}

#[tokio::test]
async fn receipt_gate_rejects_unknown_id_future_revision_and_rebinding() {
    let dir = TestDir::new("receipt-gate-identity");
    let mut monitor = MonitorEngine::start_with_source(
        monitor_config(),
        Arc::new(Source::new()),
        dir.path.join("monitor"),
    )
    .unwrap();
    let handle = monitor.handle();
    let record = received_error(&handle).await;
    assert!(handle.repair_incident("absent", "target-a", 1).is_err());
    assert!(
        handle
            .repair_incident(&record.id, "another-target", record.revision)
            .is_err()
    );
    assert!(
        handle
            .repair_incident(&record.id, "target-a", record.revision + 1)
            .is_err()
    );
    let recovery = owned_recovery(&dir, Arc::new(Backend::default()));
    assert!(recovery.submit(problem(&record)).is_err());
    bind(&recovery, handle.clone());
    assert!(
        recovery
            .bind_incident_guard(Arc::new(guard(handle)))
            .is_err()
    );
    recovery.submit(problem(&record)).unwrap();
    recovery.shutdown().await.unwrap();
    monitor.shutdown().await.unwrap();
}

#[tokio::test]
async fn source_shutdown_during_target_inspection_is_rechecked_before_consuming_approval() {
    let dir = TestDir::new("receipt-inspection-close");
    let mut monitor = MonitorEngine::start_with_source(
        monitor_config(),
        Arc::new(Source::new()),
        dir.path.join("monitor"),
    )
    .unwrap();
    let record = received_error(&monitor.handle()).await;
    let backend = Arc::new(Backend::default());
    let recovery = owned_recovery(&dir, backend.clone());
    bind(&recovery, monitor.handle());
    let task = pending(&recovery, &record).await;
    approve(&recovery, &task);
    backend.block_next_inspect.store(true, Ordering::SeqCst);
    let pending = tokio::spawn({
        let recovery = recovery.clone();
        let id = task.id.clone();
        async move { recovery.advance(&id, Cancellation::new()).await }
    });
    backend.inspect_entered.notified().await;
    monitor.shutdown().await.unwrap();
    backend.inspect_release.notify_one();
    assert!(pending.await.unwrap().is_err());
    assert_eq!(backend.executions.load(Ordering::SeqCst), 0);
    assert_eq!(
        recovery.approval(&task.id).unwrap().unwrap().state,
        ApprovalState::Approved
    );
    recovery.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn owned_receipt_lease_blocks_mutation_without_expiring_historical_logs() {
    let dir = TestDir::new("receipt-owned-gate");
    let mut monitor = MonitorEngine::start_with_source(
        monitor_config(),
        Arc::new(Source::new()),
        dir.path.join("monitor"),
    )
    .unwrap();
    let handle = monitor.handle();
    let record = received_error(&handle).await;
    let authority = guard(handle.clone());
    let context = problem(&record);
    let lease = authority.acquire_dispatch(&context).await.unwrap();
    assert!(
        handle
            .acknowledge(&record.id, record.revision, "operator", "blocked")
            .is_err()
    );
    assert!(handle.begin_shutdown().is_err());
    tokio::time::advance(Duration::from_secs(3600)).await;
    assert!(matches!(
        lease.current().unwrap(),
        IncidentReadiness::Received { .. }
    ));
    drop(lease);
    handle
        .acknowledge(&record.id, record.revision, "operator", "received")
        .unwrap();
    monitor.shutdown().await.unwrap();
}
#[tokio::test]
async fn registration_gate_covers_the_authorization_callback_until_it_returns() {
    let dir = TestDir::new("incident-guard-consume-window");
    let mut monitor = MonitorEngine::start_with_source(
        monitor_config(),
        Arc::new(Source::new()),
        dir.path.join("monitor"),
    )
    .unwrap();
    let handle = monitor.handle();
    let incident = received_error(&handle).await;
    let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
    let callback = std::thread::spawn({
        let handle = handle.clone();
        let incident = incident.clone();
        move || {
            handle.with_repair_incident(&incident.id, "target-a", incident.revision, |current| {
                entered_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(3)).unwrap();
                current.id
            })
        }
    });
    entered_rx.recv_timeout(Duration::from_secs(3)).unwrap();

    // Non-gated diagnostic reads prove the helper released store/view locks
    // before entering the callback. They run on another thread, not reentrantly.
    let (read_tx, read_rx) = std::sync::mpsc::sync_channel(1);
    let reader = std::thread::spawn({
        let handle = handle.clone();
        let id = incident.id.clone();
        move || {
            let record = handle.incident(&id).unwrap().unwrap();
            let view = handle.monitor("monitor-a").unwrap().unwrap();
            let definition = handle.definition("monitor-a").unwrap().unwrap();
            read_tx.send((record.id, view.id, definition.id)).unwrap();
        }
    });
    let reads = read_rx.recv_timeout(Duration::from_millis(500));

    let (shutdown_started_tx, shutdown_started_rx) = std::sync::mpsc::sync_channel(1);
    let (shutdown_done_tx, shutdown_done_rx) = std::sync::mpsc::sync_channel(1);
    let shutting_down = std::thread::spawn({
        let handle = handle.clone();
        move || {
            shutdown_started_tx.send(()).unwrap();
            let result = handle.begin_shutdown();
            shutdown_done_tx.send(()).unwrap();
            result
        }
    });
    shutdown_started_rx
        .recv_timeout(Duration::from_secs(1))
        .unwrap();
    let returned_without_blocking = shutdown_done_rx
        .recv_timeout(Duration::from_millis(500))
        .is_ok();
    // Always release the bounded test barrier before inspecting assertions.
    release_tx.send(()).unwrap();
    let returned_id = callback.join().unwrap().unwrap();
    assert!(shutting_down.join().unwrap().is_err());
    reader.join().unwrap();
    assert!(
        returned_without_blocking,
        "synchronous shutdown must reject a busy authority instead of blocking a runtime thread"
    );
    assert_eq!(returned_id, incident.id);
    assert_eq!(
        reads.unwrap(),
        (incident.id, "monitor-a".into(), "monitor-a".into())
    );
    handle.begin_shutdown().unwrap();
    assert!(!handle.snapshot().unwrap().running);
    monitor.shutdown().await.unwrap();
}

struct MonitorNetworkGate {
    lease: std::sync::Mutex<Option<Box<dyn IncidentDispatchLease>>>,
    handle: MonitorHandle,
    incident: IncidentRecord,
    released: tokio::sync::Notify,
}
impl recuvora_host::integrations::extensions::DispatchGuard for MonitorNetworkGate {
    fn validate(&self) -> Result<(), recuvora_host::integrations::extensions::ExtensionError> {
        use recuvora_host::integrations::extensions::ExtensionError;
        assert!(
            self.handle
                .acknowledge(
                    &self.incident.id,
                    self.incident.revision,
                    "operator",
                    "network boundary"
                )
                .is_err()
        );
        match self.lease.lock().unwrap().as_ref().unwrap().current() {
            Ok(IncidentReadiness::Received { .. }) => Ok(()),
            result => Err(ExtensionError::Rejected(format!(
                "dispatch evidence unavailable: {result:?}"
            ))),
        }
    }
    fn release(&self) {
        self.lease.lock().unwrap().take();
        self.released.notify_one();
    }
}

#[tokio::test]
async fn production_receipt_lease_reaches_network_send_and_releases_before_executor_finishes() {
    use recuvora_host::integrations::extensions::{
        ExtensionCall, ExtensionError, ProtocolSettings,
    };
    let dir = TestDir::new("receipt-network-dispatch");
    let mut monitor = MonitorEngine::start_with_source(
        monitor_config(),
        Arc::new(Source::new()),
        dir.path.join("monitor"),
    )
    .unwrap();
    let handle = monitor.handle();
    let incident = received_error(&handle).await;
    let fixture =
        network_fixture::Fixture::start("http", network_fixture::Behavior::WaitForCancel, None)
            .await
            .unwrap();
    let client = fixture.client(None);
    let metadata = client.probe().await.unwrap();
    let authority = guard(handle.clone());
    let context = problem(&incident);
    let lease = authority.acquire_dispatch(&context).await.unwrap();
    let gate = Arc::new(MonitorNetworkGate {
        lease: std::sync::Mutex::new(Some(lease)),
        handle: handle.clone(),
        incident,
        released: tokio::sync::Notify::new(),
    });
    let token = Cancellation::new();
    let cancel = token.clone();
    let dispatch = gate.clone();
    let pending = tokio::spawn(async move {
        client
            .call_with_dispatch_guard(
                ExtensionCall {
                    contract: "com.example.network".into(),
                    version: 1,
                    method: "query".into(),
                    params: json!({"target":"target-a"}),
                    timeout: Duration::from_secs(5),
                },
                metadata,
                token,
                None,
                None,
                &ProtocolSettings::default(),
                Some(dispatch),
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), gate.released.notified())
        .await
        .unwrap();
    handle
        .acknowledge(
            &gate.incident.id,
            gate.incident.revision,
            "operator",
            "after send",
        )
        .unwrap();
    fixture.state.wait_for_dispatch().await;
    assert!(!pending.is_finished());
    cancel.cancel();
    let result = tokio::time::timeout(Duration::from_secs(5), pending)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(fixture.state.calls.load(Ordering::SeqCst), 1);
    assert!(matches!(result, Err(ExtensionError::Unknown { .. })));
    fixture.shutdown().await;
    monitor.shutdown().await.unwrap();
}
