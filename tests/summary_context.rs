use super::*;
use recuvora_core::recovery::approval::ProposedOperation;
use recuvora_core::recovery::knowledge::MAX_ARTIFACT_BYTES;
use recuvora_core::recovery::workflow::{
    ExecutionResultCheck, MAX_REPAIR_REQUEST_BYTES, ProblemContext, RecoveryTask, RepairReceipt,
    ResultCheckRecord, TargetBinding, TargetObservation,
};
use serde_json::json;

fn job() -> ExperienceJob {
    let facts = BTreeMap::from([("release".into(), "v1".into())]);
    let problem = ProblemContext {
        origin: Default::default(),
        report: None,
        incident_id: "incident".into(),
        incident_revision: 1,
        target_id: "target".into(),
        fingerprint: "fault".into(),
        summary: "故障\n\"\\".repeat(100),
        occurrences: 1,
        keywords: vec!["workload".into()],
        conditions: facts.clone(),
        evidence_refs: vec!["problem:evidence".into()],
    };
    let observation = TargetObservation {
        target_id: "target".into(),
        facts: facts.clone(),
        evidence_refs: vec!["observation:evidence".into()],
        observed_at_ms: 100,
    };
    let request = HarnessRepairRequest {
        problem: problem.clone(),
        observation: observation.clone(),
        matched_experience_count: 0,
        experiences: Vec::new(),
        harness_id: "harness".into(),
        delegation: "bounded repair".into(),
        target: TargetBinding {
            target_id: "target".into(),
            executor_id: "executor".into(),
            allowed_action_kinds: vec!["repair".into()],
            verification_profile: "business".into(),
            required_facts: facts.clone(),
            action_timeout_secs: 30,
        },
        max_tool_calls: 4,
        summarize_experience: true,
        assess_scriptability: true,
    };
    assert!(serde_json::to_vec(&request).unwrap().len() <= MAX_REPAIR_REQUEST_BYTES);
    let mut payload = "\u{1f}中\"\\\n".repeat(1000);
    let padding = MAX_ARTIFACT_BYTES - serde_json::to_vec(&payload).unwrap().len();
    payload.push_str(&"x".repeat(padding));
    let action = RepairArtifact {
        id: "action".into(),
        version: 1,
        kind: "repair".into(),
        payload: json!(payload),
        preconditions: facts,
        generated_by_harness: "harness".into(),
        generated_in_session: "operation".into(),
    };
    action.validate().unwrap();
    assert_eq!(
        serde_json::to_vec(&action.payload).unwrap().len(),
        MAX_ARTIFACT_BYTES
    );
    ExperienceJob {
        id: "experience".into(),
        task: RecoveryTask {
            id: "task".into(),
            revision: 5,
            problem,
            stage: RecoveryStage::Unknown,
            approval_id: Some("approval".into()),
            operation: Some(ProposedOperation {
                task_id: "task".into(),
                task_revision: 1,
                operation_id: "operation".into(),
                target: "target".into(),
                action: json!({"kind":"repair_with_harness", "request":request}),
            }),
            observation: Some(observation),
            receipt: Some(RepairReceipt {
                execution_trace: vec![action],
                operation_id: "operation".into(),
                target_id: "target".into(),
                outcome: RepairExecutionOutcome::Unknown,
                executor_stopped: false,
                evidence_refs: vec!["execution:unknown".into()],
                summary: "Executor disconnected".into(),
            }),
            verification: Some(BusinessVerification {
                operation_id: "operation".into(),
                target_id: "target".into(),
                profile: "business".into(),
                healthy: None,
                executor_stopped: false,
                evidence_refs: vec!["business:unknown".into()],
                verified_at_ms: 100,
            }),
            result_check: Some(ResultCheckRecord {
                execution: ExecutionResultCheck {
                    operation_id: "operation".into(),
                    target_id: "target".into(),
                    executor_id: "executor".into(),
                    outcome: CheckedExecution::Unknown,
                    executor_stopped: false,
                    evidence_refs: vec!["checked:unknown".into()],
                    checked_at_ms: 100,
                },
                actor: "operator".into(),
            }),
            note: None,
            created_at_ms: 100,
            updated_at_ms: 100,
        },
        outcome: RepairOutcome::Unknown,
        recorded_at_ms: 100,
        attempt: 1,
        call_id: Some("summary-call".into()),
        report: None,
        last_error: None,
        delivered: false,
    }
}

