#![cfg(windows)]
use super::*;
use crate::harnesses::{
    ClientProjectGrouping, HarnessAdapterFactory, HarnessDefinition, HarnessProvider,
    HarnessRegistryBuilder, HarnessRegistryConfig, HarnessRunFuture, HarnessRunResult,
};
use crate::workflow_test_support::TestDir;
use std::path::Path;

#[derive(Clone, Copy)]
enum ReviewMode {
    Approve,
    Deny,
    Escalate,
    Malformed,
    WrongRequest,
    Cancel,
    TargetChanged,
    Unavailable,
    MissingRead,
}
struct Factory {
    mode: ReviewMode,
}
struct Provider {
    definition: HarnessDefinition,
    mode: ReviewMode,
}
impl HarnessAdapterFactory for Factory {
    fn adapter_id(&self) -> &str {
        "fixture"
    }
    fn build(
        &self,
        definition: HarnessDefinition,
    ) -> Result<Arc<dyn HarnessProvider>, HarnessError> {
        Ok(Arc::new(Provider {
            definition,
            mode: self.mode,
        }))
    }
}
impl HarnessProvider for Provider {
    fn definition(&self) -> &HarnessDefinition {
        &self.definition
    }
    fn run<'a>(&'a self, request: HarnessRunRequest) -> HarnessRunFuture<'a> {
        Box::pin(async move {
            let response = if request.role() == HarnessRole::Approval {
                assert!(request.tools().is_empty());
                assert!(request.tool_handler().is_none());
                assert_eq!(request.visibility(), ConversationVisibility::Hidden);
                let data: Value =
                    serde_json::from_str(request.prompt().split("Review input:\n").nth(1).unwrap())
                        .unwrap();
                let id = data["request"]["request_id"].as_str().unwrap();
                match self.mode {
                    ReviewMode::Malformed => "approved!".into(),
                    ReviewMode::WrongRequest => {
                        json!({"request_id":"wrong","decision":"approve","reason":"fixture"})
                            .to_string()
                    }
                    ReviewMode::Unavailable => {
                        return Err(HarnessError::Unavailable {
                            harness: self.definition.id.clone(),
                            message: "fixture unavailable".into(),
                        });
                    }
                    other => {
                        if matches!(other, ReviewMode::Cancel) {
                            request.cancellation().cancel();
                        }
                        if matches!(other, ReviewMode::TargetChanged) {
                            let root = data["request"]["operation"]["action"]["target_root"]
                                .as_str()
                                .unwrap();
                            std::fs::write(Path::new(root).join("a.txt"), "external change")
                                .unwrap();
                        }
                        json!({"request_id":id,"decision":match other {ReviewMode::Deny=>"deny",ReviewMode::Escalate=>"escalate",_=>"approve"},"reason":"fixture policy assessment"}).to_string()
                    }
                }
            } else {
                assert_eq!(request.tools().len(), 2);
                let handler = request.tool_handler().unwrap();
                let read = handler
                    .call(HarnessToolCall {
                        harness_id: self.definition.id.clone(),
                        thread_id: "execution-thread".into(),
                        turn_id: "execution-turn".into(),
                        call_id: "read-1".into(),
                        tool: "recuvora_read_text".into(),
                        arguments: json!({"path":if matches!(self.mode,ReviewMode::MissingRead){"not-allowed.txt"}else{"a.txt"}}),
                        cancellation: request.cancellation().clone(),
                    })
                    .await?;
                if matches!(self.mode, ReviewMode::MissingRead) {
                    assert!(!read.success);
                    return Ok(HarnessRunResult {
                        harness_id: self.definition.id.clone(),
                        adapter: "fixture".into(),
                        address: "fixture://".into(),
                        thread_id: "execution-thread".into(),
                        session_id: "execution-session".into(),
                        project_directory: request.project_directory().to_path_buf(),
                        visibility: request.visibility(),
                        native_project_id: None,
                        client_project_grouping: ClientProjectGrouping::NotApplicable,
                        final_response: "Stopped after failed read".into(),
                    });
                }
                assert!(read.success);
                let content: Value = serde_json::from_str(&read.content).unwrap();
                let write=handler.call(HarnessToolCall {harness_id:self.definition.id.clone(),thread_id:"execution-thread".into(),turn_id:"execution-turn".into(),
                    call_id:"write-1".into(),tool:"recuvora_replace_text".into(),arguments:json!({"path":"a.txt","expected":content["content"],"replacement":"after"}),cancellation:request.cancellation().clone()}).await?;
                write.content
            };
            Ok(HarnessRunResult {
                harness_id: self.definition.id.clone(),
                adapter: "fixture".into(),
                address: "fixture://".into(),
                thread_id: if request.role() == HarnessRole::Approval {
                    "review-thread"
                } else {
                    "execution-thread"
                }
                .into(),
                session_id: if request.role() == HarnessRole::Approval {
                    "review-session"
                } else {
                    "execution-session"
                }
                .into(),
                project_directory: request.project_directory().to_path_buf(),
                visibility: request.visibility(),
                native_project_id: None,
                client_project_grouping: ClientProjectGrouping::NotApplicable,
                final_response: response,
            })
        })
    }
}

