//! HTTP routing, authentication, CORS and official static delivery.
use super::handlers::*;
use super::*;

pub(super) fn router(console: Arc<Console>) -> Router {
    let api = Router::new()
        .route("/api/v1/recovery/status", get(recovery::status))
        .route("/api/v1/recovery/tasks", get(recovery::tasks))
        .route("/api/v1/recovery/tasks/{id}", get(recovery::task))
        .route(
            "/api/v1/recovery/tasks/{id}/approval",
            get(recovery::approval),
        )
        .route(
            "/api/v1/recovery/tasks/{id}/decision",
            post(recovery::decision),
        )
        .route("/api/v1/recovery/tasks/{id}/resume", post(recovery::resume))
        .route(
            "/api/v1/recovery/tasks/{id}/check_result",
            post(recovery::check_result),
        )
        .route(
            "/api/v1/recovery/knowledge/search",
            post(recovery::knowledge),
        )
        .route("/api/v1/bootstrap", get(bootstrap))
        .route("/api/v1/monitors", get(monitoring::monitors))
        .route("/api/v1/monitors/{id}", get(monitoring::monitor))
        .route("/api/v1/monitors/{id}/logs", get(project_logs::logs))
        .route(
            "/api/v1/monitoring/plugins/{id}",
            get(monitoring::plugin_monitoring),
        )
        .route("/api/v1/ui/catalog", get(monitoring::plugin_ui_catalog))
        .route(
            "/api/v1/ui/plugins/{plugin_id}/views/{view_id}",
            get(monitoring::plugin_ui_view),
        )
        .route("/api/v1/incidents", get(monitoring::incidents))
        .route("/api/v1/incidents/{id}", get(monitoring::incident))
        .route(
            "/api/v1/incidents/{id}/acknowledge",
            post(monitoring::acknowledge),
        )
        .route("/api/v1/operations", get(operation_list))
        .route("/api/v1/repairs", get(repair_list))
        .route("/api/v1/repairs/{id}", get(repair_detail))
        .route("/api/v1/approvals", get(approval_list))
        .route("/api/v1/approvals/{id}", get(approval_detail))
        .route("/api/v1/harness/{id}/projects", get(projects))
        .route("/api/v1/harness/{id}/runs", post(harness_run))
        .route("/api/v1/repairs/runs", post(repair_run))
        .route("/api/v1/approvals/{id}/{action}", post(approval))
        .route("/api/v1/simulations", get(simulation_list).post(simulation))
        .route("/api/v1/operations/{id}", get(operation))
        .route("/api/v1/operations/{id}/cancel", post(cancel))
        .route("/api/v1/logs", get(logs))
        .route("/api/v1/extensions/{id}/query", post(extension_query))
        .fallback(|| async {
            ApiError::new(StatusCode::NOT_FOUND, "not_found", "unknown API route")
        })
        .layer(DefaultBodyLimit::max(128 * 1024))
        .layer(middleware::from_fn_with_state(
            console.clone(),
            authenticate,
        ))
        .with_state(console.clone());
    let assets = Router::new()
        .route("/", get(asset_index))
        .route("/{name}", get(asset))
        .with_state(console);
    api.merge(assets)
}

async fn authenticate(State(state): State<Arc<Console>>, request: Request, next: Next) -> Response {
    let origin = request
        .headers()
        .get("origin")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    if let Some(origin) = &origin {
        let same = request
            .headers()
            .get("host")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|host| {
                origin == &format!("http://{host}") || origin == &format!("https://{host}")
            });
        if !same && !state.config.allowed_origins.contains(origin) {
            return ApiError::new(
                StatusCode::FORBIDDEN,
                "origin_denied",
                "origin is not allowed",
            )
            .into_response();
        }
    }
    if request.method() == Method::OPTIONS {
        let mut response = StatusCode::NO_CONTENT.into_response();
        cors(&mut response, origin.as_deref());
        return response;
    }
    let supplied = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .map(str::as_bytes)
        .unwrap_or_default();
    let mut mismatch = supplied.len() ^ state.secret.len();
    for (i, byte) in state.secret.iter().enumerate() {
        mismatch |= usize::from(*byte ^ supplied.get(i).copied().unwrap_or(0));
    }
    if mismatch != 0 {
        let mut response = ApiError::new(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "valid Bearer token required",
        )
        .into_response();
        cors(&mut response, origin.as_deref());
        return response;
    }
    let mut response = next.run(request).await;
    if response.status().is_client_error()
        && !response
            .headers()
            .get("content-type")
            .is_some_and(|value| value.as_bytes().starts_with(b"application/json"))
    {
        response = ApiError::new(
            response.status(),
            "invalid_request",
            "request path, content type or JSON body was rejected",
        )
        .into_response();
    }
    response
        .headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    cors(&mut response, origin.as_deref());
    response
}
fn cors(response: &mut Response, origin: Option<&str>) {
    if let Some(origin) = origin.and_then(|s| HeaderValue::from_str(s).ok()) {
        response
            .headers_mut()
            .insert("access-control-allow-origin", origin);
        response
            .headers_mut()
            .insert("vary", HeaderValue::from_static("Origin"));
        response.headers_mut().insert(
            "access-control-allow-methods",
            HeaderValue::from_static("GET, POST, OPTIONS"),
        );
        response.headers_mut().insert(
            "access-control-allow-headers",
            HeaderValue::from_static("Authorization, Content-Type"),
        );
    }
}
async fn asset_index(State(state): State<Arc<Console>>) -> Response {
    static_asset(&state, "index.html")
}
async fn asset(State(state): State<Arc<Console>>, RoutePath(name): RoutePath<String>) -> Response {
    static_asset(&state, &name)
}
fn static_asset(state: &Console, name: &str) -> Response {
    let Some((mime, bytes)) = state.assets.get(name) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    (
        [
            ("content-type", mime),
            ("cache-control", "no-cache"),
            ("x-content-type-options", "nosniff"),
        ],
        bytes,
    )
        .into_response()
}
