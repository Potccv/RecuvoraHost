#[path = "workflow_support.rs"]
mod support;
use recuvora_host::{
    integrations::recovery::*,
    persistence::{approval::*, incidents::IncidentStoreConfig, knowledge::*, legacy::*},
    runtime::operation::Cancellation,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use support::TestDir;

#[path = "legacy_bundle_fixture.rs"]
mod fixture;
use fixture::*;

#[derive(Default)]
struct NeverBackend(AtomicUsize);
impl NeverBackend {
    fn called<T>(&self) -> RecoveryFuture<'_, T> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { panic!("migration/open must not call an external backend") })
    }
}
impl RepairBackend for NeverBackend {
    fn inspect<'a>(
        &'a self,
        _: &'a TargetBinding,
        _: Cancellation,
    ) -> RecoveryFuture<'a, TargetObservation> {
        self.called()
    }
    fn diagnose(&self, _: DiagnosisInput, _: Cancellation) -> RecoveryFuture<'_, RepairPlan> {
        self.called()
    }
    fn review(&self, _: ReviewInput, _: Cancellation) -> RecoveryFuture<'_, ReviewOutput> {
        self.called()
    }
    fn execute<'a>(
        &'a self,
        _: AuthorizedScript<'a>,
        _: Cancellation,
    ) -> RecoveryFuture<'a, ScriptReceipt> {
        self.called()
    }
    fn verify(
        &self,
        _: VerificationInput,
        _: Cancellation,
    ) -> RecoveryFuture<'_, BusinessVerification> {
        self.called()
    }
}
struct Clock;
impl RecoveryClock for Clock {
    fn now_ms(&self) -> u64 {
        11_000
    }
}

#[tokio::test]
async fn complete_bundle_preserves_random_task_operation_and_stable_owner() {
    for (stage, format) in [("pending", 1), ("executing", 2), ("completed", 2)] {
        let fixture = Fixture::new(stage, format);
        let authority = Arc::new(fixture.authority());
        let old_claim = authority
            .acquire(&CanonicalTarget::new("target-a").unwrap(), &fixture.root)
            .unwrap();
        drop(old_claim);
        let original = std::fs::read(fixture.root.join("recovery.jsonl")).unwrap();
        let approvals = std::fs::read(fixture.root.join("approvals/approvals.jsonl")).unwrap();
        let report = fixture.import(authority.as_ref()).unwrap();
        assert_eq!(report.tasks, 1);
        assert_eq!(report.recovery_revisions, fixture.revisions.len());
        assert_eq!(
            std::fs::read(report.generation_directory.join("legacy-recovery.jsonl")).unwrap(),
            original
        );
        assert_eq!(
            std::fs::read(fixture.root.join("approvals/approvals.jsonl")).unwrap(),
            approvals
        );
        let marker: Value =
            serde_json::from_slice(&std::fs::read(fixture.root.join("recovery.jsonl")).unwrap())
                .unwrap();
        assert_eq!(marker["host_recovery_layout"], 1);
        let backend = Arc::new(NeverBackend::default());
        let service = RecoveryService::open_with_clock(
            &fixture.root,
            fixture.config.recovery.clone(),
            backend.clone(),
            Arc::new(Clock),
        )
        .unwrap();
        service.bind_target_ownership(authority.clone()).unwrap();
        let task = service.query(TASK).unwrap().unwrap();
        assert_eq!(task.operation.as_ref().unwrap().operation_id, OPERATION);
        assert_eq!(task.approval_id.as_deref(), Some(APPROVAL));
        assert_eq!(task.episode_count, 1);
        assert_eq!(task.diagnosis_attempts, 1);
        assert_eq!(
            task.stage,
            match stage {
                "pending" => RecoveryStage::Paused,
                "executing" => RecoveryStage::Unknown,
                _ => RecoveryStage::Completed,
            }
        );
        assert_eq!(backend.0.load(Ordering::SeqCst), 0);
        if stage == "pending" {
            let resumed = service.resume(TASK, task.revision).unwrap();
            assert_eq!(resumed.operation, task.operation);
            assert_eq!(resumed.approval_id, task.approval_id);
            assert_eq!(resumed.stage, RecoveryStage::AwaitingApproval);
        }
        if stage == "executing" {
            assert_eq!(
                service
                    .advance(TASK, Cancellation::new())
                    .await
                    .unwrap()
                    .stage,
                RecoveryStage::Unknown
            );
            assert_eq!(backend.0.load(Ordering::SeqCst), 0);
        }
        if stage == "completed" {
            let results = service
                .knowledge(&KnowledgeQuery {
                    conditions: BTreeMap::from([
                        ("workload_version".into(), "1".into()),
                        ("platform".into(), "portable".into()),
                        ("fault_fingerprint".into(), "not-ready".into()),
                    ]),
                    keywords: vec!["readiness".into()],
                    limit: 10,
                })
                .unwrap();
            assert_eq!(
                results[0].cases[0].result.id,
                format!("{OPERATION}-Verified")
            );
        }
        assert!(
            RecoveryService::open(
                &report.generation_directory,
                fixture.config.recovery.clone(),
                backend
            )
            .is_err(),
            "generation cannot become a new ownership root"
        );
        service.shutdown().await.unwrap();
        drop(service);
        drop(authority);
    }
}

