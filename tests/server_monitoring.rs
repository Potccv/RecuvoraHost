use super::{AUTH, call, config};
use crate::persistence::incidents::{
    IncidentKind, IncidentSignal, IncidentStore, IncidentStoreConfig, MonitorCommit,
    SignalCondition,
};
use crate::server::{Console, router, timestamp};
use serde_json::{Value, json};

#[tokio::test]
async fn incident_api_paginates_evidence_and_acknowledges_with_authenticated_revision() {
    let mut cfg = config("incidents");
    let directory = cfg.data_dir.clone();
    let monitoring = directory.join("monitoring");
    std::fs::create_dir(&monitoring).unwrap();
    let journal_path = monitoring.join("incidents.jsonl");
    let mut store = IncidentStore::open(&journal_path, IncidentStoreConfig::default()).unwrap();
    for index in 0..31 {
        let monitor_id = format!("monitor-{index:03}");
        store
            .commit(MonitorCommit {
                monitor_id: monitor_id.clone(),
                sequence: 1,
                checkpoint: json!({}),
                now_ms: timestamp(),
                signals: vec![IncidentSignal {
                    monitor_id,
                    target_id: "target-a".into(),
                    rule_id: "rule-a".into(),
                    kind: IncidentKind::Target,
                    condition: SignalCondition::Active,
                    summary: "Configured condition did not match".into(),
                    evidence: json!({"raw":"<script>untrusted</script>".repeat(200)}),
                }],
            })
            .unwrap();
    }
    drop(store);
    let monitor_config = directory.join("monitors.json");
    std::fs::write(&monitor_config, r#"{"schema_version":1,"monitors":[]}"#).unwrap();
    cfg.monitors_config = Some(monitor_config);
    cfg.permissions.extend([
        "monitor.read".into(),
        "incident.read".into(),
        "incident.acknowledge".into(),
    ]);
    let (state, engine) = Console::open(cfg.clone()).await.unwrap();
    let listener = tokio::net::TcpListener::bind(cfg.listen).await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = router(state.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    assert_eq!(
        call(address, "GET", "/api/v1/incidents", "", "").await.0,
        401
    );
    let bootstrap = call(address, "GET", "/api/v1/bootstrap", AUTH, "").await.1;
    assert_eq!(bootstrap["monitoring"]["configured"], true);
    assert_eq!(bootstrap["monitoring"]["monitors"], json!([]));
    assert_eq!(bootstrap["monitoring"]["discoveries"], json!([]));
    assert_eq!(bootstrap["monitoring"]["counts"]["incidents"]["active"], 31);
    let (status, first) = call(address, "GET", "/api/v1/incidents", AUTH, "").await;
    assert_eq!(status, 200);
    assert_eq!(first["total"], 31);
    assert_eq!(
        first["counts"],
        json!({"open":31,"acknowledged":0,"resolved":0,"active":31,"total":31})
    );
    let filtered = call(
        address,
        "GET",
        "/api/v1/incidents?target_id=target-a&monitor_id=monitor-001&kind=target&query=condition",
        AUTH,
        "",
    )
    .await
    .1;
    assert_eq!(filtered["total"], 1);
    assert_eq!(
        filtered["counts"]["total"], 31,
        "filtered pages must retain complete global counts"
    );
    assert_eq!(filtered["items"][0]["monitor_id"], "monitor-001");
    assert_eq!(
        call(
            address,
            "GET",
            "/api/v1/incidents?target_id=other",
            AUTH,
            ""
        )
        .await
        .1["total"],
        0
    );
    assert_eq!(
        call(address, "GET", "/api/v1/incidents?kind=unknown", AUTH, "")
            .await
            .0,
        400
    );
    assert_eq!(first["items"].as_array().unwrap().len(), 25);
    assert!(
        first["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item.get("evidence").is_none())
    );
    let id = first["items"][0]["id"].as_str().unwrap();
    let cursor = first["next_cursor"].as_str().unwrap();
    let second = call(
        address,
        "GET",
        &format!("/api/v1/incidents?cursor={cursor}"),
        AUTH,
        "",
    )
    .await
    .1;
    assert_eq!(second["items"].as_array().unwrap().len(), 6);
    assert!(second["next_cursor"].is_null());
    assert_eq!(
        call(address, "GET", "/api/v1/incidents?limit=101", AUTH, "")
            .await
            .0,
        400
    );

    let path = format!("/api/v1/incidents/{id}");
    let original = call(address, "GET", &path, AUTH, "").await.1;
    assert!(
        original["evidence"]["raw"]
            .as_str()
            .unwrap()
            .starts_with("<script>")
    );
    assert_eq!(original["allowed_actions"], json!(["acknowledge"]));
    let ack_path = format!("{path}/acknowledge");
    let body =
        json!({"revision":original["revision"],"note":"Reviewing the actual target"}).to_string();
    assert_eq!(
        call(
            address,
            "POST",
            &ack_path,
            AUTH,
            &json!({"revision":original["revision"],"note":"test","actor":"forged"}).to_string()
        )
        .await
        .0,
        422
    );
    let (status, ack) = call(address, "POST", &ack_path, AUTH, &body).await;
    assert_eq!(status, 200);
    assert_eq!(ack["status"], "acknowledged");
    assert_eq!(ack["acknowledgement"]["actor"], "test-operator");
    assert_eq!(ack["condition"], "active");
    assert_eq!(ack["business_verified"], false);
    assert_eq!(ack["allowed_actions"], json!([]));
    assert_eq!(call(address, "POST", &ack_path, AUTH, &body).await.0, 409);
    let ack_filtered = call(
        address,
        "GET",
        "/api/v1/incidents?status=acknowledged",
        AUTH,
        "",
    )
    .await
    .1;
    assert_eq!(ack_filtered["total"], 1);
    assert_eq!(ack_filtered["counts"]["open"], 30);
    assert_eq!(ack_filtered["counts"]["active"], 31);
    assert!(
        state.journal.lock().unwrap().records.is_empty(),
        "acknowledgement must not create a repair operation"
    );

    server.abort();
    let _ = server.await;
    let retained_handle = state.monitoring.clone().unwrap();
    state.shutdown().await.unwrap();
    // Runtime shutdown releases the durable writer even with cloned service handles.
    let reopened = IncidentStore::open(&journal_path, IncidentStoreConfig::default()).unwrap();
    assert_eq!(
        serde_json::to_value(reopened.get(id).unwrap()).unwrap()["status"],
        "acknowledged"
    );
    drop(reopened);
    assert!(
        retained_handle
            .acknowledge(id, ack["revision"].as_u64().unwrap(), "operator", "late")
            .is_err()
    );
    engine.shutdown().await.unwrap();
    drop(retained_handle);
    drop(state);

    cfg.permissions.retain(|permission| {
        !permission.starts_with("monitor.") && !permission.starts_with("incident.")
    });
    let (restricted, engine) = Console::open(cfg.clone()).await.unwrap();
    assert!(crate::server::views::bootstrap(&restricted).unwrap()["monitoring"].is_null());
    let listener = tokio::net::TcpListener::bind(cfg.listen).await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = router(restricted.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    for (method, path, body) in [
        ("GET", "/api/v1/monitors", ""),
        ("GET", "/api/v1/monitoring/plugins/any-plugin", ""),
        ("GET", "/api/v1/incidents", ""),
        ("POST", ack_path.as_str(), body.as_str()),
    ] {
        assert_eq!(call(address, method, path, AUTH, body).await.0, 403);
    }
    server.abort();
    let _ = server.await;
    restricted.shutdown().await.unwrap();
    engine.shutdown().await.unwrap();
    drop(restricted);
    assert!(
        directory.parent().unwrap()
            == std::path::PathBuf::from(std::env::var_os("RECUVORA_TEST_TEMP").unwrap())
                .canonicalize()
                .unwrap()
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn shipped_monitor_profile_is_valid_and_bad_rules_are_rejected() {
    let config: crate::monitoring::MonitorsConfig =
        serde_json::from_str(include_str!("../profiles/monitors.example.json")).unwrap();
    config.validate().unwrap();
    let mut raw = serde_json::to_value(config).unwrap();
    raw["monitors"][0]["params"]["cursor"] = Value::String("injected".into());
    assert!(
        serde_json::from_value::<crate::monitoring::MonitorsConfig>(raw)
            .unwrap()
            .validate()
            .is_err()
    );
}
