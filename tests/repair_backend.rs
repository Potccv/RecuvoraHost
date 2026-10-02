//! Loopback network protocol fixtures for the real host-side repair backend.
mod network_peer;
use recuvora_core::recovery::approval::{ApprovalPolicy, ProposedOperation, ReviewerConfig};
use recuvora_core::recovery::knowledge::KnowledgeQuery;
use recuvora_host::harnesses::*;
use recuvora_host::integrations::extensions::{
    AllowedMethod, ContractDeclaration, ExtensionDefinition, ExtensionKind, ExtensionMetadata,
    ExtensionRegistry, ExtensionsConfig, Message, MethodDeclaration,
};
use recuvora_host::integrations::harness::RemoteHarnessFactory;
use recuvora_host::integrations::recovery::NodeRepairBackend;
use recuvora_host::integrations::recovery::*;
use recuvora_host::runtime::operation::Cancellation;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;
static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

fn main() -> TestResult {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    for mode in [
        "unified",
        "unified-summary-failure",
        "unified-lost-receipt",
        "unified-backend-error",
        "unified-invalid-trace",
    ] {
        runtime.block_on(Box::pin(unified_repair(mode)))?;
    }
    for mode in ["good", "lost-receipt"] {
        runtime.block_on(Box::pin(managed_http(mode)))?;
    }
    runtime.block_on(Box::pin(run()))
}

