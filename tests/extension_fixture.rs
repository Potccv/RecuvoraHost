//! Shared loopback WebSocket business-contract fixtures.
use crate::network_peer::{PeerServer, Session};
use recuvora_host::integrations::extensions::{
    AllowedMethod, ExtensionDefinition, MONITORING_VIEW_CAPABILITY, MONITORING_VIEW_METHOD,
};
use recuvora_host::integrations::extensions::{
    ContractDeclaration, ExtensionKind, ExtensionMetadata, Message, MethodDeclaration, Outcome,
};
use serde_json::json;
use std::collections::BTreeMap;
use std::error::Error;
use std::sync::Mutex;

pub type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

#[derive(Default)]
pub struct FixtureServers(Mutex<Vec<PeerServer>>);

impl FixtureServers {
    pub fn shutdown(self) -> TestResult {
        for server in self
            .0
            .into_inner()
            .map_err(|_| "fixture server lock poisoned")?
        {
            server.shutdown()?;
        }
        Ok(())
    }
}

pub fn definition(
    fixtures: &FixtureServers,
    id: &str,
    kind: ExtensionKind,
    mode: &str,
) -> TestResult<ExtensionDefinition> {
    definition_with_environment(fixtures, id, kind, mode, BTreeMap::new())
}

pub fn definition_with_environment(
    fixtures: &FixtureServers,
    id: &str,
    kind: ExtensionKind,
    mode: &str,
    mut environment: BTreeMap<String, String>,
) -> TestResult<ExtensionDefinition> {
    let contract = if id == "fixture-node" {
        "recuvora.harness"
    } else {
        "com.example.logs"
    };
    let methods = if id == "fixture-node" {
        vec!["run", "projects", "create_project"]
    } else {
        vec!["query"]
    };
    let mut allow_calls = methods
        .into_iter()
        .map(|method| AllowedMethod {
            contract: contract.into(),
            version: 1,
            method: method.into(),
        })
        .collect::<Vec<_>>();
    if matches!(
        mode,
        "view-good"
            | "view-bad-result"
            | "view-callback"
            | "view-wait-cancel"
            | "view-duplicate"
            | "view-not-read-only"
    ) {
        allow_calls.push(AllowedMethod {
            contract: "com.example.logs.monitoring_view".into(),
            version: 1,
            method: MONITORING_VIEW_METHOD.into(),
        });
    }
    if mode == "view-duplicate" {
        allow_calls.push(AllowedMethod {
            contract: "com.example.logs.other_view".into(),
            version: 1,
            method: MONITORING_VIEW_METHOD.into(),
        });
    }
    environment.insert("MODE".into(), mode.into());
    let server = PeerServer::start(fixture, environment)?;
    let endpoint = server.endpoint();
    fixtures
        .0
        .lock()
        .map_err(|_| "fixture server lock poisoned")?
        .push(server);
    Ok(ExtensionDefinition {
        id: id.into(),
        kind,
        enabled: true,
        endpoint,
        namespaces: if kind == ExtensionKind::Plugin {
            vec!["com.example.logs".into()]
        } else {
            vec![]
        },
        allow_calls,
        allow_nodes: if kind == ExtensionKind::Plugin {
            vec!["logs-node".into()]
        } else {
            vec![]
        },
    })
}

