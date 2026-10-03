use recuvora_host::control::operation::{CommitError, CommitReceipt, CommitRequest, Prepared};
use serde_json::json;

fn prepared(domain: &str, input: serde_json::Value) -> Prepared<&'static str, &'static str> {
    Prepared::new_bound(
        "transaction-a".into(),
        0,
        domain.into(),
        input,
        "next-state",
        vec!["effect"],
    )
    .unwrap()
}

#[test]
fn receipts_bind_exact_domain_and_input_as_well_as_revision() {
    let original = prepared("approval", json!({"action":"first"}));
    let receipt = CommitReceipt::confirmed(original.request());
    let different = prepared("approval", json!({"action":"second"}));
    assert_eq!(
        different.confirm(receipt).unwrap_err(),
        CommitError::ReceiptMismatch
    );
    let original = prepared("approval", json!({"action":"first"}));
    let receipt = CommitReceipt::confirmed(original.request());
    let different = prepared("recovery", json!({"action":"first"}));
    assert_eq!(
        different.confirm(receipt).unwrap_err(),
        CommitError::ReceiptMismatch
    );
    let original = prepared("approval", json!({"action":"first"}));
    let mut other = original.request().clone();
    other.revision += 1;
    assert_eq!(
        original
            .confirm(CommitReceipt::confirmed(&other))
            .unwrap_err(),
        CommitError::ReceiptMismatch
    );
}

#[test]
fn matching_receipt_releases_owned_effects_after_confirmation() {
    let pending = prepared("recovery", json!({"event":"execute"}));
    let receipt = CommitReceipt::confirmed(pending.request());
    let committed = pending.confirm(receipt).unwrap();
    assert_eq!(committed.state, "next-state");
    assert_eq!(committed.effects, vec!["effect"]);
}

#[test]
fn commit_identity_and_revision_are_bounded() {
    assert!(matches!(
        CommitRequest::new("".into(), 0, "recovery".into(), json!(null)),
        Err(CommitError::InvalidIdentity)
    ));
    assert!(matches!(
        CommitRequest::new("id".into(), 0, "".into(), json!(null)),
        Err(CommitError::InvalidIdentity)
    ));
    assert!(matches!(
        CommitRequest::new("id".into(), u64::MAX, "recovery".into(), json!(null)),
        Err(CommitError::RevisionExhausted)
    ));
}
