use recuvora_host::control::operation::{CommitReceipt, Prepared};
use recuvora_host::control::recovery::approval::*;
use serde_json::json;

fn policy() -> ApprovalPolicy {
    ApprovalPolicy {
        id: "policy".into(),
        version: 1,
        reviewer: ReviewerConfig::Harness {
            harness_id: "reviewer".into(),
        },
        delegation: "Review bounded repair".into(),
        allowed_targets: vec!["target".into()],
        allowed_action_kinds: vec!["repair".into()],
        ttl_secs: 100,
    }
}
fn operation(id: &str) -> ProposedOperation {
    ProposedOperation {
        task_id: "task".into(),
        task_revision: 1,
        operation_id: id.into(),
        target: "target".into(),
        action: json!({"kind":"repair","parameters":{"expected":"original"}}),
    }
}
fn human(decision: ApprovalDecision) -> ApprovalAssessment {
    ApprovalAssessment {
        decision,
        reason: "reviewed".into(),
        reviewer: AssessmentSource::Human {
            actor: "operator".into(),
        },
    }
}
fn harness(decision: ApprovalDecision) -> ApprovalAssessment {
    ApprovalAssessment {
        decision,
        reason: "reviewed".into(),
        reviewer: AssessmentSource::Harness {
            harness_id: "reviewer".into(),
            session_id: "session".into(),
        },
    }
}
fn install(p: Prepared<ApprovalLedger, ApprovalEffect>) -> (ApprovalLedger, Vec<ApprovalEffect>) {
    let receipt = CommitReceipt::confirmed(p.request());
    let committed = p.confirm(receipt).unwrap();
    (committed.state, committed.effects)
}
fn changed(
    ledger: &ApprovalLedger,
    id: &str,
    change: ApprovalChange,
    policy: &ApprovalPolicy,
    now: u64,
) -> Result<Prepared<ApprovalLedger, ApprovalEffect>, ApprovalError> {
    ledger.prepare(
        format!("change-{}", ledger.revision()),
        ApprovalEvent::Changed {
            request_id: id.into(),
            change,
        },
        Some(policy),
        now,
    )
}
fn requested(policy: &ApprovalPolicy, operation: ProposedOperation) -> (ApprovalLedger, String) {
    let ledger = ApprovalLedger::new(ApprovalLimits::default()).unwrap();
    let (ledger, effects) = install(
        ledger
            .prepare_request("request".into(), operation, policy.clone(), 10)
            .unwrap(),
    );
    assert!(effects.is_empty());
    let id = ledger.list()[0].request.request_id.clone();
    (ledger, id)
}
fn begin(
    ledger: ApprovalLedger,
    id: &str,
    policy: &ApprovalPolicy,
    now: u64,
) -> (ApprovalLedger, ReviewAttempt) {
    let revision = ledger.get(id).unwrap().revision;
    let (ledger, mut effects) = install(
        changed(
            &ledger,
            id,
            ApprovalChange::BeginReview {
                expected_revision: revision,
                timeout_secs: 10,
            },
            policy,
            now,
        )
        .unwrap(),
    );
    let ApprovalEffect::Review(attempt) = effects.remove(0) else {
        panic!("review effect")
    };
    (ledger, attempt)
}
fn approved() -> (ApprovalLedger, String) {
    let policy = policy();
    let (ledger, id) = requested(&policy, operation("op"));
    let (ledger, attempt) = begin(ledger, &id, &policy, 11);
    let (ledger, _) = install(
        changed(
            &ledger,
            &id,
            ApprovalChange::AssessAttempt {
                attempt,
                assessment: harness(ApprovalDecision::Approve),
            },
            &policy,
            11,
        )
        .unwrap(),
    );
    (ledger, id)
}

