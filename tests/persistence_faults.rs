use super::approval::*;
use crate::workflow_test_support::TestDir;
use serde_json::json;

#[test]
fn durable_consume_with_lost_receipt_blocks_writer_and_recovers_unknown() {
    let dir = TestDir::new("approval-lost-commit-receipt");
    let operation = ProposedOperation {
        task_id: "task-a".into(),
        task_revision: 1,
        operation_id: "operation-a".into(),
        target: "target-a".into(),
        action: json!({"kind":"external_action"}),
    };
    let policy = ApprovalPolicy {
        id: "policy-a".into(),
        version: 1,
        reviewer: ReviewerConfig::Human,
        delegation: "trusted operation".into(),
        allowed_targets: vec!["target-a".into()],
        allowed_action_kinds: vec!["external_action".into()],
        ttl_secs: 100,
    };
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
            "reviewed".into(),
            "operator".into(),
            &policy,
            11,
        )
        .unwrap();
    store.fail_after_commits(0, true);
    assert!(matches!(
        store.consume(&id, &operation, &policy, 12),
        Err(ApprovalError::Io(_))
    ));
    assert_eq!(
        store.get(&id).unwrap().state,
        ApprovalState::Approved,
        "uncertain commit must not install a speculative state"
    );
    assert!(matches!(
        store.ensure_current(),
        Err(ApprovalError::Unavailable)
    ));
    assert!(store.consume(&id, &operation, &policy, 12).is_err());
    drop(store);
    let mut restored = ApprovalStore::open(&dir.path, ApprovalStoreConfig::default(), 13).unwrap();
    assert_eq!(
        restored.get(&id).unwrap().state,
        ApprovalState::Unknown,
        "the durable consumed intent must survive receipt loss"
    );
    assert!(restored.consume(&id, &operation, &policy, 14).is_err());
}
