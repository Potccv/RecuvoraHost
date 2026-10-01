mod extension_fixture;
mod network_peer;
use extension_fixture::{FixtureServers, TestResult, definition};
use recuvora_host::harnesses::*;
use recuvora_host::integrations::extensions::{ExtensionKind, ExtensionRegistry, ExtensionsConfig};
use recuvora_host::integrations::harness::RemoteHarnessFactory;
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

fn main() -> TestResult {
    let fixtures = FixtureServers::default();
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run(&fixtures))?;
    fixtures.shutdown()
}
async fn registry(fixtures: &FixtureServers, mode: &str) -> TestResult<HarnessRegistry> {
    let extensions = ExtensionRegistry::connect(ExtensionsConfig {
        schema_version: 1,
        extensions: vec![definition(
            fixtures,
            "fixture-node",
            ExtensionKind::Node,
            mode,
        )?],
    })
    .await?;
    let mut builder = HarnessRegistryBuilder::new();
    builder.register(Arc::new(RemoteHarnessFactory::new(Arc::new(extensions))))?;
    Ok(builder.build(HarnessRegistryConfig {
        schema_version: 1,
        default_harness: Some("remote".into()),
        harnesses: vec![HarnessDefinition::new(
            "remote",
            REMOTE_NODE_ADAPTER,
            "node://fixture-node",
            vec!["work".into(), "review".into()],
        )],
    })?)
}
struct Handler(Arc<AtomicUsize>);
impl HarnessToolHandler for Handler {
    fn call<'a>(&'a self, call: HarnessToolCall) -> HarnessToolFuture<'a> {
        Box::pin(async move {
            assert_eq!(call.harness_id, "remote");
            assert_eq!(call.arguments, json!({"text":"hello"}));
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(HarnessToolResult {
                content: "checked".into(),
                success: true,
            })
        })
    }
}
fn tool_request(calls: Arc<AtomicUsize>) -> HarnessRunRequest {
    HarnessRunRequest::remote("fixture-node", "work", "use host tool")
        .with_visibility(ConversationVisibility::Hidden)
        .with_tools(
            vec![HarnessTool {
                name: "read_text".into(),
                description: "Read allowed text".into(),
                input_schema: json!({"type":"object"}),
            }],
            Arc::new(Handler(calls)),
        )
}
async fn run(fixtures: &FixtureServers) -> TestResult {
    capacity_and_dropped_caller_wait(fixtures).await?;
    let normal = registry(fixtures, "good").await?;
    let projects = normal
        .list_projects(
            None,
            HarnessProjectListRequest::remote("fixture-node", "work"),
        )
        .await?;
    assert_eq!(
        projects[0].roots[0].to_str(),
        Some("C:\\OnlyOnTheNode\\Project")
    );
    let project = normal
        .create_project(
            None,
            HarnessProjectCreateRequest::remote("fixture-node", "work", "New", "once"),
        )
        .await?;
    assert_eq!(project.name, "New");
    let result = normal
        .run(
            None,
            HarnessRunRequest::remote("fixture-node", "work", "hello").with_placement(
                ConversationPlacement::ExistingProject {
                    harness_id: "remote".into(),
                    project_id: "p1".into(),
                },
            ),
        )
        .await?;
    assert_eq!(result.final_response, "REMOTE_FIXTURE");
    assert_eq!(result.native_project_id.as_deref(), Some("p1"));
    assert_eq!(
        result.project_directory,
        RemoteWorkspace {
            node_id: "fixture-node".into(),
            workspace_id: "work".into()
        }
        .resource_path()
    );
    let result = normal
        .run(
            None,
            HarnessRunRequest::remote("fixture-node", "review", "review")
                .with_role(HarnessRole::Approval),
        )
        .await?;
    assert_eq!(result.visibility, ConversationVisibility::Hidden);
    assert_eq!(result.final_response, "APPROVAL_FIXTURE");
    assert!(
        normal
            .run(
                None,
                HarnessRunRequest::new(std::env::current_dir()?, "local path")
            )
            .await
            .is_err()
    );
    assert!(
        normal
            .run(
                None,
                HarnessRunRequest::remote("other-node", "work", "wrong owner")
            )
            .await
            .is_err()
    );
    assert!(
        normal
            .run(
                None,
                HarnessRunRequest::remote("fixture-node", "../work", "escape")
            )
            .await
            .is_err()
    );
    let calls = Arc::new(AtomicUsize::new(0));
    normal.run(None, tool_request(calls.clone())).await?;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let bad = registry(fixtures, "duplicate-tool").await?;
    let result = bad.run(None, tool_request(calls.clone())).await;
    assert!(matches!(
        result,
        Err(HarnessError::ConversationOutcomeUnknown(_))
    ));
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "duplicate callback must not dispatch twice"
    );
    let bad = registry(fixtures, "wrong-parent").await?;
    assert!(bad.run(None, tool_request(calls.clone())).await.is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let bad = registry(fixtures, "disconnect").await?;
    assert!(matches!(
        bad.run(
            None,
            HarnessRunRequest::remote("fixture-node", "work", "lost")
        )
        .await,
        Err(HarnessError::ConversationOutcomeUnknown(_))
    ));
    let bad = registry(fixtures, "wait-cancel").await?;
    assert!(matches!(
        bad.run(
            None,
            HarnessRunRequest::remote("fixture-node", "work", "cancel")
                .with_timeout(Duration::from_millis(40))
        )
        .await,
        Err(HarnessError::ConversationOutcomeUnknown(_))
    ));
    println!(
        "remote_harness: workspace ownership, text, projects, role isolation, host tools, duplicate prevention and Unknown passed"
    );
    Ok(())
}

