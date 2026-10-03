use recuvora_core::recovery::approval::{
    ApprovalDecision, ApprovalPolicy, ModelAssessment, ReviewerConfig, ReviewerIdentity,
};
use recuvora_host::integrations::recovery::RecoveryScheduler;
use recuvora_host::integrations::recovery::*;
use recuvora_host::monitoring::*;
use recuvora_host::runtime::operation::Cancellation;
use serde_json::json;
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};

#[path = "workflow_support.rs"]
mod support;
use support::TestDir;

fn config() -> RecoveryConfig {
    let policy = ApprovalPolicy {
        id: "repair-policy".into(),
        version: 1,
        reviewer: ReviewerConfig::Harness {
            harness_id: "reviewer".into(),
        },
        delegation: "review explicitly scoped target proposals".into(),
        allowed_targets: vec!["target-a".into()],
        allowed_action_kinds: vec!["repair_with_harness".into()],
        ttl_secs: 60,
    };
    RecoveryConfig {
        schema_version: 2,
        execution_harness: "execution".into(),
        target: TargetBinding {
            target_id: "target-a".into(),
            executor_id: "target-node".into(),
            allowed_action_kinds: vec!["execute_script".into()],
            verification_profile: "readiness".into(),
            required_facts: BTreeMap::from([("workload_version".into(), "1".into())]),
            action_timeout_secs: 10,
        },
        approval: policy.clone(),
        summary_timeout_secs: 10,
        review_timeout_secs: 10,
        max_tool_calls: 4,
        max_tasks: 20,
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
            extension_id: "test-provider".into(),
            contract: "example.monitor".into(),
            version: 1,
            method: "observe".into(),
            params: json!({}),
            interval_ms: 10,
            timeout_ms: 100,
            stale_after_ms: 1000,
            startup_grace_ms: 10,
        }],
    }
}

struct Source {
    sequence: AtomicU64,
    unavailable: bool,
}
impl ObservationSource for Source {
    fn poll(&self, request: ObservationRequest, _: Cancellation) -> ObservationFuture<'_> {
        self.sequence.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            if self.unavailable {
                return Err(MonitorError::Observation("source unavailable".into()));
            }
            Ok(ObservationBatch {
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
                    fingerprint: "workload-exited".into(),
                    message: "ERROR workload exited\n  source stack trace".into(),
                    evidence: json!({"exit_code":17,"origin":"node-log"}),
                }],
            })
        })
    }
}

#[derive(Default)]
struct DelayedErrors {
    available: AtomicBool,
    polls: AtomicUsize,
}
impl ObservationSource for DelayedErrors {
    fn poll(&self, request: ObservationRequest, _: Cancellation) -> ObservationFuture<'_> {
        let available = self.available.load(Ordering::SeqCst);
        self.polls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            Ok(ErrorLogBatch {
                schema_version: 2,
                target_id: "target-a".into(),
                source_id: "source-a".into(),
                generation: "generation-a".into(),
                cursor: request.params["cursor"].as_str().map(str::to_owned),
                next_cursor: if available { "cursor-2" } else { "cursor-0" }.into(),
                coverage: BatchCoverage::Complete,
                has_more: false,
                source_error: None,
                errors: if available {
                    (1..=2)
                        .map(|sequence| NodeErrorLog {
                            id: format!("error-{sequence}"),
                            sequence,
                            age_ms: 86_400_000,
                            fingerprint: "same-error-kind".into(),
                            message: format!("ERROR original record {sequence}\n  完整错误堆栈"),
                            evidence: json!({"record":sequence}),
                        })
                        .collect()
                } else {
                    Vec::new()
                },
            })
        })
    }
}

