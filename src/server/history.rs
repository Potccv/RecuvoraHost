//! Bounded list projections and explicit detail reads; durable authority stays in recovery.
use super::*;
use recuvora_core::recovery::approval::{ApprovalRecord, ApprovalState};

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ListQuery {
    pub cursor: Option<String>,
    pub limit: Option<usize>,
    pub state: Option<String>,
    pub query: Option<String>,
    pub task_id: Option<String>,
}

pub(super) fn metadata(page: &Value) -> Value {
    json!({"next_cursor":page["next_cursor"],"total":page["total"],"limit":page["limit"],"order":"id_desc"})
}

fn bounded(value: &Value) -> Value {
    value
        .as_str()
        .map(|text| json!(text.chars().take(256).collect::<String>()))
        .unwrap_or(Value::Null)
}

pub(super) fn page(mut items: Vec<Value>, key: &str, query: &ListQuery) -> Result<Value, ApiError> {
    if query.cursor.as_ref().is_some_and(|s| s.len() > 256)
        || query.query.as_ref().is_some_and(|s| s.len() > 256)
        || query.state.as_ref().is_some_and(|s| s.len() > 32)
        || query.task_id.as_ref().is_some_and(|s| !valid_id(s))
    {
        return Err(ApiError::invalid("invalid bounded history query"));
    }
    let limit = query.limit.unwrap_or(25);
    if !(1..=100).contains(&limit) {
        return Err(ApiError::invalid("limit must be 1..100"));
    }
    items.retain(|item| {
        let state = item["state"]
            .as_str()
            .or_else(|| item["status"].as_str())
            .unwrap_or_default();
        query.state.as_ref().is_none_or(|filter| {
            filter == "all"
                || filter == state
                || (filter == "attention"
                    && matches!(state, "unknown" | "waiting_human" | "approved" | "pending"))
        }) && query
            .task_id
            .as_ref()
            .is_none_or(|task| item["taskId"].as_str() == Some(task))
            && query.query.as_ref().is_none_or(|text| {
                item.to_string()
                    .to_lowercase()
                    .contains(&text.to_lowercase())
            })
    });
    // IDs are immutable: terminal updates cannot move records between pages.
    // New IDs above the cursor appear when the operator refreshes the first page.
    items.sort_by(|a, b| b[key].as_str().cmp(&a[key].as_str()));
    let total = items.len();
    items.retain(|item| {
        query
            .cursor
            .as_ref()
            .is_none_or(|cursor| item[key].as_str().is_some_and(|id| id < cursor.as_str()))
    });
    let more = items.len() > limit;
    items.truncate(limit);
    let next = if more {
        items.last().map(|item| item[key].clone())
    } else {
        None
    };
    Ok(json!({"items":items,"next_cursor":next,"total":total,"limit":limit,"order":"id_desc"}))
}

#[derive(Clone)]
pub(super) struct ApprovalProjection {
    task_id: String,
    target: Value,
    state: ApprovalState,
    updated_at: u64,
    summary: Value,
}
fn records(state: &Console) -> Result<Vec<ApprovalProjection>, ApiError> {
    Ok(state
        .application
        .text_repair()
        .map(|session| {
            session.records().map(|records| {
                records
                    .iter()
                    .map(|record| ApprovalProjection {
                        task_id: record.request.operation.task_id.clone(),
                        target: bounded(&json!(record.request.operation.target)),
                        state: record.state,
                        updated_at: record.updated_at,
                        summary: approval_summary(record),
                    })
                    .collect()
            })
        })
        .transpose()?
        .unwrap_or_default())
}

fn approval_summary(record: &ApprovalRecord) -> Value {
    let operation = &record.request.operation;
    json!({"requestId":record.request.request_id,"state":record.state,"revision":record.revision,
        "taskId":operation.task_id,"operationId":operation.operation_id,"target":bounded(&json!(operation.target)),
        "path":bounded(&operation.action["path"]),"createdAt":record.request.created_at.saturating_mul(1000),
        "updatedAt":record.updated_at.saturating_mul(1000),"expiresAt":record.request.expires_at.saturating_mul(1000),
        "detail":false})
}