struct TestDir {
    path: PathBuf,
    root: PathBuf,
}
impl TestDir {
    fn new() -> TestResult<Self> {
        let root = PathBuf::from(
            std::env::var_os("RECUVORA_TEST_TEMP").ok_or("missing external test root")?,
        );
        if !root.is_absolute() {
            return Err("test root must be absolute".into());
        }
        std::fs::create_dir_all(&root)?;
        let root = root.canonicalize()?;
        if root.starts_with(Path::new(env!("CARGO_MANIFEST_DIR")).canonicalize()?) {
            return Err("test root must be external".into());
        }
        let path = root.join(format!(
            "repair-backend-{}-{}",
            std::process::id(),
            NEXT_DIR.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path)?;
        Ok(Self { path, root })
    }
}
impl Drop for TestDir {
    fn drop(&mut self) {
        if self.path.parent() == Some(self.root.as_path())
            && self.path.starts_with(&self.root)
            && let Err(error) = std::fs::remove_dir_all(&self.path)
            && !std::thread::panicking()
        {
            panic!("fixture cleanup failed: {error}");
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}
fn current_facts() -> BTreeMap<String, String> {
    BTreeMap::from([("release".into(), "v1".into())])
}
fn observation() -> TargetObservation {
    TargetObservation {
        target_id: "target".into(),
        facts: current_facts(),
        evidence_refs: vec!["observation:fixture".into()],
        observed_at_ms: now_ms(),
    }
}

fn verification_input(target: TargetBinding) -> VerificationInput {
    VerificationInput {
        target,
        operation: ProposedOperation {
            task_id: "task-fixture".into(),
            task_revision: 1,
            operation_id: "operation-fixture".into(),
            target: "target".into(),
            action: json!({"kind":"execute_script"}),
        },
        receipt: ScriptReceipt {
            execution_trace: Vec::new(),
            operation_id: "operation-fixture".into(),
            target_id: "target".into(),
            outcome: ScriptOutcome::Executed,
            executor_stopped: true,
            evidence_refs: vec!["execution:fixture".into()],
            summary: "fixture result".into(),
        },
    }
}
fn config() -> RecoveryConfig {
    let policy = ApprovalPolicy {
        id: "repair-policy".into(),
        version: 1,
        reviewer: ReviewerConfig::Harness {
            harness_id: "remote".into(),
        },
        delegation:
            "Repair only the explicitly bound target after inspecting the approved environment."
                .into(),
        allowed_targets: vec!["target".into()],
        allowed_action_kinds: vec!["execute_script".into()],
        ttl_secs: 300,
    };
    RecoveryConfig {
        schema_version: 1,
        execution_harness: "remote".into(),
        target: TargetBinding {
            target_id: "target".into(),
            executor_id: "fixture-node".into(),
            platform: "windows".into(),
            allowed_languages: vec!["powershell".into()],
            diagnostic_queries: vec!["snapshot".into()],
            verification_profile: "health".into(),
            required_facts: current_facts(),
            action_timeout_secs: 10,
        },
        approval: policy.clone(),
        script_approval: policy,
        diagnosis_timeout_secs: 10,
        review_timeout_secs: 10,
        max_tool_calls: 1,
        max_diagnoses: 2,
        minimum_script_occurrences: 2,
        max_tasks: 10,
    }
}
fn problem() -> ProblemContext {
    ProblemContext {
        incident_id: "incident-1".into(),
        incident_revision: 1,
        target_id: "target".into(),
        fingerprint: "workload-failure".into(),
        summary: "Observed workload failure".into(),
        occurrences: 2,
        keywords: vec!["failure".into()],
        conditions: current_facts(),
        evidence_refs: vec!["incident:fixture".into()],
    }
}
fn task() -> RecoveryTask {
    RecoveryTask {
        id: "task-fixture".into(),
        revision: 1,
        problem: problem(),
        episode_count: 1,
        stage: RecoveryStage::Diagnosing,
        diagnosis_attempts: 1,
        diagnosis_call: None,
        plan: None,
        knowledge_id: None,
        reused_script: false,
        approval_id: None,
        operation: None,
        observation: None,
        receipt: None,
        verification: None,
        result_check: None,
        note: None,
        created_at_ms: now_ms(),
        updated_at_ms: now_ms(),
    }
}

async fn backend(
    mode: &str,
) -> TestResult<(
    Arc<NodeRepairBackend>,
    Arc<HarnessRegistry>,
    Arc<ExtensionRegistry>,
    network_peer::PeerServer,
)> {
    let server =
        network_peer::PeerServer::start(fixture, BTreeMap::from([("mode".into(), mode.into())]))?;
    let allowed = [
        ("recuvora.harness", "run"),
        ("recuvora.harness", "projects"),
        ("recuvora.harness", "create_project"),
        ("recuvora.repair", "inspect"),
        ("recuvora.repair", "verify"),
        ("recuvora.repair", "reconcile"),
        ("recuvora.repair", "execute_script"),
    ]
    .into_iter()
    .map(|(contract, method)| AllowedMethod {
        contract: contract.into(),
        version: 1,
        method: method.into(),
    })
    .collect();
    let extensions = Arc::new(
        ExtensionRegistry::connect(ExtensionsConfig {
            schema_version: 1,
            extensions: vec![ExtensionDefinition {
                ui_links: Default::default(),
                id: "fixture-node".into(),
                kind: ExtensionKind::Node,
                enabled: true,
                endpoint: server.endpoint(),
                namespaces: vec![],
                allow_calls: allowed,
                allow_nodes: vec![],
            }],
        })
        .await?,
    );
    assert!(extensions.metadata("fixture-node").is_some());
    let mut builder = HarnessRegistryBuilder::new();
    builder.register(Arc::new(RemoteHarnessFactory::new(extensions.clone())))?;
    let harnesses = Arc::new(builder.build(HarnessRegistryConfig {
        schema_version: 1,
        default_harness: Some("remote".into()),
        harnesses: vec![HarnessDefinition::new(
            "remote",
            REMOTE_NODE_ADAPTER,
            "node://fixture-node",
            vec!["work".into(), "review".into()],
        )],
    })?);
    Ok((
        Arc::new(NodeRepairBackend::new(
            harnesses.clone(),
            extensions.clone(),
        )),
        harnesses,
        extensions,
        server,
    ))
}

struct FixtureIncidentGuard;
struct FixtureDispatchLease(u64);
impl IncidentDispatchLease for FixtureDispatchLease {
    fn current(&self) -> Result<IncidentReadiness, RecoveryError> {
        Ok(IncidentReadiness::Active { revision: self.0 })
    }
}
impl IncidentGuard for FixtureIncidentGuard {
    fn acquire_dispatch<'a>(
        &'a self,
        problem: &'a ProblemContext,
    ) -> RecoveryFuture<'a, Box<dyn IncidentDispatchLease>> {
        Box::pin(async move {
            Ok(Box::new(FixtureDispatchLease(problem.incident_revision))
                as Box<dyn IncidentDispatchLease>)
        })
    }
    fn with_current(
        &self,
        problem: &ProblemContext,
        commit: &mut dyn FnMut(IncidentReadiness) -> Result<(), RecoveryError>,
    ) -> Result<(), RecoveryError> {
        commit(IncidentReadiness::Active {
            revision: problem.incident_revision,
        })
    }
}

async fn run() -> TestResult {
    let (adapter, harnesses, extensions, server) = backend("good").await?;
    let config = config();
    let observed = adapter.inspect(&config.target, Cancellation::new()).await?;
    assert_eq!(observed.target_id, "target");
    let plan = adapter
        .diagnose(
            DiagnosisInput {
                task: task(),
                config: config.clone(),
                observation: observed,
                knowledge: vec![],
            },
            Cancellation::new(),
        )
        .await?;
    assert_eq!(plan.script.generated_by_harness, "remote");
    assert_eq!(plan.script.generated_in_session, "execution-session");
    assert_eq!(plan.script.id, "task-fixture-script-1");
    assert_eq!(plan.script.preconditions, current_facts());
    assert!(
        extensions
            .call_read_only(
                "fixture-node",
                "recuvora.repair",
                1,
                "execute_script",
                json!({}),
                Duration::from_secs(1),
                Cancellation::new()
            )
            .await
            .is_err(),
        "public read router must not bypass the permit route"
    );
    let dir = TestDir::new()?;
    let recovery =
        RecoveryService::open(dir.path.join("recovery"), config.clone(), adapter.clone())?;
    recovery.bind_target_ownership(Arc::new(FileTargetOwnership::open(
        dir.path.join("ownership"),
    )?))?;
    recovery.bind_incident_guard(Arc::new(FixtureIncidentGuard))?;
    let mut recovery_task = recovery.submit(problem())?;
    for _ in 0..12 {
        if recovery_task.stage.terminal() {
            break;
        }
        recovery_task = recovery
            .advance(&recovery_task.id, Cancellation::new())
            .await?;
    }
    assert_eq!(
        recovery_task.stage,
        RecoveryStage::Completed,
        "{:?}",
        recovery_task.note
    );
    assert_eq!(
        recovery_task.receipt.as_ref().unwrap().outcome,
        ScriptOutcome::Executed
    );
    assert_eq!(
        recovery_task.verification.as_ref().unwrap().healthy,
        Some(true)
    );
    let mut conditions = current_facts();
    conditions.insert("fault_fingerprint".into(), "workload-failure".into());
    conditions.insert("platform".into(), "windows".into());
    assert_eq!(
        recovery
            .knowledge(&KnowledgeQuery {
                conditions,
                keywords: vec!["failure".into()],
                limit: 5
            })?
            .len(),
        1
    );
    recovery.shutdown().await?;
    harnesses.shutdown().await?;
    extensions.shutdown().await?;
    server.shutdown()?;
    for mode in ["lost-receipt", "wrong-result-check", "unknown-result-check"] {
        let (adapter, harnesses, extensions, server) = backend(mode).await?;

        let dir = TestDir::new()?;
        let recovery =
            RecoveryService::open(dir.path.join("recovery"), config.clone(), adapter.clone())?;
        recovery.bind_target_ownership(Arc::new(FileTargetOwnership::open(
            dir.path.join("ownership"),
        )?))?;
        recovery.bind_incident_guard(Arc::new(FixtureIncidentGuard))?;
        let mut task = recovery.submit(problem())?;
        for _ in 0..12 {
            if task.stage == RecoveryStage::Unknown {
                break;
            }
            task = recovery.advance(&task.id, Cancellation::new()).await?;
        }
        assert_eq!(task.stage, RecoveryStage::Unknown);
        let result = adapter
            .check_task_result(
                &recovery,
                &task.id,
                task.revision,
                "operator-fixture".into(),
                Cancellation::new(),
            )
            .await;
        if mode == "wrong-result-check" {
            assert!(result.is_err());
            assert_eq!(recovery.query(&task.id)?.unwrap().revision, task.revision);
        } else {
            let recovered = result?;
            assert_eq!(
                recovered.result_check.as_ref().unwrap().actor,
                "operator-fixture"
            );
            if mode == "unknown-result-check" {
                assert_eq!(
                    recovered.stage,
                    RecoveryStage::Unknown,
                    "healthy business evidence cannot prove execution"
                );
            } else {
                assert_eq!(recovered.stage, RecoveryStage::Completed);
                assert!(matches!(
                    adapter
                        .check_task_result(
                            &recovery,
                            &task.id,
                            task.revision,
                            "operator-fixture".into(),
                            Cancellation::new()
                        )
                        .await,
                    Err(RecoveryError::Busy)
                ));
                assert_eq!(
                    recovery.advance(&task.id, Cancellation::new()).await?.stage,
                    RecoveryStage::Completed
                );
            }
        }
        recovery.shutdown().await?;
        if mode != "lost-receipt" {
            let competing = RecoveryService::open(
                dir.path.join("competing-recovery"),
                config.clone(),
                adapter.clone(),
            )?;
            assert!(
                matches!(
                    competing.bind_target_ownership(Arc::new(FileTargetOwnership::open(
                        dir.path.join("ownership"),
                    )?)),
                    Err(RecoveryError::Busy)
                ),
                "Unknown must retain ownership across state-directory changes"
            );
            competing.shutdown().await?;
            let reopened =
                RecoveryService::open(dir.path.join("recovery"), config.clone(), adapter.clone())?;
            reopened.bind_target_ownership(Arc::new(FileTargetOwnership::open(
                dir.path.join("ownership"),
            )?))?;
            assert_eq!(
                reopened.query(&task.id)?.unwrap().stage,
                RecoveryStage::Unknown
            );
            reopened.shutdown().await?;
        }
        harnesses.shutdown().await?;
        extensions.shutdown().await?;
        server.shutdown()?;
    }
    for mode in [
        "budget",
        "bad-query",
        "wrong-target-argument",
        "wrong-harness",
        "forged-draft",
    ] {
        let (bad, harnesses, extensions, server) = backend(mode).await?;
        assert!(
            bad.diagnose(
                DiagnosisInput {
                    task: task(),
                    config: config.clone(),
                    observation: observation(),
                    knowledge: vec![],
                },
                Cancellation::new()
            )
            .await
            .is_err(),
            "reject {mode}"
        );
        harnesses.shutdown().await?;
        extensions.shutdown().await?;
        server.shutdown()?;
    }
    for mode in ["stale-age", "age-overflow", "rtt-expired"] {
        let (adapter, harnesses, extensions, server) = backend(mode).await?;

        assert!(
            adapter
                .inspect(&config.target, Cancellation::new())
                .await
                .is_err(),
            "reject inspection {mode}"
        );
        assert!(
            adapter
                .verify(
                    verification_input(config.target.clone()),
                    Cancellation::new()
                )
                .await
                .is_err(),
            "reject verification {mode}"
        );
        harnesses.shutdown().await?;
        extensions.shutdown().await?;
        server.shutdown()?;
    }
    let (adapter, harnesses, extensions, server) = backend("skewed-clock").await?;
    let before = now_ms();
    let observed = adapter.inspect(&config.target, Cancellation::new()).await?;
    assert_eq!(observed.facts["node_clock_unix_ms"], "1");
    assert!(observed.observed_at_ms >= before && observed.observed_at_ms <= now_ms());
    let before = now_ms();
    let verified = adapter
        .verify(verification_input(config.target), Cancellation::new())
        .await?;
    assert!(verified.verified_at_ms >= before && verified.verified_at_ms <= now_ms());
    harnesses.shutdown().await?;
    extensions.shutdown().await?;
    server.shutdown()?;
    println!(
        "repair_backend: independent tool-free review, bounded target inspections, identity rejection, permit-only node execution and independent verification passed"
    );
    Ok(())
}

async fn managed_http(mode: &str) -> TestResult {
    use recuvora_host::configuration::RecoveryHostConfig;
    use recuvora_host::integrations::recovery::IncidentTrigger;
    use recuvora_host::monitoring::{MonitorDefinition, MonitorRule, MonitorsConfig, RuleOperator};
    use recuvora_host::server::{Console, ServerConfig};
    let dir = TestDir::new()?;
    let peer =
        network_peer::PeerServer::start(fixture, BTreeMap::from([("mode".into(), mode.into())]))?;
    let harness_path = dir.path.join("harnesses.json");
    let extension_path = dir.path.join("extensions.json");
    let monitor_path = dir.path.join("monitors.json");
    let recovery_path = dir.path.join("recovery.json");
    let token_path = dir.path.join("operator.token");
    let token = "managed-fixture-token-0123456789-abcdefghijklmnop";
    std::fs::write(&token_path, token)?;
    std::fs::write(
        &harness_path,
        serde_json::to_vec(&HarnessRegistryConfig {
            schema_version: 1,
            default_harness: Some("remote".into()),
            harnesses: vec![HarnessDefinition::new(
                "remote",
                REMOTE_NODE_ADAPTER,
                "node://fixture-node",
                vec!["work".into(), "review".into()],
            )],
        })?,
    )?;
    let allowed = ["inspect", "verify", "reconcile", "execute_script"]
        .into_iter()
        .map(|method| AllowedMethod {
            contract: "recuvora.repair".into(),
            version: 1,
            method: method.into(),
        })
        .chain(std::iter::once(AllowedMethod {
            contract: "recuvora.harness".into(),
            version: 1,
            method: "run".into(),
        }))
        .collect();
    std::fs::write(
        &extension_path,
        serde_json::to_vec(&ExtensionsConfig {
            schema_version: 1,
            extensions: vec![
                ExtensionDefinition {
                    ui_links: Default::default(),
                    id: "observation-plugin".into(),
                    kind: ExtensionKind::Plugin,
                    enabled: true,
                    endpoint: peer.endpoint(),
                    namespaces: vec!["example.observation".into()],
                    allow_calls: vec![AllowedMethod {
                        contract: "example.observation".into(),
                        version: 1,
                        method: "observe".into(),
                    }],
                    allow_nodes: vec![],
                },
                ExtensionDefinition {
                    ui_links: Default::default(),
                    id: "fixture-node".into(),
                    kind: ExtensionKind::Node,
                    enabled: true,
                    endpoint: peer.endpoint(),
                    namespaces: vec![],
                    allow_calls: allowed,
                    allow_nodes: vec![],
                },
            ],
        })?,
    )?;
    std::fs::write(
        &monitor_path,
        serde_json::to_vec(&MonitorsConfig {
            schema_version: 1,
            discoveries: vec![],
            monitors: vec![MonitorDefinition {
                id: "target-monitor".into(),
                target_id: "target".into(),
                source_id: "provider-source".into(),
                view_role: None,
                extension_id: "observation-plugin".into(),
                contract: "example.observation".into(),
                version: 1,
                method: "observe".into(),
                params: json!({}),
                interval_ms: 10,
                timeout_ms: 1000,
                stale_after_ms: 30000,
                startup_grace_ms: 1000,
                rule: MonitorRule {
                    pointer: "/ready".into(),
                    operator: RuleOperator::Eq,
                    value: json!(true),
                    failure_samples: 1,
                    success_samples: 1,
                },
            }],
        })?,
    )?;
    let mut core = config();
    core.approval.reviewer = ReviewerConfig::Human;
    let settings = RecoveryHostConfig {
        schema_version: 1,
        data_dir: dir.path.join("recovery-state"),
        ownership_dir: dir.path.join("target-ownership"),
        recovery: core,
        triggers: vec![IncidentTrigger {
            monitor_id: "target-monitor".into(),
            rule_id: "target-monitor".into(),
            fingerprint: "workload-failure".into(),
            keywords: vec!["failure".into()],
            conditions: current_facts(),
        }],
        interval_ms: 10,
        approval_store: Default::default(),
        knowledge_store: Default::default(),
    };
    std::fs::write(&recovery_path, serde_json::to_vec(&settings)?)?;
    let console_dir = dir.path.join("console-state");
    std::fs::create_dir(&console_dir)?;
    let cfg = ServerConfig {
        schema_version: 1,
        listen: "127.0.0.1:0".parse()?,
        token_file: token_path,
        operator: "operator-managed".into(),
        permissions: vec![
            "recovery.read".into(),
            "recovery.decide".into(),
            "recovery.check_result".into(),
            "knowledge.read".into(),
        ],
        allowed_origins: vec![],
        data_dir: console_dir,
        harness_config: Some(harness_path),
        extensions_config: Some(extension_path),
        repair_config: None,
        monitors_config: Some(monitor_path),
        recovery_config: Some(recovery_path),
        log_sources: vec![],
        ui_dir: None,
    };
    let (console, engine) = Console::open(cfg.clone()).await?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}", listener.local_addr()?);
    let server = tokio::spawn(
        axum::serve(listener, recuvora_host::server::router(console.clone())).into_future(),
    );
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()?;
    let status: Value = client
        .get(format!("{url}/api/v1/recovery/status"))
        .bearer_auth(token)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(status["running"], true);
    let task = wait_http_stage(&client, &url, token, "awaiting_approval").await?;
    let id = task["id"].as_str().ok_or("task ID required")?;
    let approval_url = format!("{url}/api/v1/recovery/tasks/{id}/approval");
    let approval: Value = client
        .get(&approval_url)
        .bearer_auth(token)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let decision: Value = client.post(format!("{url}/api/v1/recovery/tasks/{id}/decision")).bearer_auth(token).json(&json!({"revision":approval["record"]["revision"],"decision":"approve","reason":"exact isolated fixture scope"})).send().await?.error_for_status()?.json().await?;
    assert_eq!(
        decision["record"]["assessment"]["reviewer"]["actor"],
        "operator-managed"
    );
    if mode == "lost-receipt" {
        let unknown = wait_http_stage(&client, &url, token, "unknown").await?;
        let accepted = client
            .post(format!("{url}/api/v1/recovery/tasks/{id}/check_result"))
            .bearer_auth(token)
            .json(&json!({"operation_id":"check_result-managed","revision":unknown["revision"]}))
            .send()
            .await?;
        assert_eq!(accepted.status(), reqwest::StatusCode::ACCEPTED);
        let duplicate = client
            .post(format!("{url}/api/v1/recovery/tasks/{id}/check_result"))
            .bearer_auth(token)
            .json(&json!({"operation_id":"check_result-managed","revision":unknown["revision"]}))
            .send()
            .await?;
        assert_eq!(duplicate.status(), reqwest::StatusCode::CONFLICT);
    }
    let completed = wait_http_stage(&client, &url, token, "completed").await?;
    assert_eq!(completed["receipt"]["outcome"], "executed");
    assert_eq!(completed["verification"]["healthy"], true);
    assert!(completed.get("reconciliation").is_none());
    if mode == "lost-receipt" {
        assert_eq!(completed["result_check"]["actor"], "operator-managed");
        let execution = &completed["result_check"]["execution"];
        assert_eq!(
            execution["operation_id"],
            completed["operation"]["operation_id"]
        );
        assert!(execution["checked_at_ms"].as_u64().is_some());
        assert!(execution.get("reconciled_at_ms").is_none());
        let operation: Value = client
            .get(format!("{url}/api/v1/operations/check_result-managed"))
            .bearer_auth(token)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        assert_eq!(
            operation["result"]["task"]["result_check"],
            completed["result_check"]
        );
        assert!(operation["result"]["task"].get("reconciliation").is_none());
    }
    let knowledge: Value = client.post(format!("{url}/api/v1/recovery/knowledge/search")).bearer_auth(token).json(&json!({"conditions":{"release":"v1","platform":"windows","fault_fingerprint":"workload-failure"},"keywords":["failure"],"limit":5})).send().await?.error_for_status()?.json().await?;
    assert_eq!(
        knowledge["items"]
            .as_array()
            .ok_or("knowledge items required")?
            .len(),
        if mode == "lost-receipt" { 0 } else { 1 },
        "Core quarantines immutable script versions after an Unknown outcome"
    );
    server.abort();
    let _ = server.await;
    console.shutdown().await?;
    engine.shutdown().await?;
    drop(console);
    let (reopened, engine) = Console::open(cfg).await?;
    reopened.shutdown().await?;
    engine.shutdown().await?;
    drop(reopened);
    peer.shutdown()?;
    Ok(())
}

