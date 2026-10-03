//! Explicit refresh of read-only page descriptors; never executes page actions.
use super::*;
use crate::protocol::{ExtensionError, ExtensionKind, valid_id};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RefreshInput {}

pub(super) async fn refresh(
    State(state): State<Arc<Console>>,
    RoutePath(id): RoutePath<String>,
    Json(_input): Json<RefreshInput>,
) -> Result<Json<Value>, ApiError> {
    state.require("extension.read")?;
    if !valid_id(&id) {
        return Err(ApiError::invalid("invalid plugin identity"));
    }
    let registry = state
        .application
        .extensions()
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "not_found", "plugin not found"))?;
    if !registry
        .definitions()
        .iter()
        .any(|d| d.id == id && d.kind == ExtensionKind::Plugin)
    {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            "plugin not found",
        ));
    }
    let snapshot = registry
        .refresh_ui_links(&id, HarnessCancellation::new())
        .await
        .map_err(|error| match error {
            ExtensionError::Rejected(_) => ApiError::new(
                StatusCode::CONFLICT,
                "busy",
                "page refresh cannot be dispatched",
            ),
            _ => ApiError::unavailable("page refresh is unavailable"),
        })?;
    Ok(Json(
        json!({"schema_version":1,"plugin":snapshot,"auto_retry":false}),
    ))
}
