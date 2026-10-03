use recuvora_host::control::operation::{CommitReceipt, CommitRequest};
use recuvora_host::control::recovery::knowledge::*;
use std::collections::BTreeMap;

fn artifact(id: &str, version: u64) -> RepairArtifact {
    RepairArtifact {
        id: id.into(),
        version,
        kind: "provider-action".into(),
        payload: serde_json::json!({"action": "restore", "parameters": [1, true, null]}),
        preconditions: BTreeMap::from([("workload_version".into(), "1".into())]),
        generated_by_harness: "harness-a".into(),
        generated_in_session: "session-a".into(),
    }
}

fn experience(id: &str, outcome: RepairOutcome) -> RepairExperience {
    RepairExperience {
        id: id.into(),
        operation_id: format!("operation-{id}"),
        target_id: "target-a".into(),
        conditions: BTreeMap::from([("condition".into(), "unready".into())]),
        keywords: vec!["condition".into(), "workload".into()],
        outcome,
        evidence_refs: vec![format!("evidence:{id}")],
        recorded_at_ms: 100,
        actions: vec![],
        report: ExperienceReport {
            summary: "Target condition inspected".into(),
            lessons: "Check current target facts before selecting an action".into(),
            related_experience_ids: vec![],
            scriptability: Scriptability::Undetermined {
                reason: "More observations needed".into(),
            },
        },
    }
}

fn command(item: RepairExperience) -> KnowledgeCommand {
    KnowledgeCommand::RecordExperience(TrustedRepairExperience::attest(item).unwrap())
}

fn query() -> KnowledgeQuery {
    KnowledgeQuery {
        conditions: BTreeMap::from([("condition".into(), "unready".into())]),
        keywords: vec!["condition".into()],
        limit: 10,
    }
}

fn initial() -> KnowledgeState {
    KnowledgeState::new(KnowledgeConfig::default()).unwrap()
}

fn commit(state: KnowledgeState, command: KnowledgeCommand) -> KnowledgeState {
    let pending = state
        .propose(format!("commit-{}", state.revision()), command)
        .unwrap();
    let receipt = CommitReceipt::confirmed(pending.request());
    let confirmed = pending.confirm(receipt).unwrap();
    assert!(confirmed.effects.is_empty());
    confirmed.state
}

fn replay(
    config: KnowledgeConfig,
    history: &[(CommitRequest, KnowledgeCommand)],
) -> Result<KnowledgeState, KnowledgeError> {
    KnowledgeState::replay(
        config,
        history
            .iter()
            .map(|(request, command)| KnowledgeReplayEntry {
                request: request.clone(),
                command: command.clone(),
                receipt: CommitReceipt::confirmed(request),
            })
            .collect(),
    )
}

#[test]
fn proposals_preserve_original_state_and_require_exact_confirmation() {
    let state = initial();
    let item = experience("a", RepairOutcome::Verified);
    let pending = state.propose("experience", command(item.clone())).unwrap();
    assert!(state.get("a").is_none());
    assert_eq!(state.revision(), 0);
    let mut wrong = pending.request().clone();
    wrong.input = serde_json::Value::Null;
    assert!(pending.confirm(CommitReceipt::confirmed(&wrong)).is_err());
    let state = commit(state, command(item.clone()));
    assert_eq!(state.get("a"), Some(item));
    assert_eq!(state.search_experiences(&query()).unwrap().len(), 1);
}

#[test]
fn artifact_validation_is_neutral_bounded_and_strictly_shaped() {
    let mut item = artifact("action", 1);
    for kind in ["script", "configuration-change", "future-provider-format"] {
        item.kind = kind.into();
        item.validate().unwrap();
    }
    item.payload = serde_json::Value::String("x".repeat(MAX_ARTIFACT_BYTES - 2));
    item.validate().unwrap();
    item.payload = serde_json::Value::String("x".repeat(MAX_ARTIFACT_BYTES - 1));
    assert!(item.validate().is_err());
    item = artifact("action", 0);
    assert!(item.validate().is_err());
    item.version = 1;
    item.generated_in_session.clear();
    assert!(item.validate().is_err());
    item = artifact("action", 1);
    item.preconditions.clear();
    assert!(item.validate().is_err());
    let mut raw = serde_json::to_value(artifact("action", 1)).unwrap();
    raw["language"] = "python".into();
    assert!(serde_json::from_value::<RepairArtifact>(raw).is_err());
    assert!(
        serde_json::from_value::<KnowledgeConfig>(
            serde_json::json!({"max_records": 1, "max_cases_per_record": 1})
        )
        .is_err()
    );
}

