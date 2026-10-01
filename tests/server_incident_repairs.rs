//! Source-fault provenance persists without granting approval or dispatching a repair.
use super::{AUTH, call, config};
use crate::repair::{RepairConfig, now};
use crate::server::{Console, monitoring::repair_source_incident, router, timestamp};
use recuvora_core::recovery::approval::{
    ApprovalPolicy, ApprovalStore, ApprovalStoreConfig, ReviewerConfig,
};
use recuvora_core::recovery::incidents::{
    IncidentKind, IncidentSignal, IncidentStore, IncidentStoreConfig, MonitorCommit,
    SignalCondition,
};
use serde_json::json;

#[tokio::test]
async fn incident_provenance_is_scope_checked_durable_and_never_dispatches_without_session() {
    let mut cfg = config("incident-repair-provenance");
    let directory = cfg.data_dir.clone();
    cfg.permissions.extend([
        "monitor.read".into(),
        "incident.read".into(),
        "repair.run".into(),
    ]);
    for name in ["monitoring", "target", "review", "approval-state"] {
        std::fs::create_dir(directory.join(name)).unwrap();
    }
    let target_file = directory.join("target/example.txt");
    std::fs::write(&target_file, "before").unwrap();
    let journal_path = directory.join("monitoring/incidents.jsonl");
    let mut incidents = IncidentStore::open(&journal_path, IncidentStoreConfig::default()).unwrap();
    for (monitor, target) in [
        ("monitor-a", "configured-target"),
        ("monitor-b", "other-target"),
    ] {
        incidents
            .commit(MonitorCommit {
                monitor_id: monitor.into(),
                sequence: 1,
                checkpoint: json!({}),
                now_ms: timestamp(),
                signals: vec![IncidentSignal {
                    monitor_id: monitor.into(),
                    target_id: target.into(),
                    rule_id: "health".into(),
                    kind: IncidentKind::Target,
                    condition: SignalCondition::Active,
                    summary: "Captured target error".into(),
                    evidence: json!({"untrusted":"must not enlarge target scope"}),
                }],
            })
            .unwrap();
    }
    let source = incidents
        .list()
        .into_iter()
        .find(|record| record.monitor_id == "monitor-a")
        .unwrap();
    let other = incidents
        .list()
        .into_iter()
        .find(|record| record.monitor_id == "monitor-b")
        .unwrap();
    drop(incidents);
    let monitors_path = directory.join("monitors.json");
    std::fs::write(&monitors_path, r#"{"schema_version":1,"monitors":[]}"#).unwrap();
    cfg.monitors_config = Some(monitors_path);
    let repair_config = RepairConfig {
        schema_version: 1,
        harness_config: directory.join("unused.json"),
        extensions_config: None,
        execution_workspace: None,
        reviewer_workspace: None,
        execution_harness: "unused".into(),
        target_id: "configured-target".into(),
        target_root: directory.join("target"),
        reviewer_directory: directory.join("review"),
        data_dir: directory.join("approval-state"),
        allowed_files: vec!["example.txt".into()],
        policy: ApprovalPolicy {
            id: "bound-policy".into(),
            version: 1,
            reviewer: ReviewerConfig::Human,
            delegation: "bounded fixture file only".into(),
            allowed_targets: vec!["configured-target".into()],
            allowed_action_kinds: vec!["replace_text".into()],
            ttl_secs: 600,
        },
        timeout_secs: 30,
        max_tool_calls: 4,
    };
    let (mut state, engine) = Console::open(cfg.clone()).await.unwrap();
    // Only trusted target metadata is supplied for the presentation helper; no Core session is created.
    std::sync::Arc::get_mut(&mut state).unwrap().repair_config = Some(repair_config.clone());
    for (id, revision, status) in [
        (Some(source.id.as_str()), Some(source.revision + 1), 409),
        (Some(other.id.as_str()), Some(other.revision), 409),
        (Some("missing-incident"), Some(1), 404),
        (Some(source.id.as_str()), None, 400),
    ] {
        let error = repair_source_incident(&state, id, revision).unwrap_err();
        assert_eq!(error.status.as_u16(), status);
    }
    assert!(
        repair_source_incident(&state, None, None)
            .unwrap()
            .is_none()
    );
    let provenance = repair_source_incident(&state, Some(&source.id), Some(source.revision))
        .unwrap()
        .unwrap();
    assert_eq!(provenance["id"], source.id);
    assert_eq!(provenance["revision"], source.revision);
    assert!(provenance.get("evidence").is_none());
    assert!(state.journal.lock().unwrap().records.is_empty());

    let listener = tokio::net::TcpListener::bind(cfg.listen).await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = router(state.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let request =
        json!({"operation_id":"not-dispatched","task_id":"not-dispatched","prompt":"diagnose",
        "incident_id":source.id,"incident_revision":source.revision})
        .to_string();
    let (status, body) = call(address, "POST", "/api/v1/repairs/runs", AUTH, &request).await;
    assert_eq!(status, 503, "{body}");
    assert_eq!(body["auto_retry"], false);
    assert!(
        state.journal.lock().unwrap().records.is_empty(),
        "unconfigured repair persisted a dispatch"
    );

    // Exercise Host's durable receipt projection explicitly, without pretending a repair ran.
    state
        .begin(
            "linked-operation".into(),
            "repair",
            json!({
                "taskId":"linked-task", "target":"configured-target", "sourceIncident":provenance,
            }),
        )
        .unwrap();
    state.finish(
        "linked-operation",
        Ok(json!({"status":"failed", "message":"fixture has no repair session; no business execution"})),
    );
    let detail = call(address, "GET", "/api/v1/repairs/linked-task", AUTH, "")
        .await
        .1;
    assert_eq!(detail["status"], "failed");
    assert_eq!(detail["sourceIncident"]["id"], source.id);
    assert_eq!(detail["sourceIncident"]["revision"], source.revision);
    assert_eq!(detail["target"], "configured-target");
    assert_eq!(detail["businessVerified"], false);
    let reverse = call(
        address,
        "GET",
        &format!("/api/v1/incidents/{}", source.id),
        AUTH,
        "",
    )
    .await
    .1;
    assert_eq!(reverse["related_repairs"]["total"], 1);
    assert_eq!(reverse["related_repairs"]["items"][0]["id"], "linked-task");
    assert!(
        state
            .begin(
                "different-operation".into(),
                "repair",
                json!({"taskId":"linked-task"})
            )
            .is_err()
    );
    assert_eq!(std::fs::read_to_string(&target_file).unwrap(), "before");
    server.abort();
    let _ = server.await;
    state.shutdown().await.unwrap();
    engine.shutdown().await.unwrap();
    drop(state);

    let (state, engine) = Console::open(cfg.clone()).await.unwrap();
    let persistent = crate::server::history::repair_detail(&state, "linked-task").unwrap();
    assert_eq!(persistent["status"], "failed");
    assert_eq!(persistent["sourceIncident"]["id"], source.id);
    assert_eq!(persistent["sourceIncident"]["revision"], source.revision);
    assert_eq!(persistent["target"], "configured-target");
    assert_eq!(
        crate::server::history::incident_repairs(&state, &source.id).unwrap()["total"],
        1
    );
    state.shutdown().await.unwrap();
    engine.shutdown().await.unwrap();
    drop(state);

    cfg.permissions
        .retain(|permission| permission != "incident.read");
    let (mut state, engine) = Console::open(cfg).await.unwrap();
    std::sync::Arc::get_mut(&mut state).unwrap().repair_config = Some(repair_config.clone());
    let error =
        repair_source_incident(&state, Some(&source.id), Some(source.revision)).unwrap_err();
    assert_eq!(error.status.as_u16(), 403);
    state.shutdown().await.unwrap();
    engine.shutdown().await.unwrap();
    drop(state);
    let approvals = ApprovalStore::open(
        &repair_config.data_dir,
        ApprovalStoreConfig::default(),
        now().unwrap(),
    )
    .unwrap();
    assert!(
        approvals.list().is_empty(),
        "provenance created business approval"
    );
    drop(approvals);
    assert_eq!(std::fs::read_to_string(target_file).unwrap(), "before");
    assert_eq!(
        directory.parent(),
        Some(
            std::path::PathBuf::from(std::env::var_os("RECUVORA_TEST_TEMP").unwrap())
                .canonicalize()
                .unwrap()
                .as_path()
        )
    );
    std::fs::remove_dir_all(directory).unwrap();
}
