//! Read models and authenticated acknowledgement of durable incidents.
use super::*;
use crate::integrations::extensions::MonitoringViewRegistration;
use crate::monitoring::{MonitorError, MonitorSnapshot, MonitoringSnapshot};
use crate::persistence::incidents::{IncidentError, IncidentRecord};
use crate::protocol::{ExtensionError, ExtensionKind, valid_id};
use std::collections::BTreeSet;

const MAX_VIEW_BYTES: usize = 32 * 1024;
const MAX_VIEW_SECTIONS: usize = 8;
const MAX_VIEW_FIELDS: usize = 64;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PluginMonitoringView {
    schema_version: u32,
    title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    summary: Option<String>,
    sections: Vec<PluginMonitoringSection>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PluginMonitoringSection {
    id: String,
    title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    monitor_role: String,
    fields: Vec<PluginMonitoringField>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PluginMonitoringField {
    id: String,
    label: String,
    source: PluginMonitoringSource,
    pointer: String,
    format: PluginMonitoringFormat,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    unit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    empty: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum PluginMonitoringSource {
    Monitor,
    LastValue,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum PluginMonitoringFormat {
    Text,
    Number,
    Boolean,
    TimestampMs,
    DurationMs,
    Json,
}

fn monitor_error(error: MonitorError) -> ApiError {
    match &error {
        MonitorError::Configuration(_)
        | MonitorError::Observation(_)
        | MonitorError::Incident(IncidentError::Invalid(_)) => ApiError::invalid(error.to_string()),
        MonitorError::Incident(IncidentError::Conflict(_)) => ApiError::conflict(error.to_string()),
        MonitorError::Incident(IncidentError::NotFound(_)) => {
            ApiError::new(StatusCode::NOT_FOUND, "not_found", error.to_string())
        }
        _ => ApiError::unavailable(error.to_string()),
    }
}

fn service(state: &Console) -> Result<&MonitorHandle, ApiError> {
    state
        .monitoring
        .as_deref()
        .ok_or_else(|| ApiError::unavailable("monitoring is not configured"))
}

fn incident_counts(items: &[Value]) -> Value {
    let mut counts = json!({"open":0,"acknowledged":0,"resolved":0,"active":0,"total":items.len()});
    for item in items {
        if let Some(status) = item["status"]
            .as_str()
            .filter(|status| matches!(*status, "open" | "acknowledged" | "resolved"))
        {
            counts[status] = json!(counts[status].as_u64().unwrap_or(0) + 1);
            if status != "resolved" {
                counts["active"] = json!(counts["active"].as_u64().unwrap_or(0) + 1);
            }
        }
    }
    counts
}

fn bounded_error(message: impl ToString) -> String {
    message
        .to_string()
        .chars()
        .take(512)
        .map(|character| {
            if character == '\0' {
                '\u{fffd}'
            } else {
                character
            }
        })
        .collect()
}

fn monitoring_view_error(error: ExtensionError) -> (&'static str, String) {
    let status = if matches!(&error, ExtensionError::Protocol(_)) {
        "invalid_response"
    } else {
        "unavailable"
    };
    (status, bounded_error(error))
}

fn bounded_text(value: &str, max_bytes: usize, allow_empty: bool) -> bool {
    (allow_empty || !value.trim().is_empty()) && value.len() <= max_bytes && !value.contains('\0')
}

fn valid_pointer(pointer: &str) -> bool {
    if pointer.len() > 256 || pointer.contains('\0') {
        return false;
    }
    if pointer.is_empty() {
        return true;
    }
    if !pointer.starts_with('/') || pointer.split('/').skip(1).count() > 16 {
        return false;
    }
    let mut chars = pointer.chars();
    while let Some(ch) = chars.next() {
        if ch == '~' && !matches!(chars.next(), Some('0' | '1')) {
            return false;
        }
    }
    true
}

fn validate_view(value: Value) -> Result<PluginMonitoringView, String> {
    if serde_json::to_vec(&value).map_or(true, |bytes| bytes.len() > MAX_VIEW_BYTES) {
        return Err("monitoring view declaration exceeds 32 KiB".into());
    }
    let view: PluginMonitoringView =
        serde_json::from_value(value).map_err(|error| format!("invalid declaration: {error}"))?;
    if view.schema_version != 1
        || !bounded_text(&view.title, 128, false)
        || view
            .summary
            .as_deref()
            .is_some_and(|text| !bounded_text(text, 1024, true))
        || view.sections.len() > MAX_VIEW_SECTIONS
    {
        return Err("invalid monitoring view version, title, summary or section limit".into());
    }
    let mut section_ids = BTreeSet::new();
    let mut field_ids = BTreeSet::new();
    let mut fields = 0usize;
    for section in &view.sections {
        fields = fields.saturating_add(section.fields.len());
        if !valid_id(&section.id)
            || !section_ids.insert(&section.id)
            || !bounded_text(&section.title, 128, false)
            || section
                .description
                .as_deref()
                .is_some_and(|text| !bounded_text(text, 1024, true))
            || !valid_id(&section.monitor_role)
            || section.fields.is_empty()
        {
            return Err("invalid or duplicate monitoring view section".into());
        }
        for field in &section.fields {
            if !valid_id(&field.id)
                || !field_ids.insert(&field.id)
                || !bounded_text(&field.label, 128, false)
                || !valid_pointer(&field.pointer)
                || field
                    .unit
                    .as_deref()
                    .is_some_and(|text| !bounded_text(text, 32, true))
                || field
                    .empty
                    .as_deref()
                    .is_some_and(|text| !bounded_text(text, 128, true))
            {
                return Err("invalid or duplicate monitoring view field".into());
            }
        }
    }
    if fields > MAX_VIEW_FIELDS {
        return Err("monitoring view has more than 64 fields".into());
    }
    if serde_json::to_vec(&view).map_or(true, |bytes| bytes.len() > MAX_VIEW_BYTES) {
        return Err("normalized monitoring view declaration exceeds 32 KiB".into());
    }
    Ok(view)
}

fn owner_plugin_id(
    state: &Console,
    extension_id: &str,
    contract: &str,
    version: u32,
) -> Option<String> {
    let registry = state.extensions.as_ref()?;
    registry
        .contract_owner(contract, version)
        .map(str::to_owned)
        .or_else(|| {
            registry
                .definitions()
                .into_iter()
                .find(|definition| {
                    definition.id == extension_id && definition.kind == ExtensionKind::Plugin
                })
                .map(|definition| definition.id)
        })
}

fn monitor_owner(state: &Console, monitor: &MonitorSnapshot) -> Option<String> {
    owner_plugin_id(
        state,
        &monitor.extension_id,
        &monitor.contract,
        monitor.version,
    )
}

fn discovery_owner(
    state: &Console,
    discovery: &crate::monitoring::DiscoverySnapshot,
) -> Option<String> {
    owner_plugin_id(
        state,
        &discovery.extension_id,
        &discovery.contract,
        discovery.version,
    )
}

fn plugin_summaries(state: &Console, snapshot: Option<&MonitoringSnapshot>) -> Vec<Value> {
    let Some(registry) = &state.extensions else {
        return Vec::new();
    };
    let statuses = registry.statuses();
    registry
        .definitions()
        .into_iter()
        .filter(|definition| definition.kind == ExtensionKind::Plugin)
        .map(|definition| {
            let status = statuses.iter().find(|status| status.id == definition.id);
            let registration = if !definition.enabled {
                "disabled"
            } else if status.is_some_and(|status| status.available) {
                "registered"
            } else {
                "unavailable"
            };
            let (view_registration, view_error) = if registration != "registered" {
                ("unavailable", None)
            } else {
                match registry.monitoring_view_registration(&definition.id) {
                    Ok(Some(_)) => ("registered", None),
                    Ok(None) => ("not_registered", None),
                    Err(error) => ("invalid", Some(bounded_error(error))),
                }
            };
            let monitors = snapshot
                .map(|snapshot| {
                    snapshot
                        .monitors
                        .iter()
                        .filter(|monitor| {
                            monitor_owner(state, monitor).as_deref()
                                == Some(definition.id.as_str())
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let discoveries = snapshot
                .map(|snapshot| {
                    snapshot
                        .discoveries
                        .iter()
                        .filter(|discovery| {
                            discovery_owner(state, discovery).as_deref()
                                == Some(definition.id.as_str())
                        })
                        .count()
                })
                .unwrap_or_default();
            let targets = monitors
                .iter()
                .map(|monitor| &monitor.target_id)
                .collect::<BTreeSet<_>>()
                .len();
            json!({
                "id":definition.id,
                "registration":registration,
                "registration_error":status.and_then(|status|status.error.as_ref()).map(bounded_error),
                "view_registration":view_registration,
                "view_error":view_error,
                "monitor_count":monitors.len(),
                "target_count":targets,
                "discovery_count":discoveries,
            })
        })
        .collect()
}

fn monitor_value(state: &Console, monitor: &MonitorSnapshot) -> Result<Value, ApiError> {
    let (logs_available, log_source_id) = project_logs::availability(state, &monitor.id)?;
    let mut value = serde_json::to_value(monitor)?;
    value["owner_plugin_id"] = json!(monitor_owner(state, monitor));
    value["logs_available"] = json!(logs_available);
    value["log_source_id"] = json!(log_source_id);
    Ok(value)
}

fn discovery_value(
    state: &Console,
    discovery: &crate::monitoring::DiscoverySnapshot,
) -> Result<Value, ApiError> {
    let mut value = serde_json::to_value(discovery)?;
    value["owner_plugin_id"] = json!(discovery_owner(state, discovery));
    Ok(value)
}

pub(super) fn bootstrap(state: &Console) -> Result<Value, ApiError> {
    if !state
        .config
        .permissions
        .iter()
        .any(|permission| permission == "monitor.read")
    {
        return Ok(Value::Null);
    }
    let Some(handle) = &state.monitoring else {
        return Ok(
            json!({"configured":false,"monitors":[],"discoveries":[],"plugins":plugin_summaries(state,None),"runtime_error":null,"counts":{"monitors":0,"targets":0,"unhealthy":0,"stale":0,"collection_issues":0,"incidents":if state.require("incident.read").is_ok(){incident_counts(&[])}else{Value::Null}}}),
        );
    };
    let snapshot = handle.snapshot().map_err(monitor_error)?;
    let monitors = snapshot
        .monitors
        .iter()
        .map(|monitor| {
            let mut value = monitor_value(state, monitor)?;
            if let Some(value) = value.as_object_mut() {
                value.remove("last_value");
                value.insert("detail".into(), Value::Bool(false));
            }
            Ok(value)
        })
        .collect::<Result<Vec<_>, ApiError>>()?;
    let discoveries = snapshot
        .discoveries
        .iter()
        .map(|discovery| discovery_value(state, discovery))
        .collect::<Result<Vec<_>, ApiError>>()?;
    let counts = json!({"monitors":monitors.len(),
        "targets":snapshot.monitors.iter().map(|monitor|&monitor.target_id).collect::<std::collections::BTreeSet<_>>().len(),
        "unhealthy":monitors.iter().filter(|monitor|monitor["health"]=="unhealthy").count(),
        "stale":monitors.iter().filter(|monitor|monitor["freshness"]=="stale").count(),
        "collection_issues":monitors.iter().filter(|monitor|monitor["coverage"]!="complete"||monitor["freshness"]!="fresh").count(),
        "incidents":if state.require("incident.read").is_ok(){incident_counts(&handle.map_incidents(summary).map_err(monitor_error)?)}else{Value::Null},
    });
    Ok(
        json!({"configured":true,"monitors":monitors,"discoveries":discoveries,"plugins":plugin_summaries(state,Some(&snapshot)),"runtime_error":snapshot.runtime_error,"running":snapshot.running,"counts":counts}),
    )
}

pub(super) async fn monitors(State(state): State<Arc<Console>>) -> Result<Json<Value>, ApiError> {
    state.require("monitor.read")?;
    let snapshot = bootstrap(&state)?;
    Ok(Json(
        json!({"configured":snapshot["configured"],"items":snapshot["monitors"],"discoveries":snapshot["discoveries"],"plugins":snapshot["plugins"],"runtime_error":snapshot["runtime_error"],"running":snapshot["running"],"counts":snapshot["counts"]}),
    ))
}

pub(super) async fn monitor(
    State(state): State<Arc<Console>>,
    RoutePath(id): RoutePath<String>,
) -> Result<Json<Value>, ApiError> {
    state.require("monitor.read")?;
    let item = service(&state)?
        .monitor(&id)
        .map_err(monitor_error)?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "not_found", "monitor not found"))?;
    Ok(Json(monitor_value(&state, &item)?))
}

async fn read_registered_view(
    registry: &ExtensionRegistry,
    registration: &MonitoringViewRegistration,
) -> Result<PluginMonitoringView, (&'static str, String)> {
    let value = registry
        .call_monitoring_view(
            registration,
            json!({"schema_version":1}),
            Duration::from_secs(5),
            HarnessCancellation::new(),
        )
        .await
        .map_err(monitoring_view_error)?;
    validate_view(value).map_err(|error| ("invalid_response", bounded_error(error)))
}

pub(super) async fn plugin_monitoring(
    State(state): State<Arc<Console>>,
    RoutePath(id): RoutePath<String>,
) -> Result<Json<Value>, ApiError> {
    state.require("monitor.read")?;
    if !valid_id(&id) {
        return Err(ApiError::invalid("invalid plugin identity"));
    }
    let registry = state
        .extensions
        .as_ref()
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "not_found", "plugin not found"))?;
    if !registry
        .definitions()
        .iter()
        .any(|definition| definition.id == id && definition.kind == ExtensionKind::Plugin)
    {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            "plugin not found",
        ));
    }
    let snapshot = state
        .monitoring
        .as_ref()
        .map(|handle| handle.snapshot().map_err(monitor_error))
        .transpose()?;
    let plugin = plugin_summaries(&state, snapshot.as_ref())
        .into_iter()
        .find(|plugin| plugin["id"] == id)
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "not_found", "plugin not found"))?;
    let monitors = snapshot
        .as_ref()
        .map(|snapshot| {
            snapshot
                .monitors
                .iter()
                .filter(|monitor| monitor_owner(&state, monitor).as_deref() == Some(id.as_str()))
                .map(|monitor| monitor_value(&state, monitor))
                .collect::<Result<Vec<_>, ApiError>>()
        })
        .transpose()?
        .unwrap_or_default();
    let discoveries = snapshot
        .as_ref()
        .map(|snapshot| {
            snapshot
                .discoveries
                .iter()
                .filter(|discovery| {
                    discovery_owner(&state, discovery).as_deref() == Some(id.as_str())
                })
                .map(|discovery| discovery_value(&state, discovery))
                .collect::<Result<Vec<_>, ApiError>>()
        })
        .transpose()?
        .unwrap_or_default();

    let registration = registry.monitoring_view_registration(&id);
    let (view_status, view_error, view) = if plugin["registration"] != "registered" {
        (
            "unavailable",
            plugin["registration_error"].as_str().map(str::to_owned),
            None,
        )
    } else {
        match registration {
            Ok(None) => ("not_registered", None, None),
            Err(error) => ("invalid_registration", Some(bounded_error(error)), None),
            Ok(Some(_)) if state.require("extension.read").is_err() => (
                "permission_denied",
                Some("extension.read permission is required for plugin-provided content".into()),
                None,
            ),
            Ok(Some(registration)) => match read_registered_view(registry, &registration).await {
                Ok(view) => ("ready", None, Some(view)),
                Err((status, error)) => (status, Some(error), None),
            },
        }
    };
    Ok(Json(json!({
        "plugin":plugin,
        "view_status":view_status,
        "view_error":view_error,
        "view":view,
        "monitors":monitors,
        "discoveries":discoveries,
        "read_at":timestamp(),
        "auto_retry":false,
    })))
}

