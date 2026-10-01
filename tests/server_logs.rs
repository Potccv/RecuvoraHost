//! HTTP reads through an isolated loopback WebSocket provider. No real target.
mod network_peer;
use network_peer::{PeerServer, Session};
use recuvora_host::protocol::{
    ContractDeclaration, ExtensionMetadata, Message, MethodDeclaration, Outcome,
};
use recuvora_host::server::{Console, ServerConfig, router};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    error::Error,
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

fn main() -> TestResult {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run())
}
fn provider_server(role: &str, root: &Path) -> TestResult<PeerServer> {
    PeerServer::start(
        provider,
        BTreeMap::from([
            ("ROLE".into(), role.into()),
            (
                "ROOT".into(),
                root.to_str().ok_or("UTF-8 fixture root required")?.into(),
            ),
        ]),
    )
}

fn fixture_contract(id: &str, methods: &[&str]) -> ContractDeclaration {
    ContractDeclaration {
        id: id.into(),
        version: 1,
        methods: methods
            .iter()
            .map(|name| MethodDeclaration {
                name: (*name).into(),
                read_only: true,
                input_schema: json!({"type":"object"}),
                output_schema: json!({"type":"object"}),
            })
            .collect(),
    }
}

fn provider(mut session: Session) -> TestResult {
    let Some(Message::Hello {
        expected_id, kind, ..
    }) = session.read()?
    else {
        return Err("hello required".into());
    };
    let role = session.env("ROLE").unwrap_or("logs").to_owned();
    let (contracts, capabilities) = match role.as_str() {
        "logs" => (
            vec![
                fixture_contract("example.observation", &["observe", "query"]),
                ContractDeclaration {
                    id: "example.observation.monitoring_view".into(),
                    version: 1,
                    methods: vec![MethodDeclaration {
                        name: "describe_monitoring_view".into(),
                        read_only: true,
                        input_schema: json!({"type":"object","properties":{"schema_version":{"type":"integer","minimum":1,"maximum":1}},"required":["schema_version"],"additionalProperties":false}),
                        output_schema: json!({"type":"object"}),
                    }],
                },
            ],
            vec!["recuvora.monitoring_view.v1".into()],
        ),
        "owner" => (
            vec![
                fixture_contract("example.observation", &["observe", "inventory"]),
                ContractDeclaration {
                    id: "example.observation.monitoring_view".into(),
                    version: 1,
                    methods: vec![MethodDeclaration {
                        name: "describe_monitoring_view".into(),
                        read_only: true,
                        input_schema: json!({"type":"object","properties":{"schema_version":{"type":"integer","minimum":1,"maximum":1}},"required":["schema_version"],"additionalProperties":false}),
                        output_schema: json!({"type":"object"}),
                    }],
                },
            ],
            vec!["recuvora.monitoring_view.v1".into()],
        ),
        "node" => (
            vec![fixture_contract(
                "example.observation",
                &["observe", "inventory"],
            )],
            vec![],
        ),
        "other" => (
            vec![fixture_contract("example.other", &["observe", "inventory"])],
            vec![],
        ),
        _ => return Err("unknown fixture role".into()),
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
        if method == "observe" {
            session.write(
                Message::Result {
                    id,
                    result: json!({"schema_version":1,"target_id":params["target_id"],"source_id":params["source_id"],
                "generation":"fixture-g1","cursor":params["cursor"],"next_cursor":"observe-1","has_more":false,"coverage":"complete","error":null,
                "samples":[{"id":"fixture-sample-1","sequence":1,"age_ms":0,"value":{"ready":true},"evidence":{}}]}),
                },
            )?;
            continue;
        }
        if method == "inventory" {
            let key = if role == "other" { "other" } else { "owned" };
            session.write(
                Message::Result {
                    id,
                    result: json!({"schema_version":1,"complete":true,"targets":[{"key":key}],"error":null}),
                },
            )?;
            continue;
        }
        if method == "describe_monitoring_view" {
            if params != json!({"schema_version":1}) {
                return Err("fixed monitoring view input required".into());
            }
            session.write(Message::Result {
                id,
                result: json!({
                    "schema_version":1,
                    "title":"Observation provider <script>",
                    "summary":"Read-only fields from host-owned monitor snapshots.",
                    "sections":[{
                        "id":"status","title":"Provider status","monitor_role":"status",
                        "fields":[{
                            "id":"ready","label":"Ready <img>","source":"last_value",
                            "pointer":"/ready","format":"boolean","empty":"Not observed"
                        }]
                    }]
                }),
            })?;
            continue;
        }
        let target_key = params["target_key"].as_str().ok_or("target key required")?;
        if !matches!(target_key, "a" | "b") || params.get("file").is_some() {
            return Err("fixture target scope denied".into());
        }
        let provider_root = PathBuf::from(session.env("ROOT").ok_or("fixture root required")?);
        let fail_once = provider_root.join("fail-next-query");
        if std::fs::read_to_string(&fail_once).ok().as_deref() == Some("offline") {
            std::fs::write(&fail_once, "")?;
            session.write(Message::Error {
                id,
                code: "source_offline".into(),
                message: "isolated provider temporarily unavailable".into(),
                outcome: Outcome::Rejected,
            })?;
            continue;
        }
        let file = provider_root.join(format!("{target_key}.jsonl"));
        let entries = std::fs::read_to_string(&file)?
            .lines()
            .map(serde_json::from_str::<Value>)
            .collect::<Result<Vec<_>, _>>()?;
        let start = match params["cursor"].as_str() {
            None => entries.len().saturating_sub(32),
            Some(cursor) => {
                let Some((scope, position)) = cursor.split_once(':') else {
                    return Err("invalid fixture cursor".into());
                };
                let position = position.parse::<usize>()?;
                if scope != target_key || position > entries.len() {
                    session.write(Message::Error {
                        id,
                        code: "source_changed".into(),
                        message: "fixed fixture record stream was truncated".into(),
                        outcome: Outcome::Rejected,
                    })?;
                    continue;
                }
                position
            }
        };
        let limit = params["limit"].as_u64().ok_or("bounded limit required")? as usize;
        if !(1..=32).contains(&limit) {
            return Err("limit invalid".into());
        }
        let end = (start + limit).min(entries.len());
        session.write(
            Message::Result {
                id,
                result: json!({"entries":entries[start..end],"next_cursor":format!("{target_key}:{end}"),
            "has_more":end<entries.len(),"stream_label":format!("stream-{target_key}"),"available_streams":[],
            "coverage":"complete","error":null}),
            },
        )?;
    }
    Ok(())
}
const AUTH: &str = "Authorization: Bearer isolated-log-test-token-0123456789abcdef\r\n";
async fn call(address: std::net::SocketAddr, path: &str, auth: &str) -> TestResult<(u16, Value)> {
    let mut stream = tokio::net::TcpStream::connect(address).await?;
    stream
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n{auth}\r\n")
                .as_bytes(),
        )
        .await?;
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).await?;
    let text = String::from_utf8(bytes)?;
    let (head, body) = text
        .split_once("\r\n\r\n")
        .ok_or("HTTP response required")?;
    Ok((
        head.split_whitespace()
            .nth(1)
            .ok_or("status required")?
            .parse()?,
        serde_json::from_str(body)?,
    ))
}
fn append(path: &Path, record: Value) -> TestResult {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    serde_json::to_writer(&mut file, &record)?;
    file.write_all(b"\n")?;
    Ok(())
}

