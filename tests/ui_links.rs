//! Page discovery over isolated network peers, never actual plugin websites.
mod network_peer;
mod workflow_support;

use network_peer::{PeerServer, Session, TestResult};
use recuvora_host::integrations::extensions::{
    AllowedMethod, ExtensionDefinition, ExtensionKind, ExtensionRegistry, ExtensionsConfig,
    UI_LINKS_CAPABILITY, UI_LINKS_METHOD, UiLinksConfig,
};
use recuvora_host::protocol::{
    ContractDeclaration, ExtensionMetadata, Message, MethodDeclaration, Outcome,
};
use recuvora_host::runtime::operation::Cancellation;
use recuvora_host::server::{Console, ServerConfig, router};
use serde_json::{Value, json};
use std::{collections::BTreeMap, io::Write, path::PathBuf, time::Duration};
use workflow_support::TestDir;

const PLUGIN: &str = "workload-plugin";
const CONTRACT: &str = "com.example.workload.ui";
const BASE: &str = "https://plugins.example.com/workload/";

fn descriptor(revision: &str, relative: &str) -> Value {
    json!({"schema_version":1,"revision":revision,"pages":[{
        "id":"dashboard","title":"运行面板","entrypoint":"web",
        "relative_url":relative,"open_mode":"external"
    }]})
}

fn provider(mut session: Session) -> TestResult {
    let Some(Message::Hello {
        expected_id, kind, ..
    }) = session.read()?
    else {
        return Err("hello required".into());
    };
    let root = PathBuf::from(session.env("ROOT").ok_or("fixture root required")?);
    let mode = session.env("MODE").unwrap_or("valid");
    let mut methods = vec![MethodDeclaration {
        name: UI_LINKS_METHOD.into(),
        read_only: mode != "write-method",
        input_schema: if mode == "bad-input" {
            json!({"type":"string"})
        } else {
            json!({"type":"object","properties":{"schema_version":{"type":"integer","enum":[1]}},"required":["schema_version"],"additionalProperties":false})
        },
        output_schema: if mode == "bad-output" {
            json!({"type":"string"})
        } else {
            json!({"type":"object"})
        },
    }];
    methods.push(MethodDeclaration {
        name: "describe_monitoring_view".into(),
        read_only: true,
        input_schema: json!({"type":"object"}),
        output_schema: json!({"type":"object"}),
    });
    methods.push(MethodDeclaration {
        name: "query".into(),
        read_only: true,
        input_schema: json!({"type":"object"}),
        output_schema: json!({"type":"object"}),
    });
    let mut contracts = vec![ContractDeclaration {
        id: CONTRACT.into(),
        version: if mode == "version-two" { 2 } else { 1 },
        methods,
    }];
    if mode == "duplicate-method" {
        let mut duplicate = contracts[0].clone();
        duplicate.id = "com.example.workload.other".into();
        contracts.push(duplicate);
    }
    let capabilities = if mode == "not-declared" {
        vec![]
    } else {
        vec![
            UI_LINKS_CAPABILITY.into(),
            "recuvora.monitoring_view.v1".into(),
        ]
    };
    session.write(Message::Ready {
        metadata: ExtensionMetadata {
            protocol_version: 1,
            id: expected_id,
            kind,
            contracts,
            capabilities,
            workspaces: vec![],
        },
    })?;
    while let Some(message) = session.read()? {
        let Message::Call {
            id, method, params, ..
        } = message
        else {
            continue;
        };
        if method == "query" {
            session.write(Message::Result {
                id,
                result: json!({"available":true}),
            })?;
            continue;
        }
        assert_eq!(params, json!({"schema_version":1}));
        if method == "describe_monitoring_view" {
            session.write(Message::Result {
                id,
                result: json!({"schema_version":1,"title":"Monitor","sections":[]}),
            })?;
            continue;
        }
        assert_eq!(method, UI_LINKS_METHOD);
        writeln!(
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(root.join("calls"))?,
            "call"
        )?;
        let action = std::fs::read_to_string(root.join("action")).unwrap_or_default();
        if action == "error" {
            session.write(Message::Error {
                id,
                code: "offline".into(),
                message: "SECRET https://token:credential@private.invalid/".into(),
                outcome: Outcome::Rejected,
            })?;
        } else if action == "hold" {
            std::fs::write(root.join("started"), b"started")?;
            if let Some(Message::Cancel { id: cancelled }) = session.read()? {
                assert_eq!(cancelled, id);
                std::fs::write(root.join("cancelled"), b"cancelled")?;
                session.write(Message::Error {
                    id,
                    code: "cancelled".into(),
                    message: "cancelled".into(),
                    outcome: Outcome::Cancelled,
                })?;
            }
            return Ok(());
        } else {
            if action == "callback" {
                session.write(Message::Callback { id: "denied-callback".into(), parent_id: id.clone(),
                    method: "service.call".into(), params: json!({"node_id":"forbidden","contract":CONTRACT,"version":1,"method":"query","params":{}}) })?;
            }
            let result = serde_json::from_slice(&std::fs::read(root.join("response.json"))?)?;
            session.write(Message::Result { id, result })?;
        }
    }
    Ok(())
}

