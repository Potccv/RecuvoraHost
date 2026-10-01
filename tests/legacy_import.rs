use recuvora_host::persistence::{
    approval::{ApprovalState, ApprovalStore, ApprovalStoreConfig},
    incidents::{IncidentStore, IncidentStoreConfig},
    knowledge::{KnowledgeStore, KnowledgeStoreConfig},
    legacy::{LegacyDomain, import_legacy_journal},
};
use serde_json::{Value, json};
use std::path::PathBuf;

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let root =
            PathBuf::from(std::env::var_os("RECUVORA_TEST_TEMP").expect("external test root"));
        assert!(root.is_absolute());
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        assert!(
            !root.starts_with(
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .canonicalize()
                    .unwrap()
            )
        );
        let path = root.join(format!(
            "host-legacy-{}",
            recuvora_host::protocol::call_id()
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn source(&self, entries: Vec<Value>) -> PathBuf {
        let path = self.0.join("old.jsonl");
        let bytes = entries
            .into_iter()
            .map(|value| format!("{value}\n"))
            .collect::<String>();
        std::fs::write(&path, bytes).unwrap();
        path
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
fn policy() -> Value {
    json!({"id":"policy","version":1,"reviewer":{"mode":"human"},"delegation":"replace bounded text","allowed_targets":["target"],"allowed_action_kinds":["text_edit"],"ttl_secs":100})
}
fn approval_entries() -> Vec<Value> {
    vec![
        json!({"format":1,"sequence":1,"now":1,"event":{"event":"requested","request":{"request_id":"approval-0000000000000001","operation":{"task_id":"old-random-task","task_revision":7,"operation_id":"original-operation","target":"target","action":{"kind":"text_edit","text":"content"}},"policy":policy(),"created_at":1,"expires_at":101}}}),
        json!({"format":1,"sequence":2,"now":2,"event":{"event":"changed","request_id":"approval-0000000000000001","change":{"change":"human_decision","assessment":{"decision":"approve","reason":"reviewed original operation","reviewer":{"source":"human","actor":"operator"}}}}}),
        json!({"format":1,"sequence":3,"now":3,"event":{"event":"changed","request_id":"approval-0000000000000001","change":{"change":"consume"}}}),
    ]
}
#[test]
fn consumed_approval_import_retains_identity_and_reopens_unknown_without_new_permit() {
    let dir = Directory::new();
    std::fs::write(dir.0.join("approvals.lock"), b"").unwrap();
    let source = dir.source(approval_entries());
    let original = std::fs::read(&source).unwrap();
    let destination = dir.0.join("approvals.jsonl");
    assert_eq!(
        import_legacy_journal(
            &source,
            &destination,
            LegacyDomain::Approval(ApprovalStoreConfig::default())
        )
        .unwrap(),
        3
    );
    assert_eq!(std::fs::read(&source).unwrap(), original);
    let mut store = ApprovalStore::open(&dir.0, ApprovalStoreConfig::default(), 4).unwrap();
    let record = store.get("approval-0000000000000001").unwrap().clone();
    assert_eq!(record.state, ApprovalState::Unknown);
    assert_eq!(record.request.operation.task_id, "old-random-task");
    assert_eq!(record.request.operation.operation_id, "original-operation");
    assert!(
        store
            .consume(
                "approval-0000000000000001",
                &record.request.operation,
                &record.request.policy,
                5
            )
            .is_err()
    );
    store.close().unwrap();
}
#[test]
fn completed_approval_import_keeps_historical_consumption() {
    let dir = Directory::new();
    std::fs::write(dir.0.join("approvals.lock"), b"").unwrap();
    let mut entries = approval_entries();
    entries.push(json!({"format":1,"sequence":4,"now":4,"event":{"event":"changed","request_id":"approval-0000000000000001","change":{"change":"complete","outcome":"executed","reason":"durable external receipt"}}}));
    let source = dir.source(entries);
    import_legacy_journal(
        &source,
        &dir.0.join("approvals.jsonl"),
        LegacyDomain::Approval(ApprovalStoreConfig::default()),
    )
    .unwrap();
    let mut store = ApprovalStore::open(&dir.0, ApprovalStoreConfig::default(), 5).unwrap();
    assert_eq!(
        store.get("approval-0000000000000001").unwrap().state,
        ApprovalState::Executed
    );
    store.close().unwrap();
}
#[test]
fn legacy_harness_assessment_without_review_attempt_preserves_source() {
    let dir = Directory::new();
    std::fs::write(dir.0.join("approvals.lock"), b"").unwrap();
    let mut entries = approval_entries();
    entries.truncate(2);
    entries[0]["event"]["request"]["policy"]["reviewer"] =
        json!({"mode":"harness","harness_id":"reviewer"});
    entries[1]["event"]["change"] = json!({
        "change":"assess",
        "assessment":{
            "decision":"approve",
            "reason":"old Harness assessment without a durable attempt",
            "reviewer":{"source":"harness","harness_id":"reviewer","session_id":"old-session"}
        }
    });
    let source = dir.source(entries.clone());
    let original = std::fs::read(&source).unwrap();
    let destination = dir.0.join("approvals.jsonl");
    import_legacy_journal(
        &source,
        &destination,
        LegacyDomain::Approval(ApprovalStoreConfig::default()),
    )
    .unwrap();
    assert_eq!(std::fs::read(&source).unwrap(), original);
    let mut store = ApprovalStore::open(&dir.0, ApprovalStoreConfig::default(), 3).unwrap();
    let record = store.get("approval-0000000000000001").unwrap();
    assert_eq!(record.state, ApprovalState::Approved);
    assert_eq!(record.request.operation.operation_id, "original-operation");
    assert_eq!(record.review_attempt, 0);
    store.close().unwrap();
    for invalid_kind in ["identity", "expired", "scope"] {
        let invalid_dir = Directory::new();
        std::fs::write(invalid_dir.0.join("approvals.lock"), b"").unwrap();
        let mut invalid = entries.clone();
        match invalid_kind {
            "identity" => {
                invalid[1]["event"]["change"]["assessment"]["reviewer"]["harness_id"] =
                    json!("other-reviewer")
            }
            "expired" => invalid[1]["now"] = json!(102),
            "scope" => {
                invalid[0]["event"]["request"]["policy"]["allowed_targets"] =
                    json!(["other-target"])
            }
            _ => unreachable!(),
        }
        let source = invalid_dir.source(invalid);
        let bytes = std::fs::read(&source).unwrap();
        let destination = invalid_dir.0.join("approvals.jsonl");
        assert!(
            import_legacy_journal(
                &source,
                &destination,
                LegacyDomain::Approval(ApprovalStoreConfig::default())
            )
            .is_err(),
            "{invalid_kind}"
        );
        assert_eq!(std::fs::read(&source).unwrap(), bytes);
        assert!(!destination.exists());
    }
}

#[test]
fn incident_import_keeps_atomic_checkpoint_and_episode() {
    let dir = Directory::new();
    let source=dir.source(vec![json!({"format":1,"sequence":1,"event":{"type":"monitor","commit":{"monitor_id":"monitor","sequence":1,"checkpoint":{"cursor":"original"},"signals":[{"monitor_id":"monitor","target_id":"target","rule_id":"rule","kind":"target","condition":"active","summary":"unhealthy","evidence":{"source":"sample"}}],"now_ms":1}}})]);
    let dest = dir.0.join("incidents.jsonl");
    import_legacy_journal(
        &source,
        &dest,
        LegacyDomain::Incidents(IncidentStoreConfig::default()),
    )
    .unwrap();
    let mut store = IncidentStore::open(&dest, IncidentStoreConfig::default()).unwrap();
    assert_eq!(
        store.checkpoint("monitor").unwrap().value,
        json!({"cursor":"original"})
    );
    assert_eq!(store.list().len(), 1);
    assert_eq!(store.list()[0].occurrences, 1);
    store.close().unwrap();
}
#[test]
fn knowledge_import_preserves_unknown_isolation_and_global_case_key() {
    let dir = Directory::new();
    let candidate = json!({"id":"candidate","incident_id":"episode","summary":"bounded repair","keywords":["ready"],"conditions":{"platform":"windows"},"script":{"id":"script","version":1,"language":"powershell","platform":"windows","source":"Write-Output ready","preconditions":{"platform":"windows"},"generated_by_harness":"harness","generated_in_session":"session"},"reusable":true,"evidence_refs":["observation:one"],"created_at_ms":1});
    let case = json!({"id":"case-original","operation_id":"operation-original","target_id":"target","script_id":"script","script_version":1,"outcome":"unknown","evidence_refs":["receipt:lost"],"recorded_at_ms":2});
    let source=dir.source(vec![json!({"format":1,"sequence":1,"event":{"type":"candidate","candidate":candidate}}),json!({"format":1,"sequence":2,"event":{"type":"outcome","record_id":"candidate","case":case,"verification":null}})]);
    let dest = dir.0.join("knowledge.jsonl");
    import_legacy_journal(
        &source,
        &dest,
        LegacyDomain::Knowledge(KnowledgeStoreConfig::default()),
    )
    .unwrap();
    let mut store = KnowledgeStore::open(&dest, KnowledgeStoreConfig::default()).unwrap();
    assert!(store.is_quarantined("script", 1));
    assert_eq!(
        store.get("candidate").unwrap().cases[0].result.id,
        "case-original"
    );
    store.close().unwrap();
}
#[test]
fn invalid_history_and_switch_conflicts_preserve_source_and_destination() {
    let dir = Directory::new();
    std::fs::write(dir.0.join("approvals.lock"), b"").unwrap();
    let mut entries = approval_entries();
    entries[2]["event"]["request_id"] = json!("missing");
    let source = dir.source(entries);
    let original = std::fs::read(&source).unwrap();
    let dest = dir.0.join("destination.jsonl");
    assert!(
        import_legacy_journal(
            &source,
            &dest,
            LegacyDomain::Approval(ApprovalStoreConfig::default())
        )
        .is_err()
    );
    assert!(!dest.exists());
    assert_eq!(std::fs::read(&source).unwrap(), original);
    std::fs::write(&dest, b"existing destination").unwrap();
    assert!(
        import_legacy_journal(
            &source,
            &dest,
            LegacyDomain::Approval(ApprovalStoreConfig::default())
        )
        .is_err()
    );
    assert_eq!(std::fs::read(&dest).unwrap(), b"existing destination");
    assert!(!std::fs::read_dir(&dir.0).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".legacy-import")
    }));
}
#[test]
fn truncated_or_unexpressible_legacy_history_is_rejected_without_installation() {
    let dir = Directory::new();
    let source = dir.source(vec![
        json!({"format":1,"sequence":1,"config":{},"task":{"id":"old-random-task"}}),
    ]);
    let dest = dir.0.join("destination.jsonl");
    let original = std::fs::read(&source).unwrap();
    assert!(
        import_legacy_journal(
            &source,
            &dest,
            LegacyDomain::Knowledge(KnowledgeStoreConfig::default())
        )
        .is_err()
    );
    assert!(!dest.exists());
    assert_eq!(std::fs::read(&source).unwrap(), original);
    std::fs::write(&source, b"{\"format\":1").unwrap();
    assert!(
        import_legacy_journal(
            &source,
            &dest,
            LegacyDomain::Incidents(IncidentStoreConfig::default())
        )
        .is_err()
    );
    assert!(!dest.exists());
}
