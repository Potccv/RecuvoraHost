use recuvora_host::control::operation::{CommitReceipt, Prepared};
use recuvora_host::control::recovery::incidents::*;
use serde_json::json;

fn commit(sequence: u64, condition: SignalCondition) -> MonitorCommit {
    MonitorCommit {
        monitor_id: "monitor".into(),
        sequence,
        checkpoint: json!({"cursor":sequence}),
        signals: vec![IncidentSignal {
            monitor_id: "monitor".into(),
            target_id: "target".into(),
            rule_id: "rule".into(),
            kind: IncidentKind::Target,
            condition,
            summary: "observation".into(),
            evidence: json!({"sequence":sequence}),
        }],
        now_ms: sequence * 1000,
    }
}
fn install(proposal: Prepared<IncidentLedger>) -> IncidentLedger {
    let receipt = CommitReceipt::confirmed(proposal.request());
    let committed = proposal.confirm(receipt).unwrap();
    assert!(committed.effects.is_empty());
    committed.state
}
fn apply(ledger: &IncidentLedger, event: MonitorCommit) -> IncidentLedger {
    install(
        ledger
            .prepare_monitor(format!("commit-{}", ledger.revision()), event)
            .unwrap()
            .unwrap(),
    )
}
fn empty() -> IncidentLedger {
    IncidentLedger::new(IncidentLimits::default()).unwrap()
}

#[test]
fn prepare_and_restore_preserve_atomic_checkpoint_with_no_input_mutation() {
    let ledger = empty();
    let proposal = ledger
        .prepare_monitor("first".into(), commit(1, SignalCondition::Active))
        .unwrap()
        .unwrap();
    assert!(ledger.list().is_empty());
    assert!(ledger.checkpoint("monitor").is_none());
    let first = install(proposal);
    let second = apply(&first, commit(2, SignalCondition::Active));
    let record = second.list().remove(0);
    assert_eq!(record.occurrences, 2);
    assert_eq!(record.first_seen, 1000);
    assert_eq!(record.last_seen, 2000);
    let restored = IncidentLedger::restore(IncidentLimits::default(), second.entries()).unwrap();
    assert_eq!(restored.list(), second.list());
    assert_eq!(restored.checkpoint("monitor"), second.checkpoint("monitor"));
    assert_eq!(restored.checkpoint("monitor").unwrap().sequence, 2);
}

#[test]
fn exact_retry_does_not_overwrite_acknowledgement_or_advance_revision() {
    let ledger = apply(&empty(), commit(1, SignalCondition::Active));
    let id = ledger.list()[0].id.clone();
    let acknowledged = install(
        ledger
            .prepare_acknowledge(
                "ack".into(),
                id.clone(),
                1,
                "operator".into(),
                "investigating".into(),
                1001,
            )
            .unwrap(),
    );
    assert!(
        acknowledged
            .prepare_monitor("retry".into(), commit(1, SignalCondition::Active))
            .unwrap()
            .is_none()
    );
    assert_eq!(acknowledged.revision(), 2);
    let record = acknowledged.get(&id).unwrap();
    assert_eq!(record.status, IncidentStatus::Acknowledged);
    assert_eq!(record.condition, SignalCondition::Active);
    assert!(record.resolved_at.is_none());
    let mut conflict = commit(1, SignalCondition::Active);
    conflict.checkpoint = json!({"cursor":99});
    assert!(matches!(
        acknowledged.prepare_monitor("conflict".into(), conflict),
        Err(IncidentError::Conflict(_))
    ));
    assert!(
        acknowledged
            .prepare_acknowledge(
                "stale".into(),
                id,
                1,
                "operator".into(),
                "stale".into(),
                1002
            )
            .is_err()
    );
}

#[test]
fn unknown_preserves_episode_and_clear_allows_new_independent_episode() {
    let ledger = apply(&empty(), commit(1, SignalCondition::Active));
    let first_id = ledger.list()[0].id.clone();
    let ledger = apply(&ledger, commit(2, SignalCondition::Unknown));
    assert_eq!(ledger.get(&first_id).unwrap().status, IncidentStatus::Open);
    assert_eq!(ledger.get(&first_id).unwrap().occurrences, 1);
    let ledger = apply(&ledger, commit(3, SignalCondition::Clear));
    assert_eq!(
        ledger.get(&first_id).unwrap().status,
        IncidentStatus::Resolved
    );
    let ledger = apply(&ledger, commit(4, SignalCondition::Active));
    assert_eq!(ledger.list().len(), 2);
    assert_eq!(
        ledger
            .list()
            .iter()
            .filter(|r| r.status == IncidentStatus::Open)
            .count(),
        1
    );
    assert_eq!(ledger.get(&first_id).unwrap().resolved_at, Some(3000));
}

