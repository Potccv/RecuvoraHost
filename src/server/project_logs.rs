//! Read-only views of error records already accepted by the Host incident store.
use super::*;
use crate::persistence::incidents::{IncidentKind, IncidentRecord};

const MAX_LOG_BYTES: usize = 256 * 1024;
const MAX_CURSORS: usize = 256;
const CURSOR_TTL_MS: u64 = 30 * 60 * 1000;

pub(super) fn availability(state: &Console, id: &str) -> Result<(bool, Option<String>), ApiError> {
    if ["monitor.read", "logs.read", "extension.read"]
        .iter()
        .any(|permission| state.require(permission).is_err())
    {
        return Ok((false, None));
    }
    let Some(handle) = state.application.monitoring() else {
        return Ok((false, None));
    };
    let definition = handle
        .definition(id)
        .map_err(|error| ApiError::unavailable(error.to_string()))?;
    Ok(match definition {
        Some(definition) => (true, Some(definition.source_id)),
        None => (false, None),
    })
}

/// Opaque HTTP cursors identify a durable receipt, never a Node read position.
pub(super) struct LogCursor {
    monitor_id: String,
    target_id: String,
    source_id: String,
    last_receipt: Option<String>,
    touched_at: u64,
}

fn invalid_cursor() -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        "cursor_invalid",
        "error record cursor expired or its receipt is unavailable; explicitly start a new read",
    )
}

pub(super) fn resolve_cursor(
    state: &Console,
    monitor_id: &str,
    target_id: &str,
    source_id: &str,
    cursor: Option<&str>,
) -> Result<Option<String>, ApiError> {
    let Some(cursor) = cursor else {
        return Ok(None);
    };
    if !valid_id(cursor) {
        return Err(invalid_cursor());
    }
    let mut cursors = lock(&state.log_cursors)?;
    let entry = cursors.get_mut(cursor).ok_or_else(invalid_cursor)?;
    let now = timestamp();
    if entry.monitor_id != monitor_id
        || entry.target_id != target_id
        || entry.source_id != source_id
        || now.saturating_sub(entry.touched_at) > CURSOR_TTL_MS
    {
        return Err(invalid_cursor());
    }
    entry.touched_at = now;
    Ok(entry.last_receipt.clone())
}

pub(super) fn issue_cursor(
    state: &Console,
    monitor_id: &str,
    target_id: &str,
    source_id: &str,
    last_receipt: Option<&str>,
) -> Result<String, ApiError> {
    if last_receipt.is_some_and(|id| !valid_id(id)) {
        return Err(ApiError::unavailable(
            "invalid durable error receipt identity",
        ));
    }
    let now = timestamp();
    let mut cursors = lock(&state.log_cursors)?;
    cursors.retain(|_, entry| now.saturating_sub(entry.touched_at) <= CURSOR_TTL_MS);
    if let Some((id, entry)) = cursors.iter_mut().find(|(_, entry)| {
        entry.monitor_id == monitor_id
            && entry.target_id == target_id
            && entry.source_id == source_id
            && entry.last_receipt.as_deref() == last_receipt
    }) {
        entry.touched_at = now;
        return Ok(id.clone());
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
        "error-cursor-{now}-{}",
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    cursors.insert(
        id.clone(),
        LogCursor {
            monitor_id: monitor_id.into(),
            target_id: target_id.into(),
            source_id: source_id.into(),
            last_receipt: last_receipt.map(str::to_owned),
            touched_at: now,
        },
    );
    Ok(id)
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LogQuery {
    cursor: Option<String>,
    limit: Option<usize>,
}

fn record_view(record: &IncidentRecord) -> Value {
    json!({
        "id":record.id,
        "timestamp":record.first_seen,
        "timestamp_kind":"received_at",
        "level":"ERROR",
        "message":record.summary,
        "event":record.evidence["log"]["fingerprint"],
        "node_log_id":record.evidence["log"]["id"],
        "sequence":record.evidence["log"]["sequence"],
        "generation":record.evidence["generation"],
    })
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
    let handle = state.application.monitoring().ok_or_else(|| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "logs_unavailable",
            "error record receiver is not configured",
        )
    })?;
    let monitor = handle
        .definition(&id)
        .map_err(|error| ApiError::unavailable(error.to_string()))?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "not_found", "monitor not found"))?;
    let after = resolve_cursor(
        &state,
        &id,
        &monitor.target_id,
        &monitor.source_id,
        query.cursor.as_deref(),
    )?;
    let mut anchor_found = after.is_none();
    let mut records = BTreeMap::new();
    // Keep only the first bounded page plus one successor while holding the
    // store's read lock. Receipt IDs encode the immutable global commit order.
    handle
        .map_incidents(|record| {
            if record.kind != IncidentKind::ErrorLog
                || record.monitor_id != id
                || record.target_id != monitor.target_id
                || record.evidence["source_id"].as_str() != Some(monitor.source_id.as_str())
            {
                return;
            }
            if after.as_ref() == Some(&record.id) {
                anchor_found = true;
            }
            if after.as_ref().is_some_and(|anchor| record.id <= *anchor) {
                return;
            }
            if records.len() < limit + 1
                || records
                    .last_key_value()
                    .is_some_and(|(last, _)| &record.id < last)
            {
                records.insert(record.id.clone(), record_view(record));
                if records.len() > limit + 1 {
                    records.pop_last();
                }
            }
        })
        .map_err(|error| ApiError::unavailable(error.to_string()))?;
    if !anchor_found {
        return Err(invalid_cursor());
    }
    let snapshot = handle
        .snapshot()
        .map_err(|error| ApiError::unavailable(error.to_string()))?;
    let view = snapshot.monitors.iter().find(|view| view.id == id);
    let source_error = snapshot
        .runtime_error
        .as_ref()
        .or_else(|| view.and_then(|view| view.last_error.as_ref()));
    let mut output = json!({
        "items":[], "errors":[], "next_cursor":null, "has_more":false,
        "stream_label":monitor.source_id, "coverage":view.map(|view| view.coverage),
        "source_error":source_error, "available_streams":[], "reset":false,
        "limit":limit, "read_at":timestamp(), "monitor_id":id,
        "target_id":monitor.target_id, "source_id":monitor.source_id,
        "log_source_id":monitor.source_id, "extension_id":monitor.extension_id,
        "record_kind":"node_error", "full_log":false, "auto_retry":false,
    });
    let mut items = Vec::new();
    let mut anchor = after;
    let mut bytes = serde_json::to_vec(&output)?.len() + 256;
    let mut has_more = false;
    for (receipt, record) in records {
        // Both compatibility arrays contain the same records. Account for both
        // copies and commas; never advance a cursor past an omitted record.
        let record_bytes = serde_json::to_vec(&record)?.len() * 2 + 2;
        if items.len() == limit || bytes + record_bytes > MAX_LOG_BYTES {
            has_more = true;
            break;
        }
        bytes += record_bytes;
        anchor = Some(receipt);
        items.push(record);
    }
    if items.is_empty() && has_more {
        return Err(ApiError::unavailable(
            "one accepted error record exceeds the response limit",
        ));
    }
    output["items"] = json!(items);
    output["errors"] = output["items"].clone();
    output["has_more"] = json!(has_more);
    output["next_cursor"] = json!(issue_cursor(
        &state,
        &id,
        &monitor.target_id,
        &monitor.source_id,
        anchor.as_deref(),
    )?);
    if serde_json::to_vec(&output)?.len() > MAX_LOG_BYTES {
        return Err(ApiError::unavailable(
            "accepted error record page exceeds 256 KiB",
        ));
    }
    Ok(Json(output))
}