async fn owner_provider_http(directory: &Path, token: &Path) -> TestResult {
    let phase = directory.join("owner-provider");
    std::fs::create_dir(&phase)?;
    let extensions = phase.join("extensions.json");
    let node_peer = provider_server("node", &phase)?;
    let other_peer = provider_server("other", &phase)?;
    let owner_peer = provider_server("owner", &phase)?;
    std::fs::write(
        &extensions,
        serde_json::to_vec(&json!({"schema_version":1,"extensions":[
            {
                "id":"observation-node","kind":"node","enabled":true,"namespaces":[],"allow_nodes":[],
                "allow_calls":[
                    {"contract":"example.observation","version":1,"method":"observe"},
                    {"contract":"example.observation","version":1,"method":"inventory"}
                ],
                "endpoint":node_peer.endpoint()
            },
            {
                "id":"other-provider","kind":"plugin","enabled":true,"namespaces":["example.other"],"allow_nodes":[],
                "allow_calls":[
                    {"contract":"example.other","version":1,"method":"observe"},
                    {"contract":"example.other","version":1,"method":"inventory"}
                ],
                "endpoint":other_peer.endpoint()
            },
            {
                "id":"observation-owner","kind":"plugin","enabled":true,"namespaces":["example.observation"],"allow_nodes":[],
                "allow_calls":[
                    {"contract":"example.observation.monitoring_view","version":1,"method":"describe_monitoring_view"}
                ],
                "endpoint":owner_peer.endpoint()
            }
        ]}))?,
    )?;
    let monitor = |id: &str,
                   target_id: &str,
                   source_id: &str,
                   extension_id: &str,
                   contract: &str| {
        json!({
            "id":id,"target_id":target_id,"source_id":source_id,"view_role":"status",
            "extension_id":extension_id,"contract":contract,"version":1,"method":"observe","params":{},
            "interval_ms":3_600_000,"timeout_ms":5000,"stale_after_ms":60_000,"startup_grace_ms":60_000,
            "rule":{"pointer":"/ready","operator":"eq","value":true,"failure_samples":1,"success_samples":1}
        })
    };
    let discovery = |id: &str,
                     extension_id: &str,
                     contract: &str,
                     template_id: &str,
                     target_id: &str,
                     source_id: &str| {
        json!({
            "id":id,"extension_id":extension_id,"contract":contract,"version":1,"method":"inventory","params":{},
            "interval_ms":3_600_000,"timeout_ms":5000,"max_targets":1,"parameter":"target_key",
            "template":monitor(template_id,target_id,source_id,extension_id,contract)
        })
    };
    let monitors = phase.join("monitors.json");
    std::fs::write(
        &monitors,
        serde_json::to_vec(&json!({
            "schema_version":1,
            "monitors":[
                monitor("owner-static","owner-static-target","owner-static-source","observation-node","example.observation"),
                monitor("other-static","other-static-target","other-static-source","other-provider","example.other")
            ],
            "discoveries":[
                discovery("owner-inventory","observation-node","example.observation","owner-dynamic","owner-target","owner-source"),
                discovery("other-inventory","other-provider","example.other","other-dynamic","other-target","other-source")
            ]
        }))?,
    )?;
    let config: ServerConfig = serde_json::from_value(json!({
        "schema_version":1,"listen":"127.0.0.1:0","token_file":token,"operator":"fixture-operator",
        "permissions":["monitor.read","extension.read"],"allowed_origins":[],"data_dir":phase,
        "harness_config":null,"extensions_config":extensions,"repair_config":null,"monitors_config":monitors,
        "log_sources":[]
    }))?;
    let (state, engine) = Console::open(config).await?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let app = router(state.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await });

    let mut summary = Value::Null;
    for _ in 0..200 {
        let (status, current) = call(address, "/api/v1/monitors", AUTH).await?;
        if status == 200
            && current["items"]
                .as_array()
                .is_some_and(|items| items.len() == 4)
            && current["discoveries"]
                .as_array()
                .is_some_and(|items| items.len() == 2)
        {
            summary = current;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let plugins = summary["plugins"]
        .as_array()
        .ok_or("owner/provider plugin summaries required")?;
    assert_eq!(
        plugins.len(),
        2,
        "node provider must not become a plugin page"
    );
    assert!(
        plugins
            .iter()
            .all(|plugin| plugin["id"] != "observation-node")
    );
    let owner = plugins
        .iter()
        .find(|plugin| plugin["id"] == "observation-owner")
        .ok_or("observation owner summary required")?;
    assert_eq!(owner["monitor_count"], 2);
    assert_eq!(owner["target_count"], 2);
    assert_eq!(owner["discovery_count"], 1);
    let other = plugins
        .iter()
        .find(|plugin| plugin["id"] == "other-provider")
        .ok_or("other provider summary required")?;
    assert_eq!(other["monitor_count"], 2);
    assert_eq!(other["target_count"], 2);
    assert_eq!(other["discovery_count"], 1);

    let items = summary["items"]
        .as_array()
        .ok_or("owner/provider monitor summaries required")?;
    let owner_items = items
        .iter()
        .filter(|item| item["owner_plugin_id"] == "observation-owner")
        .collect::<Vec<_>>();
    assert_eq!(owner_items.len(), 2);
    let mut owner_ids = owner_items
        .iter()
        .map(|item| item["id"].as_str().ok_or("owner monitor identity required"))
        .collect::<Result<Vec<_>, _>>()?;
    owner_ids.sort_unstable();
    assert_eq!(owner_ids, vec!["owner-dynamic.owned", "owner-static"]);
    assert!(
        owner_items
            .iter()
            .all(|item| item["extension_id"] == "observation-node")
    );
    let owner_discoveries = summary["discoveries"]
        .as_array()
        .ok_or("owner/provider discoveries required")?
        .iter()
        .filter(|item| item["owner_plugin_id"] == "observation-owner")
        .collect::<Vec<_>>();
    assert_eq!(owner_discoveries.len(), 1);
    assert_eq!(owner_discoveries[0]["id"], "owner-inventory");
    assert_eq!(owner_discoveries[0]["extension_id"], "observation-node");

    let (status, page) = call(
        address,
        "/api/v1/monitoring/plugins/observation-owner",
        AUTH,
    )
    .await?;
    assert_eq!(status, 200);
    assert_eq!(page["view_status"], "ready");
    assert_eq!(page["plugin"]["id"], "observation-owner");
    assert_eq!(page["plugin"]["monitor_count"], 2);
    assert_eq!(page["plugin"]["target_count"], 2);
    assert_eq!(page["plugin"]["discovery_count"], 1);
    let page_monitors = page["monitors"]
        .as_array()
        .ok_or("owner plugin page monitors required")?;
    assert_eq!(page_monitors.len(), 2);
    let mut page_monitor_ids = page_monitors
        .iter()
        .map(|item| item["id"].as_str().ok_or("page monitor identity required"))
        .collect::<Result<Vec<_>, _>>()?;
    page_monitor_ids.sort_unstable();
    assert_eq!(
        page_monitor_ids,
        vec!["owner-dynamic.owned", "owner-static"]
    );
    assert!(page_monitors.iter().all(|item| {
        item["owner_plugin_id"] == "observation-owner" && item["extension_id"] == "observation-node"
    }));
    assert!(
        page_monitors
            .iter()
            .all(|item| item["owner_plugin_id"] != "other-provider")
    );
    let page_discoveries = page["discoveries"]
        .as_array()
        .ok_or("owner plugin page discoveries required")?;
    assert_eq!(page_discoveries.len(), 1);
    assert_eq!(page_discoveries[0]["id"], "owner-inventory");
    assert_eq!(page_discoveries[0]["owner_plugin_id"], "observation-owner");
    assert_eq!(page_discoveries[0]["extension_id"], "observation-node");

    server.abort();
    let _ = server.await;
    state.shutdown().await?;
    engine.shutdown().await?;
    drop(state);
    node_peer.shutdown()?;
    other_peer.shutdown()?;
    owner_peer.shutdown()?;
    Ok(())
}

async fn run() -> TestResult {
    let root = PathBuf::from(
        std::env::var_os("RECUVORA_TEST_TEMP").ok_or("external RECUVORA_TEST_TEMP required")?,
    )
    .canonicalize()?;
    if root.starts_with(Path::new(env!("CARGO_MANIFEST_DIR")).canonicalize()?) {
        return Err("external temp root required".into());
    }
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let directory = root.join(format!("server-logs-{}-{stamp}", std::process::id()));
    std::fs::create_dir(&directory)?;
    let token = directory.join("token");
    std::fs::write(&token, "isolated-log-test-token-0123456789abcdef")?;
    let file = directory.join("a.jsonl");
    append(
        &file,
        json!({"id":"a-1","timestamp":1,"level":"INFO","message":"normal observation record"}),
    )?;
    append(
        &file,
        json!({"id":"a-2","timestamp":2,"level":"ERROR","message":"captured condition record","event":"condition_active"}),
    )?;
    append(
        &directory.join("b.jsonl"),
        json!({"id":"b-1","timestamp":1,"level":"INFO","message":"different target record"}),
    )?;
    let extensions = directory.join("extensions.json");
    let provider = provider_server("logs", &directory)?;
    std::fs::write(
        &extensions,
        serde_json::to_vec(&json!({"schema_version":1,"extensions":[{
            "id":"observation-provider","kind":"plugin","enabled":true,"namespaces":["example.observation"],"allow_nodes":[],
            "allow_calls":[{"contract":"example.observation","version":1,"method":"observe"},{"contract":"example.observation","version":1,"method":"query"},
                {"contract":"example.observation.monitoring_view","version":1,"method":"describe_monitoring_view"}],
            "endpoint":provider.endpoint()
        }]}))?,
    )?;
    let monitors = directory.join("monitors.json");
    let definitions=["a","b"].into_iter().map(|name|json!({"id":format!("monitor-{name}"),"target_id":format!("target-{name}"),
        "source_id":format!("source-{name}"),"extension_id":"observation-provider","contract":"example.observation","version":1,"method":"observe","view_role":"status","params":{"target_key":name},
        "interval_ms":3_600_000,"timeout_ms":5000,"stale_after_ms":60_000,"startup_grace_ms":60_000,
        "rule":{"pointer":"/ready","operator":"eq","value":true,"failure_samples":1,"success_samples":1}})).collect::<Vec<_>>();
    std::fs::write(
        &monitors,
        serde_json::to_vec(&json!({"schema_version":1,"monitors":definitions}))?,
    )?;
    let config: ServerConfig = serde_json::from_value(
        json!({"schema_version":1,"listen":"127.0.0.1:0","token_file":token,"operator":"fixture-operator",
        "permissions":["monitor.read","logs.read","extension.read"],"allowed_origins":[],"data_dir":directory,
        "harness_config":null,"extensions_config":extensions,"repair_config":null,"monitors_config":monitors,
        "log_sources":[{"id":"record-source","extension_id":"observation-provider","contract":"example.observation","version":1,"method":"query",
            "monitor_contract":"example.observation","parameter_bindings":{"target_key":"/target_key"},
            "error_levels":["ERROR","CRITICAL"]}]}),
    )?;
    let (state, engine) = Console::open(config.clone()).await?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let app = router(state.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    assert_eq!(
        call(address, "/api/v1/monitors/monitor-a/logs", "")
            .await?
            .0,
        401
    );
    let (status, summary) = call(address, "/api/v1/monitors", AUTH).await?;
    assert_eq!(status, 200);
    assert!(
        summary["items"]
            .as_array()
            .ok_or("monitor summaries required")?
            .iter()
            .all(|monitor| monitor["logs_available"] == true)
    );
    assert_eq!(summary["plugins"][0]["id"], "observation-provider");
    assert_eq!(summary["plugins"][0]["registration"], "registered");
    assert_eq!(summary["plugins"][0]["view_registration"], "registered");
    assert_eq!(summary["plugins"][0]["monitor_count"], 2);
    assert!(
        summary["items"]
            .as_array()
            .ok_or("monitor summaries required")?
            .iter()
            .all(|monitor| monitor["owner_plugin_id"] == "observation-provider")
    );
    let mut observed_value = Value::Null;
    for _ in 0..100 {
        let (status, detail) = call(address, "/api/v1/monitors/monitor-a", AUTH).await?;
        assert_eq!(status, 200);
        observed_value = detail["last_value"].clone();
        if observed_value == json!({"ready":true}) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        observed_value,
        json!({"ready":true}),
        "monitor snapshot must retain the complete bounded observation value"
    );
    let (status, plugin_page) = call(
        address,
        "/api/v1/monitoring/plugins/observation-provider",
        AUTH,
    )
    .await?;
    assert_eq!(status, 200);
    assert_eq!(plugin_page["view_status"], "ready");
    assert_eq!(
        plugin_page["view"]["title"],
        "Observation provider <script>"
    );
    assert_eq!(plugin_page["view"]["sections"][0]["monitor_role"], "status");
    assert_eq!(plugin_page["monitors"].as_array().unwrap().len(), 2);
    assert!(
        plugin_page["monitors"]
            .as_array()
            .unwrap()
            .iter()
            .all(|monitor| monitor["view_role"] == "status"
                && monitor["owner_plugin_id"] == "observation-provider")
    );
    assert_eq!(
        plugin_page["monitors"]
            .as_array()
            .unwrap()
            .iter()
            .find(|monitor| monitor["id"] == "monitor-a")
            .ok_or("monitor-a plugin projection required")?["last_value"],
        json!({"ready":true})
    );
    let (status, first) = call(address, "/api/v1/monitors/monitor-a/logs", AUTH).await?;
    assert_eq!(status, 200);
    assert_eq!(
        first["items"]
            .as_array()
            .ok_or("log entries required")?
            .len(),
        2
    );
    assert_eq!(first["errors"][0]["id"], "a-2");
    assert_eq!(first["target_id"], "target-a");
    assert_eq!(first["stream_label"], "stream-a");
    assert_eq!(first["available_streams"], json!([]));
    let cursor = first["next_cursor"]
        .as_str()
        .ok_or("server cursor required")?;
    assert_ne!(cursor, "a:2", "HTTP must issue its own target-bound cursor");
    assert_eq!(
        call(
            address,
            &format!("/api/v1/monitors/monitor-b/logs?cursor={cursor}"),
            AUTH
        )
        .await?
        .0,
        409
    );
    assert_eq!(
        call(address, "/api/v1/monitors/monitor-a/logs?cursor=a:2", AUTH)
            .await?
            .0,
        409
    );
    append(
        &file,
        json!({"id":"a-3","timestamp":3,"level":"CRITICAL","message":"new condition record"}),
    )?;
    std::fs::write(directory.join("fail-next-query"), "offline")?;
    let (status, temporary) = call(
        address,
        &format!("/api/v1/monitors/monitor-a/logs?cursor={cursor}"),
        AUTH,
    )
    .await?;
    assert_eq!(status, 503);
    assert_eq!(
        temporary["error"]["code"], "logs_read_failed",
        "registered provider failure should allow GET backoff recovery"
    );
    let (status, second) = call(
        address,
        &format!("/api/v1/monitors/monitor-a/logs?cursor={cursor}"),
        AUTH,
    )
    .await?;
    assert_eq!(status, 200);
    assert_eq!(
        second["items"]
            .as_array()
            .ok_or("incremental items required")?
            .len(),
        1
    );
    assert_eq!(second["items"][0]["id"], "a-3");
    assert_eq!(second["errors"][0]["id"], "a-3");
    let cursor2 = second["next_cursor"]
        .as_str()
        .ok_or("incremental cursor required")?;
    std::fs::write(&file, "")?;
    let (status, invalid) = call(
        address,
        &format!("/api/v1/monitors/monitor-a/logs?cursor={cursor2}"),
        AUTH,
    )
    .await?;
    assert_eq!(status, 409);
    assert_eq!(invalid["error"]["code"], "cursor_invalid");
    let (status, reset) = call(address, "/api/v1/monitors/monitor-a/logs", AUTH).await?;
    assert_eq!(status, 200);
    assert_eq!(reset["items"], json!([]));
    assert_eq!(
        call(
            address,
            "/api/v1/monitors/monitor-a/logs?file=outside.txt",
            AUTH
        )
        .await?
        .0,
        400
    );
    assert_eq!(
        call(address, "/api/v1/monitors/missing/logs", AUTH)
            .await?
            .0,
        404
    );
    server.abort();
    let _ = server.await;
    state.shutdown().await?;
    engine.shutdown().await?;
    drop(state);

    let mut restricted = config;
    restricted
        .permissions
        .retain(|permission| permission != "extension.read");
    let (state, engine) = Console::open(restricted).await?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let app = router(state.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let (status, plugin_page) = call(
        address,
        "/api/v1/monitoring/plugins/observation-provider",
        AUTH,
    )
    .await?;
    assert_eq!(status, 200);
    assert_eq!(plugin_page["view_status"], "permission_denied");
    assert!(plugin_page["view"].is_null());
    assert_eq!(plugin_page["monitors"].as_array().unwrap().len(), 2);
    server.abort();
    let _ = server.await;
    state.shutdown().await?;
    engine.shutdown().await?;
    drop(state);

    owner_provider_http(&directory, &token).await?;
    provider.shutdown()?;
    assert_eq!(directory.parent(), Some(root.as_path()));
    std::fs::remove_dir_all(&directory)?;
    println!(
        "isolated provider HTTP observation records and owner/provider projection: records, binding, auth and bounds passed"
    );
    Ok(())
}