#[test]
fn ordered_batch_preserves_active_clear_and_recurrence_as_separate_episodes() {
    let mut batch = commit(1, SignalCondition::Active);
    batch
        .signals
        .push(commit(1, SignalCondition::Clear).signals.remove(0));
    batch
        .signals
        .push(commit(1, SignalCondition::Active).signals.remove(0));
    let ledger = apply(&empty(), batch);
    assert_eq!(ledger.list().len(), 2);
    assert_eq!(ledger.list()[0].status, IncidentStatus::Resolved);
    assert_eq!(ledger.list()[1].status, IncidentStatus::Open);
    let restored = IncidentLedger::restore(IncidentLimits::default(), ledger.entries()).unwrap();
    assert_eq!(restored.list(), ledger.list());
}

#[test]
fn limits_reject_entire_batch_without_advancing_checkpoint() {
    let ledger = IncidentLedger::new(IncidentLimits {
        max_incidents: 1,
        max_monitors: 1,
    })
    .unwrap();
    let mut batch = commit(1, SignalCondition::Active);
    let mut second = batch.signals[0].clone();
    second.target_id = "other".into();
    batch.signals.push(second);
    assert!(matches!(
        ledger.prepare_monitor("too-many".into(), batch),
        Err(IncidentError::Capacity(_))
    ));
    assert!(ledger.checkpoint("monitor").is_none());
    assert_eq!(ledger.revision(), 0);
    let ledger = apply(&ledger, commit(1, SignalCondition::Active));
    let mut second = commit(1, SignalCondition::Active);
    second.monitor_id = "other".into();
    second.signals[0].monitor_id = "other".into();
    assert!(matches!(
        ledger.prepare_monitor("other-monitor".into(), second),
        Err(IncidentError::Capacity(_))
    ));
    assert_eq!(ledger.checkpoint("monitor").unwrap().sequence, 1);
}

#[test]
fn evidence_complexity_and_mixed_monitor_identity_fail_without_partial_state() {
    let ledger = empty();
    let mut event = commit(1, SignalCondition::Active);
    event.signals[0].evidence = json!({"large":"x".repeat(20000)});
    assert!(matches!(
        ledger.prepare_monitor("large".into(), event),
        Err(IncidentError::Capacity(_))
    ));
    let mut event = commit(1, SignalCondition::Active);
    for _ in 0..40 {
        event.signals[0].evidence = json!({"nested":event.signals[0].evidence});
    }
    assert!(ledger.prepare_monitor("nested".into(), event).is_err());
    let mut event = commit(1, SignalCondition::Active);
    event.signals[0].monitor_id = "wrong".into();
    assert!(matches!(
        ledger.prepare_monitor("mixed".into(), event),
        Err(IncidentError::Invalid(_))
    ));
    assert!(ledger.list().is_empty());
    assert!(
        ledger
            .prepare_monitor("gap".into(), commit(2, SignalCondition::Active))
            .is_err()
    );
}

#[test]
fn target_and_coverage_are_independent_and_clock_rollback_is_monotonic() {
    let mut event = commit(1, SignalCondition::Active);
    let mut coverage = event.signals[0].clone();
    coverage.kind = IncidentKind::Coverage;
    event.signals.push(coverage);
    let ledger = apply(&empty(), event);
    assert_eq!(ledger.list().len(), 2);
    let target_id = ledger
        .list()
        .into_iter()
        .find(|r| r.kind == IncidentKind::Target)
        .unwrap()
        .id;
    let ledger = install(
        ledger
            .prepare_acknowledge(
                "ack".into(),
                target_id.clone(),
                1,
                "operator".into(),
                "".into(),
                3000,
            )
            .unwrap(),
    );
    let mut event = commit(2, SignalCondition::Active);
    event.now_ms = 500;
    let ledger = apply(&ledger, event);
    assert_eq!(ledger.get(&target_id).unwrap().last_seen, 3000);
    assert_eq!(ledger.checkpoint("monitor").unwrap().updated_at_ms, 3000);
}

