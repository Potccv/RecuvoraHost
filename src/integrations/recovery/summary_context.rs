//! Bounded, read-only projection for the non-authoritative summary session.
use super::{RecoveryError, service};
use recuvora_core::recovery::{
    knowledge::{RepairArtifact, RepairOutcome},
    planning::{ErrorLogEvidence, ProblemOrigin},
    workflow::{
        BusinessVerification, CheckedExecution, ExperienceJob, HarnessRepairRequest, RecoveryStage,
        RepairExecutionOutcome,
    },
};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;

#[cfg(test)]
#[path = "../../../tests/summary_context.rs"]
mod tests;

#[derive(Clone, Debug)]
pub(super) struct SummaryPrompt {
    pub(super) prompt: String,
    pub(super) omitted_fields: Vec<String>,
}

#[derive(Clone, Serialize)]
struct SummaryContext {
    schema_version: u32,
    context_complete: bool,
    job: SummaryJob,
    fault: SummaryFault,
    operation: SummaryOperation,
    actual_actions: Vec<SummaryAction>,
    execution: SummaryExecution,
    verification: Option<SummaryVerification>,
    result_check: Option<SummaryResultCheck>,
    related_experiences: Vec<RelatedExperience>,
    matched_experience_count: usize,
    omissions: Vec<Omission>,
}

#[derive(Clone, Serialize)]
struct SummaryJob {
    id: String,
    call_id: String,
    recorded_at_ms: u64,
    attempt: u64,
    outcome: RepairOutcome,
    stage: RecoveryStage,
}

#[derive(Clone, Serialize)]
struct SummaryFault {
    origin: ProblemOrigin,
    incident_id: String,
    incident_revision: u64,
    target_id: String,
    fingerprint: String,
    occurrences: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    report: Option<ErrorLogEvidence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    evidence_refs: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    conditions: Option<BTreeMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    keywords: Option<Vec<String>>,
}

#[derive(Clone, Serialize)]
struct SummaryOperation {
    task_id: String,
    task_revision: u64,
    operation_id: String,
    target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    target_required_facts: Option<BTreeMap<String, String>>,
}

#[derive(Clone, Serialize)]
struct SummaryAction {
    id: String,
    version: u64,
    kind: String,
    payload: Value,
    generated_by_harness: String,
    generated_in_session: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    preconditions: Option<BTreeMap<String, String>>,
}

impl SummaryAction {
    fn required(value: &RepairArtifact) -> Self {
        Self {
            id: value.id.clone(),
            version: value.version,
            kind: value.kind.clone(),
            payload: value.payload.clone(),
            generated_by_harness: value.generated_by_harness.clone(),
            generated_in_session: value.generated_in_session.clone(),
            preconditions: None,
        }
    }
}

#[derive(Clone, Serialize)]
struct SummaryExecution {
    operation_id: String,
    target_id: String,
    outcome: RepairExecutionOutcome,
    executor_stopped: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    evidence_refs: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<String>,
}

#[derive(Clone, Serialize)]
struct SummaryVerification {
    operation_id: String,
    target_id: String,
    profile: String,
    healthy: Option<bool>,
    executor_stopped: bool,
    verified_at_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    evidence_refs: Option<Vec<String>>,
}

impl SummaryVerification {
    fn required(value: &BusinessVerification) -> Self {
        Self {
            operation_id: value.operation_id.clone(),
            target_id: value.target_id.clone(),
            profile: value.profile.clone(),
            healthy: value.healthy,
            executor_stopped: value.executor_stopped,
            verified_at_ms: value.verified_at_ms,
            evidence_refs: None,
        }
    }
}

#[derive(Clone, Serialize)]
struct SummaryResultCheck {
    operation_id: String,
    target_id: String,
    executor_id: String,
    outcome: CheckedExecution,
    executor_stopped: bool,
    checked_at_ms: u64,
    actor: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    evidence_refs: Option<Vec<String>>,
}

#[derive(Clone, Serialize)]
struct RelatedExperience {
    id: String,
    outcome: RepairOutcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<String>,
}

#[derive(Clone, Serialize)]
struct Omission {
    field: String,
    omitted_items: usize,
    encoded_bytes: usize,
}

