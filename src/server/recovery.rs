//! Authenticated management of Host recovery facts; the scheduler owns dispatch.
use super::*;
use crate::recovery::RecoveryTask;
use recuvora_core::recovery::knowledge::KnowledgeQuery;

pub(super) async fn status(State(state): State<Arc<Console>>) -> Result<Json<Value>, ApiError> {
    state.require("recovery.read")?;
    let (running, last_error) = state.application.recovery_status().await?;
    Ok(Json(
        json!({"running":running,"last_error":last_error,"auto_retry":false}),
    ))
}

fn service(state: &Console) -> Result<Arc<RecoveryService>, ApiError> {
    state
        .application
        .recovery()
        .cloned()
        .ok_or_else(|| ApiError::unavailable("recovery service is not configured"))
}

fn task_view(task: &RecoveryTask) -> Result<Value, ApiError> {
    Ok(serde_json::to_value(task)?)
}

fn existing(
    recovery: &RecoveryService,
    id: &str,
) -> Result<crate::recovery::RecoveryTask, ApiError> {
    if !valid_id(id) {
        return Err(ApiError::invalid("invalid recovery task ID"));
    }
    recovery.query(id)?.ok_or_else(|| {
        ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            "recovery task not found",
        )
    })
}

pub(super) async fn tasks(
    State(state): State<Arc<Console>>,
    Query(query): Query<history::ListQuery>,
) -> Result<Json<Value>, ApiError> {
    state.require("recovery.read")?;
    let items = service(&state)?
        .tasks()?
        .into_iter()
        .map(|task| {
            json!({
                "id":task.id,"taskId":task.id,"revision":task.revision,"state":task.stage,
                "target_id":task.problem.target_id,"incident_id":task.problem.incident_id,
                "incident_revision":task.problem.incident_revision,"approval_id":task.approval_id,
                "updated_at_ms":task.updated_at_ms,"detail":false,
            })
        })
        .collect();
    Ok(Json(history::page(items, "id", &query)?))
}

pub(super) async fn task(
    State(state): State<Arc<Console>>,
    RoutePath(id): RoutePath<String>,
) -> Result<Json<Value>, ApiError> {
    state.require("recovery.read")?;
    let recovery = service(&state)?;
    Ok(Json(
        json!({"task":task_view(&existing(&recovery, &id)?)?,"experience_jobs":recovery.pending_experiences()?.into_iter().filter(|job| job.task.id == id).map(|job| json!({"id":job.id,"attempt":job.attempt,"pending":true,"summarized":job.report.is_some(),"last_error":job.last_error})).collect::<Vec<_>>(),"auto_retry":false}),
    ))
}

pub(super) async fn approval(
    State(state): State<Arc<Console>>,
    RoutePath(id): RoutePath<String>,
) -> Result<Json<Value>, ApiError> {
    state.require("recovery.read")?;
    let recovery = service(&state)?;
    existing(&recovery, &id)?;
    Ok(Json(
        json!({"record":recovery.approval(&id)?,"auto_retry":false}),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct HumanDecision {
    revision: u64,
    decision: ApprovalDecision,
    reason: String,
}

pub(super) async fn decision(
    State(state): State<Arc<Console>>,
    RoutePath(id): RoutePath<String>,
    Json(input): Json<HumanDecision>,
) -> Result<Json<Value>, ApiError> {
    state.require("recovery.read")?;
    state.require("recovery.decide")?;
    let recovery = service(&state)?;
    existing(&recovery, &id)?;
    let record = recovery.decide_human(
        &id,
        input.revision,
        input.decision,
        state.config.operator.clone(),
        input.reason,
    )?;
    Ok(Json(json!({"record":record,"auto_retry":false})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RevisionInput {
    revision: u64,
}

pub(super) async fn resume(
    State(state): State<Arc<Console>>,
    RoutePath(id): RoutePath<String>,
    Json(input): Json<RevisionInput>,
) -> Result<Json<Value>, ApiError> {
    state.require("recovery.read")?;
    state.require("recovery.resume")?;
    let recovery = service(&state)?;
    existing(&recovery, &id)?;
    Ok(Json(
        json!({"task":task_view(&recovery.resume(&id, input.revision)?)?,"auto_retry":false}),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CheckResultInput {
    operation_id: String,
    revision: u64,
}

pub(super) async fn check_result(
    State(state): State<Arc<Console>>,
    RoutePath(id): RoutePath<String>,
    Json(input): Json<CheckResultInput>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    state.require("recovery.read")?;
    state.require("recovery.check_result")?;
    let recovery = service(&state)?;
    existing(&recovery, &id)?;
    state.application.start_recovery_result_check(
        input.operation_id.clone(),
        id,
        input.revision,
        state.config.operator.clone(),
    )?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({"operation_id":input.operation_id,"auto_retry":false})),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct KnowledgeInput {
    conditions: BTreeMap<String, String>,
    #[serde(default)]
    keywords: Vec<String>,
    limit: usize,
}

pub(super) async fn knowledge(
    State(state): State<Arc<Console>>,
    Json(input): Json<KnowledgeInput>,
) -> Result<Json<Value>, ApiError> {
    state.require("knowledge.read")?;
    if !(1..=100).contains(&input.limit)
        || input.conditions.is_empty()
        || input.conditions.len() > 32
        || input.keywords.len() > 32
        || input.conditions.iter().any(|(key, value)| {
            key.trim().is_empty()
                || key.len() > 128
                || value.trim().is_empty()
                || value.len() > 1024
                || key.contains('\0')
                || value.contains('\0')
        })
        || input
            .keywords
            .iter()
            .any(|word| word.trim().is_empty() || word.len() > 128 || word.contains('\0'))
    {
        return Err(ApiError::invalid("invalid bounded knowledge query"));
    }
    let query = KnowledgeQuery {
        conditions: input.conditions,
        keywords: input.keywords,
        limit: input.limit,
    };
    let recovery = service(&state)?;
    let experiences = recovery.experiences(&query)?;
    Ok(Json(
        json!({"experiences":experiences,"limit":input.limit,"auto_retry":false}),
    ))
}