#[tokio::test]
async fn imported_unknown_can_be_checked_and_published_without_reexecution() {
    let mut fixture = Fixture::new("completed", 2);
    fixture.revisions.truncate(6);
    write_lines(&fixture.root.join("recovery.jsonl"), &fixture.revisions);
    let approval_path = fixture.root.join("approvals/approvals.jsonl");
    let approvals = std::fs::read_to_string(&approval_path)
        .unwrap()
        .lines()
        .take(3)
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    write_lines(&approval_path, &approvals);
    let knowledge_path = fixture.root.join("knowledge.jsonl");
    let mut knowledge = std::fs::read_to_string(&knowledge_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    knowledge[1]["event"]["case"]["id"] = json!(format!("{OPERATION}-Unknown"));
    knowledge[1]["event"]["case"]["outcome"] = json!("unknown");
    knowledge[1]["event"]["verification"] = Value::Null;
    write_lines(&knowledge_path, &knowledge);
    let authority = Arc::new(fixture.authority());
    let report = fixture.import(authority.as_ref()).unwrap();
    let backend = Arc::new(NeverBackend::default());
    let service = RecoveryService::open_with_clock(
        &fixture.root,
        fixture.config.recovery.clone(),
        backend.clone(),
        Arc::new(Clock),
    )
    .unwrap();
    service.bind_target_ownership(authority.clone()).unwrap();
    let task = service.query(TASK).unwrap().unwrap();
    assert_eq!(task.stage, RecoveryStage::Unknown);
    let checked = service
        .check_result(
            TASK,
            task.revision,
            ExecutionResultCheck {
                operation_id: OPERATION.into(),
                target_id: "target-a".into(),
                executor_id: "node-a".into(),
                outcome: CheckedExecution::Executed,
                executor_stopped: true,
                evidence_refs: vec!["executor:independent-receipt".into()],
                checked_at_ms: 11_000,
            },
            BusinessVerification {
                operation_id: OPERATION.into(),
                target_id: "target-a".into(),
                profile: "readiness".into(),
                healthy: Some(true),
                executor_stopped: true,
                evidence_refs: vec!["provider:independent-check".into()],
                verified_at_ms: 11_000,
            },
            "operator".into(),
        )
        .unwrap();
    assert_eq!(checked.stage, RecoveryStage::Completed);
    service.deliver_pending().unwrap();
    service.deliver_pending().unwrap();
    assert_eq!(backend.0.load(Ordering::SeqCst), 0);
    service.shutdown().await.unwrap();
    drop(service);
    let mut knowledge = KnowledgeStore::open(
        report.generation_directory.join("knowledge.jsonl"),
        fixture.config.knowledge,
    )
    .unwrap();
    assert!(knowledge.is_quarantined("script-a", 1));
    let record = knowledge.get(&format!("case-{TASK}-1")).unwrap();
    assert_eq!(record.cases.len(), 2);
    assert!(
        record
            .cases
            .iter()
            .any(|case| case.result.id == format!("{OPERATION}-Unknown"))
    );
    assert!(
        record
            .cases
            .iter()
            .any(|case| case.result.outcome == RepairOutcome::Verified)
    );
    knowledge.close().unwrap();
}

#[tokio::test]
async fn unconsumed_legacy_execution_intent_is_sealed_unknown_without_consumption() {
    for stage in ["executing", "unknown"] {
        let mut fixture = Fixture::new("executing", 2);
        if stage == "unknown" {
            fixture.revisions.last_mut().unwrap()["task"]["stage"] = json!("unknown");
            write_lines(&fixture.root.join("recovery.jsonl"), &fixture.revisions);
        }
        let approval_path = fixture.root.join("approvals/approvals.jsonl");
        let entries = std::fs::read_to_string(&approval_path)
            .unwrap()
            .lines()
            .take(2)
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>();
        write_lines(&approval_path, &entries);
        let authority = Arc::new(fixture.authority());
        let report = fixture.import(authority.as_ref()).unwrap();
        let mut approvals = ApprovalStore::open(
            report.generation_directory.join("approvals"),
            fixture.config.approval.clone(),
            11,
        )
        .unwrap();
        let record = approvals.get(APPROVAL).unwrap();
        assert_eq!(record.state, ApprovalState::Unknown);
        approvals.close().unwrap();
        assert!(
            !std::fs::read_to_string(
                report
                    .generation_directory
                    .join("approvals/approvals.jsonl")
            )
            .unwrap()
            .contains("\"change\":\"consume\"")
        );
        let backend = Arc::new(NeverBackend::default());
        let service = RecoveryService::open_with_clock(
            &fixture.root,
            fixture.config.recovery.clone(),
            backend.clone(),
            Arc::new(Clock),
        )
        .unwrap();
        service.bind_target_ownership(authority).unwrap();
        assert_eq!(
            service
                .advance(TASK, Cancellation::new())
                .await
                .unwrap()
                .stage,
            RecoveryStage::Unknown
        );
        assert_eq!(backend.0.load(Ordering::SeqCst), 0);
        service.shutdown().await.unwrap();
    }
}

#[test]
fn invalid_bundle_keeps_sources_and_cleans_only_its_staging() {
    let mut fixture = Fixture::new("pending", 2);
    fixture.revisions.last_mut().unwrap()["task"]["operation"]["operation_id"] =
        json!("forged-operation");
    write_lines(&fixture.root.join("recovery.jsonl"), &fixture.revisions);
    let original = std::fs::read(fixture.root.join("recovery.jsonl")).unwrap();
    assert!(fixture.import(&fixture.authority()).is_err());
    assert_eq!(
        std::fs::read(fixture.root.join("recovery.jsonl")).unwrap(),
        original
    );
    assert!(!std::fs::read_dir(&fixture.root).unwrap().any(|item| {
        item.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".core02-")
    }));
}