pub(super) async fn plugin_ui_catalog(
    State(state): State<Arc<Console>>,
) -> Result<Json<Value>, ApiError> {
    state.require("extension.read")?;
    let views = if state.require("monitor.read").is_ok() {
        plugin_summaries(&state, None)
            .into_iter()
            .filter(|plugin| {
                plugin["registration"] == "registered"
                    && plugin["view_registration"] == "registered"
            })
            .map(|plugin| {
                json!({
                    "plugin_id": plugin["id"], "view_id": "monitoring",
                    "title": format!("{} 监控", plugin["id"].as_str().unwrap_or("插件")),
                    "renderer": "monitoring_v1", "read_only": true,
                    "required_permissions": ["monitor.read", "extension.read"]
                })
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let pages = state
        .extensions
        .as_ref()
        .map(|registry| registry.ui_links_catalog())
        .transpose()
        .map_err(|_| ApiError::unavailable("page catalog unavailable"))?
        .unwrap_or_default();
    let external_links = pages
        .iter()
        .flat_map(|page| &page.links)
        .collect::<Vec<_>>();
    let link_statuses = pages.iter().map(|page| json!({
        "plugin_id":page.plugin_id,"status":page.status,"descriptor_revision":page.descriptor_revision,
        "error":page.error,"unavailable_pages":page.unavailable_pages
    })).collect::<Vec<_>>();
    Ok(Json(
        json!({"schema_version":1,"views":views,"external_links":external_links,"link_statuses":link_statuses}),
    ))
}

pub(super) async fn plugin_ui_view(
    State(state): State<Arc<Console>>,
    RoutePath((plugin_id, view_id)): RoutePath<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    if view_id != "monitoring" {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            "plugin view not found",
        ));
    }
    let Json(document) = plugin_monitoring(State(state), RoutePath(plugin_id.clone())).await?;
    Ok(Json(json!({
        "schema_version": 1,
        "plugin_id": plugin_id,
        "view_id": view_id,
        "renderer": "monitoring_v1",
        "read_only": true,
        "document": document
    })))
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct IncidentQuery {
    cursor: Option<String>,
    limit: Option<usize>,
    status: Option<String>,
    target_id: Option<String>,
    monitor_id: Option<String>,
    kind: Option<String>,
    query: Option<String>,
}

fn summary(record: &IncidentRecord) -> Value {
    // No arbitrary observation payloads or acknowledgement notes in lists.
    json!({"id":record.id,"revision":record.revision,"monitor_id":record.monitor_id,
        "target_id":record.target_id,"rule_id":record.rule_id,"kind":record.kind,
        "status":record.status,"condition":record.condition,
        "summary":record.summary.chars().take(256).collect::<String>(),
        "first_seen":record.first_seen,"last_seen":record.last_seen,"resolved_at":record.resolved_at,
        "occurrences":record.occurrences,"detail":false})
}

/// Bind diagnostic provenance to a persisted fault snapshot within the fixed repair scope.
/// The snapshot is data and never grants authority or changes incident state.
pub(super) fn repair_source_incident(
    state: &Console,
    id: Option<&str>,
    revision: Option<u64>,
) -> Result<Option<Value>, ApiError> {
    let (id, revision) = match (id, revision) {
        (None, None) => return Ok(None),
        (Some(id), Some(revision)) if valid_id(id) && revision > 0 => (id, revision),
        _ => {
            return Err(ApiError::invalid(
                "incident_id and positive incident_revision must be supplied together",
            ));
        }
    };
    state.require("incident.read")?;
    let config = state
        .repair_config
        .as_ref()
        .ok_or_else(|| ApiError::unavailable("repair target is not configured"))?;
    let record = service(state)?
        .incident(id)
        .map_err(monitor_error)?
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "not_found",
                "source incident not found",
            )
        })?;
    if record.revision != revision {
        return Err(ApiError::conflict(
            "source incident revision changed; reread the fault before starting diagnosis",
        ));
    }
    if record.target_id != config.target_id
        || !config.policy.allowed_targets.contains(&record.target_id)
    {
        return Err(ApiError::conflict(
            "source incident target is outside the configured repair scope",
        ));
    }
    let mut source = summary(&record);
    source["captured_at"] = json!(timestamp());
    Ok(Some(source))
}

