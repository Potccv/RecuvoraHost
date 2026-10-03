//! Actual process exits around atomic recovery commits; no destructor completes a commit.
use super::*;
use crate::workflow_test_support::TestDir;
use recuvora_core::recovery::{
    approval::{ApprovalPolicy, ApprovalState, ModelAssessment, ReviewerConfig, ReviewerIdentity},
    knowledge::{ExperienceReport, RepairArtifact, Scriptability},
};
use std::{collections::BTreeMap, io::Write, time::Duration};

struct Clock;
impl RecoveryClock for Clock {
    fn now_ms(&self) -> u64 {
        20_000
    }
}
struct Guard;
impl IncidentGuard for Guard {
    fn with_current(
        &self,
        _: &ProblemContext,
        commit: &mut dyn FnMut(IncidentReadiness) -> Result<(), RecoveryError>,
    ) -> Result<(), RecoveryError> {
        commit(IncidentReadiness::Active { revision: 1 })
    }
    fn acquire_dispatch<'a>(
        &'a self,
        _: &'a ProblemContext,
    ) -> RecoveryFuture<'a, Box<dyn IncidentDispatchLease>> {
        Box::pin(async { Ok(Box::new(Guard) as Box<dyn IncidentDispatchLease>) })
    }
}
impl IncidentDispatchLease for Guard {
    fn current(&self) -> Result<IncidentReadiness, RecoveryError> {
        Ok(IncidentReadiness::Active { revision: 1 })
    }
}

struct Backend {
    directory: PathBuf,
}
impl Backend {
    fn record(&self, file: &str, value: &str) -> Result<(), RecoveryError> {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.directory.join(file))?;
        writeln!(file, "{value}")?;
        file.sync_all()?;
        Ok(())
    }
}
impl RepairBackend for Backend {
    fn summarize(
        &self,
        _: recuvora_core::recovery::workflow::ExperienceJob,
        _: RecoveryConfig,
        _: Cancellation,
    ) -> RecoveryFuture<'_, ExperienceReport> {
        Box::pin(async {
            Ok(ExperienceReport {
                summary: "persisted repair result".into(),
                lessons: "verify independently".into(),
                related_experience_ids: vec![],
                scriptability: Scriptability::Undetermined {
                    reason: "fixture only".into(),
                },
            })
        })
    }
    fn inspect<'a>(
        &'a self,
        target: &'a TargetBinding,
        _: Cancellation,
    ) -> RecoveryFuture<'a, TargetObservation> {
        Box::pin(async move {
            Ok(TargetObservation {
                target_id: target.target_id.clone(),
                facts: target.required_facts.clone(),
                evidence_refs: vec!["provider:inspection".into()],
                observed_at_ms: 20_000,
            })
        })
    }
    fn review(&self, input: ReviewInput, _: Cancellation) -> RecoveryFuture<'_, ReviewOutput> {
        Box::pin(async move {
            Ok(ReviewOutput {
                assessment: ModelAssessment {
                    request_id: input.request.request_id,
                    decision: ApprovalDecision::Approve,
                    reason: "scoped fixture approval".into(),
                },
                identity: ReviewerIdentity {
                    harness_id: input.attempt.harness_id,
                    session_id: "independent-review".into(),
                },
            })
        })
    }
    fn execute<'a>(
        &'a self,
        script: AuthorizedRepair<'a>,
        _: Cancellation,
    ) -> RecoveryFuture<'a, RepairReceipt> {
        Box::pin(async move {
            let guard = script
                .repair_action_guard()
                .expect("Host repair action gate");
            let action = RepairArtifact {
                id: format!("{}-action", script.operation().operation_id),
                version: 1,
                kind: "execute_script".into(),
                payload: serde_json::json!({"language":"python","source":"fixture action"}),
                preconditions: config().target.required_facts,
                generated_by_harness: "executor".into(),
                generated_in_session: script.operation().operation_id.clone(),
            };
            guard.prepare_repair_action(&action).map_err(service)?;
            guard.validate().map_err(service)?;
            self.record("executions.log", &script.operation().operation_id)?;
            guard.release();
            Ok(RepairReceipt {
                execution_trace: vec![action],
                operation_id: script.operation().operation_id.clone(),
                target_id: script.operation().target.clone(),
                outcome: if matches!(
                    std::env::var("RECUVORA_RECOVERY_CRASH_BOUNDARY").as_deref(),
                    Ok("result_check_committed")
                ) {
                    RepairExecutionOutcome::Unknown
                } else {
                    RepairExecutionOutcome::Executed
                },
                executor_stopped: true,
                evidence_refs: vec!["executor:durable-receipt".into()],
                summary: "fixture executed once".into(),
            })
        })
    }
    fn verify(
        &self,
        input: VerificationInput,
        _: Cancellation,
    ) -> RecoveryFuture<'_, BusinessVerification> {
        Box::pin(async move {
            self.record("verifications.log", &input.operation.operation_id)?;
            Ok(BusinessVerification {
                operation_id: input.operation.operation_id,
                target_id: input.operation.target,
                profile: input.target.verification_profile,
                healthy: Some(true),
                executor_stopped: true,
                evidence_refs: vec!["independent:business-check".into()],
                verified_at_ms: 20_000,
            })
        })
    }
}

