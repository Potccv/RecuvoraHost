//! Configuration-bound, bounded reads of monitored observation records.
use super::*;
use crate::monitoring::MonitorDefinition;
use crate::protocol::ExtensionError;

const MAX_LOG_BYTES: usize = 256 * 1024;
const MAX_CURSOR_BYTES: usize = 4096;
const MAX_CURSORS: usize = 256;
const CURSOR_TTL_MS: u64 = 30 * 60 * 1000;

/// Only a trusted deployment can choose a provider, method or target mapping.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LogSourceConfig {
    pub id: String,
    pub extension_id: String,
    pub contract: String,
    pub version: u32,
    pub method: String,
    pub monitor_contract: String,
    #[serde(default = "empty_params")]
    pub params: Value,
    /// Destination parameter name -> JSON pointer into the enrolled monitor params.
    pub parameter_bindings: BTreeMap<String, String>,
    #[serde(default = "cursor_name")]
    pub cursor_parameter: String,
    #[serde(default = "limit_name")]
    pub limit_parameter: String,
    #[serde(default = "default_timeout")]
    pub timeout_ms: u64,
    #[serde(default)]
    pub result: LogResultMapping,
    #[serde(default)]
    pub error_levels: Vec<String>,
    #[serde(default)]
    pub error_events: Vec<String>,
}

fn empty_params() -> Value {
    json!({})
}
fn cursor_name() -> String {
    "cursor".into()
}
fn limit_name() -> String {
    "limit".into()
}
fn default_timeout() -> u64 {
    5000
}

/// JSON pointers, applied to the provider response and each individual record.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct LogResultMapping {
    pub entries: String,
    pub next_cursor: String,
    pub has_more: String,
    pub stream_label: String,
    pub coverage: String,
    pub source_error: String,
    pub available_streams: String,
    pub id: String,
    pub timestamp: String,
    pub level: String,
    pub message: String,
    pub event: String,
}

impl Default for LogResultMapping {
    fn default() -> Self {
        Self {
            entries: "/entries".into(),
            next_cursor: "/next_cursor".into(),
            has_more: "/has_more".into(),
            stream_label: "/stream_label".into(),
            coverage: "/coverage".into(),
            source_error: "/error".into(),
            available_streams: "/available_streams".into(),
            id: "/id".into(),
            timestamp: "/timestamp".into(),
            level: "/level".into(),
            message: "/message".into(),
            event: "/event".into(),
        }
    }
}

fn pointer_valid(pointer: &str) -> bool {
    if pointer.len() > 1024 || !pointer.starts_with('/') {
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

pub(super) fn validate_sources(sources: &[LogSourceConfig]) -> Result<(), ApiError> {
    let mut ids = std::collections::BTreeSet::new();
    let mut bindings = std::collections::BTreeSet::new();
    if sources.len() > 16 {
        return Err(ApiError::invalid("at most 16 log sources are allowed"));
    }
    for source in sources {
        let mapping = &source.result;
        let normalized_error_levels = source
            .error_levels
            .iter()
            .map(|level| level.to_ascii_uppercase())
            .collect::<std::collections::BTreeSet<_>>();
        if [
            &source.id,
            &source.extension_id,
            &source.contract,
            &source.method,
            &source.monitor_contract,
            &source.cursor_parameter,
            &source.limit_parameter,
        ]
        .iter()
        .any(|id| !valid_id(id))
            || !ids.insert(&source.id)
            || !bindings.insert((&source.extension_id, &source.monitor_contract))
            || source.contract == "recuvora"
            || source.contract.starts_with("recuvora.")
            || source.version == 0
            || !(1..=30_000).contains(&source.timeout_ms)
            || !source.params.is_object()
            || serde_json::to_vec(&source.params).map_or(true, |bytes| bytes.len() > 16 * 1024)
            || source.parameter_bindings.is_empty()
            || source.parameter_bindings.len() > 16
            || source.cursor_parameter == source.limit_parameter
            || source.params.get(&source.cursor_parameter).is_some()
            || source.params.get(&source.limit_parameter).is_some()
            || source.parameter_bindings.iter().any(|(name, pointer)| {
                !valid_id(name)
                    || !pointer_valid(pointer)
                    || name == &source.cursor_parameter
                    || name == &source.limit_parameter
                    || source.params.get(name).is_some()
            })
            || [
                &mapping.entries,
                &mapping.next_cursor,
                &mapping.has_more,
                &mapping.stream_label,
                &mapping.coverage,
                &mapping.source_error,
                &mapping.available_streams,
                &mapping.id,
                &mapping.timestamp,
                &mapping.level,
                &mapping.message,
                &mapping.event,
            ]
            .iter()
            .any(|pointer| !pointer_valid(pointer))
            || source.error_levels.len() > 16
            || normalized_error_levels.len() != source.error_levels.len()
            || source
                .error_levels
                .iter()
                .any(|level| level.len() > 32 || !valid_id(level))
            || source.error_events.len() > 16
            || source.error_events.iter().any(|event| !valid_id(event))
        {
            return Err(ApiError::invalid(
                "invalid or ambiguous trusted log source configuration",
            ));
        }
    }
    Ok(())
}

fn params(source: &LogSourceConfig, monitor: &MonitorDefinition) -> Option<Value> {
    let mut params = source.params.clone();
    for (name, pointer) in &source.parameter_bindings {
        let value = monitor.params.pointer(pointer)?;
        // Bind bounded scalar identities from trusted configuration, never HTTP params.
        if !(value.is_string() || value.is_number() || value.is_boolean())
            || value.as_str().is_some_and(|s| s.len() > 1024)
        {
            return None;
        }
        params[name] = value.clone();
    }
    Some(params)
}

fn source_for<'a>(state: &'a Console, monitor: &MonitorDefinition) -> Option<&'a LogSourceConfig> {
    state.config.log_sources.iter().find(|source| {
        source.extension_id == monitor.extension_id
            && source.monitor_contract == monitor.contract
            && params(source, monitor).is_some()
    })
}

