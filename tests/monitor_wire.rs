//! Loopback network protocol integration; the peer is only a test fixture.
mod network_peer;
use recuvora_core::recovery::incidents::{IncidentKind, IncidentStatus};
use recuvora_host::integrations::extensions::{
    AllowedMethod, ContractDeclaration, ExtensionDefinition, ExtensionKind, ExtensionMetadata,
    ExtensionRegistry, ExtensionsConfig, Message, MethodDeclaration, NetworkEndpoint,
};
use recuvora_host::integrations::monitoring::RegistryObservationSource;
use recuvora_host::monitoring::{MonitorEngine, MonitorsConfig};
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
                version: 1,
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
    if kind == ExtensionKind::Plugin {
        session.write(
            Message::Callback {
                id: "read-node".into(),
                parent_id: id.clone(),
                method: "service.call".into(),
                params: json!({"node_id":"probe-node","contract":"example.monitor","version":1,"method":"observe","params":params}),
            },
        )?;
        match session.read()? {
            Some(Message::Result { result, .. }) => {
                session.write(Message::Result { id, result })?
            }
            _ => return Ok(()), // Lost node result must propagate as Unknown, never empty healthy data.
        }
    } else {
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
                    "schema_version":1,"target_id":"service","source_id":"probe","generation":"g1",
                    "cursor":params["cursor"],"next_cursor":sequence.to_string(),"coverage":"complete","has_more":false,"error":null,
                    "samples":[{"id":format!("sample-{sequence}"),"sequence":sequence,"age_ms":0,"value":{"ready":sequence>=3},"evidence":{"source":"wire-fixture"}}]
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
            version: 1,
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
    let config: MonitorsConfig = serde_json::from_value(json!({"schema_version":1,"monitors":[{
        "id":"ready","target_id":"service","source_id":"probe","extension_id":"probe-plugin",
        "contract":"example.monitor","version":1,"method":"observe","params":{},"interval_ms":200,"timeout_ms":5000,
        "stale_after_ms":20000,"startup_grace_ms":20000,
        "rule":{"pointer":"/ready","operator":"eq","value":true,"failure_samples":2,"success_samples":2}
    }]}))?;
    let source = Arc::new(RegistryObservationSource::new(registry.clone()));
    let mut engine = MonitorEngine::start_with_source(config, source, dir.path.join("runtime"))?;
    let handle = engine.handle();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let records = handle.incidents()?;
            let recovered = records
                .iter()
                .any(|r| r.kind == IncidentKind::Target && r.status == IncidentStatus::Resolved);
            let lost = records
                .iter()
                .any(|r| r.kind == IncidentKind::Coverage && r.status == IncidentStatus::Open);
            if recovered && lost {
                return Result::Ok(());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await??;
    let records = handle.incidents()?;
    let target = records
        .iter()
        .find(|r| r.kind == IncidentKind::Target)
        .ok_or("missing target incident")?;
    assert_eq!(target.evidence["sample_id"], "sample-4");
    assert!(target.resolved_at.is_some());
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
        "monitor_wire: real plugin/node calls, durable target fault and evidence resolution, disconnect coverage passed"
    );
    Ok(())
}