fn setup(name: &str, mode: ReviewMode, human: bool) -> (TestDir, Arc<RepairSession>) {
    let dir = TestDir::new(name);
    for child in ["target", "review", "state"] {
        std::fs::create_dir(dir.path.join(child)).unwrap();
    }
    std::fs::write(dir.path.join("target/a.txt"), "before").unwrap();
    let mut builder = HarnessRegistryBuilder::new();
    builder.register(Arc::new(Factory { mode })).unwrap();
    let registry = builder
        .build(HarnessRegistryConfig {
            schema_version: 1,
            default_harness: Some("same".into()),
            harnesses: vec![HarnessDefinition::new(
                "same",
                "fixture",
                "fixture://",
                vec![dir.path.clone()],
            )],
        })
        .unwrap();
    let config = RepairConfig {
        extensions_config: None,
        execution_workspace: None,
        reviewer_workspace: None,
        schema_version: 1,
        harness_config: dir.path.join("harness.json"),
        execution_harness: "same".into(),
        target_id: "test-target".into(),
        target_root: dir.path.join("target"),
        reviewer_directory: dir.path.join("review"),
        data_dir: dir.path.join("state"),
        allowed_files: vec!["a.txt".into()],
        policy: ApprovalPolicy {
            id: "text-fixes".into(),
            version: 1,
            reviewer: if human {
                ReviewerConfig::Human
            } else {
                ReviewerConfig::Harness {
                    harness_id: "same".into(),
                }
            },
            delegation: "Allow the exact requested text edit only".into(),
            allowed_targets: vec!["test-target".into()],
            allowed_action_kinds: vec!["replace_text".into()],
            ttl_secs: 300,
        },
        timeout_secs: 60,
        max_tool_calls: 8,
    };
    let session = RepairSession::open(config, Some(Arc::new(registry)), &[]).unwrap();
    (dir, session)
}

#[tokio::test]
async fn same_harness_reviews_and_executes_once_with_durable_receipt() {
    let (dir, session) = setup("same-harness", ReviewMode::Approve, false);
    let result = session
        .run(
            "task-1".into(),
            "Replace before with after".into(),
            HarnessCancellation::new(),
        )
        .await
        .unwrap();
    assert_eq!(result.status, RepairStatus::Completed);
    assert_eq!(
        std::fs::read_to_string(dir.path.join("target/a.txt")).unwrap(),
        "after"
    );
    let record = session.records().unwrap().remove(0);
    assert_eq!(record.state, ApprovalState::Executed);
    assert!(
        session
            .apply(&record.request.request_id, &HarnessCancellation::new())
            .is_err()
    );
    assert!(
        session
            .run("task-1".into(), "repeat".into(), HarnessCancellation::new())
            .await
            .is_err()
    );
    let config = session.config.clone();
    drop(session);
    let reopened = RepairSession::open(config, None, &[]).unwrap();
    assert_eq!(
        reopened.records().unwrap()[0].state,
        ApprovalState::Executed
    );
}

#[tokio::test]
async fn denied_unknown_or_invalid_review_never_writes() {
    for (mode, state) in [
        (ReviewMode::Deny, ApprovalState::Denied),
        (ReviewMode::Escalate, ApprovalState::WaitingHuman),
        (ReviewMode::Malformed, ApprovalState::WaitingHuman),
        (ReviewMode::WrongRequest, ApprovalState::WaitingHuman),
        (ReviewMode::Unavailable, ApprovalState::WaitingHuman),
    ] {
        let (dir, session) = setup("review-stops", mode, false);
        tokio::time::timeout(
            Duration::from_secs(5),
            session.run("task".into(), "replace".into(), HarnessCancellation::new()),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path.join("target/a.txt")).unwrap(),
            "before"
        );
        assert_eq!(session.records().unwrap()[0].state, state);
    }
}