#[test]
fn preparation_is_atomic_and_wrong_receipt_releases_nothing() {
    let ledger = ApprovalLedger::new(ApprovalLimits::default()).unwrap();
    let proposal = ledger
        .prepare_request("first".into(), operation("op"), policy(), 10)
        .unwrap();
    assert!(ledger.list().is_empty());
    assert_eq!(proposal.state().list().len(), 1);
    let other = ledger
        .prepare_request("other".into(), operation("other"), policy(), 10)
        .unwrap();
    let wrong = CommitReceipt::confirmed(other.request());
    assert!(proposal.confirm(wrong).is_err());
    assert!(ledger.list().is_empty());
}

#[test]
fn one_use_permit_is_commit_gated_bound_and_not_replayed() {
    let (ledger, id) = approved();
    let proposal = changed(&ledger, &id, ApprovalChange::Consume, &policy(), 12).unwrap();
    assert_eq!(ledger.get(&id).unwrap().state, ApprovalState::Approved);
    let (executing, mut effects) = install(proposal);
    let ApprovalEffect::Execute(permit) = effects.remove(0) else {
        panic!("execution effect")
    };
    assert_eq!(permit.operation(), &operation("op"));
    assert_eq!(permit.request_id(), id);
    assert_eq!(permit.revision(), executing.get(&id).unwrap().revision);
    assert!(changed(&executing, &id, ApprovalChange::Consume, &policy(), 13).is_err());
    let restored = ApprovalLedger::restore(ApprovalLimits::default(), executing.entries()).unwrap();
    assert_eq!(restored.get(&id).unwrap().state, ApprovalState::Executing);
    let (completed, effects) = install(
        executing
            .prepare_complete(
                "complete".into(),
                permit,
                ExecutionOutcome::Executed,
                "verified receipt".into(),
                13,
            )
            .unwrap(),
    );
    assert!(effects.is_empty());
    assert_eq!(completed.get(&id).unwrap().state, ApprovalState::Executed);
    assert_eq!(
        ApprovalLedger::restore(ApprovalLimits::default(), completed.entries())
            .unwrap()
            .list(),
        completed.list()
    );
}

#[test]
fn hard_policy_blocks_model_and_human_even_for_valid_identity() {
    for assessment in [
        human(ApprovalDecision::Approve),
        harness(ApprovalDecision::Approve),
    ] {
        let mut op = operation("op");
        op.target = "other".into();
        let (ledger, id) = requested(&policy(), op);
        let (ledger, attempt) = if matches!(assessment.reviewer, AssessmentSource::Harness { .. }) {
            let (ledger, attempt) = begin(ledger, &id, &policy(), 11);
            (ledger, Some(attempt))
        } else {
            (ledger, None)
        };
        let change = if matches!(assessment.reviewer, AssessmentSource::Human { .. }) {
            ApprovalChange::HumanDecision {
                expected_revision: 0,
                assessment,
            }
        } else {
            ApprovalChange::AssessAttempt {
                attempt: attempt.unwrap(),
                assessment,
            }
        };
        assert!(matches!(
            changed(&ledger, &id, change, &policy(), 11),
            Err(ApprovalError::OutOfScope)
        ));
        assert_eq!(ledger.get(&id).unwrap().state, ApprovalState::Pending);
    }
}

#[test]
fn current_policy_is_required_and_expiry_is_exact() {
    let (ledger, id) = approved();
    let event = ApprovalEvent::Changed {
        request_id: id.clone(),
        change: ApprovalChange::Consume,
    };
    assert!(ledger.prepare("missing".into(), event, None, 12).is_err());
    let mut current = policy();
    current.version += 1;
    assert!(matches!(
        changed(&ledger, &id, ApprovalChange::Consume, &current, 12),
        Err(ApprovalError::Conflict)
    ));
    assert!(matches!(
        changed(&ledger, &id, ApprovalChange::Consume, &policy(), 110),
        Err(ApprovalError::Expired)
    ));
    let (expired, _) =
        install(changed(&ledger, &id, ApprovalChange::Expire, &policy(), 110).unwrap());
    assert_eq!(expired.get(&id).unwrap().state, ApprovalState::Expired);
}