pub(super) fn approval_counts(state: &Console) -> Result<Value, ApiError> {
    let mut counts = BTreeMap::<String, usize>::new();
    for record in records(state)? {
        if let Value::String(name) = json!(record.state) {
            *counts.entry(name).or_default() += 1;
        }
    }
    Ok(json!(counts))
}

pub(super) fn approval_page(state: &Console, query: &ListQuery) -> Result<Value, ApiError> {
    page(
        records(state)?
            .into_iter()
            .map(|record| record.summary)
            .collect(),
        "requestId",
        query,
    )
}

pub(super) fn approval_detail(state: &Console, id: &str) -> Result<Value, ApiError> {
    let session = state
        .application
        .text_repair()
        .ok_or_else(|| ApiError::unavailable("repair is not configured"))?;
    let mut value = views::approval(state, &session.record(id)?);
    value["detail"] = json!(true);
    Ok(value)
}

fn operation_summary(operation: &Operation) -> Value {
    let mut context = serde_json::Map::new();
    for key in ["taskId", "target", "scenario", "harnessId", "workspaceId"] {
        if let Some(value) = operation.context.get(key) {
            context.insert(key.into(), bounded(value));
        }
    }
    json!({"id":operation.id,"kind":operation.kind,"status":operation.status,"updatedAt":operation.updated_at,
        "error":bounded(&json!(operation.error)),"context":context,"auto_retry":false,"detail":false})
}

pub(super) fn operation_page(state: &Console, query: &ListQuery) -> Result<Value, ApiError> {
    page(
        state
            .application
            .operations()?
            .records()
            .values()
            .map(operation_summary)
            .collect(),
        "id",
        query,
    )
}

pub(super) fn repair_page(state: &Console, query: &ListQuery) -> Result<Value, ApiError> {
    let records = records(state)?;
    let journal = state.application.operations()?;
    page(
        repair_summaries(
            state,
            &journal.records().values().collect::<Vec<_>>(),
            &records,
        ),
        "id",
        query,
    )
}

pub(super) fn repair_detail(state: &Console, id: &str) -> Result<Value, ApiError> {
    let records = records(state)?;
    let journal = state.application.operations()?;
    let mut view = repair_summaries(
        state,
        &journal.records().values().collect::<Vec<_>>(),
        &records,
    )
    .into_iter()
    .find(|item| item["id"].as_str() == Some(id))
    .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "not_found", "repair not found"))?;
    let operation = journal
        .records()
        .values()
        .filter(|op| op.kind == "repair" && op.context["taskId"].as_str() == Some(id))
        .max_by_key(|op| (op.updated_at, &op.id));
    view["finalResponse"] = operation
        .and_then(|op| op.result.as_ref())
        .and_then(|result| result.get("final_response"))
        .cloned()
        .unwrap_or(Value::Null);
    view["detail"] = json!(true);
    // Related requests have their own bounded page; no approval text is duplicated here.
    Ok(view)
}

