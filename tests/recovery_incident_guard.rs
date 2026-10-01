use recuvora_core::recovery::{
    approval::{ApprovalDecision, ApprovalPolicy, ApprovalState, ReviewerConfig},
    incidents::{IncidentKind, IncidentRecord, IncidentStatus, SignalCondition},
    knowledge::ScriptArtifact,
};
use recuvora_host::integrations::recovery::*;
use recuvora_host::monitoring::*;
use recuvora_host::runtime::operation::Cancellation;
use serde_json::json;
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};

#[allow(dead_code)]
mod network_fixture;
#[path = "workflow_support.rs"]
mod support;
use support::TestDir;

const UNHEALTHY: u8 = 0;
const HEALTHY: u8 = 1;
const UNAVAILABLE: u8 = 2;

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
    let incident = active_incident(&monitor.handle()).await;
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
    state: AtomicU8,
    sequence: AtomicU64,
    age_ms: u64,
}
impl Source {
    fn new() -> Self {
        Self {
            state: AtomicU8::new(UNHEALTHY),
            sequence: AtomicU64::new(0),
            age_ms: 0,
        }
    }
    fn set(&self, state: u8) {
        self.state.store(state, Ordering::SeqCst);
    }
}
impl ObservationSource for Source {
    fn poll(&self, request: ObservationRequest, _: Cancellation) -> ObservationFuture<'_> {
        Box::pin(async move {
            let state = self.state.load(Ordering::SeqCst);
            if state == UNAVAILABLE {
                return Err(MonitorError::Observation(
                    "provider temporarily unavailable".into(),
                ));
            }
            let sequence = self.sequence.fetch_add(1, Ordering::SeqCst) + 1;
            Ok(ObservationBatch {
                schema_version: 1,
                target_id: "target-a".into(),
                source_id: "source-a".into(),
                generation: "generation-a".into(),
                cursor: request.params["cursor"].as_str().map(str::to_owned),
                next_cursor: format!("position-{sequence}"),
                coverage: BatchCoverage::Complete,
                has_more: false,
                error: None,
                samples: vec![ObservationSample {
                    id: format!("sample-{sequence}"),
                    sequence,
                    age_ms: self.age_ms,
                    value: json!({"ready":state == HEALTHY}),
                    evidence: json!({"kind":"provider-observation"}),
                }],
            })
        })
    }
}

fn monitor_config() -> MonitorsConfig {
    MonitorsConfig {
        schema_version: 1,
        discoveries: vec![],
        monitors: vec![MonitorDefinition {
            id: "monitor-a".into(),
            target_id: "target-a".into(),
            source_id: "source-a".into(),
            view_role: None,
            extension_id: "test-provider".into(),
            contract: "example.monitor".into(),
            version: 1,
            method: "observe".into(),
            params: json!({}),
            interval_ms: 10,
            timeout_ms: 100,
            stale_after_ms: 1000,
            startup_grace_ms: 100,
            rule: MonitorRule {
                pointer: "/ready".into(),
                operator: RuleOperator::Eq,
                value: json!(true),
                failure_samples: 1,
                success_samples: 1,
            },
        }],
    }
}

