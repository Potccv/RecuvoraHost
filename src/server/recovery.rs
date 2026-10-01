//! Authenticated management of Core recovery facts; the scheduler owns dispatch.
use super::*;
use crate::integrations::recovery::{NodeRepairBackend, RecoveryTask};
use recuvora_core::recovery::knowledge::KnowledgeQuery;

pub(super) async fn status(State(state): State<Arc<Console>>) -> Result<Json<Value>, ApiError> {
    state.require("recovery.read")?;
    let host = state.host.lock().await;
    let scheduler = host
        .recovery_scheduler()
        .ok_or_else(|| ApiError::unavailable("recovery service is not configured"))?;
    Ok(Json(
        json!({"running":scheduler.is_running(),"last_error":scheduler.last_error()?,"auto_retry":false}),
    ))
}

fn service(state: &Console) -> Result<Arc<RecoveryService>, ApiError> {
    state
        .recovery
        .clone()
        .ok_or_else(|| ApiError::unavailable("recovery service is not configured"))
}

fn task_view(task: &RecoveryTask) -> Result<Value, ApiError> {
    Ok(serde_json::to_value(task)?)
}

fn existing(
    recovery: &RecoveryService,
    id: &str,
) -> Result<crate::integrations::recovery::RecoveryTask, ApiError> {
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
        json!({"task":task_view(&existing(&recovery, &id)?)?,"auto_retry":false}),
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
    let task = existing(&recovery, &id)?;
    if task.revision != input.revision
        || task.stage != crate::integrations::recovery::RecoveryStage::Unknown
    {
        return Err(ApiError::conflict(
            "result checking requires the current Unknown task revision",
        ));
    }
    let backend = NodeRepairBackend::new(
        state
            .registry
            .clone()
            .ok_or_else(|| ApiError::unavailable("Harness unavailable"))?,
        state
            .extensions
            .clone()
            .ok_or_else(|| ApiError::unavailable("extension service unavailable"))?,
    );
    let cancellation = state.begin(
        input.operation_id.clone(),
        "recovery_check_result",
        json!({"taskId":id,"revision":input.revision}),
    )?;
    let operation_id = input.operation_id.clone();
    tokio::spawn(async move {
        let result = backend
            .check_task_result(
                &recovery,
                &id,
                input.revision,
                state.config.operator.clone(),
                cancellation,
            )
            .await;
        state.finish(
            &operation_id,
            result.map_err(|error| error.to_string()).and_then(|task| {
                task_view(&task)
                    .map(|task| json!({"task":task,"auto_retry":false}))
                    .map_err(|error| error.to_string())
            }),
        );
    });
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
    let records = service(&state)?.knowledge(&KnowledgeQuery {
        conditions: input.conditions,
        keywords: input.keywords,
        limit: input.limit,
    })?;
    Ok(Json(
        json!({"items":records,"limit":input.limit,"auto_retry":false}),
    ))
}
