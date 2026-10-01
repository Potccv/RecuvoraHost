use super::*;
use crate::integrations::recovery::*;
use recuvora_core::operation::Cancellation;
use recuvora_core::recovery::approval::ReviewerConfig;
use recuvora_core::recovery::knowledge::ScriptArtifact;

struct PendingBackend;
impl IncidentGuard for PendingBackend {
    fn with_current(
        &self,
        problem: &ProblemContext,
        commit: &mut dyn FnMut(IncidentReadiness) -> Result<(), RecoveryError>,
    ) -> Result<(), RecoveryError> {
        if problem.incident_id != "incident-fixture"
            || problem.target_id != "target"
            || problem.incident_revision != 1
        {
            return Err(RecoveryError::Invalid("unknown fixture incident".into()));
        }
        commit(IncidentReadiness::Active { revision: 1 })
    }
}

impl RepairBackend for PendingBackend {
    fn inspect<'a>(
        &'a self,
        target: &'a TargetBinding,
        _: Cancellation,
    ) -> RecoveryFuture<'a, TargetObservation> {
        Box::pin(async move {
            Ok(TargetObservation {
                target_id: target.target_id.clone(),
                facts: target.required_facts.clone(),
                evidence_refs: vec!["inspection:fixture".into()],
                observed_at_ms: SystemRecoveryClock.now_ms(),
            })
        })
    }
    fn diagnose(&self, input: DiagnosisInput, _: Cancellation) -> RecoveryFuture<'_, RepairPlan> {
        Box::pin(async move {
            Ok(RepairPlan {
                summary: "bounded fixture plan".into(),
                reusable: true,
                script: ScriptArtifact {
                    id: "script-fixture".into(),
                    version: 1,
                    language: "powershell".into(),
                    platform: "windows".into(),
                    source: "Write-Output 'fixture'".into(),
                    preconditions: input.config.target.required_facts,
                    generated_by_harness: input.config.execution_harness,
                    generated_in_session: "diagnosis-fixture".into(),
                },
            })
        })
    }
    fn review(&self, _: ReviewInput, _: Cancellation) -> RecoveryFuture<'_, ReviewOutput> {
        Box::pin(async { panic!("human review must not call a Harness") })
    }
    fn execute<'a>(
        &'a self,
        _: AuthorizedScript<'a>,
        _: Cancellation,
    ) -> RecoveryFuture<'a, ScriptReceipt> {
        Box::pin(async { panic!("HTTP decisions must not dispatch") })
    }
    fn verify(
        &self,
        _: VerificationInput,
        _: Cancellation,
    ) -> RecoveryFuture<'_, BusinessVerification> {
        Box::pin(async { panic!("no execution to verify") })
    }
}

#[tokio::test]
async fn recovery_http_enforces_permissions_and_explicit_configuration() {
    for writes_allowed in [false, true] {
        let mut cfg = config("recovery-unconfigured");
        cfg.permissions = vec!["recovery.read".into(), "knowledge.read".into()];
        if writes_allowed {
            cfg.permissions.extend(
                [
                    "recovery.decide",
                    "recovery.resume",
                    "recovery.check_result",
                ]
                .into_iter()
                .map(str::to_owned),
            );
        }
        let directory = cfg.data_dir.clone();
        let (state, engine) = Console::open(cfg).await.unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(axum::serve(listener, router(state.clone())).into_future());
        for path in [
            "/api/v1/recovery/status",
            "/api/v1/recovery/tasks",
            "/api/v1/recovery/tasks/task-fixture",
            "/api/v1/recovery/tasks/task-fixture/approval",
        ] {
            assert_eq!(call(address, "GET", path, AUTH, "").await.0, 503);
        }
        for (action, body) in [
            (
                "decision",
                json!({"revision":1,"decision":"approve","reason":"fixture"}),
            ),
            ("resume", json!({"revision":1})),
            (
                "check_result",
                json!({"revision":1,"operation_id":"check_result-fixture"}),
            ),
        ] {
            assert_eq!(
                call(
                    address,
                    "POST",
                    &format!("/api/v1/recovery/tasks/task-fixture/{action}"),
                    AUTH,
                    &body.to_string()
                )
                .await
                .0,
                if writes_allowed { 503 } else { 403 },
            );
        }
        assert_eq!(
            call(
                address,
                "POST",
                "/api/v1/recovery/knowledge/search",
                AUTH,
                &json!({"conditions":{"release":"1"},"limit":1}).to_string()
            )
            .await
            .0,
            503,
        );
        server.abort();
        let _ = server.await;
        state.shutdown().await.unwrap();
        engine.shutdown().await.unwrap();
        drop(state);
        std::fs::remove_dir_all(directory).unwrap();
    }
}