pub(super) fn build_summary_prompt(
    job: &ExperienceJob,
    prefix: &str,
    max_bytes: usize,
) -> Result<SummaryPrompt, RecoveryError> {
    let operation = job
        .task
        .operation
        .as_ref()
        .ok_or_else(|| service("missing repair operation"))?;
    let receipt = job
        .task
        .receipt
        .as_ref()
        .ok_or_else(|| service("missing repair receipt"))?;
    if receipt.execution_trace.len() > 1 {
        return Err(RecoveryError::Invalid(
            "summary received more than one actual repair action".into(),
        ));
    }
    let request_value = operation
        .action
        .get("request")
        .cloned()
        .ok_or_else(|| RecoveryError::Invalid("missing repair request".into()))?;
    let request: HarnessRepairRequest = serde_json::from_value(request_value)
        .map_err(|_| RecoveryError::Invalid("invalid repair request".into()))?;
    let call_id = job
        .call_id
        .clone()
        .ok_or_else(|| service("missing summary call identity"))?;
    let summaries: Vec<_> = request
        .experiences
        .iter()
        .map(|value| value.report.summary.clone())
        .collect();
    let mut omissions = vec![
        omission(
            "execution.evidence_refs",
            receipt.evidence_refs.len(),
            &receipt.evidence_refs,
        )?,
        omission(
            "fault.evidence_refs",
            job.task.problem.evidence_refs.len(),
            &job.task.problem.evidence_refs,
        )?,
        omission("fault.summary", 1, &job.task.problem.summary)?,
        omission("execution.summary", 1, &receipt.summary)?,
        omission(
            "fault.conditions",
            job.task.problem.conditions.len(),
            &job.task.problem.conditions,
        )?,
        omission(
            "fault.keywords",
            job.task.problem.keywords.len(),
            &job.task.problem.keywords,
        )?,
        omission(
            "operation.target_required_facts",
            request.target.required_facts.len(),
            &request.target.required_facts,
        )?,
    ];
    if let Some(report) = &job.task.problem.report {
        omissions.push(omission("fault.report", 1, report)?);
    }
    if let Some(verification) = &job.task.verification {
        omissions.push(omission(
            "verification.evidence_refs",
            verification.evidence_refs.len(),
            &verification.evidence_refs,
        )?);
    }
    if let Some(result_check) = &job.task.result_check {
        omissions.push(omission(
            "result_check.evidence_refs",
            result_check.execution.evidence_refs.len(),
            &result_check.execution.evidence_refs,
        )?);
    }
    for action in &receipt.execution_trace {
        omissions.push(omission(
            "actual_actions.preconditions",
            action.preconditions.len(),
            &action.preconditions,
        )?);
    }
    if !summaries.is_empty() {
        omissions.push(omission(
            "related_experiences.summary",
            summaries.len(),
            &summaries,
        )?);
    }
    let result_check = job
        .task
        .result_check
        .as_ref()
        .map(|value| SummaryResultCheck {
            operation_id: value.execution.operation_id.clone(),
            target_id: value.execution.target_id.clone(),
            executor_id: value.execution.executor_id.clone(),
            outcome: value.execution.outcome,
            executor_stopped: value.execution.executor_stopped,
            checked_at_ms: value.execution.checked_at_ms,
            actor: value.actor.clone(),
            evidence_refs: None,
        });
    let mut context = SummaryContext {
        schema_version: 1,
        context_complete: omissions.is_empty(),
        job: SummaryJob {
            id: job.id.clone(),
            call_id,
            recorded_at_ms: job.recorded_at_ms,
            attempt: job.attempt,
            outcome: job.outcome,
            stage: job.task.stage.clone(),
        },
        fault: SummaryFault {
            origin: job.task.problem.origin,
            incident_id: job.task.problem.incident_id.clone(),
            incident_revision: job.task.problem.incident_revision,
            target_id: job.task.problem.target_id.clone(),
            fingerprint: job.task.problem.fingerprint.clone(),
            occurrences: job.task.problem.occurrences,
            summary: None,
            report: None,
            evidence_refs: None,
            conditions: None,
            keywords: None,
        },
        operation: SummaryOperation {
            task_id: operation.task_id.clone(),
            task_revision: operation.task_revision,
            operation_id: operation.operation_id.clone(),
            target: operation.target.clone(),
            target_required_facts: None,
        },
        actual_actions: receipt
            .execution_trace
            .iter()
            .map(SummaryAction::required)
            .collect(),
        execution: SummaryExecution {
            operation_id: receipt.operation_id.clone(),
            target_id: receipt.target_id.clone(),
            outcome: receipt.outcome.clone(),
            executor_stopped: receipt.executor_stopped,
            evidence_refs: None,
            summary: None,
        },
        verification: job
            .task
            .verification
            .as_ref()
            .map(SummaryVerification::required),
        result_check,
        related_experiences: request
            .experiences
            .iter()
            .map(|value| RelatedExperience {
                id: value.id.clone(),
                outcome: value.outcome,
                summary: None,
            })
            .collect(),
        matched_experience_count: request.matched_experience_count,
        omissions,
    };
    ensure_fits(&context, prefix, max_bytes)?;

    include_optional(
        &mut context,
        prefix,
        max_bytes,
        "execution.evidence_refs",
        |candidate| candidate.execution.evidence_refs = Some(receipt.evidence_refs.clone()),
    )?;
    if let Some(verification) = &job.task.verification {
        include_optional(
            &mut context,
            prefix,
            max_bytes,
            "verification.evidence_refs",
            |candidate| {
                if let Some(projected) = candidate.verification.as_mut() {
                    projected.evidence_refs = Some(verification.evidence_refs.clone());
                }
            },
        )?;
    }
    if let Some(result_check) = &job.task.result_check {
        include_optional(
            &mut context,
            prefix,
            max_bytes,
            "result_check.evidence_refs",
            |candidate| {
                if let Some(projected) = candidate.result_check.as_mut() {
                    projected.evidence_refs = Some(result_check.execution.evidence_refs.clone());
                }
            },
        )?;
    }
    include_optional(
        &mut context,
        prefix,
        max_bytes,
        "fault.evidence_refs",
        |candidate| candidate.fault.evidence_refs = Some(job.task.problem.evidence_refs.clone()),
    )?;
    include_optional(
        &mut context,
        prefix,
        max_bytes,
        "fault.summary",
        |candidate| candidate.fault.summary = Some(job.task.problem.summary.clone()),
    )?;
    if let Some(report) = &job.task.problem.report {
        include_optional(
            &mut context,
            prefix,
            max_bytes,
            "fault.report",
            |candidate| {
                candidate.fault.report = Some(report.clone());
            },
        )?;
    }
    for (index, action) in receipt.execution_trace.iter().enumerate() {
        include_optional(
            &mut context,
            prefix,
            max_bytes,
            "actual_actions.preconditions",
            |candidate| {
                candidate.actual_actions[index].preconditions = Some(action.preconditions.clone());
            },
        )?;
    }
    include_optional(
        &mut context,
        prefix,
        max_bytes,
        "execution.summary",
        |candidate| candidate.execution.summary = Some(receipt.summary.clone()),
    )?;
    include_optional(
        &mut context,
        prefix,
        max_bytes,
        "fault.conditions",
        |candidate| candidate.fault.conditions = Some(job.task.problem.conditions.clone()),
    )?;
    include_optional(
        &mut context,
        prefix,
        max_bytes,
        "fault.keywords",
        |candidate| candidate.fault.keywords = Some(job.task.problem.keywords.clone()),
    )?;
    include_optional(
        &mut context,
        prefix,
        max_bytes,
        "operation.target_required_facts",
        |candidate| {
            candidate.operation.target_required_facts = Some(request.target.required_facts.clone());
        },
    )?;
    if !summaries.is_empty() {
        include_optional(
            &mut context,
            prefix,
            max_bytes,
            "related_experiences.summary",
            |candidate| {
                for (related, summary) in candidate.related_experiences.iter_mut().zip(&summaries) {
                    related.summary = Some(summary.clone());
                }
            },
        )?;
    }

    let omitted_fields = context
        .omissions
        .iter()
        .map(|value| value.field.clone())
        .collect();
    let encoded = serde_json::to_string(&context)?;
    let prompt = format!("{prefix}{encoded}");
    if prompt.len() > max_bytes {
        return Err(RecoveryError::Capacity);
    }
    Ok(SummaryPrompt {
        prompt,
        omitted_fields,
    })
}