#[test]
fn attestation_rejects_unbounded_actions_invalid_evidence_and_reports() {
    let item = experience("a", RepairOutcome::Verified);
    let mut invalid = item.clone();
    invalid.actions = vec![artifact("a", 1), artifact("b", 1)];
    assert!(TrustedRepairExperience::attest(invalid).is_err());
    let mut invalid = item.clone();
    invalid.evidence_refs.clear();
    assert!(TrustedRepairExperience::attest(invalid).is_err());
    let mut invalid = item.clone();
    invalid.operation_id.clear();
    assert!(TrustedRepairExperience::attest(invalid).is_err());
    let mut invalid = item.clone();
    invalid.report.related_experience_ids = vec!["a".into(), "a".into()];
    assert!(TrustedRepairExperience::attest(invalid).is_err());
    let mut invalid = item.clone();
    invalid.actions = vec![artifact("a", 0)];
    assert!(TrustedRepairExperience::attest(invalid).is_err());
    let mut invalid = item;
    invalid.report.scriptability = Scriptability::Possible {
        reason: "candidate".into(),
        candidate: Some(artifact("a", 0)),
    };
    assert!(TrustedRepairExperience::attest(invalid).is_err());
}

#[test]
fn experiences_are_exactly_idempotent_and_conflicting_content_is_rejected() {
    let item = experience("a", RepairOutcome::Verified);
    let state = commit(initial(), command(item.clone()));
    let state = commit(state, command(item.clone()));
    assert_eq!(state.projection().experiences, 1);
    assert_eq!(state.revision(), 2);
    for field in 0..3 {
        let mut changed = item.clone();
        match field {
            0 => changed.outcome = RepairOutcome::Unknown,
            1 => changed.actions.push(artifact("action", 1)),
            _ => changed.report.lessons.push_str(" changed"),
        }
        assert!(matches!(
            state.propose("conflict", command(changed)),
            Err(KnowledgeError::Conflict(_))
        ));
    }
    assert_eq!(state.get("a"), Some(item));
}

#[test]
fn action_and_candidate_versions_share_one_immutable_registry() {
    let original = artifact("action", 1);
    let mut item = experience("a", RepairOutcome::Verified);
    item.actions.push(original.clone());
    let state = commit(initial(), command(item));
    let mut changed = original.clone();
    changed.payload = serde_json::json!({"changed": true});
    assert!(state.validate_artifact(&changed).is_err());
    let mut item = experience("b", RepairOutcome::Verified);
    item.report.scriptability = Scriptability::Possible {
        reason: "candidate".into(),
        candidate: Some(changed.clone()),
    };
    assert!(state.propose("conflict", command(item.clone())).is_err());
    item.actions.push(original);
    assert!(
        initial()
            .propose("same-command-conflict", command(item))
            .is_err()
    );
    assert_eq!(state.projection().artifacts, 1);
    changed.version = 2;
    state.validate_artifact(&changed).unwrap();
    assert_eq!(state.projection().artifacts, 1);
}

#[test]
fn failed_and_unknown_actions_are_permanently_quarantined_but_remain_references() {
    for outcome in [RepairOutcome::Failed, RepairOutcome::Unknown] {
        let mut item = experience("failure", outcome);
        item.actions.push(artifact("executed", 1));
        item.report.scriptability = Scriptability::Possible {
            reason: "unexecuted proposal".into(),
            candidate: Some(artifact("candidate", 1)),
        };
        let state = commit(initial(), command(item.clone()));
        assert!(state.is_quarantined("executed", 1));
        assert!(!state.is_quarantined("candidate", 1));
        assert!(!state.is_quarantined("executed", 2));
        assert_eq!(state.search_experiences(&query()).unwrap(), vec![item]);
        let mut later = experience("later", RepairOutcome::Verified);
        later.actions.push(artifact("executed", 1));
        let state = commit(state, command(later));
        assert!(state.is_quarantined("executed", 1));
        assert_eq!(state.search_experiences(&query()).unwrap().len(), 2);
        assert_eq!(state.projection().quarantined_versions, 1);
    }
}