#[tokio::test]
async fn human_approval_can_be_applied_after_restart_but_not_reused() {
    let (dir, session) = setup("human", ReviewMode::Approve, true);
    let result = session
        .run("task".into(), "replace".into(), HarnessCancellation::new())
        .await
        .unwrap();
    assert_eq!(result.status, RepairStatus::WaitingHuman);
    let id = session.records().unwrap()[0].request.request_id.clone();
    session
        .decide(&id, ApprovalDecision::Approve, "reviewed exact diff".into())
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path.join("target/a.txt")).unwrap(),
        "before"
    );
    let config = session.config.clone();
    drop(session);
    let reopened = RepairSession::open(config, None, &[]).unwrap();
    assert_eq!(
        reopened
            .apply(&id, &HarnessCancellation::new())
            .unwrap()
            .state,
        ApprovalState::Executed
    );
    assert!(reopened.apply(&id, &HarnessCancellation::new()).is_err());
}

#[tokio::test]
async fn cancellation_and_changed_target_invalidate_approval_before_write() {
    for (mode, expected, state) in [
        (ReviewMode::Cancel, "before", ApprovalState::Canceled),
        (
            ReviewMode::TargetChanged,
            "external change",
            ApprovalState::Revoked,
        ),
    ] {
        let (dir, session) = setup("stale", mode, false);
        let result = session
            .run("task".into(), "replace".into(), HarnessCancellation::new())
            .await
            .unwrap();
        assert_ne!(result.status, RepairStatus::Completed);
        assert_eq!(
            std::fs::read_to_string(dir.path.join("target/a.txt")).unwrap(),
            expected
        );
        assert_eq!(session.records().unwrap()[0].state, state);
    }
}

#[tokio::test]
async fn authenticated_revision_prevents_stale_decision_and_write() {
    let (dir, session) = setup("authenticated-revision", ReviewMode::Approve, true);
    session
        .run(
            "versioned-task".into(),
            "replace".into(),
            HarnessCancellation::new(),
        )
        .await
        .unwrap();
    let pending = session.records().unwrap().remove(0);
    let id = &pending.request.request_id;
    assert!(
        session
            .decide_authenticated(
                id,
                pending.revision + 1,
                "remote-operator",
                ApprovalDecision::Approve,
                "reviewed".into()
            )
            .is_err()
    );
    let approved = session
        .decide_authenticated(
            id,
            pending.revision,
            "remote-operator",
            ApprovalDecision::Approve,
            "reviewed".into(),
        )
        .unwrap();
    assert!(
        matches!(approved.assessment.as_ref().unwrap().reviewer, recuvora_core::recovery::approval::AssessmentSource::Human{ref actor} if actor=="remote-operator")
    );
    assert!(
        session
            .decide_authenticated(
                id,
                pending.revision,
                "other",
                ApprovalDecision::Deny,
                "stale".into()
            )
            .is_err()
    );
    assert!(
        session
            .apply_versioned(id, Some(pending.revision), &HarnessCancellation::new())
            .is_err()
    );
    assert_eq!(
        std::fs::read_to_string(dir.path.join("target/a.txt")).unwrap(),
        "before"
    );
    assert_eq!(
        session
            .apply_versioned(id, Some(approved.revision), &HarnessCancellation::new())
            .unwrap()
            .state,
        ApprovalState::Executed
    );
    assert!(
        session
            .apply_versioned(id, Some(approved.revision), &HarnessCancellation::new())
            .is_err()
    );
}

#[tokio::test]
async fn unfinished_write_is_unknown_on_restart_and_result_checked_without_replay() {
    let (dir, session) = setup("unknown", ReviewMode::Approve, true);
    session
        .run("task".into(), "replace".into(), HarnessCancellation::new())
        .await
        .unwrap();
    let record = session.records().unwrap().remove(0);
    let id = record.request.request_id.clone();
    session
        .decide(&id, ApprovalDecision::Approve, "reviewed".into())
        .unwrap();
    let permit = session
        .store()
        .unwrap()
        .consume(
            &id,
            &record.request.operation,
            &session.config.policy,
            now().unwrap(),
        )
        .unwrap();
    // A real target effect happened, but the receipt never reached the journal.
    std::fs::write(dir.path.join("target/a.txt"), "after").unwrap();
    drop(permit);
    let config = session.config.clone();
    drop(session);
    let reopened = RepairSession::open(config, None, &[]).unwrap();
    assert_eq!(reopened.record(&id).unwrap().state, ApprovalState::Unknown);
    assert!(reopened.apply(&id, &HarnessCancellation::new()).is_err());
    assert_eq!(
        reopened.check_result(&id).unwrap().state,
        ApprovalState::Executed
    );
    assert_eq!(
        std::fs::read_to_string(dir.path.join("target/a.txt")).unwrap(),
        "after"
    );
}

