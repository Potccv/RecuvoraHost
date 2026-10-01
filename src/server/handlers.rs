//! HTTP input validation and dispatch into trusted host services.
use super::*;

pub(super) async fn bootstrap(State(state): State<Arc<Console>>) -> Result<Json<Value>, ApiError> {
    Ok(Json(views::bootstrap(&state)?))
}

#[derive(Deserialize)]
pub(super) struct WorkspaceQuery {
    workspace_id: String,
}
pub(super) async fn projects(
    State(state): State<Arc<Console>>,
    RoutePath(id): RoutePath<String>,
    Query(query): Query<WorkspaceQuery>,
) -> Result<Json<Value>, ApiError> {
    state.require("harness.projects")?;
    let h = state.definition(&id)?;
    let workspace = state.workspace(&h, &query.workspace_id)?;
    let request = if let Some(node) = h.address.strip_prefix("node://") {
        HarnessProjectListRequest::remote(node, &query.workspace_id)
    } else {
        HarnessProjectListRequest::new(workspace)
    };
    let registry = state
        .registry
        .as_ref()
        .ok_or_else(|| ApiError::unavailable("Harness unavailable"))?;
    let result = registry
        .list_projects(Some(&id), request)
        .await
        .map_err(|e| ApiError::unavailable(e.to_string()))?;
    let projects: Vec<_> = result
        .iter()
        .map(|p| json!({"id":p.id,"name":p.name,"roots":p.roots}))
        .collect();
    Ok(Json(
        json!({"projects":projects,"workspace_id":query.workspace_id}),
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct HarnessInput {
    operation_id: String,
    workspace_id: String,
    prompt: String,
    visibility: String,
    project_id: Option<String>,
    model: Option<String>,
    timeout_secs: u64,
}
pub(super) async fn harness_run(
    State(state): State<Arc<Console>>,
    RoutePath(id): RoutePath<String>,
    Json(input): Json<HarnessInput>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    state.require("harness.run")?;
    let h = state.definition(&id)?;
    let workspace = state.workspace(&h, &input.workspace_id)?;
    if input.prompt.trim().is_empty()
        || input.prompt.len() > 65536
        || !(1..=1800).contains(&input.timeout_secs)
    {
        return Err(ApiError::invalid("invalid prompt or timeout"));
    }
    let mut request = if let Some(node) = h.address.strip_prefix("node://") {
        HarnessRunRequest::remote(node, &input.workspace_id, input.prompt)
    } else {
        HarnessRunRequest::new(workspace, input.prompt)
    };
    let visibility = match input.visibility.as_str() {
        "hidden" => ConversationVisibility::Hidden,
        "client" => ConversationVisibility::Client,
        _ => return Err(ApiError::invalid("visibility must be hidden or client")),
    };
    request = request
        .with_visibility(visibility)
        .with_timeout(Duration::from_secs(input.timeout_secs));
    if let Some(project) = input.project_id {
        request = request.with_placement(ConversationPlacement::ExistingProject {
            harness_id: id.clone(),
            project_id: project,
        });
    }
    if let Some(model) = input.model {
        request = request.with_model(model);
    }
    let token = state.begin(
        input.operation_id.clone(),
        "harness",
        json!({"harnessId":id,"workspaceId":input.workspace_id}),
    )?;
    let op = input.operation_id.clone();
    let registry = state
        .registry
        .clone()
        .ok_or_else(|| ApiError::unavailable("Harness unavailable"))?;
    tokio::spawn(async move {
        let result = registry
            .run(Some(&id), request.with_cancellation(token))
            .await;
        state.finish(&op,Ok(match result {Ok(r)=>json!({"status":"completed","harness_id":r.harness_id,"adapter":r.adapter,"address":r.address,"session_id":r.session_id,"thread_id":r.thread_id,"cwd":r.project_directory,"visibility":match r.visibility {ConversationVisibility::Hidden=>"hidden",ConversationVisibility::Client=>"client"},"final_response":r.final_response,"native_project_id":r.native_project_id,"client_project_grouping":crate::presentation::grouping_json(&r.client_project_grouping),"business_verified":false}),Err(e)=>crate::presentation::error_json(&e)}));
    });
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({"operation_id":input.operation_id,"auto_retry":false})),
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RepairInput {
    operation_id: String,
    task_id: String,
    prompt: String,
    incident_id: Option<String>,
    incident_revision: Option<u64>,
}
pub(super) async fn repair_run(
    State(state): State<Arc<Console>>,
    Json(input): Json<RepairInput>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    state.require("repair.run")?;
    let repair = state
        .repair
        .clone()
        .ok_or_else(|| ApiError::unavailable("repair is not configured"))?;
    if !valid_id(&input.task_id) || input.prompt.trim().is_empty() || input.prompt.len() > 8192 {
        return Err(ApiError::invalid("invalid repair task or prompt"));
    }
    if repair
        .records()?
        .iter()
        .any(|record| record.request.operation.task_id == input.task_id)
    {
        return Err(ApiError::conflict(
            "repair task_id already has persistent approvals; inspect its existing requests instead of starting another run",
        ));
    }
    let source_incident = monitoring::repair_source_incident(
        &state,
        input.incident_id.as_deref(),
        input.incident_revision,
    )?;
    let target = state.repair_config.as_ref().map(|config| &config.target_id);
    let token = state.begin(
        input.operation_id.clone(),
        "repair",
        json!({"taskId":input.task_id,"target":target,"sourceIncident":source_incident}),
    )?;
    let id = input.operation_id.clone();
    tokio::spawn(async move {
        let result = repair.run(input.task_id, input.prompt, token).await;
        state.finish(
            &id,
            result
                .map(|result| crate::presentation::repair_result_json(&result))
                .map_err(|e| e.to_string()),
        );
    });
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({"operation_id":input.operation_id,"auto_retry":false})),
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DecisionInput {
    revision: u64,
    reason: String,
}
pub(super) async fn approval(
    State(state): State<Arc<Console>>,
    RoutePath((id, action)): RoutePath<(String, String)>,
    Json(input): Json<DecisionInput>,
) -> Result<Json<Value>, ApiError> {
    state.require(match action.as_str() {
        "apply" => "approval.apply",
        "check_result" => "approval.check_result",
        _ => "approval.decide",
    })?;
    if input.reason.trim().is_empty() || input.reason.len() > 4096 {
        return Err(ApiError::invalid("bounded nonempty reason required"));
    }
    let repair = state
        .repair
        .clone()
        .ok_or_else(|| ApiError::unavailable("repair is not configured"))?;
    let actor = state.config.operator.clone();
    let record = tokio::task::spawn_blocking(move || match action.as_str() {
        "approve" => repair.decide_authenticated(
            &id,
            input.revision,
            &actor,
            ApprovalDecision::Approve,
            input.reason,
        ),
        "deny" => repair.decide_authenticated(
            &id,
            input.revision,
            &actor,
            ApprovalDecision::Deny,
            input.reason,
        ),
        "revoke" => repair.revoke_authenticated(&id, input.revision, &actor, input.reason),
        "apply" => repair.apply_versioned(&id, Some(input.revision), &HarnessCancellation::new()),
        "check_result" => repair.check_result_authenticated(&id, Some(input.revision), &actor),
        _ => Err(WorkflowError::Invalid("unknown approval action".into())),
    })
    .await
    .map_err(|e| ApiError::unavailable(e.to_string()))??;
    Ok(Json(
        json!({"record":record,"business_verified":false,"auto_retry":false}),
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SimulationInput {
    operation_id: String,
    task_id: String,
    target: String,
    scenario: String,
    timeout_ms: u64,
}
pub(super) async fn simulation(
    State(state): State<Arc<Console>>,
    Json(input): Json<SimulationInput>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    state.require("simulation.run")?;
    let sim = match input.scenario.as_str() {
        "succeed" => Simulation::Succeed,
        "fail" => Simulation::Fail,
        "hang" => Simulation::Hang,
        "exit" => Simulation::Exit,
        "verification_failed" => Simulation::VerificationFailed,
        _ => return Err(ApiError::invalid("unknown closed simulation scenario")),
    };
    if !valid_id(&input.task_id)
        || !valid_id(&input.target)
        || !(1..=60000).contains(&input.timeout_ms)
    {
        return Err(ApiError::invalid("invalid simulation input"));
    }
    let token = state.begin(
        input.operation_id.clone(),
        "simulation",
        json!({"taskId":input.task_id,"target":input.target,"scenario":input.scenario}),
    )?;
    let id = input.operation_id.clone();
    tokio::spawn(async move {
        let engine = state.simulation.clone();
        let result=async {
            let mut snapshot=engine.submit(TaskSpec::simulated(&input.task_id,&input.target,sim).authorize_simulation().with_timeout(Duration::from_millis(input.timeout_ms))).await?;
            while !snapshot.state.is_terminal() {
                tokio::select! {
                    _=token.cancelled()=> {if let Some(next)=engine.cancel(&input.task_id).await? {snapshot=next;}},
                    _=tokio::time::sleep(Duration::from_millis(50))=> {if let Some(next)=engine.query(&input.task_id).await? {snapshot=next;}},
                }
            }
            Ok::<_,crate::simulation::EngineError>(json!({"status":match snapshot.state {crate::simulation::TaskState::Unknown=>"unknown",crate::simulation::TaskState::Canceled=>"canceled",_=>"completed"},"task":snapshot}))
        }.await;
        state.finish(&id, result.map_err(|e| e.to_string()));
    });
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({"operation_id":input.operation_id,"auto_retry":false})),
    ))
}
pub(super) async fn operation(
    State(state): State<Arc<Console>>,
    RoutePath(id): RoutePath<String>,
) -> Result<Json<Value>, ApiError> {
    let journal = lock(&state.journal)?;
    let op = journal.records.get(&id).ok_or_else(|| {
        ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            "operation not found; acceptance is not confirmed",
        )
    })?;
    Ok(Json(json!(op)))
}
pub(super) async fn cancel(
    State(state): State<Arc<Console>>,
    RoutePath(id): RoutePath<String>,
) -> Result<Json<Value>, ApiError> {
    state.require("operation.cancel")?;
    let calls = lock(&state.calls)?;
    if let Some(token) = calls.get(&id) {
        token.cancel();
        Ok(Json(
            json!({"operation_id":id,"status":"cancel_requested","auto_retry":false}),
        ))
    } else {
        Err(ApiError::conflict(
            "operation is no longer active; query its recorded outcome",
        ))
    }
}
#[derive(Default, Deserialize)]
pub(super) struct LogsQuery {
    level: Option<String>,
    source: Option<String>,
    task_id: Option<String>,
    operation_id: Option<String>,
    query: Option<String>,
    cursor: Option<usize>,
    limit: Option<usize>,
}
pub(super) async fn logs(
    State(state): State<Arc<Console>>,
    Query(query): Query<LogsQuery>,
) -> Result<Json<Value>, ApiError> {
    state.require("logs.read")?;
    let journal = lock(&state.journal)?;
    let mut rows = Vec::new();
    let start = query
        .cursor
        .unwrap_or(journal.events.len())
        .min(journal.events.len());
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    let mut next = None;
    for index in (0..start).rev() {
        let op = &journal.events[index];
        let level = if op.status == "unknown" || op.status == "failed" {
            "error"
        } else {
            "info"
        };
        let message = op
            .error
            .clone()
            .unwrap_or_else(|| format!("{} {}", op.kind, op.status));
        if query.level.as_ref().is_some_and(|q| q != level)
            || query.source.as_ref().is_some_and(|q| q != &op.kind)
            || query.operation_id.as_ref().is_some_and(|q| q != &op.id)
            || query
                .task_id
                .as_ref()
                .is_some_and(|q| op.context.get("taskId").and_then(Value::as_str) != Some(q))
            || query.query.as_ref().is_some_and(|q| !message.contains(q))
        {
            continue;
        }
        rows.push(json!({"id":format!("event-{index}"),"timestamp":op.updated_at,"level":level,"source":op.kind,"taskId":op.context.get("taskId"),"operationId":op.id,"message":message}));
        if rows.len() == limit {
            next = Some(index);
            break;
        }
    }
    Ok(Json(json!({"items":rows,"next_cursor":next})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ExtensionInput {
    contract: String,
    version: u32,
    method: String,
    params: Value,
}
pub(super) async fn extension_query(
    State(state): State<Arc<Console>>,
    RoutePath(id): RoutePath<String>,
    Json(input): Json<ExtensionInput>,
) -> Result<Json<Value>, ApiError> {
    state.require("extension.read")?;
    let extensions = state
        .extensions
        .as_ref()
        .ok_or_else(|| ApiError::unavailable("extensions not configured"))?;
    let result = extensions
        .call_read_only(
            &id,
            &input.contract,
            input.version,
            &input.method,
            input.params,
            Duration::from_secs(30),
            HarnessCancellation::new(),
        )
        .await
        .map_err(|e| ApiError::unavailable(e.to_string()))?;
    Ok(Json(json!({"result":result,"auto_retry":false})))
}

macro_rules! list_handler {
    ($name:ident, $view:ident) => {
        pub(super) async fn $name(
            State(state): State<Arc<Console>>,
            Query(query): Query<history::ListQuery>,
        ) -> Result<Json<Value>, ApiError> {
            Ok(Json(history::$view(&state, &query)?))
        }
    };
}
list_handler!(operation_list, operation_page);
list_handler!(repair_list, repair_page);
list_handler!(approval_list, approval_page);
list_handler!(simulation_list, simulation_page);

pub(super) async fn repair_detail(
    State(state): State<Arc<Console>>,
    RoutePath(id): RoutePath<String>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(history::repair_detail(&state, &id)?))
}
pub(super) async fn approval_detail(
    State(state): State<Arc<Console>>,
    RoutePath(id): RoutePath<String>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(history::approval_detail(&state, &id)?))
}