pub(super) async fn incidents(
    State(state): State<Arc<Console>>,
    Query(query): Query<IncidentQuery>,
) -> Result<Json<Value>, ApiError> {
    state.require("incident.read")?;
    let limit = query.limit.unwrap_or(25);
    let status = query.status.as_deref().unwrap_or("active");
    if !(1..=100).contains(&limit)
        || query.cursor.as_ref().is_some_and(|id| !valid_id(id))
        || query
            .target_id
            .as_ref()
            .is_some_and(|id| !id.is_empty() && !valid_id(id))
        || query
            .monitor_id
            .as_ref()
            .is_some_and(|id| !id.is_empty() && !valid_id(id))
        || query
            .kind
            .as_deref()
            .is_some_and(|kind| !matches!(kind, "" | "all" | "target" | "coverage"))
        || query.query.as_ref().is_some_and(|text| text.len() > 256)
        || !matches!(
            status,
            "active" | "all" | "open" | "acknowledged" | "resolved"
        )
    {
        return Err(ApiError::invalid(
            "invalid incident status, cursor or limit",
        ));
    }
    let mut items = match &state.monitoring {
        Some(handle) => handle.map_incidents(summary).map_err(monitor_error)?,
        None => Vec::new(),
    };
    let counts = incident_counts(&items);
    let search = query.query.as_deref().unwrap_or("").to_lowercase();
    items.retain(|item| {
        (status == "all"
            || item["status"] == status
            || (status == "active" && item["status"] != "resolved"))
            && query
                .target_id
                .as_deref()
                .filter(|id| !id.is_empty())
                .is_none_or(|id| item["target_id"] == id)
            && query
                .monitor_id
                .as_deref()
                .filter(|id| !id.is_empty())
                .is_none_or(|id| item["monitor_id"] == id)
            && query
                .kind
                .as_deref()
                .filter(|kind| !matches!(*kind, "" | "all"))
                .is_none_or(|kind| item["kind"] == kind)
            && (search.is_empty()
                || ["id", "monitor_id", "target_id", "rule_id", "summary"]
                    .iter()
                    .any(|field| {
                        item[*field]
                            .as_str()
                            .is_some_and(|value| value.to_lowercase().contains(&search))
                    }))
    });
    items.sort_by(|a, b| b["id"].as_str().cmp(&a["id"].as_str()));
    let total = items.len();
    items.retain(|item| {
        query
            .cursor
            .as_ref()
            .is_none_or(|cursor| item["id"].as_str().is_some_and(|id| id < cursor.as_str()))
    });
    let more = items.len() > limit;
    items.truncate(limit);
    let next_cursor = if more {
        items.last().map(|item| item["id"].clone())
    } else {
        None
    };
    Ok(Json(
        json!({"items":items,"total":total,"counts":counts,"next_cursor":next_cursor,"limit":limit,"order":"id_desc","read_at":timestamp()}),
    ))
}

