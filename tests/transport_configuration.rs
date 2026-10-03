//! Required network endpoints, rejected legacy commands and trust-file protection.
mod workflow_support;
use recuvora_host::configuration::extension_protected_paths;
use recuvora_host::integrations::extensions::{
    ExtensionsConfig, NetworkEndpoint, NodeSettings, ProtocolSettings,
};
use serde_json::{Value, json};
use workflow_support::TestDir;

fn endpoint(url: &str) -> NetworkEndpoint {
    NetworkEndpoint {
        url: url.into(),
        bearer_token_env: None,
        ca_certificate: None,
    }
}

#[test]
fn protocol_and_node_runtime_settings_are_strict_and_bounded() {
    let protocol: ProtocolSettings = serde_json::from_value(json!({})).unwrap();
    assert_eq!(
        protocol,
        ProtocolSettings {
            connect_timeout_ms: 5_000,
            handshake_timeout_ms: 5_000,
            io_timeout_ms: 5_000,
            close_timeout_ms: 10_000,
            cancel_grace_ms: 10_000,
            http_poll_timeout_ms: 35_000,
            http_server_wait_secs: 30,
            http_empty_backoff_ms: 100,
            websocket_idle_timeout_ms: 1_810_000,
            incoming_queue_capacity: 8,
        }
    );
    protocol.validate().unwrap();
    assert!(serde_json::from_value::<ProtocolSettings>(json!({"unexpected":true})).is_err());

    let mut invalid = protocol.clone();
    invalid.http_poll_timeout_ms = invalid.http_server_wait_secs * 1_000;
    assert!(invalid.validate().is_err());
    let mut invalid = protocol;
    invalid.websocket_idle_timeout_ms = 1_800_000 + invalid.cancel_grace_ms - 1;
    assert!(invalid.validate().is_err());

    let nodes: NodeSettings = serde_json::from_value(json!({})).unwrap();
    assert_eq!(nodes, NodeSettings::default());
    nodes.validate().unwrap();
    assert!(
        serde_json::from_value::<NodeSettings>(json!({"ordinary_concurrency":0}))
            .unwrap()
            .validate()
            .is_err()
    );
    assert!(serde_json::from_value::<NodeSettings>(json!({"unknown":1})).is_err());
}

fn configuration(connection: Value) -> Value {
    let mut extension = json!({"id":"target-node","kind":"node","enabled":true});
    extension
        .as_object_mut()
        .unwrap()
        .extend(connection.as_object().unwrap().clone());
    json!({"schema_version":1,"extensions":[extension]})
}

#[test]
fn explicit_network_schemes_and_credential_free_urls() {
    for scheme in ["http", "https", "ws", "wss"] {
        endpoint(&format!("{scheme}://node.example:8443/protocol"))
            .validate()
            .unwrap();
    }
    for url in [
        "ssh://node.example",
        "tcp://node.example",
        "file:///node",
        "https://user:secret@node.example",
        "wss://node.example/?token=secret",
        "http://node.example/#secret",
        "ws://node.example:0",
        "http://node. example",
    ] {
        assert!(endpoint(url).validate().is_err(), "accepted {url}");
    }
    let mut config = endpoint("https://node.example/protocol");
    config.bearer_token_env = Some("RECUVORA_NODE_TOKEN".into());
    config.validate().unwrap();
    config.bearer_token_env = Some("token\r\nheader".into());
    assert!(config.validate().is_err());
}

#[test]
fn endpoints_are_required_and_legacy_process_commands_are_rejected() {
    let command = json!({"program":"node.exe","args":[],"cwd":".","env":{}});
    let network: ExtensionsConfig = serde_json::from_value(configuration(
        json!({"endpoint":{"url":"https://node.example/protocol"}}),
    ))
    .unwrap();
    network.validate().unwrap();
    assert_eq!(
        network.extensions[0].endpoint.url,
        "https://node.example/protocol"
    );
    for connection in [
        json!({}),
        json!({"endpoint":null}),
        json!({"command":command}),
        json!({"command":command,"endpoint":{"url":"ws://node.example/protocol"}}),
    ] {
        assert!(serde_json::from_value::<ExtensionsConfig>(configuration(connection)).is_err());
    }
}

#[test]
fn extension_templates_use_valid_network_endpoints() {
    for template in [
        include_str!("../profiles/extensions.example.json"),
        include_str!("../profiles/extensions.network.example.json"),
        include_str!("../profiles/extensions.pages.example.json"),
    ] {
        let config: ExtensionsConfig = serde_json::from_str(template).unwrap();
        config.validate().unwrap();
        assert!(!config.extensions.is_empty());
    }
}

#[test]
fn relative_ca_is_loaded_and_protected_from_repair() {
    let dir = TestDir::new("network-config");
    let ca = dir.path.join("trust.pem");
    std::fs::write(&ca, b"placeholder; validated as PEM only when connecting").unwrap();
    let path = dir.path.join("extensions.json");
    std::fs::write(
        &path,
        serde_json::to_vec(&configuration(
            json!({"endpoint":{"url":"wss://node.example/protocol","ca_certificate":"trust.pem"}}),
        ))
        .unwrap(),
    )
    .unwrap();
    let config = ExtensionsConfig::load(&path).unwrap();
    assert_eq!(
        config.extensions[0]
            .endpoint
            .ca_certificate
            .as_ref()
            .unwrap(),
        &ca
    );
    let target = dir.path.join("target");
    std::fs::create_dir(&target).unwrap();
    let protected = extension_protected_paths(&path, &target).unwrap();
    assert!(protected.contains(&path.canonicalize().unwrap()));
    assert!(protected.contains(&ca.canonicalize().unwrap()));
    assert!(extension_protected_paths(&path, &ca).is_err());
    assert!(extension_protected_paths(&path, &path).is_err());
}