fn config() -> RecoveryConfig {
    let policy = |id: &str, reviewer| ApprovalPolicy {
        id: id.into(),
        version: 1,
        reviewer,
        delegation: "repair only a currently active target fault".into(),
        allowed_targets: vec!["target-a".into()],
        allowed_action_kinds: vec!["execute_script".into()],
        ttl_secs: 600,
    };
    RecoveryConfig {
        schema_version: 1,
        execution_harness: "executor".into(),
        target: TargetBinding {
            target_id: "target-a".into(),
            executor_id: "target-node".into(),
            platform: "portable".into(),
            allowed_languages: vec!["python".into()],
            diagnostic_queries: vec!["snapshot".into()],
            verification_profile: "readiness".into(),
            required_facts: BTreeMap::from([("workload_version".into(), "1".into())]),
            action_timeout_secs: 10,
        },
        approval: policy("human-repair", ReviewerConfig::Human),
        script_approval: policy(
            "reused-repair",
            ReviewerConfig::Harness {
                harness_id: "reviewer".into(),
            },
        ),
        diagnosis_timeout_secs: 10,
        review_timeout_secs: 10,
        max_tool_calls: 4,
        max_diagnoses: 2,
        minimum_script_occurrences: 2,
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
    fn diagnose(&self, input: DiagnosisInput, _: Cancellation) -> RecoveryFuture<'_, RepairPlan> {
        Box::pin(async move {
            Ok(RepairPlan {
                summary: "bounded workload proposal".into(),
                reusable: false,
                script: ScriptArtifact {
                    id: "script-a".into(),
                    version: 1,
                    language: "python".into(),
                    platform: input.config.target.platform,
                    source: "print('test-only proposal')".into(),
                    preconditions: input.observation.facts,
                    generated_by_harness: input.config.execution_harness,
                    generated_in_session: "execution-session".into(),
                },
            })
        })
    }
    fn review(&self, _: ReviewInput, _: Cancellation) -> RecoveryFuture<'_, ReviewOutput> {
        Box::pin(async { panic!("human-review fixture must not invoke a model reviewer") })
    }
    fn execute<'a>(
        &'a self,
        script: AuthorizedScript<'a>,
        _: Cancellation,
    ) -> RecoveryFuture<'a, ScriptReceipt> {
        Box::pin(async move {
            self.executions.fetch_add(1, Ordering::SeqCst);
            Ok(ScriptReceipt {
                operation_id: script.operation().operation_id.clone(),
                target_id: script.operation().target.clone(),
                outcome: ScriptOutcome::Executed,
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

async fn active_incident(handle: &MonitorHandle) -> IncidentRecord {
    wait_for(|| {
        handle.incidents().unwrap().iter().any(|record| {
            record.kind == IncidentKind::Target
                && record.condition == SignalCondition::Active
                && record.status != IncidentStatus::Resolved
        })
    })
    .await;
    handle
        .incidents()
        .unwrap()
        .into_iter()
        .find(|record| {
            record.kind == IncidentKind::Target
                && record.condition == SignalCondition::Active
                && record.status != IncidentStatus::Resolved
        })
        .unwrap()
}

fn problem(incident: &IncidentRecord) -> ProblemContext {
    ProblemContext {
        incident_id: incident.id.clone(),
        incident_revision: incident.revision,
        target_id: incident.target_id.clone(),
        fingerprint: "workload-not-ready".into(),
        summary: incident.summary.clone(),
        occurrences: 1,
        keywords: vec!["readiness".into()],
        conditions: BTreeMap::from([("workload_version".into(), "1".into())]),
        evidence_refs: vec![format!("incident:{}", incident.id)],
    }
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

fn bind(recovery: &RecoveryService, handle: MonitorHandle) {
    let trigger = IncidentTrigger {
        monitor_id: "monitor-a".into(),
        rule_id: "monitor-a".into(),
        fingerprint: "workload-not-ready".into(),
        keywords: vec!["readiness".into()],
        conditions: BTreeMap::from([("workload_version".into(), "1".into())]),
    };
    recovery
        .bind_incident_guard(Arc::new(
            MonitorIncidentGuard::new(handle, vec![trigger]).unwrap(),
        ))
        .unwrap();
}

fn guard(handle: MonitorHandle) -> MonitorIncidentGuard {
    MonitorIncidentGuard::new(
        handle,
        vec![IncidentTrigger {
            monitor_id: "monitor-a".into(),
            rule_id: "monitor-a".into(),
            fingerprint: "workload-not-ready".into(),
            keywords: vec!["readiness".into()],
            conditions: BTreeMap::from([("workload_version".into(), "1".into())]),
        }],
    )
    .unwrap()
}

#[tokio::test]
async fn human_approved_work_cannot_execute_after_the_incident_clears() {
    let dir = TestDir::new("incident-guard-cleared");
    let source = Arc::new(Source::new());
    let mut monitor = MonitorEngine::start_with_source(
        monitor_config(),
        source.clone(),
        dir.path.join("monitor"),
    )
    .unwrap();
    let handle = monitor.handle();
    let incident = active_incident(&handle).await;
    let backend = Arc::new(Backend::default());
    let recovery =
        RecoveryService::open(dir.path.join("recovery"), config(), backend.clone()).unwrap();
    recovery
        .bind_target_ownership(Arc::new(
            FileTargetOwnership::open(dir.path.join("ownership")).unwrap(),
        ))
        .unwrap();
    bind(&recovery, handle.clone());
    let task = pending(&recovery, &incident).await;
    approve(&recovery, &task);
    source.set(HEALTHY);
    wait_for(|| handle.incident(&incident.id).unwrap().unwrap().status == IncidentStatus::Resolved)
        .await;
    let resolved = handle
        .repair_incident(&incident.id, "target-a", incident.revision)
        .unwrap();
    assert_eq!(resolved.status, IncidentStatus::Resolved);
    assert_blocked(&recovery, &backend, &task).await;
    recovery.shutdown().await.unwrap();
    monitor.shutdown().await.unwrap();
}

#[tokio::test]
async fn lost_observation_coverage_prevents_consuming_old_approval() {
    let dir = TestDir::new("incident-guard-unknown");
    let source = Arc::new(Source::new());
    let mut monitor = MonitorEngine::start_with_source(
        monitor_config(),
        source.clone(),
        dir.path.join("monitor"),
    )
    .unwrap();
    let handle = monitor.handle();
    let incident = active_incident(&handle).await;
    let backend = Arc::new(Backend::default());
    let recovery =
        RecoveryService::open(dir.path.join("recovery"), config(), backend.clone()).unwrap();
    recovery
        .bind_target_ownership(Arc::new(
            FileTargetOwnership::open(dir.path.join("ownership")).unwrap(),
        ))
        .unwrap();
    bind(&recovery, handle.clone());
    let task = pending(&recovery, &incident).await;
    approve(&recovery, &task);
    source.set(UNAVAILABLE);
    wait_for(|| {
        handle.incident(&incident.id).unwrap().unwrap().condition == SignalCondition::Unknown
    })
    .await;
    assert!(
        handle
            .repair_incident(&incident.id, "target-a", incident.revision)
            .is_err()
    );
    assert_blocked(&recovery, &backend, &task).await;
    recovery.shutdown().await.unwrap();
    monitor.shutdown().await.unwrap();
}

#[tokio::test]
async fn stopped_monitor_cannot_authorize_an_active_persisted_incident() {
    let dir = TestDir::new("incident-guard-stopped");
    let mut monitor = MonitorEngine::start_with_source(
        monitor_config(),
        Arc::new(Source::new()),
        dir.path.join("monitor"),
    )
    .unwrap();
    let handle = monitor.handle();
    let incident = active_incident(&handle).await;
    let backend = Arc::new(Backend::default());
    let recovery =
        RecoveryService::open(dir.path.join("recovery"), config(), backend.clone()).unwrap();
    recovery
        .bind_target_ownership(Arc::new(
            FileTargetOwnership::open(dir.path.join("ownership")).unwrap(),
        ))
        .unwrap();
    bind(&recovery, handle.clone());
    let task = pending(&recovery, &incident).await;
    approve(&recovery, &task);
    monitor.shutdown().await.unwrap();
    assert_eq!(
        handle.incident(&incident.id).unwrap().unwrap().condition,
        SignalCondition::Active
    );
    assert!(
        handle
            .repair_incident(&incident.id, "target-a", incident.revision)
            .is_err()
    );
    assert_blocked(&recovery, &backend, &task).await;
    recovery.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_new_episode_cannot_reuse_the_old_episode_approval() {
    let dir = TestDir::new("incident-guard-episode");
    let source = Arc::new(Source::new());
    let mut monitor = MonitorEngine::start_with_source(
        monitor_config(),
        source.clone(),
        dir.path.join("monitor"),
    )
    .unwrap();
    let handle = monitor.handle();
    let old = active_incident(&handle).await;
    let backend = Arc::new(Backend::default());
    let recovery =
        RecoveryService::open(dir.path.join("recovery"), config(), backend.clone()).unwrap();
    recovery
        .bind_target_ownership(Arc::new(
            FileTargetOwnership::open(dir.path.join("ownership")).unwrap(),
        ))
        .unwrap();
    bind(&recovery, handle.clone());
    let task = pending(&recovery, &old).await;
    approve(&recovery, &task);
    source.set(HEALTHY);
    wait_for(|| handle.incident(&old.id).unwrap().unwrap().status == IncidentStatus::Resolved)
        .await;
    source.set(UNHEALTHY);
    let new = active_incident(&handle).await;
    assert_ne!(old.id, new.id);
    assert!(
        handle
            .repair_incident(&new.id, "target-a", new.revision)
            .is_ok()
    );
    assert_blocked(&recovery, &backend, &task).await;
    recovery.shutdown().await.unwrap();
    monitor.shutdown().await.unwrap();
}

#[tokio::test]
async fn acknowledged_still_active_episode_can_execute_with_a_newer_revision() {
    let dir = TestDir::new("incident-guard-acknowledged");
    let mut monitor = MonitorEngine::start_with_source(
        monitor_config(),
        Arc::new(Source::new()),
        dir.path.join("monitor"),
    )
    .unwrap();
    let handle = monitor.handle();
    let original = active_incident(&handle).await;
    let backend = Arc::new(Backend::default());
    let recovery =
        RecoveryService::open(dir.path.join("recovery"), config(), backend.clone()).unwrap();
    recovery
        .bind_target_ownership(Arc::new(
            FileTargetOwnership::open(dir.path.join("ownership")).unwrap(),
        ))
        .unwrap();
    bind(&recovery, handle.clone());
    let task = pending(&recovery, &original).await;
    let latest = handle.incident(&original.id).unwrap().unwrap();
    let acknowledged = handle
        .acknowledge(
            &latest.id,
            latest.revision,
            "operator",
            "acknowledged current incident",
        )
        .unwrap();
    assert!(acknowledged.revision > original.revision);
    assert_eq!(acknowledged.status, IncidentStatus::Acknowledged);
    assert!(
        handle
            .repair_incident(&original.id, "target-a", original.revision)
            .is_ok()
    );
    approve(&recovery, &task);
    let completed = recovery
        .advance(&task.id, Cancellation::new())
        .await
        .unwrap();
    assert_eq!(completed.stage, RecoveryStage::Completed);
    assert_eq!(backend.executions.load(Ordering::SeqCst), 1);
    recovery.shutdown().await.unwrap();
    monitor.shutdown().await.unwrap();
}

#[tokio::test]
async fn absent_incident_guard_rejects_episode_registration_before_dispatch() {
    let dir = TestDir::new("incident-guard-missing");
    let mut monitor = MonitorEngine::start_with_source(
        monitor_config(),
        Arc::new(Source::new()),
        dir.path.join("monitor"),
    )
    .unwrap();
    let incident = active_incident(&monitor.handle()).await;
    let backend = Arc::new(Backend::default());
    let recovery =
        RecoveryService::open(dir.path.join("recovery"), config(), backend.clone()).unwrap();
    recovery
        .bind_target_ownership(Arc::new(
            FileTargetOwnership::open(dir.path.join("ownership")).unwrap(),
        ))
        .unwrap();
    assert!(matches!(
        recovery.submit(problem(&incident)),
        Err(RecoveryError::Service(_))
    ));
    assert!(recovery.tasks().unwrap().is_empty());
    assert_eq!(backend.executions.load(Ordering::SeqCst), 0);
    recovery.shutdown().await.unwrap();
    monitor.shutdown().await.unwrap();
}

#[tokio::test]
async fn authority_read_rejects_wrong_target_unknown_id_and_future_revision() {
    let dir = TestDir::new("incident-guard-identity");
    let mut monitor = MonitorEngine::start_with_source(
        monitor_config(),
        Arc::new(Source::new()),
        dir.path.join("monitor"),
    )
    .unwrap();
    let handle = monitor.handle();
    let incident = active_incident(&handle).await;
    assert!(
        handle
            .repair_incident(&incident.id, "other-target", incident.revision)
            .is_err()
    );
    assert!(
        handle
            .repair_incident("nonexistent-episode", "target-a", 1)
            .is_err()
    );
    assert!(
        handle
            .repair_incident(&incident.id, "target-a", u64::MAX)
            .is_err()
    );
    assert!(handle.repair_incident(&incident.id, "target-a", 0).is_err());
    monitor.shutdown().await.unwrap();
}

#[tokio::test]
async fn guard_binding_is_fixed_and_cannot_be_replaced_after_registration() {
    let dir = TestDir::new("incident-guard-fixed-binding");
    let mut monitor = MonitorEngine::start_with_source(
        monitor_config(),
        Arc::new(Source::new()),
        dir.path.join("monitor"),
    )
    .unwrap();
    let handle = monitor.handle();
    let backend = Arc::new(Backend::default());
    let recovery = RecoveryService::open(dir.path.join("recovery"), config(), backend).unwrap();
    recovery
        .bind_target_ownership(Arc::new(
            FileTargetOwnership::open(dir.path.join("ownership")).unwrap(),
        ))
        .unwrap();
    recovery
        .bind_incident_guard(Arc::new(guard(handle.clone())))
        .unwrap();
    assert!(
        recovery
            .bind_incident_guard(Arc::new(guard(handle)))
            .is_err()
    );
    recovery.shutdown().await.unwrap();
    monitor.shutdown().await.unwrap();
}

#[tokio::test]
async fn fault_clearing_during_awaited_target_inspection_is_rechecked_before_execution() {
    let dir = TestDir::new("incident-guard-inspection-race");
    let source = Arc::new(Source::new());
    let mut monitor = MonitorEngine::start_with_source(
        monitor_config(),
        source.clone(),
        dir.path.join("monitor"),
    )
    .unwrap();
    let handle = monitor.handle();
    let incident = active_incident(&handle).await;
    let backend = Arc::new(Backend::default());
    let recovery =
        RecoveryService::open(dir.path.join("recovery"), config(), backend.clone()).unwrap();
    recovery
        .bind_target_ownership(Arc::new(
            FileTargetOwnership::open(dir.path.join("ownership")).unwrap(),
        ))
        .unwrap();
    bind(&recovery, handle.clone());
    let task = pending(&recovery, &incident).await;
    approve(&recovery, &task);
    backend.block_next_inspect.store(true, Ordering::SeqCst);
    let advancing = tokio::spawn({
        let recovery = recovery.clone();
        let id = task.id.clone();
        async move { recovery.advance(&id, Cancellation::new()).await }
    });
    tokio::time::timeout(Duration::from_secs(3), backend.inspect_entered.notified())
        .await
        .unwrap();
    source.set(HEALTHY);
    wait_for(|| handle.incident(&incident.id).unwrap().unwrap().status == IncidentStatus::Resolved)
        .await;
    backend.inspect_release.notify_one();
    let _ = tokio::time::timeout(Duration::from_secs(3), advancing)
        .await
        .unwrap()
        .unwrap();
    assert_blocked(&recovery, &backend, &task).await;
    recovery.shutdown().await.unwrap();
    monitor.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn monotonic_deadline_rejects_stale_evidence_before_the_next_view_tick() {
    let dir = TestDir::new("incident-guard-monotonic-deadline");
    let mut definition = monitor_config();
    definition.monitors[0].interval_ms = 1000;
    definition.monitors[0].startup_grace_ms = 1000;
    let mut source = Source::new();
    source.age_ms = 990;
    let mut monitor =
        MonitorEngine::start_with_source(definition, Arc::new(source), dir.path.join("monitor"))
            .unwrap();
    let handle = monitor.handle();
    let incident = active_incident(&handle).await;
    assert!(
        handle
            .repair_incident(&incident.id, "target-a", incident.revision)
            .is_ok()
    );
    tokio::time::advance(Duration::from_millis(20)).await;
    // No 250ms housekeeping tick has run: the ordinary diagnostic view is cached.
    assert_eq!(
        handle.monitor("monitor-a").unwrap().unwrap().freshness,
        Freshness::Fresh
    );
    assert_eq!(
        handle.incident(&incident.id).unwrap().unwrap().condition,
        SignalCondition::Active
    );
    assert!(
        handle
            .repair_incident(&incident.id, "target-a", incident.revision)
            .is_err()
    );
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
    let incident = active_incident(&handle).await;
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

#[tokio::test(start_paused = true)]
async fn owned_incident_lease_blocks_mutation_without_blocking_runtime_and_rechecks_freshness() {
    let dir = TestDir::new("owned-incident-dispatch-gate");
    let source = Arc::new(Source::new());
    let mut monitor = MonitorEngine::start_with_source(
        monitor_config(),
        source.clone(),
        dir.path.join("monitor"),
    )
    .unwrap();
    let handle = monitor.handle();
    let incident = active_incident(&handle).await;
    let lease = handle
        .acquire_repair_incident(&incident.id, "target-a", incident.revision)
        .await
        .unwrap();
    assert_eq!(lease.current().unwrap().revision, incident.revision);
    assert!(
        handle
            .acknowledge(&incident.id, incident.revision, "operator", "busy lease")
            .is_err()
    );
    assert!(
        handle
            .repair_incident(&incident.id, "target-a", incident.revision)
            .is_err()
    );
    source.set(HEALTHY);
    // The monitor writer waits asynchronously; the runtime still advances timers.
    tokio::time::advance(Duration::from_millis(1100)).await;
    assert_eq!(
        handle.incident(&incident.id).unwrap().unwrap().revision,
        incident.revision
    );
    assert!(
        lease.current().is_err(),
        "holding the mutation gate must not freeze evidence age"
    );
    drop(lease);
    wait_for(|| handle.incident(&incident.id).unwrap().unwrap().status == IncidentStatus::Resolved)
        .await;
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
            Ok(IncidentReadiness::Active { .. }) => Ok(()),
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
async fn production_incident_lease_reaches_network_send_and_stale_evidence_sends_nothing() {
    use recuvora_host::integrations::extensions::{
        ExtensionCall, ExtensionError, ProtocolSettings,
    };
    for expire in [false, true] {
        let dir = TestDir::new("incident-network-dispatch");
        let source = Arc::new(Source::new());
        let mut monitor = MonitorEngine::start_with_source(
            monitor_config(),
            source.clone(),
            dir.path.join("monitor"),
        )
        .unwrap();
        let handle = monitor.handle();
        let incident = active_incident(&handle).await;
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
        source.set(HEALTHY);
        if expire {
            tokio::time::sleep(Duration::from_millis(1100)).await;
        }
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
        wait_for(|| {
            handle.incident(&gate.incident.id).unwrap().unwrap().status == IncidentStatus::Resolved
        })
        .await;
        if !expire {
            fixture.state.wait_for_dispatch().await;
            assert!(!pending.is_finished());
            cancel.cancel();
        }
        let result = tokio::time::timeout(Duration::from_secs(5), pending)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            fixture.state.calls.load(Ordering::SeqCst),
            usize::from(!expire)
        );
        if expire {
            assert!(matches!(result, Err(ExtensionError::Rejected(_))));
        } else {
            assert!(matches!(result, Err(ExtensionError::Unknown { .. })));
        }
        fixture.shutdown().await;
        monitor.shutdown().await.unwrap();
    }
}