pub(super) fn incident_repairs(state: &Console, id: &str) -> Result<Value, ApiError> {
    let records = records(state)?;
    let journal = state.application.operations()?;
    let tasks = journal
        .records()
        .values()
        .filter(|operation| {
            operation.kind == "repair" && operation.context["sourceIncident"]["id"] == id
        })
        .filter_map(|operation| operation.context["taskId"].as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let mut items = repair_summaries(
        state,
        &journal.records().values().collect::<Vec<_>>(),
        &records,
    );
    items.retain(|item| item["id"].as_str().is_some_and(|id| tasks.contains(id)));
    items.sort_by(|a, b| b["id"].as_str().cmp(&a["id"].as_str()));
    let total = items.len();
    items.truncate(25);
    Ok(json!({"items":items,"total":total,"limit":25}))
}

pub(super) fn simulation_page(state: &Console, query: &ListQuery) -> Result<Value, ApiError> {
    let journal = state.application.operations()?;
    let items = journal.records().values().filter(|op| op.kind == "simulation").map(|op| {
        let task = op.result.as_ref().and_then(|result| result.get("task"));
        let state = task.and_then(|task| task["state"].as_str()).map(|name| match name {
            "TimedOut" => "timed_out".into(), other => other.to_ascii_lowercase()
        }).unwrap_or_else(|| op.status.clone());
        json!({"id":op.id,"taskId":op.context["taskId"],"target":op.context["target"],"scenario":op.context["scenario"],
            "state":if op.status == "unknown" { "unknown" } else { &state },
            "revision":task.and_then(|task| task.get("revision")),"duration":"—"})
    }).collect();
    page(items, "id", query)
}

pub(super) fn repair_summaries(
    state: &Console,
    operations: &[&Operation],
    records: &[ApprovalProjection],
) -> Vec<Value> {
    let mut repairs = BTreeMap::<String, Value>::new();
    let mut ordered = operations.to_vec();
    ordered.sort_by_key(|op| (op.updated_at, &op.id));
    for op in ordered {
        if op.kind == "repair" {
            let task = op.context["taskId"].as_str().unwrap_or(&op.id);
            let result = op.result.as_ref().unwrap_or(&Value::Null);
            repairs.insert(task.to_owned(),json!({"id":task,"target":op.context.get("target").cloned().unwrap_or_else(||json!(state.application.text_repair_config().map(|c|&c.target_id))),
                "status":if op.status=="unknown" {json!("unknown")} else {result.get("status").cloned().unwrap_or(json!(op.status))},"summary":"受限修复流程",
                "operationCount":0,"reviewer":"可信审批服务","updatedAt":op.updated_at,
                "harnessId":result["execution_context"]["harness_id"],
                "threadId":result["execution_context"]["thread_id"],"sessionId":result["execution_context"]["session_id"],
                "businessVerified":false,"detail":false,"sourceIncident":op.context["sourceIncident"]}));
        }
    }
    for record in records {
        let id = &record.task_id;
        let entry=repairs.entry(id.clone()).or_insert_with(||json!({"id":id,"target":record.target,
            "status":"blocked","summary":"持久审批记录中的修复任务","operationCount":0,"reviewer":"可信审批服务",
            "updatedAt":record.updated_at*1000,"harnessId":null,"threadId":null,"sessionId":null,
            "businessVerified":false,"detail":false,"sourceIncident":null}));
        entry["operationCount"] = json!(entry["operationCount"].as_u64().unwrap_or_default() + 1);
        entry["updatedAt"] = json!(
            entry["updatedAt"]
                .as_u64()
                .unwrap_or_default()
                .max(record.updated_at.saturating_mul(1000))
        );
    }
    for (task_id, view) in &mut repairs {
        let related: Vec<_> = records
            .iter()
            .filter(|record| &record.task_id == task_id)
            .collect();
        let running = operations.iter().any(|operation| {
            operation.kind == "repair"
                && operation.status == "running"
                && operation.context["taskId"].as_str() == Some(task_id)
        });
        let uncertain = operations.iter().any(|operation| {
            operation.kind == "repair"
                && operation.status == "unknown"
                && operation.context["taskId"].as_str() == Some(task_id)
        });
        if related.is_empty() {
            if uncertain {
                view["status"] = json!("unknown");
            } else if running {
                view["status"] = json!("running");
            }
            continue;
        }
        // Latest durable approval facts supersede a finished workflow's old
        // waiting-human summary. Transport uncertainty remains conservative.
        let status = if uncertain
            || related
                .iter()
                .any(|record| record.state == ApprovalState::Unknown)
        {
            "unknown"
        } else if running
            || related
                .iter()
                .any(|record| record.state == ApprovalState::Executing)
        {
            "running"
        } else if related.iter().any(|record| {
            matches!(
                record.state,
                ApprovalState::Denied
                    | ApprovalState::Revoked
                    | ApprovalState::Expired
                    | ApprovalState::Failed
            )
        }) {
            "blocked"
        } else if related.iter().any(|record| {
            matches!(
                record.state,
                ApprovalState::Pending | ApprovalState::WaitingHuman | ApprovalState::Approved
            )
        }) {
            "waiting_human"
        } else if related
            .iter()
            .all(|record| record.state == ApprovalState::Executed)
        {
            "completed"
        } else {
            "canceled"
        };
        view["status"] = json!(status);
    }
    repairs
        .into_values()
        .map(|mut view| {
            if let Some(fields) = view.as_object_mut() {
                for value in fields.values_mut() {
                    if value.is_string() {
                        *value = bounded(value);
                    }
                }
            }
            view
        })
        .collect()
}