fn include_optional(
    context: &mut SummaryContext,
    prefix: &str,
    max_bytes: usize,
    field: &str,
    include: impl FnOnce(&mut SummaryContext),
) -> Result<(), RecoveryError> {
    let mut candidate = context.clone();
    let omission = candidate
        .omissions
        .iter()
        .position(|value| value.field == field)
        .ok_or_else(|| service("summary omission budget is inconsistent"))?;
    candidate.omissions.remove(omission);
    include(&mut candidate);
    candidate.context_complete = candidate.omissions.is_empty();
    if encoded_prompt_len(&candidate, prefix)? <= max_bytes {
        *context = candidate;
    }
    Ok(())
}

fn omission(
    field: &str,
    omitted_items: usize,
    value: &impl Serialize,
) -> Result<Omission, RecoveryError> {
    Ok(Omission {
        field: field.into(),
        omitted_items,
        encoded_bytes: encoded_len(value)?,
    })
}

fn encoded_len(value: &impl Serialize) -> Result<usize, RecoveryError> {
    Ok(serde_json::to_vec(value)?.len())
}

fn encoded_prompt_len(context: &SummaryContext, prefix: &str) -> Result<usize, RecoveryError> {
    prefix
        .len()
        .checked_add(serde_json::to_vec(context)?.len())
        .ok_or(RecoveryError::Capacity)
}

fn ensure_fits(
    context: &SummaryContext,
    prefix: &str,
    max_bytes: usize,
) -> Result<(), RecoveryError> {
    if encoded_prompt_len(context, prefix)? > max_bytes {
        return Err(RecoveryError::Capacity);
    }
    Ok(())
}