#[tokio::test]
async fn recovery_http_uses_core_records_revisions_and_authenticated_actor() {
    let mut cfg = config("recovery-http");
    cfg.permissions.extend(
        [
            "recovery.read",
            "recovery.decide",
            "recovery.resume",
            "recovery.check_result",
            "knowledge.read",
        ]
        .into_iter()
        .map(str::to_owned),
    );
    let directory = cfg.data_dir.clone();
    let (mut state, engine) = Console::open(cfg).await.unwrap();
    let mut settings: crate::configuration::RecoveryHostConfig =
        serde_json::from_str(include_str!("../profiles/repair.recovery.example.json")).unwrap();
    settings.recovery.approval.reviewer = ReviewerConfig::Human;
    let recovery = RecoveryService::open(
        directory.join("authority"),
        settings.recovery.clone(),
        Arc::new(PendingBackend),
    )
    .unwrap();
    recovery
        .bind_target_ownership(Arc::new(
            FileTargetOwnership::open(directory.join("target-ownership")).unwrap(),
        ))
        .unwrap();
    recovery
        .bind_incident_guard(Arc::new(PendingBackend))
        .unwrap();
    let mut task = recovery
        .submit(ProblemContext {
            incident_id: "incident-fixture".into(),
            incident_revision: 1,
            target_id: "target".into(),
            fingerprint: "workload-condition-v1".into(),
            summary: "provider observation".into(),
            occurrences: 1,
            keywords: vec!["workload".into()],
            conditions: settings.recovery.target.required_facts.clone(),
            evidence_refs: vec!["incident:fixture".into()],
        })
        .unwrap();
    for _ in 0..6 {
        if task.stage == RecoveryStage::AwaitingApproval {
            break;
        }
        task = recovery
            .advance(&task.id, Cancellation::new())
            .await
            .unwrap();
    }
    assert_eq!(task.stage, RecoveryStage::AwaitingApproval);
    Arc::get_mut(&mut state).unwrap().recovery = Some(recovery.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(axum::serve(listener, router(state.clone())).into_future());
    let path = format!("/api/v1/recovery/tasks/{}", task.id);
    assert_eq!(call(address, "GET", &path, "", "").await.0, 401);
    let (status, detail) = call(address, "GET", &path, AUTH, "").await;
    assert_eq!(status, 200);
    assert_eq!(detail["task"]["id"], task.id);
    assert_eq!(detail["task"]["revision"], task.revision);
    assert_eq!(
        detail["task"]["problem"],
        serde_json::to_value(&task.problem).unwrap()
    );
    assert_eq!(
        detail["task"]["plan"],
        serde_json::to_value(&task.plan).unwrap()
    );
    assert_eq!(
        detail["task"]["operation"],
        serde_json::to_value(&task.operation).unwrap()
    );
    assert!(detail["task"]["result_check"].is_null());
    assert!(detail["task"].get("reconciliation").is_none());
    let (_, list) = call(address, "GET", "/api/v1/recovery/tasks?limit=1", AUTH, "").await;
    assert_eq!(list["items"][0]["state"], "awaiting_approval");
    assert!(list["items"][0].get("plan").is_none());
    assert_eq!(
        call(address, "GET", "/api/v1/recovery/tasks?limit=101", AUTH, "")
            .await
            .0,
        400
    );
    assert_eq!(
        call(address, "GET", "/api/v1/recovery/tasks/missing", AUTH, "")
            .await
            .0,
        404
    );
    let record = recovery.approval(&task.id).unwrap().unwrap();
    let decision_path = format!("{path}/decision");
    let forged = json!({"revision":record.revision,"decision":"approve","reason":"fixture","actor":"forged"}).to_string();
    assert_eq!(
        call(address, "POST", &decision_path, AUTH, &forged).await.0,
        422
    );
    let stale =
        json!({"revision":record.revision+1,"decision":"approve","reason":"fixture"}).to_string();
    assert_eq!(
        call(address, "POST", &decision_path, AUTH, &stale).await.0,
        409
    );
    let body =
        json!({"revision":record.revision,"decision":"approve","reason":"bounded human decision"})
            .to_string();
    let (status, decided) = call(address, "POST", &decision_path, AUTH, &body).await;
    assert_eq!(status, 200);
    assert_eq!(
        decided["record"]["assessment"]["reviewer"]["actor"],
        "test-operator"
    );
    assert_eq!(
        decided["record"],
        serde_json::to_value(recovery.approval(&task.id).unwrap().unwrap()).unwrap()
    );
    assert_eq!(
        recovery.query(&task.id).unwrap().unwrap().stage,
        RecoveryStage::AwaitingApproval
    );
    assert_eq!(
        call(address, "POST", &decision_path, AUTH, &body).await.0,
        409
    );
    assert_eq!(
        call(
            address,
            "POST",
            &format!("{path}/resume"),
            AUTH,
            &json!({"revision":task.revision}).to_string()
        )
        .await
        .0,
        409
    );
    assert_eq!(
        call(
            address,
            "POST",
            &format!("{path}/check_result"),
            AUTH,
            &json!({"operation_id":"check_result-fixture","revision":task.revision,"execution":{}})
                .to_string()
        )
        .await
        .0,
        422
    );
    let query = json!({"conditions":{"workload_version":"1"},"keywords":[],"limit":5}).to_string();
    assert_eq!(
        call(
            address,
            "POST",
            "/api/v1/recovery/knowledge/search",
            AUTH,
            &query
        )
        .await
        .1["items"],
        json!([])
    );
    server.abort();
    let _ = server.await;
    state.shutdown().await.unwrap();
    engine.shutdown().await.unwrap();
    recovery.shutdown().await.unwrap();
    drop(state);
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn recovery_configuration_is_explicit_strict_and_does_not_create_storage() {
    let cfg = config("recovery-config");
    let directory = cfg.data_dir.clone();
    let path = directory.join("recovery.json");
    let mut value: Value =
        serde_json::from_str(include_str!("../profiles/repair.recovery.example.json")).unwrap();
    value["data_dir"] = json!("../recovery-state-fixture");
    std::fs::write(&path, value.to_string()).unwrap();
    let loaded = crate::configuration::RecoveryHostConfig::load(&path).unwrap();
    assert!(!loaded.data_dir.exists());
    assert!(!loaded.ownership_dir.exists());
    let valid = value.clone();
    for ownership in [
        "../recovery-state-fixture",
        "../recovery-state-fixture/ownership",
        ".",
    ] {
        value = valid.clone();
        value["ownership_dir"] = json!(ownership);
        std::fs::write(&path, value.to_string()).unwrap();
        assert!(crate::configuration::RecoveryHostConfig::load(&path).is_err());
    }
    value = valid.clone();
    value.as_object_mut().unwrap().remove("ownership_dir");
    std::fs::write(&path, value.to_string()).unwrap();
    assert!(crate::configuration::RecoveryHostConfig::load(&path).is_err());
    value = valid.clone();
    value["ownership_dir"] = json!(env!("CARGO_MANIFEST_DIR"));
    std::fs::write(&path, value.to_string()).unwrap();
    assert!(crate::configuration::RecoveryHostConfig::load(&path).is_err());
    value = valid;
    value["knowledge_store"] = json!({"max_records":0});
    std::fs::write(&path, value.to_string()).unwrap();
    assert!(crate::configuration::RecoveryHostConfig::load(&path).is_err());
    value.as_object_mut().unwrap().remove("knowledge_store");
    value["data_dir"] = json!(".");
    std::fs::write(&path, value.to_string()).unwrap();
    assert!(crate::configuration::RecoveryHostConfig::load(&path).is_err());
    value["data_dir"] = json!("../recovery-state-fixture");
    std::fs::write(&path, value.to_string()).unwrap();
    let mut configured = cfg;
    configured.recovery_config = Some(path);
    assert!(
        Console::open(configured).await.is_err(),
        "explicit recovery needs monitoring and Harness services"
    );
    assert!(!loaded.data_dir.exists());
    assert!(!loaded.ownership_dir.exists());
    std::fs::remove_dir_all(directory).unwrap();
}