#[test]
fn human_denial_is_terminal_and_stale_revision_is_rejected() {
    let (ledger, id) = requested(&policy(), operation("op"));
    assert!(matches!(
        changed(
            &ledger,
            &id,
            ApprovalChange::HumanDecision {
                expected_revision: 1,
                assessment: human(ApprovalDecision::Approve)
            },
            &policy(),
            11
        ),
        Err(ApprovalError::Conflict)
    ));
    let (denied, _) = install(
        changed(
            &ledger,
            &id,
            ApprovalChange::HumanDecision {
                expected_revision: 0,
                assessment: human(ApprovalDecision::Deny),
            },
            &policy(),
            11,
        )
        .unwrap(),
    );
    for change in [
        ApprovalChange::BeginReview {
            expected_revision: 1,
            timeout_secs: 5,
        },
        ApprovalChange::Consume,
    ] {
        assert!(changed(&denied, &id, change, &policy(), 12).is_err());
    }
}

fn fallback_policy() -> ApprovalPolicy {
    ApprovalPolicy {
        reviewer: ReviewerConfig::HumanThenHarness {
            harness_id: "reviewer".into(),
            human_wait_secs: 10,
            review_timeout_secs: 20,
        },
        ..policy()
    }
}
#[test]
fn bounded_review_handoff_honors_human_deadline_and_late_results() {
    let policy = fallback_policy();
    let (ledger, id) = requested(&policy, operation("op"));
    assert!(matches!(
        changed(
            &ledger,
            &id,
            ApprovalChange::BeginReview {
                expected_revision: 0,
                timeout_secs: 20
            },
            &policy,
            19
        ),
        Err(ApprovalError::ReviewNotDue)
    ));
    let (reviewing, mut effects) = install(
        changed(
            &ledger,
            &id,
            ApprovalChange::BeginReview {
                expected_revision: 0,
                timeout_secs: 20,
            },
            &policy,
            20,
        )
        .unwrap(),
    );
    let ApprovalEffect::Review(attempt) = effects.remove(0) else {
        panic!("review effect")
    };
    assert_eq!(attempt.deadline, 40);
    assert_eq!(reviewing.get(&id).unwrap().request.expires_at, 110);
    assert!(matches!(
        changed(
            &reviewing,
            &id,
            ApprovalChange::AssessAttempt {
                attempt: attempt.clone(),
                assessment: harness(ApprovalDecision::Approve)
            },
            &policy,
            40
        ),
        Err(ApprovalError::ReviewTimedOut)
    ));
    let (waiting, _) = install(
        changed(
            &reviewing,
            &id,
            ApprovalChange::FailReview {
                attempt: attempt.clone(),
                reason: "deadline elapsed".into(),
            },
            &policy,
            40,
        )
        .unwrap(),
    );
    assert_eq!(
        waiting.get(&id).unwrap().review_stage,
        ReviewStage::NeedsHuman
    );
    assert!(
        changed(
            &waiting,
            &id,
            ApprovalChange::BeginReview {
                expected_revision: 2,
                timeout_secs: 20
            },
            &policy,
            41
        )
        .is_err()
    );
    assert!(
        changed(
            &waiting,
            &id,
            ApprovalChange::AssessAttempt {
                attempt,
                assessment: harness(ApprovalDecision::Approve)
            },
            &policy,
            41
        )
        .is_err()
    );
}

