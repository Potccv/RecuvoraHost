use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn call(
    address: SocketAddr,
    method: &str,
    path: &str,
    headers: &str,
    body: &str,
) -> (u16, Value) {
    let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\nContent-Type: application/json\r\n{headers}Content-Length: {}\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut out = Vec::new();
    stream.read_to_end(&mut out).await.unwrap();
    let out = String::from_utf8(out).unwrap();
    let (head, body) = out.split_once("\r\n\r\n").unwrap();
    (
        head.split_whitespace().nth(1).unwrap().parse().unwrap(),
        serde_json::from_str(body).unwrap_or(Value::Null),
    )
}
fn config(name: &str) -> ServerConfig {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let base =
        std::env::var_os("RECUVORA_TEST_TEMP").expect("external RECUVORA_TEST_TEMP required");
    let base = PathBuf::from(base).canonicalize().unwrap();
    assert!(
        !base.starts_with(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .canonicalize()
                .unwrap()
        )
    );
    let data = base.join(format!(
        "server-{name}-{}-{}-{}",
        std::process::id(),
        timestamp(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir(&data).unwrap();
    let token = data.join("token");
    std::fs::write(&token, "test-token-0123456789-abcdefghijklmnop").unwrap();
    ServerConfig {
        schema_version: 1,
        listen: "127.0.0.1:0".parse().unwrap(),
        token_file: token,
        operator: "test-operator".into(),
        permissions: vec![
            "simulation.run".into(),
            "logs.read".into(),
            "operation.cancel".into(),
        ],
        allowed_origins: vec![],
        data_dir: data,
        harness_config: None,
        extensions_config: None,
        repair_config: None,
        monitors_config: None,
        recovery_config: None,
        log_sources: vec![],
        ui_dir: None,
    }
}
const AUTH: &str = "Authorization: Bearer test-token-0123456789-abcdefghijklmnop\r\n";

#[path = "server_monitoring.rs"]
mod monitoring_cases;

#[path = "server_project_logs.rs"]
mod project_logs_cases;

#[path = "server_static.rs"]
mod static_cases;

#[path = "server_recovery.rs"]
mod recovery_cases;

#[path = "server_incident_repairs.rs"]
#[cfg(windows)]
mod incident_repairs_cases;

#[tokio::test]
async fn large_history_is_bounded_and_cursor_survives_updates() {
    let cfg = config("bounded-history");
    let directory = cfg.data_dir.clone();
    let (state, engine) = Console::open(cfg.clone()).await.unwrap();
    let text = "large model response ".repeat(3500);
    for index in 0..80 {
        let id = format!("operation-{index:03}");
        state
            .begin(id.clone(), "repair", json!({"taskId":id}))
            .unwrap();
        if index != 5 {
            state.finish(
                &id,
                Ok(json!({"status":"completed","final_response":text,"business_verified":false})),
            );
        }
    }
    assert!(
        std::fs::metadata(directory.join("operations.jsonl"))
            .unwrap()
            .len()
            > 4 * 1024 * 1024
    );
    let listener = tokio::net::TcpListener::bind(cfg.listen).await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = router(state.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let (status, snapshot) = call(address, "GET", "/api/v1/bootstrap", AUTH, "").await;
    assert_eq!(status, 200);
    assert!(serde_json::to_vec(&snapshot).unwrap().len() < 128 * 1024);
    assert_eq!(snapshot["operations"].as_array().unwrap().len(), 25);
    assert_eq!(snapshot["pages"]["operations"]["total"], 80);
    assert_eq!(snapshot["pages"]["repairs"]["total"], 80);
    assert!(snapshot["operations"][0].get("result").is_none());
    assert!(snapshot["repairs"][0].get("finalResponse").is_none());
    let mut cursor = snapshot["pages"]["operations"]["next_cursor"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut ids: Vec<String> = snapshot["operations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["id"].as_str().unwrap().into())
        .collect();
    // A terminal update must not move an old record above the cursor and hide it.
    state.finish(
        "operation-005",
        Ok(json!({"status":"completed","final_response":text})),
    );
    state
        .begin("zzz-new".into(), "repair", json!({"taskId":"zzz-new"}))
        .unwrap();
    state.finish("zzz-new", Ok(json!({"status":"completed"})));
    loop {
        let (status, page) = call(
            address,
            "GET",
            &format!("/api/v1/operations?cursor={cursor}&limit=25"),
            AUTH,
            "",
        )
        .await;
        assert_eq!(status, 200);
        assert!(page["items"].as_array().unwrap().len() <= 25);
        ids.extend(
            page["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|item| item["id"].as_str().unwrap().to_owned()),
        );
        match page["next_cursor"].as_str() {
            Some(next) => cursor = next.to_owned(),
            None => break,
        }
    }
    assert_eq!(ids.len(), 80);
    assert_eq!(
        ids.iter().collect::<std::collections::BTreeSet<_>>().len(),
        80
    );
    assert!(ids.contains(&"operation-005".to_owned()));
    assert!(!ids.contains(&"zzz-new".to_owned()));
    let detail = call(address, "GET", "/api/v1/operations/operation-079", AUTH, "")
        .await
        .1;
    assert_eq!(detail["result"]["final_response"], text);
    let detail = call(address, "GET", "/api/v1/repairs/operation-079", AUTH, "")
        .await
        .1;
    assert_eq!(detail["finalResponse"], text);
    assert_eq!(
        call(address, "GET", "/api/v1/operations?limit=101", AUTH, "")
            .await
            .0,
        400
    );
    assert_eq!(call(address, "GET", "/api/v1/repairs", "", "").await.0, 401);
    server.abort();
    let _ = server.await;
    state.shutdown().await.unwrap();
    drop(state);
    engine.shutdown().await.unwrap();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn api_auth_permissions_receipts_and_restart() {
    let cfg = config("api");
    let dir = cfg.data_dir.clone();
    let (state, engine) = Console::open(cfg.clone()).await.unwrap();
    let listener = tokio::net::TcpListener::bind(cfg.listen).await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = router(state.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    assert_eq!(
        call(address, "GET", "/api/v1/bootstrap", "", "").await.0,
        401
    );
    assert_eq!(
        call(
            address,
            "GET",
            "/api/v1/bootstrap",
            &format!("{AUTH}Origin: https://evil.invalid\r\n"),
            ""
        )
        .await
        .0,
        403
    );
    let (status, body) = call(address, "GET", "/api/v1/bootstrap", AUTH, "").await;
    assert_eq!(status, 200);
    assert!(body["harnesses"].as_array().unwrap().is_empty());
    assert_eq!(body["mode"], "live");
    let input = r#"{"operation_id":"sim-1","task_id":"task-1","target":"closed","scenario":"succeed","timeout_ms":1000}"#;
    assert_eq!(
        call(address, "POST", "/api/v1/simulations", AUTH, input)
            .await
            .0,
        202
    );
    assert_eq!(
        call(address, "POST", "/api/v1/simulations", AUTH, input)
            .await
            .0,
        409
    );
    for _ in 0..100 {
        let op = call(address, "GET", "/api/v1/operations/sim-1", AUTH, "")
            .await
            .1;
        if op["status"] != "running" {
            assert_eq!(op["result"]["task"]["state"], "Succeeded");
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        call(address, "GET", "/api/v1/operations/sim-1", AUTH, "")
            .await
            .1["status"],
        "completed"
    );
    assert!(
        !call(address, "GET", "/api/v1/logs?limit=1", AUTH, "")
            .await
            .1["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        call(
            address,
            "POST",
            "/api/v1/approvals/any/approve",
            AUTH,
            r#"{"revision":1,"reason":"test"}"#
        )
        .await
        .0,
        403
    );
    state
        .begin("interrupted".into(), "harness", json!({}))
        .unwrap();
    server.abort();
    let _ = server.await;
    drop(state);
    engine.shutdown().await.unwrap();
    let (state, engine) = Console::open(cfg).await.unwrap();
    assert_eq!(
        lock(&state.journal).unwrap().records["interrupted"].status,
        "unknown"
    );
    assert!(
        state
            .begin("interrupted".into(), "harness", json!({}))
            .is_err()
    );
    drop(state);
    engine.shutdown().await.unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn configuration_rejects_network_listener_and_weak_token() {
    let mut cfg = config("config");
    let dir = cfg.data_dir.clone();
    cfg.listen = "0.0.0.0:8080".parse().unwrap();
    assert!(Console::open(cfg.clone()).await.is_err());
    cfg.listen = "127.0.0.1:0".parse().unwrap();
    std::fs::write(&cfg.token_file, "weak").unwrap();
    assert!(Console::open(cfg).await.is_err());
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn console_rejects_core_source_root_before_creating_operation_journal() {
    let mut cfg = config("core-source-protection");
    let temporary = cfg.data_dir.clone();
    let core = PathBuf::from(env!("RECUVORA_CORE_SOURCE_DIR"))
        .canonicalize()
        .unwrap();
    let journal = core.join("operations.jsonl");
    assert!(
        !journal.exists(),
        "source tree already contains an operation journal"
    );
    cfg.data_dir = core;
    let error = Console::open(cfg)
        .await
        .err()
        .expect("Core source data directory was accepted");
    assert!(error.to_string().contains("source"), "{error}");
    assert!(
        !journal.exists(),
        "rejected configuration created a source journal"
    );
    std::fs::remove_dir_all(temporary).unwrap();
}

fn accepted(id: &str) -> Operation {
    Operation {
        id: id.into(),
        kind: "harness".into(),
        status: "running".into(),
        updated_at: timestamp(),
        result: None,
        error: None,
        auto_retry: false,
        context: json!({"workspaceId":"default"}),
    }
}

fn completed(previous: &Operation) -> Operation {
    Operation {
        status: "completed".into(),
        result: Some(json!({"status":"completed","final_response":"fixture"})),
        ..previous.clone()
    }
}

fn write_records(path: &Path, records: &[Value]) {
    use std::io::Write;
    let mut file = std::fs::File::create(path).unwrap();
    for record in records {
        serde_json::to_writer(&mut file, record).unwrap();
        file.write_all(b"\n").unwrap();
    }
    file.sync_all().unwrap();
}

#[test]
fn journal_rejects_incomplete_tail_without_modifying_evidence() {
    for (label, tail) in [
        ("partial-json", b"{\"id\":\"broken".to_vec()),
        (
            "missing-newline",
            serde_json::to_vec(&accepted("second")).unwrap(),
        ),
    ] {
        let cfg = config(label);
        let path = cfg.data_dir.join("operations.jsonl");
        let mut bytes = serde_json::to_vec(&accepted("first")).unwrap();
        bytes.push(b'\n');
        bytes.extend(tail);
        std::fs::write(&path, &bytes).unwrap();
        assert!(Journal::open(&cfg.data_dir).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        std::fs::remove_dir_all(cfg.data_dir).unwrap();
    }
}

#[test]
fn journal_rejects_invalid_complete_records_and_impossible_transitions() {
    let initial = accepted("call");
    let terminal = completed(&initial);
    let mut unknown_state = json!(initial);
    unknown_state["status"] = "executing".into();
    let mut retry_enabled = json!(initial);
    retry_enabled["auto_retry"] = true.into();
    let mut extra_field = json!(initial);
    extra_field["authorization"] = true.into();
    let mut altered_kind = json!(terminal);
    altered_kind["kind"] = "repair".into();
    let mut altered_context = json!(terminal);
    altered_context["context"] = json!({"workspaceId":"other"});
    let mut inconsistent = json!(terminal);
    inconsistent["result"]["status"] = "unknown".into();
    let scenarios = vec![
        vec![unknown_state],
        vec![retry_enabled],
        vec![extra_field],
        vec![json!(terminal)],
        vec![json!(initial), json!(initial)],
        vec![json!(initial), json!(terminal), json!(initial)],
        vec![json!(initial), json!(terminal), json!(terminal)],
        vec![json!(initial), altered_kind],
        vec![json!(initial), altered_context],
        vec![json!(initial), inconsistent],
    ];
    for records in scenarios {
        let cfg = config("invalid-journal");
        let path = cfg.data_dir.join("operations.jsonl");
        write_records(&path, &records);
        let before = std::fs::read(&path).unwrap();
        assert!(
            Journal::open(&cfg.data_dir).is_err(),
            "accepted invalid records: {records:?}"
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
        std::fs::remove_dir_all(cfg.data_dir).unwrap();
    }
}

#[test]
fn journal_enforces_record_limit_before_recovery() {
    let cfg = config("large-record");
    let path = cfg.data_dir.join("operations.jsonl");
    let mut record = json!(accepted("large"));
    record["context"] = json!({"oversized":"x".repeat(1024*1024)});
    write_records(&path, &[record]);
    assert!(Journal::open(&cfg.data_dir).is_err());
    std::fs::remove_dir_all(cfg.data_dir).unwrap();
}

#[test]
fn journal_rejects_replay_on_append_and_retains_terminal_record() {
    let cfg = config("append-state");
    let mut journal = Journal::open(&cfg.data_dir).unwrap();
    let initial = accepted("one");
    journal.append(initial.clone()).unwrap();
    assert!(journal.append(initial.clone()).is_err());
    journal.append(completed(&initial)).unwrap();
    assert!(journal.append(initial.clone()).is_err());
    assert!(journal.append(completed(&initial)).is_err());
    assert_eq!(journal.events.len(), 2);
    drop(journal);
    let journal = Journal::open(&cfg.data_dir).unwrap();
    assert_eq!(journal.records["one"].status, "completed");
    assert_eq!(journal.events.len(), 2);
    drop(journal);
    std::fs::remove_dir_all(cfg.data_dir).unwrap();
}

#[tokio::test]
async fn console_configuration_allows_only_one_writer_until_released() {
    let cfg = config("single-writer");
    let (state, engine) = Console::open(cfg.clone()).await.unwrap();
    assert!(Console::open(cfg.clone()).await.is_err());
    drop(state);
    engine.shutdown().await.unwrap();
    let (state, engine) = Console::open(cfg.clone()).await.unwrap();
    drop(state);
    engine.shutdown().await.unwrap();
    std::fs::remove_dir_all(cfg.data_dir).unwrap();
}

#[cfg(windows)]
#[tokio::test]
async fn production_core_rejects_control_target_and_http_never_fabricates_approval() {
    use crate::persistence::approval::{
        ApprovalAssessment, ApprovalDecision, ApprovalError, ApprovalPolicy, ApprovalState,
        ApprovalStore, ApprovalStoreConfig, AssessmentSource, ProposedOperation, ReviewerConfig,
    };
    use crate::repair::now;
    let mut cfg = config("production-approval-boundary");
    cfg.permissions
        .extend(["approval.decide".into(), "approval.apply".into()]);
    for name in [".control/target", "review", "approval-state"] {
        std::fs::create_dir_all(cfg.data_dir.join(name)).unwrap();
    }
    let target = cfg.data_dir.join(".control/target").canonicalize().unwrap();
    let target_file = target.join("example.txt");
    std::fs::write(&target_file, "before").unwrap();
    let policy = ApprovalPolicy {
        id: "http-policy".into(),
        version: 1,
        reviewer: ReviewerConfig::Human,
        delegation: "replace fixture before with after".into(),
        allowed_targets: vec!["fixture-target".into()],
        allowed_action_kinds: vec!["replace_text".into()],
        ttl_secs: 600,
    };
    let repair_config = RepairConfig {
        schema_version: 1,
        harness_config: cfg.data_dir.join("unused-harness.json"),
        extensions_config: None,
        execution_workspace: None,
        reviewer_workspace: None,
        execution_harness: "unused".into(),
        target_id: "fixture-target".into(),
        target_root: target.clone(),
        reviewer_directory: cfg.data_dir.join("review"),
        data_dir: cfg.data_dir.join("approval-state"),
        allowed_files: vec!["example.txt".into()],
        policy: policy.clone(),
        timeout_secs: 30,
        max_tool_calls: 4,
    };
    let mut store = ApprovalStore::open(
        &repair_config.data_dir,
        ApprovalStoreConfig::default(),
        now().unwrap(),
    )
    .unwrap();
    let pending = store.request(ProposedOperation {
        task_id: "http-task".into(), task_revision: 1, operation_id: "http-operation".into(),
        target: "fixture-target".into(),
        action: json!({"kind":"replace_text","target_root":target,"path":"example.txt","expected":"before","replacement":"after"}),
    }, policy.clone(), now().unwrap()).unwrap();
    let assessment = ApprovalAssessment {
        decision: ApprovalDecision::Approve,
        reason: "reviewed exact fixture edit".into(),
        reviewer: AssessmentSource::Human {
            actor: cfg.operator.clone(),
        },
    };
    assert!(matches!(
        store.decide_human_at_revision(
            &pending.request.request_id,
            pending.revision + 1,
            assessment.clone(),
            &policy,
            now().unwrap()
        ),
        Err(ApprovalError::Conflict)
    ));
    assert_eq!(
        store.get(&pending.request.request_id).unwrap().state,
        ApprovalState::Pending
    );
    let approved = store
        .decide_human_at_revision(
            &pending.request.request_id,
            pending.revision,
            assessment,
            &policy,
            now().unwrap(),
        )
        .unwrap();
    assert_eq!(approved.state, ApprovalState::Approved);
    drop(store);

    // Core is a production dependency: Host tests cannot opt into its private test path exception.
    let error = RepairSession::open(repair_config.clone(), None, &[])
        .err()
        .expect("control target was accepted");
    assert!(error.to_string().contains("control directory"), "{error}");
    let (state, engine) = Console::open(cfg.clone()).await.unwrap();
    let listener = tokio::net::TcpListener::bind(cfg.listen).await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = router(state.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let approve = format!("/api/v1/approvals/{}/approve", pending.request.request_id);
    let apply = format!("/api/v1/approvals/{}/apply", pending.request.request_id);
    let valid =
        json!({"revision":approved.revision,"reason":"must remain unavailable"}).to_string();
    assert_eq!(call(address, "POST", &approve, "", &valid).await.0, 401);
    let forged = json!({"revision":approved.revision,"reason":"fixture","actor":"forged-operator"})
        .to_string();
    assert_eq!(call(address, "POST", &approve, AUTH, &forged).await.0, 422);
    for route in [&approve, &apply] {
        let (status, body) = call(address, "POST", route, AUTH, &valid).await;
        assert_eq!(status, 503, "{body}");
        assert_eq!(body["auto_retry"], false);
    }
    assert!(state.journal.lock().unwrap().records.is_empty());
    assert_eq!(std::fs::read_to_string(&target_file).unwrap(), "before");
    let store = ApprovalStore::open(
        &repair_config.data_dir,
        ApprovalStoreConfig::default(),
        now().unwrap(),
    )
    .unwrap();
    let persisted = store.get(&pending.request.request_id).unwrap();
    assert_eq!(persisted.state, ApprovalState::Approved);
    assert_eq!(persisted.revision, approved.revision);
    assert_eq!(
        persisted.assessment.as_ref().unwrap().reviewer,
        AssessmentSource::Human {
            actor: cfg.operator.clone()
        }
    );
    drop(store);
    server.abort();
    let _ = server.await;
    state.shutdown().await.unwrap();
    engine.shutdown().await.unwrap();
    drop(state);
    std::fs::remove_dir_all(cfg.data_dir).unwrap();
}