fn config() -> RecoveryConfig {
    let policy = ApprovalPolicy {
        id: "policy-a".into(),
        version: 1,
        reviewer: ReviewerConfig::Harness {
            harness_id: "reviewer".into(),
        },
        delegation: "bounded external action".into(),
        allowed_targets: vec!["target-a".into()],
        allowed_action_kinds: vec!["repair_with_harness".into()],
        ttl_secs: 60,
    };
    RecoveryConfig {
        schema_version: 2,
        execution_harness: "executor".into(),
        target: TargetBinding {
            target_id: "target-a".into(),
            executor_id: "node-a".into(),
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
fn problem() -> ProblemContext {
    ProblemContext {
        incident_id: "incident-a".into(),
        incident_revision: 1,
        target_id: "target-a".into(),
        fingerprint: "not-ready".into(),
        summary: "provider reports unavailable".into(),
        occurrences: 1,
        keywords: vec!["readiness".into()],
        conditions: config().target.required_facts,
        evidence_refs: vec!["provider:incident-a".into()],
    }
}
fn open(directory: &Path) -> Arc<RecoveryService> {
    let backend = Arc::new(Backend {
        directory: directory.to_path_buf(),
    });
    let service = RecoveryService::open_with_clock(
        directory.join("state"),
        config(),
        backend,
        Arc::new(Clock),
    )
    .unwrap();
    service
        .bind_target_ownership(Arc::new(
            FileTargetOwnership::open(directory.join("ownership")).unwrap(),
        ))
        .unwrap();
    service.bind_incident_guard(Arc::new(Guard)).unwrap();
    service
}
fn lines(directory: &Path, name: &str) -> usize {
    std::fs::read_to_string(directory.join(name)).map_or(0, |text| text.lines().count())
}

#[tokio::test]
async fn crash_probe() {
    let Some(directory) = std::env::var_os("RECUVORA_RECOVERY_CRASH_DIRECTORY") else {
        return;
    };
    let directory = PathBuf::from(directory);
    let service = open(&directory);
    let task = service.submit(problem()).unwrap();
    let task = service
        .advance(&task.id, Cancellation::new())
        .await
        .unwrap();
    if matches!(
        std::env::var("RECUVORA_RECOVERY_CRASH_BOUNDARY").as_deref(),
        Ok("result_check_committed")
    ) {
        let operation = task.operation.as_ref().unwrap();
        service
            .check_result(
                &task.id,
                task.revision,
                ExecutionResultCheck {
                    operation_id: operation.operation_id.clone(),
                    target_id: operation.target.clone(),
                    executor_id: config().target.executor_id,
                    outcome: CheckedExecution::Executed,
                    executor_stopped: true,
                    evidence_refs: vec!["executor:durable-receipt".into()],
                    checked_at_ms: 20_000,
                },
                BusinessVerification {
                    operation_id: operation.operation_id.clone(),
                    target_id: operation.target.clone(),
                    profile: config().target.verification_profile,
                    healthy: Some(true),
                    executor_stopped: true,
                    evidence_refs: vec!["independent:business-check".into()],
                    verified_at_ms: 20_000,
                },
                "operator".into(),
            )
            .unwrap();
    }
    panic!("requested crash boundary was never reached");
}

async fn assert_boundary(boundary: &str) {
    let dir = TestDir::new(boundary);
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "recovery::service::commit_tests::crash_probe",
            "--nocapture",
        ])
        .env("RECUVORA_RECOVERY_CRASH_BOUNDARY", boundary)
        .env("RECUVORA_RECOVERY_CRASH_DIRECTORY", &dir.path)
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert_eq!(
                status.code(),
                Some(93),
                "{boundary}: subprocess must exit at the selected commit boundary"
            );
            break;
        }
        if std::time::Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("{boundary}: child timed out");
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let executed_before = lines(&dir.path, "executions.log");
    let service = open(&dir.path);
    let tasks = service.tasks().unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(
        lines(&dir.path, "executions.log"),
        executed_before,
        "open cannot replay effects"
    );
    let mut task = tasks[0].clone();
    let original_operation = task.operation.clone().unwrap();
    match boundary {
        "start_committed" => {
            assert_eq!(executed_before, 0);
            assert_eq!(task.stage, RecoveryStage::Paused);
            task = service.resume(&task.id, task.revision).unwrap();
            assert_eq!(task.operation.as_ref(), Some(&original_operation));
            task = service
                .advance(&task.id, Cancellation::new())
                .await
                .unwrap();
            assert_eq!(task.stage, RecoveryStage::Completed);
            assert_eq!(lines(&dir.path, "executions.log"), 1);
        }
        "authorization_committed" => {
            assert_eq!(executed_before, 0);
            assert_eq!(
                task.stage,
                RecoveryStage::Canceled,
                "Host dispatch journal independently proves action was not sent"
            );
            assert!(task.result_check.is_some());
            task = service
                .advance(&task.id, Cancellation::new())
                .await
                .unwrap();
            assert_eq!(task.stage, RecoveryStage::Canceled);
            assert_eq!(lines(&dir.path, "executions.log"), 0);
        }
        "receipt_committed" => {
            assert_eq!(executed_before, 1);
            assert_eq!(task.stage, RecoveryStage::Verifying);
            task = service
                .advance(&task.id, Cancellation::new())
                .await
                .unwrap();
            assert_eq!(task.stage, RecoveryStage::Completed);
            assert_eq!(lines(&dir.path, "executions.log"), 1);
            assert_eq!(lines(&dir.path, "verifications.log"), 1);
        }
        "verification_committed" | "experience_committed" => {
            assert_eq!(task.stage, RecoveryStage::Completed);
            service
                .with_state(|state| {
                    assert_eq!(
                        state.session.pending_experiences().len(),
                        usize::from(boundary == "verification_committed")
                    );
                    Ok(())
                })
                .unwrap();
            task = service
                .advance(&task.id, Cancellation::new())
                .await
                .unwrap();
            assert_eq!(task.stage, RecoveryStage::Completed);
            assert_eq!(lines(&dir.path, "executions.log"), 1);
            assert_eq!(lines(&dir.path, "verifications.log"), 1);
        }
        _ => panic!("unknown boundary"),
    }
    assert_eq!(task.operation.as_ref(), Some(&original_operation));
    service
        .summarize_pending(Cancellation::new())
        .await
        .unwrap();
    service
        .with_state(|state| {
            assert_eq!(
                state.session.approval_count(),
                1,
                "original operation has exactly one approval"
            );
            assert!(state.session.pending_experiences().is_empty());
            let knowledge = state.session.knowledge_snapshot();
            assert_eq!(knowledge.experiences.len(), 1);
            let ids = knowledge
                .experiences
                .iter()
                .map(|item| &item.id)
                .collect::<BTreeSet<_>>();
            assert_eq!(
                ids.len(),
                knowledge.experiences.len(),
                "delivery retry cannot duplicate an experience"
            );
            Ok(())
        })
        .unwrap();
    service.shutdown().await.unwrap();
    drop(service);
}