pub(super) fn availability(state: &Console, id: &str) -> Result<(bool, Option<String>), ApiError> {
    if ["monitor.read", "logs.read", "extension.read"]
        .iter()
        .any(|permission| state.require(permission).is_err())
    {
        return Ok((false, None));
    }
    let Some(handle) = &state.monitoring else {
        return Ok((false, None));
    };
    let Some(monitor) = handle
        .definition(id)
        .map_err(|error| ApiError::unavailable(error.to_string()))?
    else {
        return Ok((false, None));
    };
    let source = source_for(state, &monitor);
    let available = source.is_some_and(|source| {
        state.extensions.as_ref().is_some_and(|registry| {
            registry
                .statuses()
                .iter()
                .any(|status| status.id == source.extension_id && status.available)
                && registry.definitions().iter().any(|definition| {
                    definition.id == source.extension_id
                        && definition.enabled
                        && definition.allow_calls.iter().any(|method| {
                            method.contract == source.contract
                                && method.version == source.version
                                && method.method == source.method
                        })
                })
                && registry
                    .metadata(&source.extension_id)
                    .is_some_and(|metadata| {
                        metadata.contracts.iter().any(|contract| {
                            contract.id == source.contract
                                && contract.version == source.version
                                && contract
                                    .methods
                                    .iter()
                                    .any(|method| method.name == source.method && method.read_only)
                        })
                    })
        })
    });
    Ok((available, source.map(|source| source.id.clone())))
}

/// HTTP cursors are server-issued handles, bound to the fixed source and target.
/// Provider cursors are not accepted directly, and never expose target authority.
pub(super) struct LogCursor {
    monitor_id: String,
    source_id: String,
    provider_cursor: String,
    touched_at: u64,
}

fn invalid_cursor() -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        "cursor_invalid",
        "log cursor expired or source changed; explicitly start a new log read",
    )
}

pub(super) fn resolve_cursor(
    state: &Console,
    monitor_id: &str,
    source_id: &str,
    cursor: Option<&str>,
) -> Result<Option<String>, ApiError> {
    let Some(cursor) = cursor.filter(|cursor| !cursor.is_empty()) else {
        return Ok(None);
    };
    if !valid_id(cursor) {
        return Err(invalid_cursor());
    }
    let mut cursors = lock(&state.log_cursors)?;
    let entry = cursors.get_mut(cursor).ok_or_else(invalid_cursor)?;
    let now = timestamp();
    if entry.monitor_id != monitor_id
        || entry.source_id != source_id
        || now.saturating_sub(entry.touched_at) > CURSOR_TTL_MS
    {
        return Err(invalid_cursor());
    }
    entry.touched_at = now;
    Ok(Some(entry.provider_cursor.clone()))
}

