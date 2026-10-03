//! Loopback network protocol integration; the peer is only a test fixture.
mod network_peer;
use recuvora_host::control::recovery::incidents::{IncidentKind, IncidentStatus};
use recuvora_host::integrations::extensions::{
    AllowedMethod, ContractDeclaration, ExtensionDefinition, ExtensionKind, ExtensionMetadata,
    ExtensionRegistry, ExtensionsConfig, Message, MethodDeclaration, NetworkEndpoint,
};
use recuvora_host::integrations::monitoring::RegistryObservationSource;
use recuvora_host::monitoring::{
    MonitorEngine, MonitorsConfig, ObservationRequest, ObservationSource,
};
use recuvora_host::runtime::operation::Cancellation;
use serde_json::json;
use std::collections::BTreeMap;
use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

type Result<T = ()> = std::result::Result<T, Box<dyn Error + Send + Sync>>;
fn main() -> Result {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run())
}
fn fixture(mut session: network_peer::Session) -> Result {
    let Some(Message::Hello {
        expected_id, kind, ..
    }) = session.read()?
    else {
        return Err("expected hello".into());
    };
    session.write(Message::Ready {
        metadata: ExtensionMetadata {
            protocol_version: 1,
            id: expected_id,
            kind,
            contracts: vec![ContractDeclaration {
                id: "example.monitor".into(),
                version: 2,
                methods: vec![MethodDeclaration {
                    name: "observe".into(),
                    read_only: true,
                    input_schema: json!({"type":"object"}),
                    output_schema: json!({"type":"object"}),
                }],
            }],
            capabilities: vec![],
            workspaces: vec![],
        },
    })?;
    let Some(Message::Call { id, params, .. }) = session.read()? else {
        return Ok(());
    };
    if kind != ExtensionKind::Node {
        return Err("plugin must not be invoked as error source".into());
    }
    {
        let sequence = params["cursor"]
            .as_str()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0)
            + 1;
        if sequence >= 5 {
            return Ok(());
        }
        session.write(
            Message::Result {
                id,
                result: json!({
                    "schema_version":2,"target_id":"service","source_id":"probe","generation":"g1",
                    "cursor":params["cursor"],"next_cursor":sequence.to_string(),"coverage":"complete","has_more":false,"source_error":null,
                    "errors":[{"id":format!("error-{sequence}"),"sequence":sequence,"age_ms":0,"fingerprint":"node-error","message":"node reported error","evidence":{"source":"wire-fixture"}}]
                }),
            },
        )?;
    }
    // The peer answers the client's WebSocket Close handshake after the result.
    while session.read()?.is_some() {}
    Ok(())
}

struct TestDir {
    path: PathBuf,
    root: PathBuf,
}
impl TestDir {
    fn new() -> Result<Self> {
        let root = PathBuf::from(
            std::env::var_os("RECUVORA_TEST_TEMP").ok_or("external test root required")?,
        );
        if !root.is_absolute() {
            return Err("absolute test root required".into());
        }
        std::fs::create_dir_all(&root)?;
        let root = root.canonicalize()?;
        if root.starts_with(Path::new(env!("CARGO_MANIFEST_DIR")).canonicalize()?) {
            return Err("external root required".into());
        }
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let path = root.join(format!("monitor-wire-{}-{stamp}", std::process::id()));
        std::fs::create_dir(&path)?;
        Ok(Self { path, root })
    }
}
impl Drop for TestDir {
    fn drop(&mut self) {
        assert_eq!(self.path.parent(), Some(self.root.as_path()));
        std::fs::remove_dir_all(&self.path).expect("remove recorded monitor wire fixture");
    }
}
fn definition(
    id: &str,
    kind: ExtensionKind,
    endpoint: &NetworkEndpoint,
) -> Result<ExtensionDefinition> {
    Ok(ExtensionDefinition {
        ui_links: Default::default(),
        id: id.into(),
        kind,
        enabled: true,
        endpoint: endpoint.clone(),
        namespaces: if kind == ExtensionKind::Plugin {
            vec!["example.monitor".into()]
        } else {
            vec![]
        },
        allow_calls: vec![AllowedMethod {
            contract: "example.monitor".into(),
            version: 2,
            method: "observe".into(),
        }],
        allow_nodes: if kind == ExtensionKind::Plugin {
            vec!["probe-node".into()]
        } else {
            vec![]
        },
    })
}
async fn run() -> Result {
    let dir = TestDir::new()?;
    let server = network_peer::PeerServer::start(fixture, BTreeMap::new())?;
    let endpoint = server.endpoint();
    let registry = Arc::new(
        ExtensionRegistry::connect(ExtensionsConfig {
            schema_version: 1,
            extensions: vec![
                definition("probe-plugin", ExtensionKind::Plugin, &endpoint)?,
                definition("probe-node", ExtensionKind::Node, &endpoint)?,
            ],
        })
        .await?,
    );
    let config: MonitorsConfig = serde_json::from_value(json!({"schema_version":2,"monitors":[{
        "id":"ready","target_id":"service","source_id":"probe","extension_id":"probe-node",
        "contract":"example.monitor","version":2,"method":"observe","params":{},"interval_ms":200,"timeout_ms":5000,
        "stale_after_ms":20000,"startup_grace_ms":20000
    }]}))?;
    let source = Arc::new(RegistryObservationSource::new(registry.clone()));
    let plugin_result = source
        .poll(
            ObservationRequest {
                extension_id: "probe-plugin".into(),
                contract: "example.monitor".into(),
                version: 2,
                method: "observe".into(),
                params: json!({}),
                timeout: Duration::from_secs(1),
            },
            Cancellation::new(),
        )
        .await;
    assert!(
        plugin_result.is_err(),
        "a plugin cannot impersonate the Node error source"
    );
    let mut engine = MonitorEngine::start_with_source(config, source, dir.path.join("runtime"))?;
    let handle = engine.handle();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let records = handle.incidents()?;
            let received = records
                .iter()
                .filter(|r| r.kind == IncidentKind::ErrorLog)
                .count()
                == 4;
            let lost = records
                .iter()
                .any(|r| r.kind == IncidentKind::Coverage && r.status == IncidentStatus::Open);
            if received && lost {
                return Result::Ok(());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await??;
    let records = handle.incidents()?;
    let received: Vec<_> = records
        .iter()
        .filter(|r| r.kind == IncidentKind::ErrorLog)
        .collect();
    assert_eq!(received.len(), 4);
    assert!(
        received
            .iter()
            .all(|record| record.status == IncidentStatus::Open && record.resolved_at.is_none())
    );
    assert!(
        received
            .iter()
            .any(|record| record.evidence["log"]["id"] == "error-4")
    );
    assert!(
        handle
            .monitor("ready")?
            .ok_or("missing monitor")?
            .last_error
            .is_some()
    );
    engine.shutdown().await?;
    registry.shutdown().await?;
    server.shutdown()?;
    println!(
        "monitor_wire: Node errors durably received, Plugin source rejected, disconnect coverage passed"
    );
    Ok(())
}