struct Backend {
    inspections: AtomicUsize,
    block_inspection: bool,
    completed_pending: AtomicBool,
}
impl Backend {
    fn new(block_inspection: bool) -> Self {
        Self {
            inspections: AtomicUsize::new(0),
            block_inspection,
            completed_pending: AtomicBool::new(false),
        }
    }
}
impl RepairBackend for Backend {
    fn inspect<'a>(
        &'a self,
        target: &'a TargetBinding,
        cancellation: Cancellation,
    ) -> RecoveryFuture<'a, TargetObservation> {
        Box::pin(async move {
            self.inspections.fetch_add(1, Ordering::SeqCst);
            if self.block_inspection {
                cancellation.cancelled().await;
                self.completed_pending.store(true, Ordering::SeqCst);
                return Err(RecoveryError::Service(
                    "inspection cooperatively canceled".into(),
                ));
            }
            Ok(TargetObservation {
                target_id: target.target_id.clone(),
                facts: target.required_facts.clone(),
                evidence_refs: vec!["inspection:1".into()],
                observed_at_ms: SystemRecoveryClock.now_ms(),
            })
        })
    }
    fn review(&self, input: ReviewInput, _: Cancellation) -> RecoveryFuture<'_, ReviewOutput> {
        Box::pin(async move {
            Ok(ReviewOutput {
                assessment: ModelAssessment {
                    request_id: input.request.request_id,
                    decision: ApprovalDecision::Deny,
                    reason: "test denial".into(),
                },
                identity: ReviewerIdentity {
                    harness_id: input.attempt.harness_id,
                    session_id: "isolated-review-session".into(),
                },
            })
        })
    }
    fn execute<'a>(
        &'a self,
        _: AuthorizedRepair<'a>,
        _: Cancellation,
    ) -> RecoveryFuture<'a, RepairReceipt> {
        Box::pin(async { panic!("denied or canceled tasks must never execute") })
    }
    fn verify(
        &self,
        _: VerificationInput,
        _: Cancellation,
    ) -> RecoveryFuture<'_, BusinessVerification> {
        Box::pin(async { panic!("denied or canceled tasks have no business verification") })
    }
}

async fn wait_for(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("condition reached within bounded test deadline");
}

#[tokio::test]
async fn same_error_log_is_not_reinspected_after_terminal_task_or_restart() {
    let dir = TestDir::new("scheduler-dedup");
    let mut monitor = MonitorEngine::start_with_source(
        monitor_config(),
        Arc::new(Source {
            sequence: AtomicU64::new(0),
            unavailable: false,
        }),
        dir.path.join("monitor"),
    )
    .unwrap();
    let backend = Arc::new(Backend::new(false));
    let recovery =
        RecoveryService::open(dir.path.join("recovery"), config(), backend.clone()).unwrap();
    recovery
        .bind_target_ownership(Arc::new(
            FileTargetOwnership::open(dir.path.join("ownership")).unwrap(),
        ))
        .unwrap();
    let scheduler = RecoveryScheduler::start(
        recovery.clone(),
        monitor.handle(),
        Duration::from_millis(10),
    )
    .unwrap();
    wait_for(|| {
        recovery
            .tasks()
            .unwrap()
            .first()
            .is_some_and(|task| task.stage == RecoveryStage::Denied)
    })
    .await;
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert_eq!(backend.inspections.load(Ordering::SeqCst), 2);
    assert_eq!(recovery.tasks().unwrap().len(), 1);
    let received = &recovery.tasks().unwrap()[0].problem;
    assert_eq!(received.origin, ProblemOrigin::ErrorLog);
    assert_eq!(
        received.summary,
        "ERROR workload exited\n  source stack trace"
    );
    let report = received.report.as_ref().unwrap();
    assert_eq!(report.age_ms, 86_400_000);
    assert_eq!(report.evidence, json!({"exit_code":17,"origin":"node-log"}));

    scheduler.shutdown().await.unwrap();
    assert!(monitor.handle().snapshot().unwrap().running);
    drop(scheduler);
    drop(recovery);
    let reopened =
        RecoveryService::open(dir.path.join("recovery"), config(), backend.clone()).unwrap();
    reopened
        .bind_target_ownership(Arc::new(
            FileTargetOwnership::open(dir.path.join("ownership")).unwrap(),
        ))
        .unwrap();
    let scheduler = RecoveryScheduler::start(
        reopened.clone(),
        monitor.handle(),
        Duration::from_millis(10),
    )
    .unwrap();
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert_eq!(backend.inspections.load(Ordering::SeqCst), 2);
    assert_eq!(reopened.tasks().unwrap().len(), 1);
    scheduler.shutdown().await.unwrap();
    monitor.shutdown().await.unwrap();
}

#[tokio::test]
async fn shutdown_cancels_and_waits_for_original_inspection_without_stopping_monitor() {
    let dir = TestDir::new("scheduler-wait_for_idle");
    let mut monitor = MonitorEngine::start_with_source(
        monitor_config(),
        Arc::new(Source {
            sequence: AtomicU64::new(0),
            unavailable: false,
        }),
        dir.path.join("monitor"),
    )
    .unwrap();
    let backend = Arc::new(Backend::new(true));
    let recovery =
        RecoveryService::open(dir.path.join("recovery"), config(), backend.clone()).unwrap();
    recovery
        .bind_target_ownership(Arc::new(
            FileTargetOwnership::open(dir.path.join("ownership")).unwrap(),
        ))
        .unwrap();
    let scheduler =
        RecoveryScheduler::start(recovery, monitor.handle(), Duration::from_millis(10)).unwrap();
    wait_for(|| backend.inspections.load(Ordering::SeqCst) == 1).await;
    tokio::time::timeout(Duration::from_secs(3), scheduler.shutdown())
        .await
        .unwrap()
        .unwrap();
    assert!(backend.completed_pending.load(Ordering::SeqCst));
    assert!(!scheduler.is_running());
    assert!(monitor.handle().snapshot().unwrap().running);
    scheduler.shutdown().await.unwrap();
    monitor.shutdown().await.unwrap();
}

