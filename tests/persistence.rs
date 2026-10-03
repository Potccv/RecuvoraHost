#[path = "workflow_support.rs"]
mod support;
use recuvora_core::operation::CommitRequest;
use recuvora_host::persistence::{
    approval::*,
    incidents::*,
    journal::{Journal, JournalError},
    knowledge::*,
};
use serde_json::json;
use support::TestDir;

#[test]
fn journal_cas_commit_identity_and_configuration_are_bound() {
    let dir = TestDir::new("journal-cas");
    let path = dir.path.join("state.jsonl");
    let config = json!({"limit": 10});
    let mut journal = Journal::open(&path, "fixture", config.clone(), 1_000_000).unwrap();
    let request =
        CommitRequest::new("commit-a".into(), 0, "fixture".into(), json!({"event":1})).unwrap();
    journal.commit(&request, json!({"result":1})).unwrap();
    assert!(
        matches!(
            journal.commit(&request, json!({"result":1})),
            Err(JournalError::Conflict(_))
        ),
        "an already committed intent cannot release a second receipt"
    );
    assert!(matches!(
        journal.commit(&request, json!({"result":2})),
        Err(JournalError::Conflict(_))
    ));
    let stale =
        CommitRequest::new("commit-b".into(), 0, "fixture".into(), json!({"event":2})).unwrap();
    assert!(matches!(
        journal.commit(&stale, json!(2)),
        Err(JournalError::Conflict(_))
    ));
    assert!(
        Journal::open(&path, "fixture", config.clone(), 1_000_000).is_err(),
        "second writer must be excluded"
    );
    drop(journal);
    assert!(Journal::open(&path, "fixture", json!({"limit":11}), 1_000_000).is_err());
    let restored = Journal::open(&path, "fixture", config, 1_000_000).unwrap();
    assert_eq!(restored.records().len(), 1);
    assert_eq!(restored.records()[0].request, request);
}

#[test]
fn partial_transaction_is_preserved_and_blocks_reopen() {
    use std::io::Write;
    let dir = TestDir::new("journal-torn");
    let path = dir.path.join("state.jsonl");
    drop(Journal::open(&path, "fixture", json!({}), 1_000_000).unwrap());
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"{\"request\":")
        .unwrap();
    let original = std::fs::read(&path).unwrap();
    assert!(matches!(
        Journal::open(&path, "fixture", json!({}), 1_000_000),
        Err(JournalError::Corrupt(_))
    ));
    assert_eq!(std::fs::read(path).unwrap(), original);
}

#[test]
fn journal_capacity_rejection_does_not_publish_or_append() {
    let dir = TestDir::new("journal-capacity");
    let path = dir.path.join("state.jsonl");
    let mut journal = Journal::open(&path, "fixture", json!({}), 512).unwrap();
    let before = journal.bytes();
    let request = CommitRequest::new(
        "commit-a".into(),
        0,
        "fixture".into(),
        json!({"large":"x".repeat(1024)}),
    )
    .unwrap();
    assert!(matches!(
        journal.commit(&request, json!(1)),
        Err(JournalError::Capacity)
    ));
    assert_eq!(journal.bytes(), before);
    assert_eq!(journal.revision(), 0);
    drop(journal);
    assert!(
        Journal::open(path, "fixture", json!({}), 512)
            .unwrap()
            .records()
            .is_empty()
    );
}

#[test]
fn journal_process_lock_probe() {
    let Some(path) = std::env::var_os("RECUVORA_JOURNAL_LOCK_PROBE") else {
        return;
    };
    assert!(
        Journal::open(
            std::path::PathBuf::from(path),
            "fixture",
            json!({}),
            1_000_000
        )
        .is_err()
    );
}

#[test]
fn journal_excludes_an_independent_process() {
    let dir = TestDir::new("journal-process-lock");
    let path = dir.path.join("state.jsonl");
    let journal = Journal::open(&path, "fixture", json!({}), 1_000_000).unwrap();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "journal_process_lock_probe"])
        .env("RECUVORA_JOURNAL_LOCK_PROBE", &path)
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        if std::time::Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("lock probe timed out");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    drop(journal);
    assert!(Journal::open(path, "fixture", json!({}), 1_000_000).is_ok());
}

#[test]
fn sources_are_rejected_before_creating_journal_data() {
    for source in [env!("CARGO_MANIFEST_DIR"), env!("RECUVORA_CORE_SOURCE_DIR")] {
        let path = std::path::Path::new(source)
            .join("forbidden-persistence-test")
            .join("journal.jsonl");
        assert!(Journal::open(&path, "fixture", json!({}), 512).is_err());
        assert!(!path.parent().unwrap().exists());
    }
}

