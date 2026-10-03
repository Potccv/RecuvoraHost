use super::{AUTH, call, config};
use crate::server::{Console, router};
use serde_json::json;

#[tokio::test]
async fn receipt_cursors_bind_monitor_target_and_source_and_remain_bounded() {
    use crate::server::project_logs::{issue_cursor, resolve_cursor};
    let cfg = config("log-cursors");
    let directory = cfg.data_dir.clone();
    let (state, engine) = Console::open(cfg).await.unwrap();
    let receipt = "incident-0000000000000001-0000";
    let id = issue_cursor(&state, "monitor-a", "target-a", "source-a", Some(receipt)).unwrap();
    assert_eq!(
        resolve_cursor(&state, "monitor-a", "target-a", "source-a", Some(&id))
            .unwrap()
            .as_deref(),
        Some(receipt)
    );
    for (monitor, target, source) in [
        ("monitor-b", "target-a", "source-a"),
        ("monitor-a", "target-b", "source-a"),
        ("monitor-a", "target-a", "source-b"),
    ] {
        assert!(resolve_cursor(&state, monitor, target, source, Some(&id)).is_err());
    }
    for invalid in [receipt, "guessed-unissued-cursor", "invalid/cursor"] {
        assert!(
            resolve_cursor(&state, "monitor-a", "target-a", "source-a", Some(invalid)).is_err()
        );
    }
    assert_eq!(
        issue_cursor(&state, "monitor-a", "target-a", "source-a", Some(receipt)).unwrap(),
        id
    );
    let empty = issue_cursor(&state, "monitor-a", "target-a", "source-a", None).unwrap();
    assert_eq!(
        resolve_cursor(&state, "monitor-a", "target-a", "source-a", Some(&empty)).unwrap(),
        None
    );
    for index in 0..300 {
        issue_cursor(
            &state,
            "monitor-a",
            "target-a",
            "source-a",
            Some(&format!("receipt-{index}")),
        )
        .unwrap();
    }
    assert_eq!(state.log_cursors.lock().unwrap().len(), 256);
    assert!(
        issue_cursor(
            &state,
            "monitor-a",
            "target-a",
            "source-a",
            Some(&"x".repeat(129))
        )
        .is_err()
    );
    state.shutdown().await.unwrap();
    engine.shutdown().await.unwrap();
    drop(state);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn legacy_remote_log_sources_and_client_query_fields_are_rejected() {
    let cfg = config("removed-log-sources");
    let directory = cfg.data_dir.clone();
    let mut serialized = json!({
        "schema_version":1,"listen":"127.0.0.1:0","token_file":cfg.token_file,
        "operator":"fixture-operator","permissions":[],"allowed_origins":[],"data_dir":directory,
    });
    assert!(serde_json::from_value::<crate::server::ServerConfig>(serialized.clone()).is_ok());
    serialized["log_sources"] = json!([]);
    assert!(serde_json::from_value::<crate::server::ServerConfig>(serialized).is_err());
    assert!(
        serde_json::from_value::<crate::server::project_logs::LogQuery>(
            json!({"file":"outside.txt"})
        )
        .is_err()
    );
    assert!(
        serde_json::from_value::<crate::server::project_logs::LogQuery>(json!({"method":"query"}))
            .is_err()
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn error_receipt_api_requires_all_read_permissions_and_reports_missing_receiver() {
    let mut cfg = config("record-permissions");
    cfg.permissions
        .extend(["monitor.read".into(), "extension.read".into()]);
    let directory = cfg.data_dir.clone();
    let (state, engine) = Console::open(cfg.clone()).await.unwrap();
    let listener = tokio::net::TcpListener::bind(cfg.listen).await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = router(state.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
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
    state.shutdown().await.unwrap();
    engine.shutdown().await.unwrap();
    drop(state);
    for permission in ["monitor.read", "extension.read", "logs.read"] {
        let mut restricted = cfg.clone();
        restricted.permissions.retain(|item| item != permission);
        let (state, engine) = Console::open(restricted).await.unwrap();
        let listener = tokio::net::TcpListener::bind(cfg.listen).await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = router(state.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        assert_eq!(
            call(address, "GET", "/api/v1/monitors/monitor-a/logs", AUTH, "")
                .await
                .0,
            403
        );
        server.abort();
        let _ = server.await;
        state.shutdown().await.unwrap();
        engine.shutdown().await.unwrap();
        drop(state);
    }
    std::fs::remove_dir_all(directory).unwrap();
}