#[tokio::test]
async fn crash_after_atomic_start() {
    assert_boundary("start_committed").await;
}
#[tokio::test]
async fn crash_after_atomic_authorization() {
    assert_boundary("authorization_committed").await;
}
#[tokio::test]
async fn crash_after_atomic_receipt() {
    assert_boundary("receipt_committed").await;
}
#[tokio::test]
async fn crash_after_business_verification() {
    assert_boundary("verification_committed").await;
}
#[tokio::test]
async fn crash_after_atomic_experience_delivery() {
    assert_boundary("experience_committed").await;
}

async fn crash_existing(directory: &Path, boundary: &str) {
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "recovery::service::commit_tests::crash_probe",
            "--nocapture",
        ])
        .env("RECUVORA_RECOVERY_CRASH_BOUNDARY", boundary)
        .env("RECUVORA_RECOVERY_CRASH_DIRECTORY", directory)
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert_eq!(status.code(), Some(93));
            break;
        }
        if std::time::Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("crash probe timed out");
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn restart_preserves_atomic_not_dispatched_result() {
    let dir = TestDir::new("not-dispatched-final-gap");
    crash_existing(&dir.path, "authorization_committed").await;
    crash_existing(&dir.path, "not_dispatched_committed").await;
    let service = open(&dir.path);
    let task = service.tasks().unwrap().pop().unwrap();
    assert_eq!(task.stage, RecoveryStage::Canceled);
    assert_eq!(
        task.result_check.unwrap().execution.outcome,
        CheckedExecution::NotExecuted
    );
    assert_eq!(lines(&dir.path, "executions.log"), 0);
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn restart_preserves_atomic_independent_check_without_reexecuting() {
    let dir = TestDir::new("checked-final-gap");
    crash_existing(&dir.path, "result_check_committed").await;
    let service = open(&dir.path);
    let task = service.tasks().unwrap().pop().unwrap();
    assert_eq!(task.stage, RecoveryStage::Completed);
    assert_eq!(task.result_check.unwrap().execution.checked_at_ms, 20_000);
    assert_eq!(lines(&dir.path, "executions.log"), 1);
    service.deliver_pending().unwrap();
    service.shutdown().await.unwrap();
}

struct LaterClock;
impl RecoveryClock for LaterClock {
    fn now_ms(&self) -> u64 {
        60_001
    }
}
#[tokio::test]
async fn restart_preserves_committed_result_without_refreshing_evidence() {
    let dir = TestDir::new("checked-expired-gap");
    crash_existing(&dir.path, "result_check_committed").await;
    let service = RecoveryService::open_with_clock(
        dir.path.join("state"),
        config(),
        Arc::new(Backend {
            directory: dir.path.clone(),
        }),
        Arc::new(LaterClock),
    )
    .unwrap();
    service
        .bind_target_ownership(Arc::new(
            FileTargetOwnership::open(dir.path.join("ownership")).unwrap(),
        ))
        .unwrap();
    let task = service.tasks().unwrap().pop().unwrap();
    assert_eq!(task.stage, RecoveryStage::Completed);
    assert_eq!(task.result_check.unwrap().execution.checked_at_ms, 20_000);
    assert_eq!(task.verification.unwrap().verified_at_ms, 20_000);
    assert_eq!(lines(&dir.path, "executions.log"), 1);
    service.shutdown().await.unwrap();
}

struct RetryRelease {
    inner: Box<dyn TargetLease>,
    failed: bool,
}
impl TargetLease for RetryRelease {
    fn target(&self) -> &CanonicalTarget {
        self.inner.target()
    }
    fn recovery_directory(&self) -> &Path {
        self.inner.recovery_directory()
    }
    fn validate(&self) -> Result<(), RecoveryError> {
        self.inner.validate()
    }
    fn release(&mut self) -> Result<(), RecoveryError> {
        if !self.failed {
            self.failed = true;
            return Err(service("transient ownership release failure"));
        }
        self.inner.release()
    }
}
#[tokio::test]
async fn shutdown_retry_preserves_all_journals_until_ownership_release() {
    let dir = TestDir::new("shutdown-retry");
    let service = open(&dir.path);
    {
        let mut ownership = lock(&service.ownership).unwrap();
        let inner = ownership.take().unwrap();
        *ownership = Some(Box::new(RetryRelease {
            inner,
            failed: false,
        }));
    }
    assert!(service.shutdown().await.is_err());
    service
        .read_state(|state| {
            state.journal.available().map_err(super::service)?;
            state.dispatch.available().map_err(super::service)?;
            Ok(())
        })
        .unwrap();
    service.shutdown().await.unwrap();
    let competing = RecoveryService::open_with_clock(
        dir.path.join("other-state"),
        config(),
        Arc::new(Backend {
            directory: dir.path.clone(),
        }),
        Arc::new(Clock),
    )
    .unwrap();
    competing
        .bind_target_ownership(Arc::new(
            FileTargetOwnership::open(dir.path.join("ownership")).unwrap(),
        ))
        .unwrap();
    competing.shutdown().await.unwrap();
}

#[tokio::test]
async fn atomic_independent_check_restores_matching_approval() {
    let dir = TestDir::new("checked-before-reconcile-gap");
    crash_existing(&dir.path, "result_check_committed").await;
    let service = open(&dir.path);
    let task = service.tasks().unwrap().pop().unwrap();
    assert_eq!(task.stage, RecoveryStage::Completed);
    assert_eq!(
        service.approval(&task.id).unwrap().unwrap().state,
        ApprovalState::Executed
    );
    assert_eq!(task.result_check.unwrap().execution.checked_at_ms, 20_000);
    assert_eq!(lines(&dir.path, "executions.log"), 1);
    service.deliver_pending().unwrap();
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn lost_aggregate_authorization_receipt_never_releases_execution() {
    let dir = TestDir::new("aggregate-authorization-receipt");
    let service = open(&dir.path);
    let task = service.submit(problem()).unwrap();
    service
        .with_state(|state| {
            state.apply(
                SessionCommand::Start {
                    task_id: task.id.clone(),
                    revision: task.revision,
                    observation: TargetObservation {
                        target_id: config().target.target_id,
                        facts: config().target.required_facts,
                        evidence_refs: vec!["provider:inspection".into()],
                        observed_at_ms: 20_000,
                    },
                },
                20_000,
            )?;
            Ok(())
        })
        .unwrap();
    let approval = service.approval(&task.id).unwrap().unwrap();
    service
        .decide_human(
            &task.id,
            approval.revision,
            ApprovalDecision::Approve,
            "operator".into(),
            "scoped".into(),
        )
        .unwrap();
    service
        .with_state(|state| {
            state.journal.fail_after_commits(0, true);
            Ok(())
        })
        .unwrap();
    assert!(
        service
            .advance(&task.id, Cancellation::new())
            .await
            .is_err()
    );
    assert_eq!(
        service.query(&task.id).unwrap().unwrap().stage,
        RecoveryStage::AwaitingApproval
    );
    assert_eq!(
        service.approval(&task.id).unwrap().unwrap().state,
        ApprovalState::Approved
    );
    assert_eq!(lines(&dir.path, "executions.log"), 0);
    assert!(
        service
            .advance(&task.id, Cancellation::new())
            .await
            .is_err()
    );
    assert!(service.shutdown().await.is_err());
    drop(service);
    let restored = open(&dir.path);
    let task = restored.query(&task.id).unwrap().unwrap();
    assert_eq!(task.stage, RecoveryStage::Canceled);
    assert_eq!(
        restored.approval(&task.id).unwrap().unwrap().state,
        ApprovalState::Failed
    );
    assert_eq!(
        task.result_check.unwrap().execution.outcome,
        CheckedExecution::NotExecuted
    );
    assert_eq!(lines(&dir.path, "executions.log"), 0);
    restored.shutdown().await.unwrap();
}