fn approval_input() -> (ProposedOperation, ApprovalPolicy) {
    (
        ProposedOperation {
            task_id: "task-a".into(),
            task_revision: 1,
            operation_id: "operation-a".into(),
            target: "target-a".into(),
            action: json!({"kind":"external_action","script":"bounded"}),
        },
        ApprovalPolicy {
            id: "policy-a".into(),
            version: 1,
            reviewer: ReviewerConfig::Human,
            delegation: "trusted human".into(),
            allowed_targets: vec!["target-a".into()],
            allowed_action_kinds: vec!["external_action".into()],
            ttl_secs: 3600,
        },
    )
}

#[test]
fn consumed_approval_restores_unknown_without_another_permit() {
    let dir = TestDir::new("approval-restart");
    let (operation, policy) = approval_input();
    let mut store = ApprovalStore::open(&dir.path, ApprovalStoreConfig::default(), 10).unwrap();
    let record = store
        .request(operation.clone(), policy.clone(), 10)
        .unwrap();
    let id = record.request.request_id;
    store
        .decide_human(
            &id,
            ApprovalDecision::Approve,
            "accepted".into(),
            "operator".into(),
            &policy,
            11,
        )
        .unwrap();
    let permit = store.consume(&id, &operation, &policy, 12).unwrap();
    drop(permit);
    drop(store);
    let mut restored = ApprovalStore::open(&dir.path, ApprovalStoreConfig::default(), 13).unwrap();
    assert_eq!(restored.get(&id).unwrap().state, ApprovalState::Unknown);
    assert!(restored.consume(&id, &operation, &policy, 14).is_err());
    assert_eq!(
        restored
            .request(operation, policy, 14)
            .unwrap()
            .request
            .request_id,
        id
    );
}

#[test]
fn approval_completion_uses_the_original_noncloneable_permit() {
    let dir = TestDir::new("approval-complete");
    let (operation, policy) = approval_input();
    let mut store = ApprovalStore::open(&dir.path, ApprovalStoreConfig::default(), 10).unwrap();
    let id = store
        .request(operation.clone(), policy.clone(), 10)
        .unwrap()
        .request
        .request_id;
    store
        .decide_human(
            &id,
            ApprovalDecision::Approve,
            "accepted".into(),
            "operator".into(),
            &policy,
            11,
        )
        .unwrap();
    let permit = store.consume(&id, &operation, &policy, 12).unwrap();
    store
        .complete(permit, ExecutionOutcome::Executed, "receipt".into(), 13)
        .unwrap();
    drop(store);
    let restored = ApprovalStore::open(&dir.path, ApprovalStoreConfig::default(), 14).unwrap();
    assert_eq!(restored.get(&id).unwrap().state, ApprovalState::Executed);
}

#[test]
fn incident_observation_and_checkpoint_restore_atomically() {
    let dir = TestDir::new("incident-atomic");
    let path = dir.path.join("incidents.jsonl");
    let mut store = IncidentStore::open(&path, IncidentStoreConfig::default()).unwrap();
    let commit = MonitorCommit {
        monitor_id: "monitor-a".into(),
        sequence: 1,
        checkpoint: json!({"cursor":1}),
        signals: vec![IncidentSignal {
            monitor_id: "monitor-a".into(),
            target_id: "target-a".into(),
            rule_id: "rule-a".into(),
            kind: IncidentKind::Target,
            condition: SignalCondition::Active,
            summary: "failure observed".into(),
            evidence: json!({"source":"fixture"}),
        }],
        now_ms: 10,
    };
    store.commit(commit.clone()).unwrap();
    let revision = store.state().revision();
    store.commit(commit).unwrap();
    assert_eq!(store.state().revision(), revision);
    let records = store.list();
    drop(store);
    let restored = IncidentStore::open(path, IncidentStoreConfig::default()).unwrap();
    assert_eq!(restored.list(), records);
    assert_eq!(
        restored.checkpoint("monitor-a").unwrap().value,
        json!({"cursor":1})
    );
}

fn experience() -> RepairExperience {
    RepairExperience {
        id: "experience-a".into(),
        operation_id: "operation-a".into(),
        target_id: "target-a".into(),
        conditions: std::collections::BTreeMap::from([("workload".into(), "a".into())]),
        keywords: vec!["repair".into()],
        outcome: RepairOutcome::Unknown,
        evidence_refs: vec!["receipt-a".into()],
        recorded_at_ms: 11,
        actions: vec![RepairArtifact {
            id: "action-a".into(),
            version: 1,
            kind: "execute_script".into(),
            payload: json!({"language":"python", "source":"bounded action"}),
            preconditions: std::collections::BTreeMap::from([("workload".into(), "a".into())]),
            generated_by_harness: "harness-a".into(),
            generated_in_session: "operation-a".into(),
        }],
        report: ExperienceReport {
            summary: "uncertain action".into(),
            lessons: "reconcile before another action".into(),
            related_experience_ids: vec![],
            scriptability: Scriptability::Undetermined {
                reason: "unknown outcome".into(),
            },
        },
    }
}

