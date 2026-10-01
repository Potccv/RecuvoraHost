use super::{AUTH, call, config};
use crate::server::project_logs::{project, validate_sources};
use crate::server::{Console, LogSourceConfig, router};
use serde_json::json;

fn source() -> LogSourceConfig {
    serde_json::from_value(
        json!({"id":"record-source","extension_id":"observation-provider",
        "contract":"example.records","version":1,"method":"query","monitor_contract":"example.observation",
        "parameter_bindings":{"target_key":"/target_key"},"error_levels":["ERROR"],
        "error_events":["condition_active"]}),
    )
    .unwrap()
}

#[test]
fn observation_record_projection_preserves_actual_error_records_and_enforces_bounds() {
    let source = source();
    validate_sources(std::slice::from_ref(&source)).unwrap();
    let result = json!({"entries":[
        {"id":"line-1","timestamp":"2026-09-16 10:00:00","level":"INFO","message":"<script>read as data</script>"},
        {"id":"line-2","timestamp":12,"level":"ERROR","message":"actual target error","event":"exception"},
        {"id":"line-3","level":"WARNING","message":"condition active","event":"condition_active"}],
        "next_cursor":"opaque-provider-position","has_more":false,"stream_label":"stream-a",
        "available_streams":[],
        "coverage":"partial","error":"source_partial"});
    let projected = project(&source, &result, 32).unwrap();
    assert_eq!(projected["items"].as_array().unwrap().len(), 3);
    assert_eq!(projected["errors"].as_array().unwrap().len(), 2);
    assert_eq!(projected["stream_label"], "stream-a");
    assert_eq!(projected["available_streams"], json!([]));
    assert_eq!(
        projected["items"][0]["message"],
        "<script>read as data</script>"
    );
    assert_eq!(projected["errors"][0]["id"], "line-2");
    assert_eq!(projected["source_error"], "source_partial");
    assert!(project(&source, &result, 2).is_err());
    let mut invalid = result.clone();
    invalid["entries"][1]["id"] = json!("line-1");
    assert!(
        project(&source, &invalid, 32).is_err(),
        "duplicate source event IDs are not silently accepted"
    );
    invalid = result.clone();
    invalid["entries"][0]["message"] = json!("x".repeat(16 * 1024 + 1));
    assert!(project(&source, &invalid, 32).is_err());
    invalid = result.clone();
    invalid["next_cursor"] = serde_json::Value::Null;
    assert!(
        project(&source, &invalid, 32).is_err(),
        "records require an incrementally readable continuation"
    );
    let empty = project(
        &source,
        &json!({"entries":[],"next_cursor":null,"has_more":false,"error":"source_empty"}),
        32,
    )
    .unwrap();
    assert_eq!(
        empty["errors"],
        json!([]),
        "collector failure is not an observed target record error"
    );
}

#[test]
fn trusted_log_sources_reject_ambiguous_or_client_position_bindings() {
    let source = source();
    assert!(validate_sources(&[source.clone(), source.clone()]).is_err());
    let mut invalid = source.clone();
    invalid
        .parameter_bindings
        .insert("cursor".into(), "/target_key".into());
    assert!(validate_sources(&[invalid]).is_err());
    let mut invalid = source.clone();
    invalid
        .parameter_bindings
        .insert("other".into(), "/broken~pointer".into());
    assert!(validate_sources(&[invalid]).is_err());
    let mut invalid = source;
    invalid.contract = "recuvora.approval".into();
    assert!(validate_sources(&[invalid]).is_err());
}

#[tokio::test]
async fn log_cursors_bind_fixed_target_and_provider_and_remain_bounded() {
    use crate::server::project_logs::{issue_cursor, resolve_cursor};
    let cfg = config("log-cursors");
    let directory = cfg.data_dir.clone();
    let (state, engine) = Console::open(cfg).await.unwrap();
    let id = issue_cursor(
        &state,
        "monitor-a",
        "source-a",
        Some("private-provider-position"),
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        resolve_cursor(&state, "monitor-a", "source-a", Some(&id))
            .unwrap()
            .as_deref(),
        Some("private-provider-position")
    );
    assert!(resolve_cursor(&state, "monitor-b", "source-a", Some(&id)).is_err());
    assert!(resolve_cursor(&state, "monitor-a", "source-b", Some(&id)).is_err());
    assert!(
        resolve_cursor(
            &state,
            "monitor-a",
            "source-a",
            Some("private-provider-position")
        )
        .is_err()
    );
    assert!(
        resolve_cursor(
            &state,
            "monitor-a",
            "source-a",
            Some("guessed-unissued-cursor")
        )
        .is_err()
    );
    assert_eq!(
        issue_cursor(
            &state,
            "monitor-a",
            "source-a",
            Some("private-provider-position")
        )
        .unwrap()
        .as_deref(),
        Some(id.as_str())
    );
    for index in 0..300 {
        issue_cursor(
            &state,
            "monitor-a",
            "source-a",
            Some(&format!("position-{index}")),
        )
        .unwrap();
    }
    assert_eq!(state.log_cursors.lock().unwrap().len(), 256);
    assert!(issue_cursor(&state, "monitor-a", "source-a", Some(&"x".repeat(4097))).is_err());
    state.shutdown().await.unwrap();
    engine.shutdown().await.unwrap();
    drop(state);
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn observation_record_api_requires_all_read_permissions_and_reports_unconfigured_sources() {
    let mut cfg = config("record-permissions");
    cfg.permissions
        .extend(["monitor.read".into(), "extension.read".into()]);
    let directory = cfg.data_dir.clone();
    let (state, engine) = Console::open(cfg.clone()).await.unwrap();
    let listener = tokio::net::TcpListener::bind(cfg.listen).await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, router(state)).await.unwrap();
    });
    assert_eq!(
        call(address, "GET", "/api/v1/monitors/monitor-a/logs", "", "")
            .await
            .0,
        401
    );
    let (status, result) = call(address, "GET", "/api/v1/monitors/monitor-a/logs", AUTH, "").await;
    assert_eq!(status, 503);
    assert_eq!(result["error"]["code"], "logs_unavailable");
    assert_eq!(
        call(
            address,
            "GET",
            "/api/v1/monitors/monitor-a/logs?limit=33",
            AUTH,
            ""
        )
        .await
        .0,
        400
    );
    server.abort();
    let _ = server.await;
    engine.shutdown().await.unwrap();
    for permission in ["monitor.read", "extension.read", "logs.read"] {
        let mut restricted = cfg.clone();
        restricted.permissions.retain(|item| item != permission);
        let (state, engine) = Console::open(restricted).await.unwrap();
        let listener = tokio::net::TcpListener::bind(cfg.listen).await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, router(state)).await.unwrap();
        });
        assert_eq!(
            call(address, "GET", "/api/v1/monitors/monitor-a/logs", AUTH, "")
                .await
                .0,
            403
        );
        server.abort();
        let _ = server.await;
        engine.shutdown().await.unwrap();
    }
    std::fs::remove_dir_all(directory).unwrap();
}