fn fixture(mut session: Session) -> TestResult {
    let mode = session.env("MODE").unwrap_or("good").to_owned();
    let Some(Message::Hello {
        expected_id, kind, ..
    }) = session.read()?
    else {
        return Err("expected hello".into());
    };
    let harness = expected_id == "fixture-node";
    let mut contracts = if harness {
        vec![ContractDeclaration {
            id: "recuvora.harness".into(),
            version: 1,
            methods: [
                ("run", false, "object"),
                ("projects", true, "array"),
                ("create_project", false, "object"),
            ]
            .into_iter()
            .map(|(name, read_only, output)| MethodDeclaration {
                name: name.into(),
                read_only,
                input_schema: json!({"type":"object"}),
                output_schema: json!({"type":output}),
            })
            .collect(),
        }]
    } else {
        vec![ContractDeclaration {
            id: "com.example.logs".into(),
            version: 1,
            methods: vec![MethodDeclaration {
                name: "query".into(),
                read_only: true,
                input_schema: json!({"type":"object","properties":{"needle":{"type":"string","maxLength":64}},"required":["needle"],"additionalProperties":false}),
                output_schema: json!({"type":"object","properties":{"entries":{"type":"array","items":{"type":"string"},"maxItems":16}},"required":["entries"],"additionalProperties":false}),
            }],
        }]
    };
    if !harness && mode.starts_with("view-") {
        contracts.push(ContractDeclaration {
            id: "com.example.logs.monitoring_view".into(),
            version: 1,
            methods: vec![MethodDeclaration {
                name: MONITORING_VIEW_METHOD.into(),
                read_only: mode != "view-not-read-only",
                input_schema: json!({"type":"object","properties":{"schema_version":{"type":"integer","enum":[1]}},"required":["schema_version"],"additionalProperties":false}),
                output_schema: json!({"type":"object"}),
            }],
        });
        if mode == "view-duplicate" {
            contracts.push(ContractDeclaration {
                id: "com.example.logs.other_view".into(),
                version: 1,
                methods: vec![MethodDeclaration {
                    name: MONITORING_VIEW_METHOD.into(),
                    read_only: true,
                    input_schema: json!({"type":"object"}),
                    output_schema: json!({"type":"object"}),
                }],
            });
        }
    }
    let mut metadata = ExtensionMetadata {
        protocol_version: if mode == "bad-version" { 9 } else { 1 },
        id: if mode == "bad-identity" {
            "impostor".into()
        } else {
            expected_id.clone()
        },
        kind,
        contracts,
        capabilities: if harness {
            ["text", "projects", "tools", "client_visibility", "approval"]
                .into_iter()
                .map(str::to_owned)
                .collect()
        } else if mode.starts_with("view-") {
            vec![MONITORING_VIEW_CAPABILITY.into()]
        } else {
            vec![]
        },
        workspaces: if harness {
            vec!["work".into(), "review".into()]
        } else {
            vec![]
        },
    };
    if mode == "bad-schema" {
        metadata.contracts[0].methods[0].input_schema =
            json!({"type":"object","$ref":"unimplemented"});
    }
    if harness && mode.starts_with("schema-") {
        for declaration in &mut metadata.contracts[0].methods {
            if mode == "schema-input" {
                declaration.input_schema = json!({"type":"object","properties":{"required_by_node":{"type":"string"}},"required":["required_by_node"]});
            } else if declaration.name == "projects" {
                declaration.output_schema =
                    json!({"type":"array","maxItems":if mode == "schema-output" {0} else {1}});
            } else {
                let field = if declaration.name == "run" {
                    "final_response"
                } else {
                    "name"
                };
                declaration.output_schema = json!({"type":"object","properties":{field:{"type":"string","maxLength":if mode == "schema-output" {0} else {64}}},"required":[field]});
            }
        }
    }
    session.write(Message::Ready { metadata })?;
    let Some(Message::Call {
        id, method, params, ..
    }) = session.read()?
    else {
        return Ok(());
    };
    assert_ne!(
        mode, "schema-input",
        "invalid schema input must not dispatch a node call"
    );
    if mode == "disconnect" {
        return Ok(());
    }
    if mode == "oversized-frame" {
        session.write_text("x".repeat(recuvora_host::protocol::MAX_FRAME_BYTES + 1))?;
        let _ = session.read();
        return Ok(());
    }
    if mode == "wait-cancel" {
        if let Some(Message::Cancel { id: cancel_id }) = session.read()? {
            assert_eq!(cancel_id, id);
            session.write(Message::Error {
                id,
                code: "cancelled".into(),
                message: "dispatched request cancelled; persistent effects need a result check"
                    .into(),
                outcome: Outcome::Unknown,
            })?;
        }
        return Ok(());
    }
    if mode == "view-wait-cancel" && method == MONITORING_VIEW_METHOD {
        let marker = session
            .env("RECUVORA_VIEW_STARTED")
            .ok_or("view wait fixture requires a start marker")?;
        std::fs::write(marker, b"started")?;
        if let Some(Message::Cancel { id: cancel_id }) = session.read()? {
            assert_eq!(cancel_id, id);
            session.write(Message::Error {
                id,
                code: "cancelled".into(),
                message: "monitoring view descriptor cancelled".into(),
                outcome: Outcome::Cancelled,
            })?;
        }
        return Ok(());
    }
    if !harness {
        let result = if method == MONITORING_VIEW_METHOD {
            if mode == "view-callback" {
                session.write(Message::Callback {
                        id: "view-read-node".into(),
                        parent_id: id.clone(),
                        method: "service.call".into(),
                        params: json!({"node_id":"logs-node","contract":"com.example.logs","version":1,"method":"query","params":{"needle":"forbidden"}}),
                    },
                )?;
            }
            if mode == "view-bad-result" {
                json!("invalid descriptor")
            } else {
                json!({"schema_version":1,"title":"Fixture monitoring","sections":[]})
            }
        } else if mode == "bad-result" {
            json!("invalid output")
        } else if kind == ExtensionKind::Plugin {
            session.write(Message::Callback {
                    id: "read-node".into(),
                    parent_id: id.clone(),
                    method: "service.call".into(),
                    params: json!({"node_id":"logs-node","contract":"com.example.logs","version":1,"method":"query","params":params}),
                },
            )?;
            match session.read()? {
                Some(Message::Result {
                    id: callback_id,
                    result,
                }) if callback_id == "read-node" => {
                    json!({"entries":[format!("consumed:{}",result["entries"][0].as_str().unwrap_or(""))]})
                }
                _ => return Err("plugin callback failed".into()),
            }
        } else {
            json!({"entries":[params["needle"]]})
        };
        session.write(Message::Result { id, result })?;
    } else {
        assert_eq!(params["workspace"]["node_id"], "fixture-node");
        assert!(
            ["work", "review"]
                .contains(&params["workspace"]["workspace_id"].as_str().unwrap_or(""))
        );
        let remote_path = "C:\\OnlyOnTheNode\\Project";
        let result = match method.as_str() {
            "projects" => json!([{"id":"p1","name":"Example","roots":[remote_path]}]),
            "create_project" => json!({"id":"created","name":params["name"],"roots":[remote_path]}),
            "run" => {
                let approval = params["role"] == "approval";
                if approval {
                    assert_eq!(params["visibility"], "hidden");
                    assert_eq!(params["placement"]["type"], "none");
                    assert!(params["tools"].as_array().is_some_and(Vec::is_empty));
                }
                if params["tools"]
                    .as_array()
                    .is_some_and(|tools| !tools.is_empty())
                {
                    let callback = Message::Callback {
                        id: "tool-rpc".into(),
                        parent_id: if mode == "wrong-parent" {
                            "other".into()
                        } else {
                            id.clone()
                        },
                        method: "tool".into(),
                        params: json!({"harness_id":params["harness_id"],"thread_id":"thread-1","turn_id":"turn-1","call_id":"call-1","tool":params["tools"][0]["name"],"arguments":{"text":"hello"}}),
                    };
                    session.write(callback.clone())?;
                    match session.read()? {
                        Some(Message::Result { result, .. }) => assert_eq!(result["success"], true),
                        Some(Message::Error { outcome, .. }) if mode.starts_with("tool-error-") => {
                            assert_eq!(
                                outcome,
                                match mode.as_str() {
                                    "tool-error-cancelled" => Outcome::Cancelled,
                                    "tool-error-rejected" => Outcome::Rejected,
                                    _ => Outcome::Unknown,
                                }
                            );
                        }
                        Some(Message::Cancel { .. }) | None => return Ok(()),
                        _ => return Err("tool callback failed".into()),
                    }
                    if mode == "duplicate-tool" {
                        session.write(callback)?;
                        let _ = session.read()?;
                        return Ok(());
                    }
                }
                json!({"thread_id":"thread-1","session_id":"session-1","project_directory":remote_path,"visibility":params["visibility"],"native_project_id":params["placement"].get("project_id"),"client_project_grouping":{"type":if params["visibility"]=="hidden" {"not_applicable"}else{"unverified"}},"final_response":if approval {"APPROVAL_FIXTURE"}else{"REMOTE_FIXTURE"}})
            }
            _ => return Err("unknown Harness method".into()),
        };
        session.write(Message::Result { id, result })?;
    }
    // The host explicitly closes each probe and call WebSocket session.
    let _ = session.read()?;
    Ok(())
}