struct Fixture {
    peer: PeerServer,
    root: TestDir,
}
impl Fixture {
    fn new(mode: &str) -> TestResult<Self> {
        let root = TestDir::new("ui-links");
        std::fs::write(
            root.path.join("response.json"),
            serde_json::to_vec(&descriptor("r1", "dashboard"))?,
        )?;
        let peer = PeerServer::start(
            provider,
            BTreeMap::from([
                (
                    "ROOT".into(),
                    root.path
                        .to_str()
                        .ok_or("UTF-8 fixture path required")?
                        .into(),
                ),
                ("MODE".into(), mode.into()),
            ]),
        )?;
        Ok(Self { peer, root })
    }
    fn definition(&self) -> ExtensionDefinition {
        ExtensionDefinition {
            id: PLUGIN.into(),
            kind: ExtensionKind::Plugin,
            enabled: true,
            endpoint: self.peer.endpoint(),
            namespaces: vec!["com.example.workload".into()],
            allow_calls: [UI_LINKS_METHOD, "describe_monitoring_view", "query"]
                .into_iter()
                .map(|method| AllowedMethod {
                    contract: CONTRACT.into(),
                    version: 1,
                    method: method.into(),
                })
                .collect(),
            allow_nodes: vec![],
            ui_links: UiLinksConfig {
                enabled: true,
                entrypoints: BTreeMap::from([("web".into(), BASE.into())]),
            },
        }
    }
    async fn registry(&self) -> TestResult<ExtensionRegistry> {
        connect(self.definition()).await
    }
    fn response(&self, value: &Value) -> TestResult {
        std::fs::write(
            self.root.path.join("response.json"),
            serde_json::to_vec(value)?,
        )?;
        Ok(())
    }
    fn action(&self, action: &str) -> TestResult {
        std::fs::write(self.root.path.join("action"), action)?;
        Ok(())
    }
    fn calls(&self) -> usize {
        std::fs::read_to_string(self.root.path.join("calls"))
            .unwrap_or_default()
            .lines()
            .count()
    }
}
async fn connect(definition: ExtensionDefinition) -> TestResult<ExtensionRegistry> {
    Ok(ExtensionRegistry::connect(ExtensionsConfig {
        schema_version: 1,
        extensions: vec![definition],
    })
    .await?)
}