async fn wait_http_stage(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    stage: &str,
) -> TestResult<Value> {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let tasks: Value = client
                .get(format!("{url}/api/v1/recovery/tasks"))
                .bearer_auth(token)
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            if let Some(task) = tasks["items"]
                .as_array()
                .and_then(|items| items.first())
                .filter(|task| task["state"] == stage)
            {
                let id = task["id"].as_str().ok_or("task ID required")?;
                let detail: Value = client
                    .get(format!("{url}/api/v1/recovery/tasks/{id}"))
                    .bearer_auth(token)
                    .send()
                    .await?
                    .error_for_status()?
                    .json()
                    .await?;
                if detail["task"]["stage"] == stage {
                    return Ok::<_, Box<dyn Error + Send + Sync>>(detail["task"].clone());
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?
}

fn fixture(mut session: network_peer::Session) -> TestResult {
    let mode = session.env("mode").unwrap_or("good").to_owned();
    let mode = mode.as_str();
    let Some(Message::Hello {
        expected_id, kind, ..
    }) = session.read()?
    else {
        return Err("missing handshake".into());
    };
    let contracts = if kind == ExtensionKind::Plugin {
        vec![ContractDeclaration {
            id: "example.observation".into(),
            version: 1,
            methods: vec![MethodDeclaration {
                name: "observe".into(),
                read_only: true,
                input_schema: json!({"type":"object"}),
                output_schema: json!({"type":"object"}),
            }],
        }]
    } else {
        [
            (
                "recuvora.harness",
                vec![
                    ("run", false),
                    ("projects", true),
                    ("create_project", false),
                ],
            ),
            (
                "recuvora.repair",
                vec![
                    ("inspect", true),
                    ("verify", true),
                    ("reconcile", true),
                    ("execute_script", false),
                ],
            ),
        ]
        .into_iter()
        .map(|(id, methods)| ContractDeclaration {
            id: id.into(),
            version: 1,
            methods: methods
                .into_iter()
                .map(|(name, read_only)| MethodDeclaration {
                    name: name.into(),
                    read_only,
                    input_schema: json!({"type":"object"}),
                    output_schema: json!({"type":"object"}),
                })
                .collect(),
        })
        .collect()
    };
    session.write(Message::Ready {
        metadata: ExtensionMetadata {
            protocol_version: 1,
            id: expected_id,
            kind,
            contracts,
            capabilities: ["text", "projects", "tools", "client_visibility", "approval"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            workspaces: vec!["work".into(), "review".into()],
        },
    })?;
    let Some(Message::Call {
        id,
        contract,
        method,
        params,
        ..
    }) = session.read()?
    else {
        return Ok(());
    };
    let result = if contract == "example.observation" {
        assert_eq!(method, "observe");
        let sequence = params["cursor"]
            .as_str()
            .and_then(|cursor| cursor.parse::<u64>().ok())
            .unwrap_or_default()
            + 1;
        json!({"schema_version":1,"target_id":params["target_id"],"source_id":params["source_id"],"generation":"fixture-g1","cursor":params["cursor"],"next_cursor":sequence.to_string(),"coverage":"complete","has_more":false,"error":null,"samples":[{"id":format!("sample-{sequence}"),"sequence":sequence,"age_ms":0,"value":{"ready":false},"evidence":{}}]})
    } else if contract == "recuvora.repair" {
        assert_eq!(
            params
                .get("target_id")
                .or_else(|| params
                    .get("operation")
                    .and_then(|operation| operation.get("target")))
                .and_then(Value::as_str),
            Some("target")
        );
        let age_ms = match mode {
            "stale-age" => 30_001,
            "age-overflow" => u64::MAX,
            "rtt-expired" => {
                std::thread::sleep(Duration::from_millis(5));
                30_000
            }
            _ => 0,
        };
        match method.as_str() {
            "inspect" => {
                assert_eq!(params["query"], "snapshot");
                let mut facts = current_facts();
                if mode == "skewed-clock" {
                    facts.insert("node_clock_unix_ms".into(), "1".into());
                }
                json!({"target_id":"target","facts":facts,"evidence_refs":["observation:fixture"],"age_ms":age_ms})
            }
            "verify" => {
                assert_eq!(params["profile"], "health");
                json!({"operation_id":params["operation_id"],"target_id":"target","profile":"health","healthy":true,"executor_stopped":true,"evidence_refs":["verification:fixture"],"age_ms":age_ms})
            }
            "reconcile" => {
                json!({"operation_id":params["operation_id"],"target_id":if mode == "wrong-result-check" { "wrong-target" } else { "target" },"executor_id":"fixture-node","outcome":if mode == "unknown-result-check" {"unknown"} else {"executed"},"executor_stopped":true,"evidence_refs":["execution:checked-fixture"],"age_ms":age_ms})
            }
            "execute_script" => {
                assert!(
                    params["request_id"]
                        .as_str()
                        .is_some_and(|id| id.starts_with("approval-"))
                );
                assert_eq!(params["operation"]["action"]["script"]["source"], "exit 0");
                if matches!(
                    mode,
                    "lost-receipt"
                        | "wrong-result-check"
                        | "unknown-result-check"
                        | "unified-lost-receipt"
                ) {
                    session.write(Message::Result {
                        id,
                        result: json!({"invalid":"receipt lost"}),
                    })?;
                    return Ok(());
                }
                json!({"operation_id":params["operation"]["operation_id"],"target_id":"target","outcome":"executed","executor_stopped":true,"evidence_refs":["execution:fixture"],"summary":"fixture script execution receipt"})
            }
            _ => return Err("unexpected repair method".into()),
        }
    } else {
        assert_eq!(contract, "recuvora.harness");
        assert_eq!(method, "run");
        let approval = params["role"] == "approval";
        let final_response = if approval {
            assert_eq!(params["workspace"]["workspace_id"], "review");
            assert_eq!(params["visibility"], "hidden");
            assert_eq!(params["placement"]["type"], "none");
            assert!(params["tools"].as_array().is_some_and(Vec::is_empty));
            let prompt = params["prompt"].as_str().ok_or("missing review prompt")?;
            let context: Value = serde_json::from_str(
                prompt
                    .rsplit_once("Context:\n")
                    .ok_or("missing exact review context")?
                    .1,
            )?;
            if mode.starts_with("unified") {
                assert_eq!(
                    context["request"]["operation"]["action"]["kind"],
                    "repair_with_harness"
                );
            } else {
                assert_eq!(
                    context["request"]["operation"]["action"]["script"]["source"],
                    "exit 0"
                );
            }
            assert_eq!(context["current_observation"]["facts"]["release"], "v1");
            json!({"request_id":context["request"]["request_id"],"decision":"approve","reason":"exact fixture scope reviewed"}).to_string()
        } else if mode.starts_with("unified") {
            let tools = params["tools"].as_array().ok_or("missing tools")?;
            if tools.is_empty() {
                assert!(
                    params["prompt"]
                        .as_str()
                        .unwrap()
                        .starts_with("Summarize the ACTUAL")
                );
                if mode == "unified-summary-failure" {
                    "invalid report".into()
                } else {
                    json!({"summary":"Repair observation", "lessons":"Interactive repair requires current context", "related_experience_ids":[], "assessment":"not_suitable", "reason":"Context dependent", "script":null}).to_string()
                }
            } else {
                assert_eq!(tools.len(), 2);
                assert_eq!(tools[1]["name"], "apply_repair");
                let request: Value = serde_json::from_str(
                    params["prompt"]
                        .as_str()
                        .unwrap()
                        .rsplit_once("Request: ")
                        .unwrap()
                        .1,
                )?;
                assert_eq!(request["summarize_experience"], true);
                for index in 0..2 {
                    session.write(Message::Callback { id: format!("repair-callback-{index}"), parent_id: id.clone(), method: "tool".into(),
                        params: json!({"harness_id":"remote", "thread_id":"execution-thread", "turn_id":"turn-1", "call_id":format!("repair-call-{index}"), "tool":"apply_repair", "arguments":{"language":"powershell","source":"exit 0","preconditions":{"release":"v1"}}}) })?;
                    match session.read()? {
                        Some(Message::Result { result, .. }) => assert_eq!(
                            result["success"],
                            json!(index == 0 && mode != "unified-lost-receipt"),
                            "{result}"
                        ),
                        _ => return Err("missing repair tool result".into()),
                    }
                }
                "Model claims success, but Host must use the independent executor receipt".into()
            }
        } else {
            assert_eq!(params["workspace"]["workspace_id"], "work");
            let tools = params["tools"].as_array().ok_or("missing tools")?;
            assert_eq!(tools.len(), 1);
            assert_eq!(tools[0]["name"], "inspect_target");
            for index in 0..if mode == "budget" { 2 } else { 1 } {
                let arguments = match mode {
                    "bad-query" => json!({"query":"unapproved"}),
                    "wrong-target-argument" => json!({"query":"snapshot","target_id":"other"}),
                    _ => json!({"query":"snapshot"}),
                };
                session.write(
                    Message::Callback {
                        id: format!("callback-{index}"),
                        parent_id: id.clone(),
                        method: "tool".into(),
                        params: json!({
                            "harness_id":if mode=="wrong-harness" {"forged"} else {"remote"},"thread_id":"execution-thread","turn_id":"turn-1","call_id":format!("call-{index}"),
                            "tool":"inspect_target","arguments":arguments,
                        }),
                    },
                )?;
                match session.read()? {
                    Some(Message::Result { result, .. }) => assert_eq!(
                        result["success"],
                        json!(
                            index == 0
                                && !matches!(
                                    mode,
                                    "bad-query" | "wrong-target-argument" | "wrong-harness"
                                )
                        )
                    ),
                    Some(Message::Error { .. }) | Some(Message::Cancel { .. }) | None => {
                        return Ok(());
                    }
                    _ => return Err("unexpected callback response".into()),
                }
            }
            let mut draft = json!({"summary":"Repair fixture workload","language":"powershell","source":"exit 0","preconditions":{"release":"v1"},"reusable":true});
            if mode == "forged-draft" {
                draft["generated_by_harness"] = json!("forged");
            }
            draft.to_string()
        };
        json!({"thread_id":if approval {"review-thread"} else {"execution-thread"},"session_id":if approval {"review-session"} else {"execution-session"},
            "project_directory":"C:\\FixtureOnly\\Workspace","visibility":params["visibility"],"native_project_id":null,"client_project_grouping":{"type":"not_applicable"},"final_response":final_response})
    };
    session.write(Message::Result { id, result })?;
    let _ = session.read()?;
    Ok(())
}

/// Inject failure after the real adapter has durably prepared and sent its action.
struct PostExecutionFault {
    inner: Arc<NodeRepairBackend>,
    invalid_trace: bool,
}
impl RepairBackend for PostExecutionFault {
    fn inspect<'a>(
        &'a self,
        target: &'a TargetBinding,
        cancel: Cancellation,
    ) -> RecoveryFuture<'a, TargetObservation> {
        self.inner.inspect(target, cancel)
    }
    fn diagnose(
        &self,
        input: DiagnosisInput,
        cancel: Cancellation,
    ) -> RecoveryFuture<'_, RepairPlan> {
        self.inner.diagnose(input, cancel)
    }
    fn review(&self, input: ReviewInput, cancel: Cancellation) -> RecoveryFuture<'_, ReviewOutput> {
        self.inner.review(input, cancel)
    }
    fn execute<'a>(
        &'a self,
        script: AuthorizedScript<'a>,
        cancel: Cancellation,
    ) -> RecoveryFuture<'a, ScriptReceipt> {
        Box::pin(async move {
            let mut receipt = self.inner.execute(script, cancel).await?;
            if self.invalid_trace {
                receipt.execution_trace.clear();
                Ok(receipt)
            } else {
                Err(RecoveryError::Service(
                    "backend failed after dispatch".into(),
                ))
            }
        })
    }
    fn verify(
        &self,
        input: VerificationInput,
        cancel: Cancellation,
    ) -> RecoveryFuture<'_, BusinessVerification> {
        self.inner.verify(input, cancel)
    }
    fn summarize(
        &self,
        job: recuvora_core::recovery::workflow::ExperienceJob,
        config: RecoveryConfig,
        cancel: Cancellation,
    ) -> RecoveryFuture<'_, recuvora_core::recovery::knowledge::ExperienceReport> {
        self.inner.summarize(job, config, cancel)
    }
}