#[tokio::test]
async fn source_failure_never_invents_an_error_log_or_repair_task() {
    let dir = TestDir::new("scheduler-coverage");
    let mut monitor = MonitorEngine::start_with_source(
        monitor_config(),
        Arc::new(Source {
            sequence: AtomicU64::new(0),
            unavailable: true,
        }),
        dir.path.join("monitor"),
    )
    .unwrap();
    let recovery = RecoveryService::open(
        dir.path.join("recovery"),
        config(),
        Arc::new(Backend::new(false)),
    )
    .unwrap();
    recovery
        .bind_target_ownership(Arc::new(
            FileTargetOwnership::open(dir.path.join("ownership")).unwrap(),
        ))
        .unwrap();
    let scheduler = RecoveryScheduler::start(
        recovery.clone(),
        monitor.handle(),
        Duration::from_millis(10),
    )
    .unwrap();
    wait_for(|| {
        monitor
            .handle()
            .monitor("monitor-a")
            .unwrap()
            .is_some_and(|view| view.last_error.is_some())
    })
    .await;
    assert!(
        monitor
            .handle()
            .incidents()
            .unwrap()
            .iter()
            .all(|record| record.kind
                != recuvora_host::control::recovery::incidents::IncidentKind::ErrorLog)
    );
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert!(recovery.tasks().unwrap().is_empty());
    scheduler.shutdown().await.unwrap();
    monitor.shutdown().await.unwrap();
}

#[tokio::test]
async fn human_wait_does_not_start_repeated_inspections_or_block_shutdown() {
    let dir = TestDir::new("scheduler-human");
    let mut monitor = MonitorEngine::start_with_source(
        monitor_config(),
        Arc::new(Source {
            sequence: AtomicU64::new(0),
            unavailable: false,
        }),
        dir.path.join("monitor"),
    )
    .unwrap();
    let backend = Arc::new(Backend::new(false));
    let mut config = config();
    config.approval.reviewer = ReviewerConfig::Human;
    let recovery =
        RecoveryService::open(dir.path.join("recovery"), config, backend.clone()).unwrap();
    recovery
        .bind_target_ownership(Arc::new(
            FileTargetOwnership::open(dir.path.join("ownership")).unwrap(),
        ))
        .unwrap();
    let scheduler = RecoveryScheduler::start(
        recovery.clone(),
        monitor.handle(),
        Duration::from_millis(10),
    )
    .unwrap();
    wait_for(|| {
        recovery
            .tasks()
            .unwrap()
            .first()
            .is_some_and(|task| task.stage == RecoveryStage::AwaitingApproval)
    })
    .await;
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert_eq!(backend.inspections.load(Ordering::SeqCst), 1);
    assert_eq!(recovery.tasks().unwrap().len(), 1);
    scheduler.shutdown().await.unwrap();
    monitor.shutdown().await.unwrap();
}

#[tokio::test]
async fn durable_receipts_wake_scheduler_and_busy_backlog_runs_without_poll_delay() {
    let dir = TestDir::new("scheduler-receipt-wakeup");
    let source = Arc::new(DelayedErrors::default());
    let mut monitor = MonitorEngine::start_with_source(
        monitor_config(),
        source.clone(),
        dir.path.join("monitor"),
    )
    .unwrap();
    wait_for(|| {
        monitor
            .handle()
            .monitor("monitor-a")
            .unwrap()
            .unwrap()
            .cursor
            .is_some()
    })
    .await;
    let backend = Arc::new(Backend::new(false));
    let recovery =
        RecoveryService::open(dir.path.join("recovery"), config(), backend.clone()).unwrap();
    recovery
        .bind_target_ownership(Arc::new(
            FileTargetOwnership::open(dir.path.join("ownership")).unwrap(),
        ))
        .unwrap();
    let scheduler = RecoveryScheduler::start(
        recovery.clone(),
        monitor.handle(),
        Duration::from_secs(3600),
    )
    .unwrap();
    // The initial scan has no receipts. Both the first task and the Busy retry
    // must therefore finish through notifications, well before the 1h timer.
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(recovery.tasks().unwrap().is_empty());
    source.available.store(true, Ordering::SeqCst);
    wait_for(|| {
        let tasks = recovery.tasks().unwrap();
        tasks.len() == 2 && tasks.iter().all(|task| task.stage == RecoveryStage::Denied)
    })
    .await;
    assert_eq!(backend.inspections.load(Ordering::SeqCst), 4);
    for task in recovery.tasks().unwrap() {
        let report = task.problem.report.as_ref().unwrap();
        assert_eq!(
            task.problem.summary,
            format!("ERROR original record {}\n  完整错误堆栈", report.sequence)
        );
        assert_eq!(report.evidence, json!({"record":report.sequence}));
    }
    scheduler.shutdown().await.unwrap();
    monitor.shutdown().await.unwrap();
}