#[test]
fn replay_rejects_sequence_gaps_duplicate_commits_and_invalid_signal_evidence() {
    let ledger = apply(&empty(), commit(1, SignalCondition::Active));
    let mut entries = ledger.entries().to_vec();
    entries[0].sequence = 2;
    assert!(IncidentLedger::restore(IncidentLimits::default(), &entries).is_err());
    let mut entries = ledger.entries().to_vec();
    let mut duplicate = entries[0].clone();
    duplicate.sequence = 2;
    entries.push(duplicate);
    assert!(IncidentLedger::restore(IncidentLimits::default(), &entries).is_err());
    let mut entries = ledger.entries().to_vec();
    let IncidentEvent::Monitor { commit } = &mut entries[0].event else {
        panic!("monitor")
    };
    commit.signals[0].evidence = json!("not an object");
    assert!(IncidentLedger::restore(IncidentLimits::default(), &entries).is_err());
}

#[test]
fn wrong_host_receipt_cannot_confirm_a_proposal() {
    let ledger = empty();
    let first = ledger
        .prepare_monitor("first".into(), commit(1, SignalCondition::Active))
        .unwrap()
        .unwrap();
    let other = ledger
        .prepare_monitor("other".into(), commit(1, SignalCondition::Clear))
        .unwrap()
        .unwrap();
    assert!(
        first
            .confirm(CommitReceipt::confirmed(other.request()))
            .is_err()
    );
    assert_eq!(ledger.revision(), 0);
}

#[test]
fn history_digest_rejects_changed_prefix_config_and_cross_branch_receipts() {
    let first = apply(&empty(), commit(1, SignalCondition::Active));
    let second = apply(&first, commit(2, SignalCondition::Unknown));
    let mut history = second.entries();
    history[1].prior_digest = "0".repeat(64);
    assert!(IncidentLedger::restore(IncidentLimits::default(), history).is_err());
    let mut history = second.entries();
    let IncidentEvent::Monitor { commit } = &mut history[0].event else {
        panic!("monitor")
    };
    commit.checkpoint = json!({"other":true});
    assert!(IncidentLedger::restore(IncidentLimits::default(), history).is_err());
    let mut limits = IncidentLimits::default();
    limits.max_incidents += 1;
    assert!(IncidentLedger::restore(limits, second.entries()).is_err());
    let mut alternate = crate::commit(1, SignalCondition::Active);
    alternate.checkpoint = json!({"other":true});
    let branch = apply(&empty(), alternate);
    let left = first
        .prepare_monitor("next".into(), crate::commit(2, SignalCondition::Unknown))
        .unwrap()
        .unwrap();
    let right = branch
        .prepare_monitor("next".into(), crate::commit(2, SignalCondition::Unknown))
        .unwrap()
        .unwrap();
    assert!(
        right
            .confirm(CommitReceipt::confirmed(left.request()))
            .is_err()
    );
}

#[test]
fn scale_healthy_polling_and_incident_growth_keep_requests_bounded() {
    for growing in [false, true] {
        let mut ledger = empty();
        let start = std::time::Instant::now();
        let mut largest = 0;
        let mut cumulative = 0;
        for i in 1..=2000 {
            let mut input = commit(i, SignalCondition::Active);
            input.checkpoint = json!({"cursor":i,"padding":"x".repeat(1024)});
            input.now_ms = i * 30_000;
            if growing {
                input.signals[0].rule_id = format!("rule-{i}");
            } else {
                input.signals.clear();
            }
            let pending = ledger
                .prepare_monitor(format!("commit-{i}"), input)
                .unwrap()
                .unwrap();
            let bytes = serde_json::to_vec(pending.request()).unwrap().len();
            largest = largest.max(bytes);
            cumulative += bytes;
            assert!(bytes < 2200);
            ledger = install(pending);
            if [1, 100, 1000, 2000].contains(&i) {
                println!(
                    "scale incidents growing={growing} n={i} request={bytes} cumulative={cumulative}"
                );
            }
        }
        let prepare = start.elapsed();
        let entries = ledger.entries();
        let bytes = serde_json::to_vec(&entries).unwrap().len();
        let start = std::time::Instant::now();
        let restored = IncidentLedger::restore(IncidentLimits::default(), &entries).unwrap();
        let restore = start.elapsed();
        assert_eq!(restored.list(), ledger.list());
        assert_eq!(restored.list().len(), if growing { 2000 } else { 0 });
        let IncidentEvent::Monitor { commit } = &ledger.latest_entry().unwrap().event else {
            panic!("monitor")
        };
        assert!(
            ledger
                .prepare_monitor("exact-retry".into(), commit.clone())
                .unwrap()
                .is_none()
        );
        println!(
            "scale incidents growing={growing} n=2000 max_request={largest} history_bytes={bytes} prepare={prepare:?} restore={restore:?}"
        );
    }
}