async fn unified_repair(mode: &str) -> TestResult {
    let (adapter, harnesses, extensions, server) = backend(mode).await?;

    let dir = TestDir::new()?;
    let mut settings = config();
    settings.approval.allowed_action_kinds = vec!["repair_with_harness".into()];
    let service_backend: Arc<dyn RepairBackend> =
        if matches!(mode, "unified-backend-error" | "unified-invalid-trace") {
            Arc::new(PostExecutionFault {
                inner: adapter.clone(),
                invalid_trace: mode == "unified-invalid-trace",
            })
        } else {
            adapter.clone()
        };
    let recovery =
        RecoveryService::open(dir.path.join("unified"), settings.clone(), service_backend)?;
    recovery.bind_target_ownership(Arc::new(FileTargetOwnership::open(
        dir.path.join("ownership"),
    )?))?;
    recovery.bind_incident_guard(Arc::new(FixtureIncidentGuard))?;
    let task = recovery.submit(problem())?;

    let mut task = recovery.advance(&task.id, Cancellation::new()).await?;
    assert!(task.plan.is_none());

    if matches!(
        mode,
        "unified-lost-receipt" | "unified-backend-error" | "unified-invalid-trace"
    ) {
        assert_eq!(task.stage, RecoveryStage::Unknown);
        assert_eq!(task.receipt.as_ref().unwrap().execution_trace.len(), 1);
        task = adapter
            .check_task_result(
                &recovery,
                &task.id,
                task.revision,
                "operator".into(),
                Cancellation::new(),
            )
            .await?;
        assert_eq!(task.stage, RecoveryStage::Completed);
    } else {
        assert_eq!(task.stage, RecoveryStage::Completed, "{:?}", task.note);
    }
    assert_eq!(task.receipt.as_ref().unwrap().execution_trace.len(), 1);
    if mode == "unified-summary-failure" {
        for _ in 0..4 {
            recovery.summarize_pending(Cancellation::new()).await?;
        }
        let jobs = recovery.pending_experiences()?;
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].attempt, 3);
        assert!(jobs[0].last_error.is_some());
        assert_eq!(
            recovery.query(&task.id)?.unwrap().stage,
            RecoveryStage::Completed
        );
        recovery.shutdown().await?;
        let restored = RecoveryService::open(dir.path.join("unified"), settings, adapter.clone())?;
        assert_eq!(restored.pending_experiences()?[0].attempt, 3);
        restored.retry_experiences(Cancellation::new()).await?;
        assert_eq!(restored.pending_experiences()?[0].attempt, 4);
        restored.shutdown().await?;
    } else {
        recovery.summarize_pending(Cancellation::new()).await?;

        let mut conditions = current_facts();
        conditions.insert("fault_fingerprint".into(), "workload-failure".into());
        conditions.insert("platform".into(), "windows".into());
        let query = KnowledgeQuery {
            conditions,
            keywords: vec!["failure".into()],
            limit: 4,
        };
        assert!(!recovery.experiences(&query)?.is_empty());
        assert!(recovery.knowledge(&query)?.is_empty());
        let mut next = problem();
        next.incident_id = "next-unified-incident".into();

        let next = recovery.submit(next)?;

        let next = recovery.advance(&next.id, Cancellation::new()).await?;
        assert!(
            !next.operation.as_ref().unwrap().action["request"]["experiences"]
                .as_array()
                .unwrap()
                .is_empty()
        );

        if next.stage == RecoveryStage::Unknown {
            adapter
                .check_task_result(
                    &recovery,
                    &next.id,
                    next.revision,
                    "operator".into(),
                    Cancellation::new(),
                )
                .await?;
        }

        recovery.shutdown().await?;
    }
    harnesses.shutdown().await?;
    extensions.shutdown().await?;
    server.shutdown()?;
    Ok(())
}