#[test]
fn knowledge_replay_preserves_unknown_quarantine_and_experience_idempotency() {
    let dir = TestDir::new("knowledge-restart");
    let path = dir.path.join("knowledge.jsonl");
    let mut store = KnowledgeStore::open(&path, KnowledgeStoreConfig::default()).unwrap();
    store.record_experience(experience()).unwrap();
    drop(store);
    let mut restored = KnowledgeStore::open(path, KnowledgeStoreConfig::default()).unwrap();
    assert!(restored.is_quarantined("action-a", 1));
    restored.record_experience(experience()).unwrap();
    assert_eq!(restored.snapshot().experiences.len(), 1);
    assert_eq!(
        restored.get("experience-a").unwrap().outcome,
        RepairOutcome::Unknown
    );
    let mut conflicting = experience();
    conflicting.report.summary = "changed historical evidence".into();
    assert!(restored.record_experience(conflicting).is_err());
}

#[test]
fn verified_experience_replays_without_erasing_prior_unknown() {
    let dir = TestDir::new("knowledge-verification");
    let path = dir.path.join("knowledge.jsonl");
    let mut store = KnowledgeStore::open(&path, KnowledgeStoreConfig::default()).unwrap();
    store.record_experience(experience()).unwrap();
    let mut verified = experience();
    verified.id = "experience-verified".into();
    verified.outcome = RepairOutcome::Verified;
    verified.recorded_at_ms = 12;
    verified.evidence_refs = vec!["independent:business-evidence".into()];
    store.record_experience(verified).unwrap();
    let expected = store.snapshot();
    drop(store);
    let restored = KnowledgeStore::open(path, KnowledgeStoreConfig::default()).unwrap();
    assert_eq!(restored.snapshot(), expected);
    assert!(restored.is_quarantined("action-a", 1));
    assert_eq!(
        restored
            .search_experiences(&KnowledgeQuery {
                conditions: experience().conditions,
                keywords: vec![],
                limit: 10,
            })
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn event_payload_tampering_is_rejected_without_rewriting_history() {
    let dir = TestDir::new("approval-tamper");
    let path = dir.path.join("approvals.jsonl");
    let (operation, policy) = approval_input();
    let mut store = ApprovalStore::open(&dir.path, ApprovalStoreConfig::default(), 10).unwrap();
    store.request(operation, policy, 10).unwrap();
    drop(store);
    let original = std::fs::read_to_string(&path).unwrap();
    let mut lines = original
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    lines[1]["payload"]["event"]["request"]["operation"]["action"]["script"] =
        json!("changed action");
    let mutated = lines
        .iter()
        .map(|line| format!("{}\n", serde_json::to_string(line).unwrap()))
        .collect::<String>();
    std::fs::write(&path, &mutated).unwrap();
    assert!(matches!(
        ApprovalStore::open(&dir.path, ApprovalStoreConfig::default(), 11),
        Err(ApprovalError::Corrupt(_))
    ));
    assert_eq!(std::fs::read_to_string(path).unwrap(), mutated);
}

#[test]
fn capacity_expansion_replays_from_original_configuration_and_keeps_quarantine() {
    let dir = TestDir::new("knowledge-expand");
    let path = dir.path.join("knowledge.jsonl");
    let original = KnowledgeStoreConfig {
        max_records: 1,
        ..KnowledgeStoreConfig::default()
    };
    let mut store = KnowledgeStore::open(&path, original.clone()).unwrap();
    store.record_experience(experience()).unwrap();
    let before = store.snapshot();
    let expanded = KnowledgeConfig { max_records: 2 };
    store
        .expand_capacity(before.config.clone(), expanded.clone())
        .unwrap();
    assert_eq!(store.snapshot().experiences, before.experiences);
    assert!(
        store
            .expand_capacity(before.config.clone(), expanded.clone())
            .is_err(),
        "stale configuration is not another expansion"
    );
    assert!(store.is_quarantined("action-a", 1));
    let mut another = experience();
    another.id = "experience-b".into();
    store.record_experience(another).unwrap();
    let expected = store.snapshot();
    drop(store);
    let changed_header = KnowledgeStoreConfig {
        max_records: 2,
        ..original.clone()
    };
    assert!(
        KnowledgeStore::open(&path, changed_header).is_err(),
        "new limits cannot replace trusted initial configuration"
    );
    let restored = KnowledgeStore::open(path, original).unwrap();
    assert_eq!(restored.snapshot(), expected);
    assert_eq!(restored.snapshot().config, expanded);
    assert!(restored.is_quarantined("action-a", 1));
}