struct WaitingHandler {
    started: tokio::sync::mpsc::UnboundedSender<()>,
    canceled: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}
impl HarnessToolHandler for WaitingHandler {
    fn call<'a>(&'a self, call: HarnessToolCall) -> HarnessToolFuture<'a> {
        Box::pin(async move {
            let release = self.release.notified();
            tokio::pin!(release);
            release.as_mut().enable();
            let _ = self.started.send(());
            tokio::select! {
                _ = &mut release => {},
                _ = call.cancellation.cancelled() => {
                    self.canceled.notify_one();
                    release.await;
                },
            }
            Ok(HarnessToolResult {
                content: "durable handler finished".into(),
                success: true,
            })
        })
    }
}
async fn capacity_and_dropped_caller_wait(fixtures: &FixtureServers) -> TestResult {
    let registry = Arc::new(registry(fixtures, "good").await?);
    let (started, mut starts) = tokio::sync::mpsc::unbounded_channel();
    let release = Arc::new(tokio::sync::Notify::new());
    let canceled = Arc::new(tokio::sync::Notify::new());
    let mut tasks = Vec::new();
    for _ in 0..4 {
        let registry = registry.clone();
        let handler = Arc::new(WaitingHandler {
            started: started.clone(),
            canceled: canceled.clone(),
            release: release.clone(),
        });
        let request = HarnessRunRequest::remote("fixture-node", "work", "bounded callback")
            .with_visibility(ConversationVisibility::Hidden)
            .with_tools(
                vec![HarnessTool {
                    name: "wait".into(),
                    description: "Wait for durable outcome".into(),
                    input_schema: json!({"type":"object"}),
                }],
                handler,
            );
        tasks.push(tokio::spawn(
            async move { registry.run(None, request).await },
        ));
    }
    for _ in 0..4 {
        tokio::time::timeout(Duration::from_secs(5), starts.recv())
            .await?
            .ok_or("missing callback")?;
    }
    let full = registry
        .run(
            None,
            HarnessRunRequest::remote("fixture-node", "work", "fifth"),
        )
        .await;
    assert!(
        matches!(full, Err(HarnessError::TurnFailed { .. })),
        "ordinary capacity must reject instead of queueing"
    );
    let approval = registry
        .run(
            None,
            HarnessRunRequest::remote("fixture-node", "review", "review while execution is full")
                .with_role(HarnessRole::Approval),
        )
        .await?;
    assert_eq!(approval.final_response, "APPROVAL_FIXTURE");
    let dropped = tasks.remove(0);
    dropped.abort();
    let _ = dropped.await;
    tokio::time::timeout(Duration::from_secs(5), canceled.notified()).await?;
    assert!(
        registry
            .run(
                None,
                HarnessRunRequest::remote("fixture-node", "work", "still full")
            )
            .await
            .is_err(),
        "dropped caller must retain capacity until trusted handler finishes"
    );
    release.notify_waiters();
    for task in tasks {
        tokio::time::timeout(Duration::from_secs(5), task).await???;
    }
    registry
        .run(
            None,
            HarnessRunRequest::remote("fixture-node", "work", "capacity recovered"),
        )
        .await?;
    Ok(())
}