fn detail(state: &Console, record: IncidentRecord) -> Result<Value, ApiError> {
    let mut value = serde_json::to_value(record)?;
    let can_ack = value["status"] == "open" && state.require("incident.acknowledge").is_ok();
    value["allowed_actions"] = json!(if can_ack {
        vec!["acknowledge"]
    } else {
        Vec::<&str>::new()
    });
    value["auto_retry"] = json!(false);
    value["business_verified"] = json!(false);
    value["related_repairs"] =
        history::incident_repairs(state, value["id"].as_str().unwrap_or_default())?;
    Ok(value)
}

pub(super) async fn incident(
    State(state): State<Arc<Console>>,
    RoutePath(id): RoutePath<String>,
) -> Result<Json<Value>, ApiError> {
    state.require("incident.read")?;
    let record = service(&state)?
        .incident(&id)
        .map_err(monitor_error)?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "not_found", "incident not found"))?;
    Ok(Json(detail(&state, record)?))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AcknowledgeInput {
    revision: u64,
    note: String,
}

pub(super) async fn acknowledge(
    State(state): State<Arc<Console>>,
    RoutePath(id): RoutePath<String>,
    Json(input): Json<AcknowledgeInput>,
) -> Result<Json<Value>, ApiError> {
    state.require("incident.read")?;
    state.require("incident.acknowledge")?;
    if input.note.len() > crate::persistence::incidents::MAX_ACK_NOTE_BYTES || input.revision == 0 {
        return Err(ApiError::invalid(
            "revision and a bounded acknowledgement note are required",
        ));
    }
    let record = service(&state)?
        .acknowledge(&id, input.revision, &state.config.operator, &input.note)
        .map_err(monitor_error)?;
    Ok(Json(detail(&state, record)?))
}

#[cfg(test)]
#[path = "../../tests/server_monitoring_view.rs"]
mod plugin_view_tests;