#[test]
fn tracked_review_validates_identity_and_human_takeover_invalidates_callback() {
    let policy = policy();
    let (ledger, id) = requested(&policy, operation("op"));
    let (reviewing, mut effects) = install(
        changed(
            &ledger,
            &id,
            ApprovalChange::BeginReview {
                expected_revision: 0,
                timeout_secs: 20,
            },
            &policy,
            11,
        )
        .unwrap(),
    );
    let ApprovalEffect::Review(attempt) = effects.remove(0) else {
        panic!("review effect")
    };
    let mut wrong = harness(ApprovalDecision::Approve);
    wrong.reviewer = AssessmentSource::Harness {
        harness_id: "wrong".into(),
        session_id: "session".into(),
    };
    assert!(
        changed(
            &reviewing,
            &id,
            ApprovalChange::AssessAttempt {
                attempt: attempt.clone(),
                assessment: wrong
            },
            &policy,
            12
        )
        .is_err()
    );
    let restored = ApprovalLedger::restore(ApprovalLimits::default(), reviewing.entries()).unwrap();
    assert_eq!(
        restored.get(&id).unwrap().active_review_attempt(),
        Some(attempt.clone())
    );
    assert!(
        changed(
            &restored,
            &id,
            ApprovalChange::BeginReview {
                expected_revision: 1,
                timeout_secs: 10
            },
            &policy,
            12
        )
        .is_err()
    );
    let (denied, _) = install(
        changed(
            &reviewing,
            &id,
            ApprovalChange::HumanDecision {
                expected_revision: 1,
                assessment: human(ApprovalDecision::Deny),
            },
            &policy,
            12,
        )
        .unwrap(),
    );
    assert!(
        changed(
            &denied,
            &id,
            ApprovalChange::AssessAttempt {
                attempt,
                assessment: harness(ApprovalDecision::Approve)
            },
            &policy,
            13
        )
        .is_err()
    );
}

#[test]
fn uncertain_execution_blocks_target_until_explicit_reconciliation() {
    let (ledger, id) = approved();
    let (executing, mut effects) =
        install(changed(&ledger, &id, ApprovalChange::Consume, &policy(), 12).unwrap());
    let ApprovalEffect::Execute(permit) = effects.remove(0) else {
        panic!("execution effect")
    };
    let (unknown, _) = install(
        changed(
            &executing,
            &id,
            ApprovalChange::Cancel {
                reason: "cancel is not stop proof".into(),
            },
            &policy(),
            13,
        )
        .unwrap(),
    );
    assert_eq!(unknown.get(&id).unwrap().state, ApprovalState::Unknown);
    assert!(
        unknown
            .prepare_complete(
                "late".into(),
                permit,
                ExecutionOutcome::Executed,
                "late result".into(),
                14
            )
            .is_err()
    );
    let (other, _) = install(
        unknown
            .prepare_request("other".into(), operation("other"), policy(), 14)
            .unwrap(),
    );
    let other_id = other
        .find_operation("task", "other")
        .unwrap()
        .request
        .request_id
        .clone();
    let (other, attempt) = begin(other, &other_id, &policy(), 15);
    let (other, _) = install(
        changed(
            &other,
            &other_id,
            ApprovalChange::AssessAttempt {
                attempt,
                assessment: harness(ApprovalDecision::Approve),
            },
            &policy(),
            15,
        )
        .unwrap(),
    );
    assert!(matches!(
        changed(&other, &other_id, ApprovalChange::Consume, &policy(), 16),
        Err(ApprovalError::TargetBusy)
    ));
    assert!(
        changed(
            &other,
            &id,
            ApprovalChange::Reconcile {
                outcome: ExecutionOutcome::Unknown,
                reason: "uncertain".into(),
                actor: "operator".into()
            },
            &policy(),
            16
        )
        .is_err()
    );
    let (reconciled, _) = install(
        changed(
            &other,
            &id,
            ApprovalChange::Reconcile {
                outcome: ExecutionOutcome::Failed,
                reason: "old executor stopped and effect checked".into(),
                actor: "operator".into(),
            },
            &policy(),
            16,
        )
        .unwrap(),
    );
    assert!(
        changed(
            &reconciled,
            &other_id,
            ApprovalChange::Consume,
            &policy(),
            17
        )
        .is_ok()
    );
}