#[tokio::test]
async fn tool_error_cannot_be_reported_as_successful_model_completion() {
    let (_dir, session) = setup("tool-error", ReviewMode::MissingRead, false);
    let result = session
        .run("task".into(), "replace".into(), HarnessCancellation::new())
        .await
        .unwrap();
    assert_eq!(result.status, RepairStatus::Failed);
    assert!(session.records().unwrap().is_empty());
}

#[tokio::test]
async fn renaming_target_label_cannot_bypass_unknown_physical_target() {
    let (_dir, session) = setup("target-label", ReviewMode::Approve, true);
    session
        .run("task".into(), "replace".into(), HarnessCancellation::new())
        .await
        .unwrap();
    let record = session.records().unwrap().remove(0);
    let id = record.request.request_id.clone();
    session
        .decide(&id, ApprovalDecision::Approve, "reviewed".into())
        .unwrap();
    let permit = session
        .store()
        .unwrap()
        .consume(
            &id,
            &record.request.operation,
            &session.config.policy,
            now().unwrap(),
        )
        .unwrap();
    drop(permit);
    let mut config = session.config.clone();
    drop(session);
    config.target_id = "renamed".into();
    config.policy.allowed_targets = vec!["renamed".into()];
    config.policy.version += 1;
    let reopened = RepairSession::open(config, None, &[]).unwrap();
    assert!(matches!(
        reopened
            .run(
                "new-task".into(),
                "replace".into(),
                HarnessCancellation::new()
            )
            .await,
        Err(WorkflowError::Approval(ApprovalError::TargetBusy))
    ));
}

#[tokio::test]
async fn receipt_journal_failure_returns_structured_unknown_after_actual_write() {
    let (dir, session) = setup("receipt-failure", ReviewMode::Approve, true);
    session
        .run("task".into(), "replace".into(), HarnessCancellation::new())
        .await
        .unwrap();
    let record = session.records().unwrap().remove(0);
    let id = record.request.request_id.clone();
    session
        .decide(&id, ApprovalDecision::Approve, "reviewed".into())
        .unwrap();
    let journal = dir.path.join("state/approvals.jsonl");
    let before = std::fs::metadata(&journal).unwrap().len();
    let permit = session
        .store()
        .unwrap()
        .consume(
            &id,
            &record.request.operation,
            &session.config.policy,
            now().unwrap(),
        )
        .unwrap();
    drop(permit);
    let after_intent = std::fs::metadata(&journal).unwrap().len();
    let config = session.config.clone();
    drop(session);
    // Fixture reset: no effect was dispatched above. Restore the pre-intent
    // snapshot, then cap the journal to exactly one more execution intent.
    std::fs::OpenOptions::new()
        .write(true)
        .open(&journal)
        .unwrap()
        .set_len(before)
        .unwrap();
    let reopened = RepairSession::open(config, None, &[]).unwrap();
    let RepairSession {
        config,
        files,
        store,
        registry,
    } = Arc::try_unwrap(reopened).ok().unwrap();
    drop(store);
    let store = ApprovalStore::open(
        &config.data_dir,
        ApprovalStoreConfig {
            max_requests: 100,
            max_journal_bytes: after_intent,
        },
        now().unwrap(),
    )
    .unwrap();
    let bounded = RepairSession {
        config,
        files,
        store: Mutex::new(store),
        registry,
    };
    assert!(matches!(
        bounded.apply(&id, &HarnessCancellation::new()),
        Err(WorkflowError::OutcomeUnknown { .. })
    ));
    assert_eq!(
        std::fs::read_to_string(dir.path.join("target/a.txt")).unwrap(),
        "after"
    );
    assert_eq!(bounded.record(&id).unwrap().state, ApprovalState::Executing);
}