#[tokio::test]
async fn documented_vectors_and_browser_url_boundaries() -> TestResult {
    let vectors: Value = serde_json::from_slice(&std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docs/extensions/examples/pages.json"),
    )?)?;
    let Message::Ready { metadata } = serde_json::from_value::<Message>(vectors["ready"].clone())?
    else {
        return Err("ready vector required".into());
    };
    let declaration = &metadata.contracts[0].methods[0];
    recuvora_host::protocol::validate_schema(&declaration.input_schema)?;
    recuvora_host::protocol::validate_schema(&declaration.output_schema)?;
    recuvora_host::protocol::validate_value(&declaration.input_schema, &vectors["call"]["params"])?;
    recuvora_host::protocol::validate_value(
        &declaration.output_schema,
        &vectors["result"]["result"],
    )?;
    let fixture = Fixture::new("valid")?;
    fixture.response(&vectors["result"]["result"])?;
    let registry = fixture.registry().await?;
    let initial = registry.ui_links(PLUGIN)?;
    assert_eq!(initial.status, "ready");
    assert_eq!(
        serde_json::to_value(&initial.links[0])?,
        vectors["navigation_entry"]
    );
    for (index, case) in vectors["valid_responses"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
    {
        fixture.response(&case["value"])?;
        assert_eq!(
            registry
                .refresh_ui_links(PLUGIN, Cancellation::new())
                .await?
                .status,
            "ready",
            "valid {index}: {case}"
        );
    }
    for case in vectors["invalid_responses"].as_array().unwrap() {
        fixture.response(&case["value"])?;
        let snapshot = registry
            .refresh_ui_links(PLUGIN, Cancellation::new())
            .await?;
        assert_eq!(snapshot.status, "invalid_response", "{case}");
        assert!(snapshot.links.is_empty());
    }
    for (index, case) in vectors["resolution_cases"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
    {
        fixture.response(&descriptor(
            &format!("url-{index}"),
            case["relative_url"].as_str().unwrap(),
        ))?;
        let snapshot = registry
            .refresh_ui_links(PLUGIN, Cancellation::new())
            .await?;
        if let Some(expected) = case["expected_url"].as_str() {
            assert_eq!(snapshot.status, "ready", "{case}");
            assert_eq!(snapshot.links[0].url, expected);
        } else {
            assert_eq!(snapshot.status, "invalid_response", "{case}");
            assert!(snapshot.links.is_empty());
        }
    }
    registry.shutdown().await?;
    fixture.peer.shutdown()?;
    Ok(())
}

#[tokio::test]
async fn snapshots_are_cached_revisions_are_consistent_and_failures_remove_links() -> TestResult {
    let fixture = Fixture::new("valid")?;
    let registry = fixture.registry().await?;
    assert_eq!(fixture.calls(), 1);
    for _ in 0..3 {
        assert_eq!(registry.ui_links_catalog()?[0].status, "ready");
    }
    assert_eq!(fixture.calls(), 1, "catalog reads must not contact plugins");
    fixture.response(&descriptor("r1", "changed"))?;
    let invalid = registry
        .refresh_ui_links(PLUGIN, Cancellation::new())
        .await?;
    assert_eq!(invalid.status, "invalid_response");
    assert!(invalid.links.is_empty());
    assert_eq!(invalid.descriptor_revision.as_deref(), Some("r1"));
    fixture.response(&descriptor("r2", "changed"))?;
    assert_eq!(
        registry
            .refresh_ui_links(PLUGIN, Cancellation::new())
            .await?
            .links[0]
            .url,
        format!("{BASE}changed")
    );
    fixture.action("error")?;
    let failed = registry
        .refresh_ui_links(PLUGIN, Cancellation::new())
        .await?;
    assert_eq!(failed.status, "unavailable");
    assert!(failed.links.is_empty());
    assert!(!serde_json::to_string(&failed)?.contains("SECRET"));
    assert!(!serde_json::to_string(&failed)?.contains("credential"));
    fixture.action("")?;
    fixture.response(&json!({"schema_version":1,"revision":"r3","pages":[]}))?;
    assert!(
        registry
            .refresh_ui_links(PLUGIN, Cancellation::new())
            .await?
            .links
            .is_empty()
    );
    registry.shutdown().await?;
    assert_eq!(registry.ui_links(PLUGIN)?.status, "unavailable");
    assert!(
        registry
            .refresh_ui_links(PLUGIN, Cancellation::new())
            .await
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn descriptor_registration_is_optional_and_cannot_bypass_allowlists() -> TestResult {
    for (mode, expected) in [
        ("not-declared", "not_declared"),
        ("write-method", "invalid_registration"),
        ("bad-input", "invalid_registration"),
        ("duplicate-method", "invalid_registration"),
        ("version-two", "invalid_registration"),
        ("valid", "invalid_registration"),
    ] {
        let fixture = Fixture::new(mode)?;
        let mut definition = fixture.definition();
        if mode == "valid" {
            definition.allow_calls.clear();
        }
        let registry = connect(definition).await?;
        assert_eq!(registry.ui_links(PLUGIN)?.status, expected, "{mode}");
        assert_eq!(
            fixture.calls(),
            0,
            "invalid registration must not dispatch: {mode}"
        );
        if mode == "write-method" {
            assert!(registry.metadata(PLUGIN).is_some());
            assert_eq!(
                registry
                    .call_read_only(
                        PLUGIN,
                        CONTRACT,
                        1,
                        "query",
                        json!({}),
                        Duration::from_secs(3),
                        Cancellation::new()
                    )
                    .await?,
                json!({"available":true})
            );
        }
        registry.shutdown().await?;
    }
    for disabled_plugin in [false, true] {
        let fixture = Fixture::new("valid")?;
        let mut definition = fixture.definition();
        if disabled_plugin {
            definition.enabled = false;
        } else {
            definition.ui_links.enabled = false;
        }
        let registry = connect(definition).await?;
        assert_eq!(registry.ui_links(PLUGIN)?.status, "disabled");
        assert_eq!(fixture.calls(), 0);
        registry.shutdown().await?;
    }
    Ok(())
}

#[tokio::test]
async fn generic_dispatch_cannot_bypass_descriptor_limits_or_fixed_input() -> TestResult {
    let fixture = Fixture::new("valid")?;
    let registry = fixture.registry().await?;
    for params in [
        json!({"schema_version":1}),
        json!({"schema_version":1,"url":"https://untrusted.invalid"}),
    ] {
        assert!(
            registry
                .call_read_only(
                    PLUGIN,
                    CONTRACT,
                    1,
                    UI_LINKS_METHOD,
                    params,
                    Duration::from_secs(3),
                    Cancellation::new()
                )
                .await
                .is_err()
        );
    }
    assert_eq!(fixture.calls(), 1);
    registry.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn missing_bindings_hide_only_the_unbound_page() -> TestResult {
    let fixture = Fixture::new("valid")?;
    let mut response = descriptor("r1", "dashboard");
    let mut missing = response["pages"][0].clone();
    missing["id"] = json!("logs");
    missing["entrypoint"] = json!("records");
    response["pages"].as_array_mut().unwrap().push(missing);
    fixture.response(&response)?;
    let registry = fixture.registry().await?;
    let snapshot = registry.ui_links(PLUGIN)?;
    assert_eq!(snapshot.status, "ready");
    assert_eq!(snapshot.links.len(), 1);
    assert_eq!(snapshot.unavailable_pages.len(), 1);
    assert_eq!(snapshot.unavailable_pages[0].page_id, "logs");
    assert_eq!(snapshot.unavailable_pages[0].reason, "binding_missing");
    registry.shutdown().await?;
    Ok(())
}

#[test]
fn trusted_bindings_validate_even_when_disabled() -> TestResult {
    let fixture = Fixture::new("valid")?;
    for base in [
        "file:///tmp/",
        "javascript:alert(1)",
        "https://example.com",
        "https://example.com/base",
        "https://user:password@example.com/",
        "https://example.com/?token=x",
        "https://example.com/#hash",
        "https://example.com/a/../",
        "https://example.com/%2e%2e/",
        "https://example.com/a%2fb/",
        "https://example.com/%252e/",
        "https://example.com:0/",
    ] {
        let mut definition = fixture.definition();
        definition.ui_links.enabled = false;
        definition
            .ui_links
            .entrypoints
            .insert("web".into(), base.into());
        assert!(
            ExtensionsConfig {
                schema_version: 1,
                extensions: vec![definition]
            }
            .validate()
            .is_err(),
            "{base}"
        );
    }
    let mut node = fixture.definition();
    node.kind = ExtensionKind::Node;
    node.namespaces.clear();
    assert!(
        ExtensionsConfig {
            schema_version: 1,
            extensions: vec![node]
        }
        .validate()
        .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn descriptor_bounds_and_declared_output_schema_are_enforced() -> TestResult {
    let fixture = Fixture::new("valid")?;
    let registry = fixture.registry().await?;
    let mut cases = Vec::new();
    let mut title = descriptor("too-long-title", "dashboard");
    title["pages"][0]["title"] = json!("面".repeat(43));
    cases.push(title);
    let mut null = descriptor("null-description", "dashboard");
    null["pages"][0]["description"] = Value::Null;
    cases.push(null);
    let mut many = descriptor("too-many-pages", "dashboard");
    many["pages"] = Value::Array(
        (0..17)
            .map(|index| {
                let mut page = descriptor("x", "dashboard")["pages"][0].clone();
                page["id"] = json!(format!("page-{index}"));
                page
            })
            .collect(),
    );
    cases.push(many);
    let mut huge = descriptor("too-large", "dashboard");
    huge["pages"] = Value::Array(
        (0..16)
            .map(|index| {
                let mut page = descriptor("x", &"x".repeat(1900))["pages"][0].clone();
                page["id"] = json!(format!("page-{index}"));
                page["description"] = json!("x".repeat(1000));
                page
            })
            .collect(),
    );
    cases.push(huge);
    for case in cases {
        fixture.response(&case)?;
        let snapshot = registry
            .refresh_ui_links(PLUGIN, Cancellation::new())
            .await?;
        assert_eq!(snapshot.status, "invalid_response", "{}", case["revision"]);
        assert!(snapshot.links.is_empty());
    }
    registry.shutdown().await?;
    let output = Fixture::new("bad-output")?;
    let registry = output.registry().await?;
    assert_eq!(registry.ui_links(PLUGIN)?.status, "invalid_response");
    registry.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn descriptor_callbacks_are_denied_and_shutdown_drains_dropped_refresh() -> TestResult {
    let fixture = Fixture::new("valid")?;
    fixture.action("callback")?;
    let registry = fixture.registry().await?;
    assert_eq!(registry.ui_links(PLUGIN)?.status, "unavailable");
    assert!(registry.ui_links(PLUGIN)?.links.is_empty());
    fixture.action("hold")?;
    let refresh = {
        let registry = registry.clone();
        tokio::spawn(async move { registry.refresh_ui_links(PLUGIN, Cancellation::new()).await })
    };
    tokio::time::timeout(Duration::from_secs(3), async {
        while !fixture.root.path.join("started").exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await?;
    let capacity = registry.refresh_ui_links(PLUGIN, Cancellation::new()).await;
    assert!(
        capacity.is_err(),
        "a second refresh must fail without dispatch"
    );
    assert_eq!(fixture.calls(), 2);
    refresh.abort();
    let _ = refresh.await;
    tokio::time::timeout(Duration::from_secs(5), registry.shutdown()).await??;
    assert!(
        fixture.root.path.join("cancelled").exists(),
        "shutdown must cancel the owned network call"
    );
    assert_eq!(registry.ui_links(PLUGIN)?.status, "unavailable");
    assert!(registry.ui_links(PLUGIN)?.links.is_empty());
    Ok(())
}

#[tokio::test]
async fn http_catalog_and_explicit_refresh_obey_permissions() -> TestResult {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let fixture = Fixture::new("valid")?;
    let token = fixture.root.path.join("token");
    const TOKEN: &str = "ui-links-isolated-test-token-0123456789";
    std::fs::write(&token, TOKEN)?;
    let extensions = fixture.root.path.join("extensions.json");
    std::fs::write(
        &extensions,
        serde_json::to_vec(&ExtensionsConfig {
            schema_version: 1,
            extensions: vec![fixture.definition()],
        })?,
    )?;
    for (index, permissions) in [
        vec!["extension.read"],
        vec!["monitor.read"],
        vec!["extension.read", "monitor.read"],
    ]
    .into_iter()
    .enumerate()
    {
        std::fs::create_dir(fixture.root.path.join(format!("server-{index}")))?;
        let config: ServerConfig = serde_json::from_value(json!({
            "schema_version":1,"listen":"127.0.0.1:0","token_file":token,
            "operator":"ui-links-test","permissions":permissions,"allowed_origins":[],
            "data_dir":fixture.root.path.join(format!("server-{index}")),"extensions_config":extensions,
            "harness_config":null,"repair_config":null,"monitors_config":null
        }))?;
        let (state, engine) = Console::open(config).await?;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let base = format!("http://{}", listener.local_addr()?);
        let app = router(state.clone());
        let server = tokio::spawn(async move { axum::serve(listener, app).await });
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(5))
            .build()?;
        let catalog = format!("{base}/api/v1/ui/catalog");
        let calls_before_catalog = fixture.calls();
        assert_eq!(client.get(&catalog).send().await?.status(), 401);
        let response = client.get(&catalog).bearer_auth(TOKEN).send().await?;
        if permissions.contains(&"extension.read") {
            assert_eq!(response.status(), 200);
            let body: Value = response.json().await?;
            assert_eq!(body["schema_version"], 1);
            assert_eq!(body["external_links"][0]["url"], format!("{BASE}dashboard"));
            assert_eq!(body["link_statuses"][0]["status"], "ready");
            assert!(body["link_statuses"][0].get("links").is_none());
            assert_eq!(
                body["views"].as_array().unwrap().len(),
                usize::from(permissions.contains(&"monitor.read"))
            );
        } else {
            assert_eq!(response.status(), 403);
        }
        assert_eq!(
            fixture.calls(),
            calls_before_catalog,
            "HTTP reads use snapshots"
        );
        let refresh_url = format!("{base}/api/v1/ui/plugins/{PLUGIN}/links/refresh");
        assert_eq!(
            client
                .post(&refresh_url)
                .json(&json!({}))
                .send()
                .await?
                .status(),
            401
        );
        assert_eq!(
            client
                .post(&refresh_url)
                .bearer_auth(TOKEN)
                .header("Origin", "https://untrusted.invalid")
                .json(&json!({}))
                .send()
                .await?
                .status(),
            403
        );
        let before = fixture.calls();
        let response = client
            .post(&refresh_url)
            .bearer_auth(TOKEN)
            .json(&json!({}))
            .send()
            .await?;
        if permissions.contains(&"extension.read") {
            assert_eq!(response.status(), 200);
            let body: Value = response.json().await?;
            assert_eq!(body["auto_retry"], false);
            assert_eq!(body["plugin"]["status"], "ready");
            assert_eq!(fixture.calls(), before + 1);
            assert_eq!(
                client
                    .post(&refresh_url)
                    .bearer_auth(TOKEN)
                    .json(&json!({"url":"https://untrusted.invalid"}))
                    .send()
                    .await?
                    .status(),
                422
            );
            assert_eq!(
                client
                    .post(format!("{base}/api/v1/ui/plugins/missing/links/refresh"))
                    .bearer_auth(TOKEN)
                    .json(&json!({}))
                    .send()
                    .await?
                    .status(),
                404
            );
            fixture.action("error")?;
            let failed = client
                .post(&refresh_url)
                .bearer_auth(TOKEN)
                .json(&json!({}))
                .send()
                .await?;
            assert_eq!(failed.status(), 200);
            let failed: Value = failed.json().await?;
            assert_eq!(failed["plugin"]["status"], "unavailable");
            assert!(failed["plugin"]["links"].as_array().unwrap().is_empty());
            let cached: Value = client
                .get(&catalog)
                .bearer_auth(TOKEN)
                .send()
                .await?
                .json()
                .await?;
            assert!(cached["external_links"].as_array().unwrap().is_empty());
            assert_eq!(cached["link_statuses"][0]["status"], "unavailable");
            assert_eq!(
                fixture.calls(),
                before + 2,
                "no automatic retries or requests for catalog reads"
            );
            fixture.action("")?;
        } else {
            assert_eq!(response.status(), 403);
            assert_eq!(fixture.calls(), before);
        }
        server.abort();
        let _ = server.await;
        state.shutdown().await?;
        engine.shutdown().await?;
    }
    Ok(())
}