#[test]
fn restore_rejects_sequence_identity_and_semantically_invalid_history() {
    let (ledger, id) = approved();
    let mut entries = ledger.entries().to_vec();
    entries[1].sequence += 1;
    assert!(ApprovalLedger::restore(ApprovalLimits::default(), &entries).is_err());
    let mut entries = ledger.entries().to_vec();
    entries[1].event = ApprovalEvent::Changed {
        request_id: id,
        change: ApprovalChange::Consume,
    };
    assert!(ApprovalLedger::restore(ApprovalLimits::default(), &entries).is_err());
    let mut entries = ledger.entries().to_vec();
    let ApprovalEvent::Requested { request } = &mut entries[0].event else {
        panic!("request")
    };
    request.operation.target = "other".into();
    assert!(ApprovalLedger::restore(ApprovalLimits::default(), &entries).is_err());
}

#[test]
fn duplicate_operation_identity_conflicts_and_limits_do_not_mutate_state() {
    let (ledger, _) = requested(&policy(), operation("op"));
    assert!(matches!(
        ledger.prepare_request("duplicate".into(), operation("op"), policy(), 11),
        Err(ApprovalError::Conflict)
    ));
    let mut replacement = operation("op");
    replacement.action["parameters"] = json!({"changed":true});
    assert!(
        ledger
            .prepare_request("conflict".into(), replacement, policy(), 11)
            .is_err()
    );
    assert_eq!(ledger.revision(), 1);
    assert!(ApprovalLedger::restore(ApprovalLimits { max_requests: 1 }, ledger.entries()).is_err());
    let bounded = ApprovalLedger::new(ApprovalLimits { max_requests: 1 }).unwrap();
    let (bounded, _) = install(
        bounded
            .prepare_request("request".into(), operation("op"), policy(), 10)
            .unwrap(),
    );
    let bounded =
        ApprovalLedger::restore(ApprovalLimits { max_requests: 1 }, bounded.entries()).unwrap();
    let (bounded, _) = install(bounded.prepare_recovery("recover".into(), 11).unwrap());
    assert!(matches!(
        bounded.prepare_request("capacity".into(), operation("other"), policy(), 11),
        Err(ApprovalError::Capacity)
    ));
    assert_eq!(bounded.list().len(), 1);
}

#[test]
fn restore_requires_recovery_commit_and_never_reissues_lost_permits() {
    let (ledger, id) = approved();
    let restored = ApprovalLedger::restore(ApprovalLimits::default(), ledger.entries()).unwrap();
    assert!(restored.recovery_required());
    assert!(matches!(
        changed(&restored, &id, ApprovalChange::Consume, &policy(), 12),
        Err(ApprovalError::RecoveryRequired)
    ));
    let (restored, effects) = install(restored.prepare_recovery("resume".into(), 12).unwrap());
    assert!(effects.is_empty());
    assert!(!restored.recovery_required());
    let (executing, _) =
        install(changed(&restored, &id, ApprovalChange::Consume, &policy(), 13).unwrap());
    let restored = ApprovalLedger::restore(ApprovalLimits::default(), executing.entries()).unwrap();
    let (recovered, effects) = install(
        restored
            .prepare_recovery("recover-intent".into(), 14)
            .unwrap(),
    );
    assert!(effects.is_empty());
    assert_eq!(recovered.get(&id).unwrap().state, ApprovalState::Unknown);
    assert!(changed(&recovered, &id, ApprovalChange::Consume, &policy(), 15).is_err());
}

#[test]
fn restart_recovery_invalidates_inflight_review_and_preserves_waiting_deadlines() {
    let policy = fallback_policy();
    let (waiting, id) = requested(&policy, operation("op"));
    let restored = ApprovalLedger::restore(ApprovalLimits::default(), waiting.entries()).unwrap();
    let (waiting, effects) = install(
        restored
            .prepare_recovery("recover-wait".into(), 15)
            .unwrap(),
    );
    assert!(effects.is_empty());
    assert_eq!(waiting.get(&id).unwrap().human_deadline, Some(20));
    let (reviewing, mut effects) = install(
        changed(
            &waiting,
            &id,
            ApprovalChange::BeginReview {
                expected_revision: 0,
                timeout_secs: 20,
            },
            &policy,
            20,
        )
        .unwrap(),
    );
    let ApprovalEffect::Review(attempt) = effects.remove(0) else {
        panic!("review")
    };
    let restored = ApprovalLedger::restore(ApprovalLimits::default(), reviewing.entries()).unwrap();
    let (recovered, effects) = install(
        restored
            .prepare_recovery("recover-review".into(), 21)
            .unwrap(),
    );
    assert!(effects.is_empty());
    assert_eq!(
        recovered.get(&id).unwrap().review_stage,
        ReviewStage::NeedsHuman
    );
    assert!(
        changed(
            &recovered,
            &id,
            ApprovalChange::AssessAttempt {
                attempt,
                assessment: harness(ApprovalDecision::Approve)
            },
            &policy,
            22
        )
        .is_err()
    );
}

