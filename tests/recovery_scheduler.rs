use recuvora_core::recovery::approval::{
    ApprovalDecision, ApprovalPolicy, ModelAssessment, ReviewerConfig, ReviewerIdentity,
};
use recuvora_host::integrations::recovery::*;
use recuvora_host::integrations::recovery::{IncidentTrigger, RecoveryScheduler};
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

fn trigger() -> IncidentTrigger {
    IncidentTrigger {
        monitor_id: "monitor-a".into(),
        rule_id: "monitor-a".into(),
        fingerprint: "not-ready".into(),
        keywords: vec!["readiness".into()],
        conditions: BTreeMap::from([("workload_version".into(), "1".into())]),
    }
}

fn monitor_config() -> MonitorsConfig {
    MonitorsConfig {
        schema_version: 1,
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

struct Source {
    sequence: AtomicU64,
    unavailable: bool,
}
impl ObservationSource for Source {
    fn poll(&self, request: ObservationRequest, _: Cancellation) -> ObservationFuture<'_> {
        let sequence = self.sequence.fetch_add(1, Ordering::SeqCst) + 1;
        Box::pin(async move {
            if self.unavailable {
                return Err(MonitorError::Observation("source unavailable".into()));
            }
            Ok(ObservationBatch {
                schema_version: 1,
                target_id: "target-a".into(),
                source_id: "source-a".into(),
                generation: "generation-a".into(),
                cursor: request.params["cursor"].as_str().map(str::to_owned),
                next_cursor: sequence.to_string(),
                coverage: BatchCoverage::Complete,
                has_more: false,
                error: None,
                samples: vec![ObservationSample {
                    id: format!("sample-{sequence}"),
                    sequence,
                    age_ms: 0,
                    value: json!({"ready":false}),
                    evidence: json!({"origin":"test-observation"}),
                }],
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
async fn same_active_incident_is_not_reinspected_after_terminal_task_or_restart() {
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
        vec![trigger()],
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
        vec![trigger()],
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
    let scheduler = RecoveryScheduler::start(
        recovery,
        monitor.handle(),
        vec![trigger()],
        Duration::from_millis(10),
    )
    .unwrap();
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
async fn coverage_failure_never_creates_a_repair_task() {
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
        vec![trigger()],
        Duration::from_millis(10),
    )
    .unwrap();
    wait_for(|| !monitor.handle().incidents().unwrap().is_empty()).await;
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
        vec![trigger()],
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