#[test]
fn summary_retains_unknown_facts_exact_payload_and_original_job() {
    let job = job();
    let before = serde_json::to_value(&job).unwrap();
    let result = build_summary_prompt(&job, "Evidence: ", 64 * 1024).unwrap();
    let context: Value =
        serde_json::from_str(result.prompt.strip_prefix("Evidence: ").unwrap()).unwrap();
    assert_eq!(context["context_complete"], true);
    assert!(result.omitted_fields.is_empty());
    assert_eq!(context["job"]["outcome"], "unknown");
    assert_eq!(context["job"]["stage"], "unknown");
    assert_eq!(context["job"]["call_id"], "summary-call");
    assert_eq!(context["execution"]["outcome"], "unknown");
    assert_eq!(context["execution"]["executor_stopped"], false);
    assert_eq!(context["verification"]["healthy"], Value::Null);
    assert_eq!(context["result_check"]["outcome"], "unknown");
    assert_eq!(context["result_check"]["actor"], "operator");
    assert_eq!(
        context["actual_actions"][0]["payload"],
        before["task"]["receipt"]["execution_trace"][0]["payload"]
    );
    assert_eq!(
        context["operation"]["target_required_facts"]["release"],
        "v1"
    );
    assert_eq!(serde_json::to_value(&job).unwrap(), before);
}

#[test]
fn omission_metadata_remains_budgeted_when_an_earlier_field_nearly_fills_the_prompt() {
    let mut job = job();
    let preconditions = (0..32)
        .map(|index| (format!("condition-{index:02}"), "\u{1f}".repeat(1024)))
        .collect();
    let action = &mut job.task.receipt.as_mut().unwrap().execution_trace[0];
    action.preconditions = preconditions;
    action.validate().unwrap();
    job.task.verification.as_mut().unwrap().evidence_refs = (0..32)
        .map(|index| format!("{index:02}{}", "\u{1f}".repeat(1022)))
        .collect();
    let prefix = format!("{}Evidence: ", "instructions ".repeat(70));
    let mut included = 0;
    let mut omitted = 0;
    // Sweep across the acceptance boundary using legal whole evidence fields.
    // Later large evidence still needs an omission record after this decision.
    for total_bytes in (27_000..=32_700).step_by(31) {
        let evidence: Vec<_> = (0..32)
            .map(|index| {
                let length = total_bytes / 32 + usize::from(index < total_bytes % 32);
                format!("{index:02}{}", "x".repeat(length - 2))
            })
            .collect();
        job.task.receipt.as_mut().unwrap().evidence_refs = evidence.clone();
        let result = build_summary_prompt(&job, &prefix, 64 * 1024).unwrap();
        assert!(result.prompt.len() <= 64 * 1024);
        let context: Value =
            serde_json::from_str(result.prompt.strip_prefix(&prefix).unwrap()).unwrap();
        assert_eq!(context["context_complete"], false);
        assert!(
            result
                .omitted_fields
                .iter()
                .any(|field| field == "verification.evidence_refs")
        );
        assert!(
            result
                .omitted_fields
                .iter()
                .any(|field| field == "actual_actions.preconditions")
        );
        if context["execution"].get("evidence_refs").is_some() {
            included += 1;
            assert_eq!(context["execution"]["evidence_refs"], json!(evidence));
        } else {
            omitted += 1;
            assert!(
                result
                    .omitted_fields
                    .iter()
                    .any(|field| field == "execution.evidence_refs")
            );
        }
        assert_eq!(
            context["actual_actions"][0]["payload"],
            job.task.receipt.as_ref().unwrap().execution_trace[0].payload
        );
    }
    assert!(included > 0 && omitted > 0);
}

#[test]
fn error_report_origin_and_original_evidence_reach_the_summary_context() {
    let mut job = job();
    job.task.problem.origin = ProblemOrigin::ErrorLog;
    job.task.problem.summary = "Original Node error\nsecond line".into();
    job.task.problem.report = Some(ErrorLogEvidence {
        source_id: "source".into(),
        generation: "generation-1".into(),
        record_id: "error-1".into(),
        sequence: 1,
        age_ms: 60_000,
        evidence: json!({"trace":"original trace","details":{"code":42}}),
    });
    let before = serde_json::to_value(&job).unwrap();
    let result = build_summary_prompt(&job, "Evidence: ", 64 * 1024).unwrap();
    let context: Value =
        serde_json::from_str(result.prompt.strip_prefix("Evidence: ").unwrap()).unwrap();
    assert_eq!(context["fault"]["origin"], "error_log");
    assert_eq!(context["fault"]["summary"], job.task.problem.summary);
    assert_eq!(
        context["fault"]["report"],
        before["task"]["problem"]["report"]
    );
    assert!(result.omitted_fields.is_empty());
    assert_eq!(serde_json::to_value(&job).unwrap(), before);
}