#[test]
fn scriptless_experiences_and_candidates_do_not_create_execution_authority() {
    let mut item = experience("a", RepairOutcome::Verified);
    item.report.scriptability = Scriptability::Possible {
        reason: "proposal".into(),
        candidate: Some(artifact("candidate", 1)),
    };
    let state = commit(initial(), command(item));
    assert!(state.get("a").unwrap().actions.is_empty());
    assert_eq!(state.projection().artifacts, 1);
    assert!(!state.is_quarantined("candidate", 1));
    let mut item = experience("b", RepairOutcome::Failed);
    item.report.scriptability = Scriptability::NotSuitable {
        reason: "requires interactive investigation".into(),
    };
    let state = commit(state, command(item));
    assert_eq!(state.search_experiences(&query()).unwrap().len(), 2);
}

#[test]
fn exact_matching_precedes_limits_with_stable_time_and_identity_order() {
    let mut state = initial();
    for (id, time, condition) in [
        ("newest-non-match", 400, "other"),
        ("d", 300, "unready"),
        ("c", 300, "unready"),
        ("b", 300, "unready"),
        ("a", 300, "unready"),
        ("f", 200, "unready"),
        ("e", 200, "unready"),
        ("oldest", 100, "unready"),
    ] {
        let mut item = experience(id, RepairOutcome::Unknown);
        item.recorded_at_ms = time;
        item.conditions.insert("condition".into(), condition.into());
        state = commit(state, command(item));
    }
    let mut request = query();
    request.limit = 4;
    assert_eq!(
        state
            .search_experiences(&request)
            .unwrap()
            .iter()
            .map(|v| v.id.as_str())
            .collect::<Vec<_>>(),
        ["a", "b", "c", "d"]
    );
    request.limit = 7;
    assert_eq!(
        state
            .search_experiences(&request)
            .unwrap()
            .iter()
            .map(|v| v.id.as_str())
            .collect::<Vec<_>>(),
        ["a", "b", "c", "d", "e", "f", "oldest"]
    );
    request.keywords.push("missing".into());
    assert!(state.search_experiences(&request).unwrap().is_empty());
    request.keywords = vec!["Condition".into()];
    assert!(state.search_experiences(&request).unwrap().is_empty());
    request.limit = 0;
    assert!(state.search_experiences(&request).is_err());
}

#[test]
fn capacity_expansion_and_replay_preserve_experiences_and_isolation() {
    let original = KnowledgeConfig { max_records: 1 };
    let target = KnowledgeConfig { max_records: 2 };
    let mut item = experience("a", RepairOutcome::Unknown);
    item.actions.push(artifact("action", 1));
    let first = command(item.clone());
    let mut state = KnowledgeState::new(original.clone()).unwrap();
    let pending = state.propose("first", first.clone()).unwrap();
    let mut history = vec![(pending.request().clone(), first)];
    state = pending
        .confirm(CommitReceipt::confirmed(&history[0].0))
        .unwrap()
        .state;
    let repeated = command(item);
    assert!(state.propose("retry", repeated).is_ok());
    let second = command(experience("b", RepairOutcome::Verified));
    assert!(matches!(
        state.propose("full", second.clone()),
        Err(KnowledgeError::Capacity(_))
    ));
    let before = state.snapshot();
    let expansion = KnowledgeCommand::ExpandCapacity {
        expected: original.clone(),
        target: target.clone(),
    };
    let pending = state.propose("expand", expansion.clone()).unwrap();
    let request = pending.request().clone();
    drop(pending);
    assert_eq!(state.snapshot(), before);
    assert_eq!(
        replay(original.clone(), &history).unwrap().snapshot(),
        before
    );
    assert!(replay(target.clone(), &history).is_err());
    history.push((request, expansion.clone()));
    state = replay(original.clone(), &history).unwrap();
    assert_eq!(state.config(), &target);
    assert_eq!(state.snapshot().experiences, before.experiences);
    assert!(state.is_quarantined("action", 1));
    assert!(state.propose("repeat-expand", expansion).is_err());
    for limit in [0, 1, 2, 100_001] {
        assert!(
            state
                .propose(
                    "invalid-expansion",
                    KnowledgeCommand::ExpandCapacity {
                        expected: target.clone(),
                        target: KnowledgeConfig { max_records: limit }
                    }
                )
                .is_err()
        );
    }
    let pending = state.propose("second", second.clone()).unwrap();
    history.push((pending.request().clone(), second));
    state = pending
        .confirm(CommitReceipt::confirmed(&history.last().unwrap().0))
        .unwrap()
        .state;
    assert_eq!(
        replay(original, &history).unwrap().snapshot(),
        state.snapshot()
    );
    assert!(state.is_quarantined("action", 1));
    assert!(
        state
            .propose("first", command(experience("c", RepairOutcome::Verified)))
            .is_err()
    );
}