pub(super) fn issue_cursor(
    state: &Console,
    monitor_id: &str,
    source_id: &str,
    provider: Option<&str>,
) -> Result<Option<String>, ApiError> {
    let Some(provider) = provider else {
        return Ok(None);
    };
    if provider.len() > MAX_CURSOR_BYTES || !provider.bytes().all(|byte| byte.is_ascii_graphic()) {
        return Err(ApiError::unavailable(
            "provider returned invalid log cursor",
        ));
    }
    let now = timestamp();
    let mut cursors = lock(&state.log_cursors)?;
    cursors.retain(|_, entry| now.saturating_sub(entry.touched_at) <= CURSOR_TTL_MS);
    if let Some((id, entry)) = cursors.iter_mut().find(|(_, entry)| {
        entry.monitor_id == monitor_id
            && entry.source_id == source_id
            && entry.provider_cursor == provider
    }) {
        entry.touched_at = now;
        return Ok(Some(id.clone()));
    }
    if cursors.len() >= MAX_CURSORS {
        let oldest = cursors
            .iter()
            .min_by_key(|(_, entry)| entry.touched_at)
            .map(|(id, _)| id.clone());
        if let Some(id) = oldest {
            cursors.remove(&id);
        }
    }
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let id = format!(
        "log-cursor-{now}-{}",
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    cursors.insert(
        id.clone(),
        LogCursor {
            monitor_id: monitor_id.into(),
            source_id: source_id.into(),
            provider_cursor: provider.into(),
            touched_at: now,
        },
    );
    Ok(Some(id))
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LogQuery {
    cursor: Option<String>,
    limit: Option<usize>,
}

fn bounded_text(value: Option<&Value>, maximum: usize) -> Result<Option<String>, ApiError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) if text.len() <= maximum => Ok(Some(text.clone())),
        _ => Err(ApiError::unavailable(
            "provider log response contains invalid text field",
        )),
    }
}

/// Keeps source failures separate from records classified by trusted configuration.
pub(super) fn project(
    source: &LogSourceConfig,
    result: &Value,
    limit: usize,
) -> Result<Value, ApiError> {
    if serde_json::to_vec(result)?.len() > MAX_LOG_BYTES {
        return Err(ApiError::unavailable(
            "provider log response exceeds 256 KiB",
        ));
    }
    let mapping = &source.result;
    let entries = result
        .pointer(&mapping.entries)
        .and_then(Value::as_array)
        .ok_or_else(|| ApiError::unavailable("provider log entries are missing"))?;
    if entries.len() > limit {
        return Err(ApiError::unavailable(
            "provider exceeded requested log record limit",
        ));
    }
    let mut items = Vec::new();
    let mut errors = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for entry in entries {
        let id = bounded_text(entry.pointer(&mapping.id), 128)?
            .filter(|id| valid_id(id))
            .ok_or_else(|| {
                ApiError::unavailable("provider log record requires a stable bounded ID")
            })?;
        if !seen.insert(id.clone()) {
            return Err(ApiError::unavailable("provider returned duplicate log IDs"));
        }
        let timestamp = match entry.pointer(&mapping.timestamp) {
            Some(Value::String(text)) if text.len() <= 64 => Value::String(text.clone()),
            Some(Value::Number(number))
                if number
                    .as_u64()
                    .is_some_and(|value| value <= 9_007_199_254_740_992) =>
            {
                Value::Number(number.clone())
            }
            None | Some(Value::Null) => Value::Null,
            _ => {
                return Err(ApiError::unavailable(
                    "provider returned invalid log timestamp",
                ));
            }
        };
        let level =
            bounded_text(entry.pointer(&mapping.level), 32)?.unwrap_or_else(|| "UNKNOWN".into());
        let message = bounded_text(entry.pointer(&mapping.message), 16 * 1024)?
            .ok_or_else(|| ApiError::unavailable("provider log message is missing"))?;
        let event = bounded_text(entry.pointer(&mapping.event), 128)?;
        let record =
            json!({"id":id,"timestamp":timestamp,"level":level,"message":message,"event":event});
        if source
            .error_levels
            .iter()
            .any(|configured| configured.eq_ignore_ascii_case(&level))
            || event
                .as_ref()
                .is_some_and(|event| source.error_events.contains(event))
        {
            errors.push(record.clone());
        }
        items.push(record);
    }
    let cursor = bounded_text(result.pointer(&mapping.next_cursor), MAX_CURSOR_BYTES)?;
    let has_more = result
        .pointer(&mapping.has_more)
        .and_then(Value::as_bool)
        .ok_or_else(|| ApiError::unavailable("provider log has_more is missing"))?;
    if (has_more || !items.is_empty()) && cursor.is_none() {
        return Err(ApiError::unavailable(
            "provider log continuation cursor is missing",
        ));
    }
    let mut available = Vec::new();
    if let Some(value) = result
        .pointer(&mapping.available_streams)
        .filter(|value| !value.is_null())
    {
        let streams = value
            .as_array()
            .filter(|streams| streams.len() <= 32)
            .ok_or_else(|| ApiError::unavailable("provider returned invalid available streams"))?;
        for stream in streams {
            available.push(bounded_text(Some(stream), 256)?.ok_or_else(|| {
                ApiError::unavailable("provider returned invalid record stream label")
            })?);
        }
    }
    Ok(
        json!({"items":items,"errors":errors,"next_cursor":cursor,"has_more":has_more,
        "stream_label":bounded_text(result.pointer(&mapping.stream_label),256)?,
        "coverage":bounded_text(result.pointer(&mapping.coverage),32)?,
        "source_error":bounded_text(result.pointer(&mapping.source_error),1024)?,
        "available_streams":available,"reset":false,"limit":limit}),
    )
}

