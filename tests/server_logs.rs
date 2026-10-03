//! HTTP reads of durable Node error receipts through isolated loopback peers.
mod network_peer;
use network_peer::{PeerServer, Session};
use recuvora_host::protocol::{ContractDeclaration, ExtensionMetadata, Message, MethodDeclaration};
use recuvora_host::server::{Console, ServerConfig, router};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    error::Error,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;
static NODE_CALLS: AtomicU64 = AtomicU64::new(0);

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
        version: 2,
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
fn fixture_message(index: usize) -> String {
    format!(
        "Node error {index:02} <script>read as data</script> {}",
        "x".repeat(7000)
    )
}
fn provider(mut session: Session) -> TestResult {
    let Some(Message::Hello {
        expected_id, kind, ..
    }) = session.read()?
    else {
        return Err("hello required".into());
    };
    let role = session.env("ROLE").unwrap_or("node").to_owned();
    let contract = if role == "other" {
        "example.other"
    } else {
        "example.observation"
    };
    let mut contracts = vec![fixture_contract(contract, &["observe", "inventory"])];
    let mut capabilities = vec![];
    if role == "owner" {
        contracts.push(ContractDeclaration {
            id:"example.observation.monitoring_view".into(), version:1,
            methods:vec![MethodDeclaration {
                name:"describe_monitoring_view".into(), read_only:true,
                input_schema:json!({"type":"object","properties":{"schema_version":{"type":"integer","minimum":1,"maximum":1}},"required":["schema_version"],"additionalProperties":false}),
                output_schema:json!({"type":"object"}),
            }],
        });
        capabilities.push("recuvora.monitoring_view.v1".into());
    }
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
        match method.as_str() {
            "observe" => {
                assert_ne!(role, "owner", "contract owner must not collect error facts");
                NODE_CALLS.fetch_add(1, Ordering::SeqCst);
                let errors = if params["target_key"] == "a" && params["cursor"].is_null() {
                    (1..=32).map(|index| json!({"id":format!("a-{index:02}"),"sequence":index,"age_ms":0,
                        "fingerprint":"target.failure","message":fixture_message(index),"evidence":{"node":"fixture"}})).collect::<Vec<_>>()
                } else {
                    vec![]
                };
                session.write(Message::Result {id,result:json!({"schema_version":2,"target_id":params["target_id"],"source_id":params["source_id"],
                    "generation":"fixture-g1","cursor":params["cursor"],"next_cursor":"received-32","has_more":false,"coverage":"complete","source_error":null,"errors":errors})})?;
            }
            "inventory" => {
                let key = if role == "other" { "other" } else { "owned" };
                session.write(Message::Result {id,result:json!({"schema_version":1,"complete":true,"targets":[{"key":key}],"error":null})})?;
            }
            "describe_monitoring_view" => {
                assert_eq!(params, json!({"schema_version":1}));
                session.write(Message::Result {id,result:json!({"schema_version":1,"title":"Observation provider <script>",
                    "summary":"Read-only fields from host-owned receiver snapshots.","sections":[{"id":"status","title":"Node errors","monitor_role":"status",
                    "fields":[{"id":"message","label":"Message <img>","source":"last_error_log","pointer":"/message","format":"text","empty":"No error received"}]}]})})?;
            }
            _ => {
                return Err(
                    "unexpected fixture method; HTTP receipt reads must not call a Node".into(),
                );
            }
        }
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
        serde_json::from_str(body).unwrap_or_else(|_| Value::String(body.into())),
    ))
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
                    {"contract":"example.observation","version":2,"method":"observe"},
                    {"contract":"example.observation","version":2,"method":"inventory"}
                ],
                "endpoint":node_peer.endpoint()
            },
            {
                "id":"other-node","kind":"node","enabled":true,"namespaces":[],"allow_nodes":[],
                "allow_calls":[
                    {"contract":"example.other","version":2,"method":"observe"},
                    {"contract":"example.other","version":2,"method":"inventory"}
                ],
                "endpoint":other_peer.endpoint()
            },
            {
                "id":"other-provider","kind":"plugin","enabled":true,"namespaces":["example.other"],"allow_nodes":[],
                "allow_calls":[
                    {"contract":"example.other","version":2,"method":"observe"},
                    {"contract":"example.other","version":2,"method":"inventory"}
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
            "extension_id":extension_id,"contract":contract,"version":2,"method":"observe","params":{},
            "interval_ms":3_600_000,"timeout_ms":5000,"stale_after_ms":60_000,"startup_grace_ms":60_000
        })
    };
    let discovery = |id: &str,
                     extension_id: &str,
                     contract: &str,
                     template_id: &str,
                     target_id: &str,
                     source_id: &str| {
        json!({
            "id":id,"extension_id":extension_id,"contract":contract,"version":2,"method":"inventory","params":{},
            "interval_ms":3_600_000,"timeout_ms":5000,"max_targets":1,"parameter":"target_key",
            "template":monitor(template_id,target_id,source_id,extension_id,contract)
        })
    };
    let monitors = phase.join("monitors.json");
    std::fs::write(
        &monitors,
        serde_json::to_vec(&json!({
            "schema_version":2,
            "monitors":[
                monitor("owner-static","owner-static-target","owner-static-source","observation-node","example.observation"),
                monitor("other-static","other-static-target","other-static-source","other-node","example.other")
            ],
            "discoveries":[
                discovery("owner-inventory","observation-node","example.observation","owner-dynamic","owner-target","owner-source"),
                discovery("other-inventory","other-node","example.other","other-dynamic","other-target","other-source")
            ]
        }))?,
    )?;
    let config: ServerConfig = serde_json::from_value(json!({
        "schema_version":1,"listen":"127.0.0.1:0","token_file":token,"operator":"fixture-operator",
        "permissions":["monitor.read","extension.read"],"allowed_origins":[],"data_dir":phase,
        "harness_config":null,"extensions_config":extensions,"repair_config":null,"monitors_config":monitors
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
    let owner = provider_server("owner", &directory)?;
    let node = provider_server("node", &directory)?;
    let extensions = directory.join("extensions.json");
    let write_extensions = |node: &PeerServer| -> TestResult {
        std::fs::write(
            &extensions,
            serde_json::to_vec(&json!({"schema_version":1,"extensions":[
                {"id":"observation-provider","kind":"plugin","enabled":true,"namespaces":["example.observation"],"allow_nodes":[],
                 "allow_calls":[{"contract":"example.observation.monitoring_view","version":1,"method":"describe_monitoring_view"}],"endpoint":owner.endpoint()},
                {"id":"observation-node","kind":"node","enabled":true,"namespaces":[],"allow_nodes":[],
                 "allow_calls":[{"contract":"example.observation","version":2,"method":"observe"}],"endpoint":node.endpoint()}
            ]}))?,
        )?;
        Ok(())
    };
    write_extensions(&node)?;
    let monitors = directory.join("monitors.json");
    let definitions = ["a","b"].into_iter().map(|name|json!({"id":format!("monitor-{name}"),"target_id":format!("target-{name}"),
        "source_id":format!("source-{name}"),"extension_id":"observation-node","contract":"example.observation","version":2,"method":"observe","view_role":"status","params":{"target_key":name},
        "interval_ms":3_600_000,"timeout_ms":5000,"stale_after_ms":60_000,"startup_grace_ms":60_000})).collect::<Vec<_>>();
    std::fs::write(
        &monitors,
        serde_json::to_vec(&json!({"schema_version":2,"monitors":definitions}))?,
    )?;
    let config: ServerConfig = serde_json::from_value(
        json!({"schema_version":1,"listen":"127.0.0.1:0","token_file":token,"operator":"fixture-operator",
        "permissions":["monitor.read","logs.read","extension.read","incident.read"],"allowed_origins":[],"data_dir":directory,
        "harness_config":null,"extensions_config":extensions,"repair_config":null,"monitors_config":monitors}),
    )?;
    let (state, engine) = Console::open(config.clone()).await?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let app = router(state.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let mut summary = Value::Null;
    for _ in 0..200 {
        summary = call(address, "/api/v1/monitors", AUTH).await?.1;
        if summary["items"].as_array().is_some_and(|items| {
            items.len() == 2
                && items.iter().all(|item| item["coverage"] == "complete")
                && items
                    .iter()
                    .any(|item| item["id"] == "monitor-a" && item["received_error_count"] == 32)
        }) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let items = summary["items"]
        .as_array()
        .ok_or("receiver summary required")?;
    assert!(
        items.iter().all(|item| item["logs_available"] == true
            && item["owner_plugin_id"] == "observation-provider")
    );
    let detail = call(address, "/api/v1/monitors/monitor-a", AUTH).await?.1;
    assert_eq!(
        detail["received_error_count"], 32,
        "Node errors must first commit before HTTP can read them"
    );
    assert_eq!(detail["last_error_log"]["message"], fixture_message(32));
    assert!(
        detail.get("health").is_none(),
        "Host does not infer target health"
    );
    let node_calls = NODE_CALLS.load(Ordering::SeqCst);
    let incidents_before = call(address, "/api/v1/incidents?limit=100", AUTH).await?.1;
    let (status, operations_before) = call(address, "/api/v1/operations?limit=100", AUTH).await?;
    assert_eq!(status, 200);
    assert_eq!(
        call(address, "/api/v1/monitors/monitor-a/logs", "")
            .await?
            .0,
        401
    );
    let (status, first) = call(address, "/api/v1/monitors/monitor-a/logs", AUTH).await?;
    assert_eq!(status, 200);
    assert_eq!(first["items"], first["errors"]);
    assert_eq!(first["items"][0]["node_log_id"], "a-01");
    assert_eq!(first["items"][0]["message"], fixture_message(1));
    assert_eq!(first["items"][0]["level"], "ERROR");
    assert_eq!(first["items"][0]["event"], "target.failure");
    assert_eq!(first["full_log"], false);
    assert_eq!(first["record_kind"], "node_error");
    assert_eq!(
        first["has_more"], true,
        "byte limit must paginate a full accepted batch"
    );
    assert!(serde_json::to_vec(&first)?.len() <= 256 * 1024);
    let mut all = first["items"]
        .as_array()
        .ok_or("receipt page required")?
        .clone();
    let first_cursor = first["next_cursor"]
        .as_str()
        .ok_or("receipt cursor required")?
        .to_owned();
    let mut cursor = first_cursor.clone();
    let mut pages = 1;
    loop {
        let path = format!("/api/v1/monitors/monitor-a/logs?cursor={cursor}");
        let (status, page) = call(address, &path, AUTH).await?;
        assert_eq!(status, 200);
        assert_eq!(page["items"], page["errors"]);
        assert!(serde_json::to_vec(&page)?.len() <= 256 * 1024);
        all.extend(
            page["items"]
                .as_array()
                .ok_or("receipt page required")?
                .clone(),
        );
        cursor = page["next_cursor"]
            .as_str()
            .ok_or("next receipt cursor required")?
            .to_owned();
        pages += 1;
        assert!(pages < 5, "page cursor must make bounded progress");
        if page["has_more"] == false {
            break;
        }
    }
    assert_eq!(all.len(), 32, "pagination must not skip accepted errors");
    for (index, item) in all.iter().enumerate() {
        assert_eq!(item["node_log_id"], format!("a-{:02}", index + 1));
        assert_eq!(item["message"], fixture_message(index + 1));
        if index > 0 {
            assert!(all[index - 1]["id"].as_str() < item["id"].as_str());
        }
    }
    let tail_path = format!("/api/v1/monitors/monitor-a/logs?cursor={cursor}");
    let tail = call(address, &tail_path, AUTH).await?.1;
    assert_eq!(tail["items"], json!([]));
    assert_eq!(tail["next_cursor"], cursor);
    let replay = call(
        address,
        &format!("/api/v1/monitors/monitor-a/logs?cursor={first_cursor}"),
        AUTH,
    )
    .await?
    .1;
    assert_eq!(
        replay["items"],
        json!(all[first["items"].as_array().unwrap().len()..])
    );
    assert_eq!(
        call(
            address,
            &format!("/api/v1/monitors/monitor-b/logs?cursor={first_cursor}"),
            AUTH
        )
        .await?
        .0,
        409
    );
    assert_eq!(
        call(
            address,
            "/api/v1/monitors/monitor-a/logs?cursor=received-32",
            AUTH
        )
        .await?
        .0,
        409
    );
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
        call(address, "/api/v1/monitors/monitor-a/logs?limit=33", AUTH)
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
    let empty = call(address, "/api/v1/monitors/monitor-b/logs", AUTH)
        .await?
        .1;
    assert_eq!(empty["items"], json!([]));
    assert!(empty.get("health").is_none());
    let one = call(address, "/api/v1/monitors/monitor-a/logs?limit=1", AUTH)
        .await?
        .1;
    assert_eq!(one["items"].as_array().unwrap().len(), 1);
    assert_eq!(one["has_more"], true);
    let plugin_page = call(
        address,
        "/api/v1/monitoring/plugins/observation-provider",
        AUTH,
    )
    .await?
    .1;
    assert_eq!(plugin_page["view_status"], "ready");
    assert_eq!(
        plugin_page["view"]["sections"][0]["fields"][0]["source"],
        "last_error_log"
    );
    assert_eq!(plugin_page["monitors"].as_array().unwrap().len(), 2);
    assert_eq!(
        NODE_CALLS.load(Ordering::SeqCst),
        node_calls,
        "HTTP receipt reads must not dispatch Node calls"
    );
    assert_eq!(
        call(address, "/api/v1/incidents?limit=100", AUTH).await?.1["items"],
        incidents_before["items"],
        "GET must not mutate incident revisions"
    );
    assert_eq!(
        call(address, "/api/v1/operations?limit=100", AUTH).await?.1["items"],
        operations_before["items"],
        "GET must not create application/Core work"
    );
    assert_eq!(
        call(address, "/api/v1/monitors/monitor-a", AUTH).await?.1["cursor"],
        detail["cursor"],
        "HTTP paging must not move the Node checkpoint"
    );
    node.shutdown()?;
    let offline = call(address, "/api/v1/monitors/monitor-a/logs", AUTH).await?;
    assert_eq!(
        offline.0, 200,
        "accepted receipts remain readable with the Node disconnected"
    );
    assert_eq!(offline.1["items"], first["items"]);
    server.abort();
    let _ = server.await;
    state.shutdown().await?;
    engine.shutdown().await?;
    drop(state);

    // A fresh HTTP cursor table must not revive stale cursors; durable receipts survive.
    let node = provider_server("node", &directory)?;
    write_extensions(&node)?;
    let (state, engine) = Console::open(config.clone()).await?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let app = router(state.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    assert_eq!(
        call(
            address,
            &format!("/api/v1/monitors/monitor-a/logs?cursor={first_cursor}"),
            AUTH
        )
        .await?
        .0,
        409
    );
    assert_eq!(
        call(address, "/api/v1/monitors/monitor-a/logs", AUTH)
            .await?
            .1["items"],
        first["items"]
    );
    server.abort();
    let _ = server.await;
    state.shutdown().await?;
    engine.shutdown().await?;
    drop(state);
    for denied in ["extension.read", "logs.read"] {
        let mut restricted = config.clone();
        restricted
            .permissions
            .retain(|permission| permission != denied);
        let (state, engine) = Console::open(restricted).await?;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let app = router(state.clone());
        let server = tokio::spawn(async move { axum::serve(listener, app).await });
        let page = call(
            address,
            "/api/v1/monitoring/plugins/observation-provider",
            AUTH,
        )
        .await?
        .1;
        assert_eq!(page["monitors"].as_array().unwrap().len(), 2);
        if denied == "extension.read" {
            assert_eq!(page["view_status"], "permission_denied");
            assert!(page["view"].is_null());
        } else {
            assert_eq!(page["view_status"], "ready");
            assert!(
                page["monitors"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|monitor| monitor.get("last_error_log").is_none()),
                "plugin monitor projections must not bypass logs.read"
            );
            let detail = call(address, "/api/v1/monitors/monitor-a", AUTH).await?.1;
            assert!(
                detail.get("last_error_log").is_none(),
                "monitor detail must not bypass logs.read"
            );
        }
        assert_eq!(
            call(address, "/api/v1/monitors/monitor-a/logs", AUTH)
                .await?
                .0,
            403
        );
        server.abort();
        let _ = server.await;
        state.shutdown().await?;
        engine.shutdown().await?;
        drop(state);
    }
    node.shutdown()?;
    owner.shutdown()?;
    owner_provider_http(&directory, &token).await?;
    assert_eq!(directory.parent(), Some(root.as_path()));
    std::fs::remove_dir_all(&directory)?;
    println!(
        "isolated Node error receipt HTTP tests: persistence, read-only paging, auth, bounds and plugin ownership passed"
    );
    Ok(())
}