#[test]
fn complete_cannot_be_forged_and_same_commit_identity_cannot_grant_twice() {
    let (ledger, id) = approved();
    let (executing, _) =
        install(changed(&ledger, &id, ApprovalChange::Consume, &policy(), 12).unwrap());
    assert!(
        changed(
            &executing,
            &id,
            ApprovalChange::Complete {
                outcome: ExecutionOutcome::Executed,
                reason: "invented".into()
            },
            &policy(),
            13
        )
        .is_err()
    );
    assert!(
        executing
            .prepare(
                "request".into(),
                ApprovalEvent::Changed {
                    request_id: id,
                    change: ApprovalChange::RecoverUnknown
                },
                None,
                13
            )
            .is_err()
    );
    assert!(serde_json::to_value(&executing).unwrap()["history"].is_array());
}

#[test]
fn receipt_binds_event_and_policy_even_with_same_id_and_revision() {
    let (ledger, id) = requested(&policy(), operation("op"));
    let approved = changed(
        &ledger,
        &id,
        ApprovalChange::HumanDecision {
            expected_revision: 0,
            assessment: human(ApprovalDecision::Approve),
        },
        &policy(),
        11,
    )
    .unwrap();
    let denied = changed(
        &ledger,
        &id,
        ApprovalChange::HumanDecision {
            expected_revision: 0,
            assessment: human(ApprovalDecision::Deny),
        },
        &policy(),
        11,
    )
    .unwrap();
    assert_eq!(approved.request().id, denied.request().id);
    assert!(
        approved
            .confirm(CommitReceipt::confirmed(denied.request()))
            .is_err()
    );
}

#[test]
fn removed_untracked_assessment_and_import_are_rejected_at_decode() {
    assert!(
        serde_json::from_value::<ApprovalChange>(
            serde_json::json!({"change":"assess","assessment":harness(ApprovalDecision::Approve)})
        )
        .is_err()
    );
    assert!(
        serde_json::from_value::<ApprovalEvent>(
            serde_json::json!({"event":"imported","history":{}})
        )
        .is_err()
    );
}

#[test]
fn scale_approval_history_is_incremental_and_restores_without_permits() {
    let mut ledger = ApprovalLedger::new(ApprovalLimits::default()).unwrap();
    let start = std::time::Instant::now();
    let mut largest = 0;
    for i in 0..500 {
        let pending = ledger
            .prepare_request(
                format!("request-{i}"),
                operation(&format!("op-{i}")),
                policy(),
                10,
            )
            .unwrap();
        largest = largest.max(serde_json::to_vec(pending.request()).unwrap().len());
        assert!(largest < 2000);
        ledger = install(pending).0;
    }
    let prepare = start.elapsed();
    let entries = ledger.entries();
    let bytes = serde_json::to_vec(&entries).unwrap().len();
    let start = std::time::Instant::now();
    let restored = ApprovalLedger::restore(ApprovalLimits::default(), &entries).unwrap();
    let restore = start.elapsed();
    assert_eq!(restored.list(), ledger.list());
    let (_, effects) = install(restored.prepare_recovery("restore".into(), 20).unwrap());
    assert!(effects.is_empty());
    println!(
        "scale approval n=500 max_request={largest} history_bytes={bytes} prepare={prepare:?} restore={restore:?}"
    );
}