pub(super) async fn logs(
    State(state): State<Arc<Console>>,
    RoutePath(id): RoutePath<String>,
    Query(query): Query<LogQuery>,
) -> Result<Json<Value>, ApiError> {
    for permission in ["monitor.read", "logs.read", "extension.read"] {
        state.require(permission)?;
    }
    let limit = query.limit.unwrap_or(32);
    if !valid_id(&id) || !(1..=32).contains(&limit) {
        return Err(ApiError::invalid(
            "log limit must be 1..32 and monitor ID must be valid",
        ));
    }
    let handle = state.monitoring.as_ref().ok_or_else(|| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "logs_unavailable",
            "monitoring is not configured",
        )
    })?;
    let monitor = handle
        .definition(&id)
        .map_err(|error| ApiError::unavailable(error.to_string()))?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "not_found", "monitor not found"))?;
    let source = source_for(&state, &monitor).ok_or_else(|| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "logs_unavailable",
            "no trusted log source is configured for this monitor",
        )
    })?;
    let registry = state.extensions.as_ref().ok_or_else(|| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "logs_unavailable",
            "log provider is not configured",
        )
    })?;
    if !availability(&state, &id)?.0 {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "logs_unavailable",
            "trusted log provider route is not registered or enabled",
        ));
    }
    let provider_cursor = resolve_cursor(&state, &id, &source.id, query.cursor.as_deref())?;
    let mut input = params(source, &monitor)
        .ok_or_else(|| ApiError::unavailable("log target binding is unavailable"))?;
    input[&source.limit_parameter] = json!(limit);
    if let Some(cursor) = &provider_cursor {
        input[&source.cursor_parameter] = json!(cursor);
    }
    let result = registry
        .call_read_only(
            &source.extension_id,
            &source.contract,
            source.version,
            &source.method,
            input,
            Duration::from_millis(source.timeout_ms),
            HarnessCancellation::new(),
        )
        .await
        .map_err(|error| match error {
            ExtensionError::Rejected(message)
                if provider_cursor.is_some()
                    && ["source_changed", "invalid_cursor", "cursor"]
                        .iter()
                        .any(|part| message.contains(part)) =>
            {
                invalid_cursor()
            }
            error => ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "logs_read_failed",
                error.to_string(),
            ),
        })?;
    let mut output = project(source, &result, limit)?;
    if output["has_more"] == true
        && provider_cursor
            .as_deref()
            .is_some_and(|cursor| output["next_cursor"] == cursor)
    {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "logs_read_failed",
            "log provider did not advance its continuation cursor",
        ));
    }
    output["next_cursor"] = json!(issue_cursor(
        &state,
        &id,
        &source.id,
        output["next_cursor"].as_str()
    )?);
    output["read_at"] = json!(timestamp());
    output["monitor_id"] = json!(id);
    output["target_id"] = json!(monitor.target_id);
    output["source_id"] = json!(monitor.source_id);
    output["log_source_id"] = json!(source.id);
    output["extension_id"] = json!(source.extension_id);
    output["auto_retry"] = json!(false);
    Ok(Json(output))
}