#[test]
fn replay_rejects_altered_commands_history_binding_receipts_and_duplicate_commits() {
    let original = command(experience("a", RepairOutcome::Verified));
    let pending = initial().propose("first", original.clone()).unwrap();
    let request = pending.request().clone();
    let history = vec![(request.clone(), original.clone())];
    assert!(replay(KnowledgeConfig::default(), &history).is_ok());
    let mut altered = history.clone();
    altered[0].1 = command(experience("b", RepairOutcome::Verified));
    assert!(replay(KnowledgeConfig::default(), &altered).is_err());
    let mut altered = history.clone();
    altered[0].0.input[0] = "altered-digest".into();
    assert!(replay(KnowledgeConfig::default(), &altered).is_err());
    let mut altered = history.clone();
    altered[0].0.revision += 1;
    assert!(replay(KnowledgeConfig::default(), &altered).is_err());
    let mut duplicate = history.clone();
    duplicate.extend(history);
    assert!(replay(KnowledgeConfig::default(), &duplicate).is_err());
    let mut wrong = request.clone();
    wrong.domain = "recovery".into();
    assert!(
        KnowledgeState::replay(
            KnowledgeConfig::default(),
            vec![KnowledgeReplayEntry {
                request,
                command: original,
                receipt: CommitReceipt::confirmed(&wrong)
            }]
        )
        .is_err()
    );
}

#[test]
fn snapshot_is_complete_and_has_one_experience_protocol() {
    let empty = serde_json::to_value(initial().snapshot()).unwrap();
    assert_eq!(empty["experiences"], serde_json::json!([]));
    assert_eq!(empty.as_object().unwrap().len(), 3);
    let mut item = experience("a", RepairOutcome::Unknown);
    item.actions.push(artifact("action", 1));
    let state = commit(initial(), command(item.clone()));
    let encoded = serde_json::to_value(state.snapshot()).unwrap();
    assert_eq!(
        encoded["experiences"][0],
        serde_json::to_value(item).unwrap()
    );
    assert!(encoded.get("records").is_none());
}

#[test]
fn scale_knowledge_commits_bind_incremental_commands() {
    let mut state = initial();
    let start = std::time::Instant::now();
    let mut history = Vec::new();
    let mut largest = 0;
    for index in 0..500 {
        let entry = command(experience(
            &format!("experience-{index}"),
            RepairOutcome::Verified,
        ));
        let pending = state
            .propose(format!("commit-{index}"), entry.clone())
            .unwrap();
        largest = largest.max(serde_json::to_vec(pending.request()).unwrap().len());
        assert!(largest < 2000);
        let request = pending.request().clone();
        state = pending
            .confirm(CommitReceipt::confirmed(&request))
            .unwrap()
            .state;
        history.push((request, entry));
    }
    let prepare = start.elapsed();
    let start = std::time::Instant::now();
    let restored = replay(KnowledgeConfig::default(), &history).unwrap();
    let restore = start.elapsed();
    assert_eq!(restored.snapshot(), state.snapshot());
    println!(
        "scale knowledge n=500 max_request={largest} snapshot_bytes={} prepare={prepare:?} restore={restore:?}",
        serde_json::to_vec(&state.snapshot()).unwrap().len()
    );
}