#[test]
fn another_owner_cannot_import_into_a_new_root() {
    let fixture = Fixture::new("executing", 2);
    let authority = fixture.authority();
    let other = fixture.dir.path.join("other-root");
    std::fs::create_dir(&other).unwrap();
    let lease = authority
        .acquire(&CanonicalTarget::new("target-a").unwrap(), &other)
        .unwrap();
    drop(lease);
    let original = std::fs::read(fixture.root.join("recovery.jsonl")).unwrap();
    assert!(fixture.import(&authority).is_err());
    assert_eq!(
        std::fs::read(fixture.root.join("recovery.jsonl")).unwrap(),
        original
    );
}

#[test]
fn frozen_incident_identity_is_required_for_bundle() {
    let fixture = Fixture::new("pending", 2);
    std::fs::write(&fixture.incidents, b"").unwrap();
    let original = std::fs::read(fixture.root.join("recovery.jsonl")).unwrap();
    assert!(fixture.import(&fixture.authority()).is_err());
    assert_eq!(
        std::fs::read(fixture.root.join("recovery.jsonl")).unwrap(),
        original
    );
}

#[test]
fn active_source_writer_blocks_the_entire_switch() {
    use fs2::FileExt;
    let fixture = Fixture::new("pending", 2);
    let authority = fixture.authority();
    for path in [
        fixture.root.join("recovery.lock"),
        fixture.root.join("approvals/approvals.lock"),
        fixture.root.join("knowledge.jsonl"),
        fixture.incidents.clone(),
    ] {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .unwrap();
        file.try_lock_exclusive().unwrap();
        let original = std::fs::read(fixture.root.join("recovery.jsonl")).unwrap();
        assert!(fixture.import(&authority).is_err());
        assert_eq!(
            std::fs::read(fixture.root.join("recovery.jsonl")).unwrap(),
            original
        );
        drop(file);
    }
}