#[tokio::test]
async fn busy_receipt_survives_both_services_restart_and_keeps_original_identity() {
    let dir = TestDir::new("scheduler-persisted-backlog");
    let source = Arc::new(DelayedErrors::default());
    source.available.store(true, Ordering::SeqCst);
    let mut monitor = MonitorEngine::start_with_source(
        monitor_config(),
        source.clone(),
        dir.path.join("monitor"),
    )
    .unwrap();
    wait_for(|| {
        monitor
            .handle()
            .incidents()
            .unwrap()
            .iter()
            .filter(|record| {
                record.kind == recuvora_host::control::recovery::incidents::IncidentKind::ErrorLog
            })
            .count()
            == 2
    })
    .await;
    let mut settings = config();
    settings.approval.reviewer = ReviewerConfig::Human;
    let backend = Arc::new(Backend::new(false));
    let recovery =
        RecoveryService::open(dir.path.join("recovery"), settings.clone(), backend.clone())
            .unwrap();
    recovery
        .bind_target_ownership(Arc::new(
            FileTargetOwnership::open(dir.path.join("ownership")).unwrap(),
        ))
        .unwrap();
    let scheduler = RecoveryScheduler::start(
        recovery.clone(),
        monitor.handle(),
        Duration::from_millis(10),
    )
    .unwrap();
    wait_for(|| {
        recovery
            .tasks()
            .unwrap()
            .first()
            .is_some_and(|task| task.stage == RecoveryStage::AwaitingApproval)
    })
    .await;
    let accepted = recovery.tasks().unwrap().remove(0);
    let receipts: Vec<_> = monitor
        .handle()
        .incidents()
        .unwrap()
        .into_iter()
        .filter(|record| {
            record.kind == recuvora_host::control::recovery::incidents::IncidentKind::ErrorLog
        })
        .collect();
    assert_eq!(recovery.tasks().unwrap().len(), 1);
    scheduler.shutdown().await.unwrap();
    monitor.shutdown().await.unwrap();
    drop(scheduler);
    drop(recovery);
    drop(monitor);

    let mut monitor =
        MonitorEngine::start_with_source(monitor_config(), source, dir.path.join("monitor"))
            .unwrap();
    let recovery =
        RecoveryService::open(dir.path.join("recovery"), settings, backend.clone()).unwrap();
    recovery
        .bind_target_ownership(Arc::new(
            FileTargetOwnership::open(dir.path.join("ownership")).unwrap(),
        ))
        .unwrap();
    let restored = recovery.query(&accepted.id).unwrap().unwrap();
    assert_eq!(restored.stage, RecoveryStage::Paused);
    recovery.resume(&accepted.id, restored.revision).unwrap();
    let approval = recovery.approval(&accepted.id).unwrap().unwrap();
    recovery
        .decide_human(
            &accepted.id,
            approval.revision,
            ApprovalDecision::Deny,
            "operator".into(),
            "reviewed original receipt".into(),
        )
        .unwrap();
    let scheduler = RecoveryScheduler::start(
        recovery.clone(),
        monitor.handle(),
        Duration::from_millis(10),
    )
    .unwrap();
    wait_for(|| {
        let tasks = recovery.tasks().unwrap();
        tasks.len() == 2
            && tasks
                .iter()
                .any(|task| task.id != accepted.id && task.stage == RecoveryStage::AwaitingApproval)
    })
    .await;
    assert_eq!(
        recovery.query(&accepted.id).unwrap().unwrap().problem,
        accepted.problem
    );
    let tasks = recovery.tasks().unwrap();
    for receipt in receipts {
        let task = tasks
            .iter()
            .find(|task| task.problem.incident_id == receipt.id)
            .unwrap();
        assert_eq!(task.problem.summary, receipt.summary);
        assert_eq!(
            task.problem.report.as_ref().unwrap().record_id,
            receipt.evidence["log"]["id"].as_str().unwrap()
        );
    }
    assert_eq!(backend.inspections.load(Ordering::SeqCst), 2);
    scheduler.shutdown().await.unwrap();
    monitor.shutdown().await.unwrap();
}
