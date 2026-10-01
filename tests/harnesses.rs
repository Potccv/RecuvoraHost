//! Provider-neutral multi-Harness routing, project and role checks.
//! Default tests use only in-process fixtures and never contact an external Harness.

use recuvora_host::harnesses::{
    ClientProjectGrouping, ConversationPlacement, ConversationVisibility, HarnessAdapterFactory,
    HarnessCancellation, HarnessDefinition, HarnessError, HarnessProject,
    HarnessProjectCreateFuture, HarnessProjectCreateRequest, HarnessProjectListFuture,
    HarnessProjectListRequest, HarnessProvider, HarnessRegistryBuilder, HarnessRegistryConfig,
    HarnessRole, HarnessRunFuture, HarnessRunRequest, HarnessRunResult, HarnessTool,
    HarnessToolCall, HarnessToolFuture, HarnessToolHandler,
};
use std::error::Error;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

#[tokio::test]
async fn cancellation_wakes_all_waiters_and_remains_observable() -> TestResult {
    let cancellation = HarnessCancellation::default();
    let first = cancellation.clone();
    let second = cancellation.clone();
    require(!cancellation.is_cancelled(), "fresh token is cancelled")?;
    tokio::time::timeout(Duration::from_secs(2), async {
        tokio::join!(first.cancelled(), second.cancelled(), async {
            cancellation.cancel();
        });
        cancellation.cancelled().await;
    })
    .await?;
    require(
        first.is_cancelled() && second.is_cancelled(),
        "cancellation was not retained across clones",
    )
}

#[tokio::test]
async fn routes_multiple_instances_exactly_and_uses_the_configured_default() -> TestResult {
    let project = project_directory()?;
    let config = HarnessRegistryConfig {
        schema_version: 1,
        default_harness: Some("local".to_owned()),
        harnesses: vec![
            HarnessDefinition::new("local", "fixture", "fixture://local", vec![project.clone()]),
            HarnessDefinition::new(
                "lan",
                "fixture",
                "fixture://lan-node",
                vec![project.clone()],
            ),
        ],
    };
    let registry = registry(config, Arc::new(FixtureFactory))?;

    let local = registry
        .run(None, HarnessRunRequest::new(&project, "local turn"))
        .await?;
    require(local.harness_id == "local", "default Harness was not used")?;
    require(
        local.address == "fixture://local",
        "default Harness address changed",
    )?;

    let lan = registry
        .run(Some("lan"), HarnessRunRequest::new(&project, "LAN turn"))
        .await?;
    require(lan.harness_id == "lan", "explicit Harness was not used")?;
    require(
        lan.address == "fixture://lan-node",
        "explicit Harness address changed",
    )?;
    require(
        lan.final_response == "fixture response from lan",
        "provider result was not associated with the selected Harness",
    )?;
    Ok(())
}

#[tokio::test]
async fn run_defaults_to_client_visibility_without_project_assignment() -> TestResult {
    let project = project_directory()?;
    let registry = registry(
        single_fixture_config("local", &project),
        Arc::new(FixtureFactory),
    )?;

    let result = registry
        .run(None, HarnessRunRequest::new(&project, "default placement"))
        .await?;

    require(
        result.visibility == ConversationVisibility::Client,
        "new requests no longer default to client-visible conversations",
    )?;
    require(
        result.native_project_id.is_none()
            && result.client_project_grouping == ClientProjectGrouping::Unverified,
        "new requests unexpectedly default to a native project",
    )?;
    Ok(())
}

#[tokio::test]
async fn hidden_conversations_run_without_a_native_project_and_reject_placement() -> TestResult {
    let project = project_directory()?;
    let registry = registry(
        single_fixture_config("local", &project),
        Arc::new(FixtureFactory),
    )?;

    let hidden = registry
        .run(
            None,
            HarnessRunRequest::new(&project, "hidden without a native project")
                .with_visibility(ConversationVisibility::Hidden),
        )
        .await?;
    require(
        hidden.visibility == ConversationVisibility::Hidden
            && hidden.native_project_id.is_none()
            && hidden.client_project_grouping == ClientProjectGrouping::NotApplicable,
        "hidden conversation without a native project changed its effective settings",
    )?;

    let error = registry
        .run(
            None,
            HarnessRunRequest::new(&project, "hidden project must not dispatch")
                .with_visibility(ConversationVisibility::Hidden)
                .with_placement(ConversationPlacement::ExistingProject {
                    harness_id: "local".to_owned(),
                    project_id: "project-local".to_owned(),
                }),
        )
        .await
        .expect_err("hidden conversation requested a native project");
    require(
        matches!(error, HarnessError::InvalidRequest(message)
            if message.contains("hidden conversation")),
        "hidden project placement returned the wrong error",
    )?;
    Ok(())
}

#[tokio::test]
async fn discovers_creates_and_selects_projects_for_one_harness() -> TestResult {
    let project = project_directory()?;
    let registry = registry(
        single_fixture_config("local", &project),
        Arc::new(FixtureFactory),
    )?;

    let projects = registry
        .list_projects(None, HarnessProjectListRequest::new(&project))
        .await?;
    require(projects.len() == 1, "fixture project discovery changed")?;
    let discovered = &projects[0];
    require(
        discovered.harness_id == "local"
            && discovered.id == "project-local"
            && discovered.roots == vec![project.clone()],
        "discovered project lost its Harness scope or root",
    )?;

    let created = registry
        .create_project(
            None,
            HarnessProjectCreateRequest::new("New Fixture Project", &project, "fixture-create-key"),
        )
        .await?;
    require(
        created.harness_id == "local"
            && created.id == "created-local"
            && created.name == "New Fixture Project"
            && created.roots == vec![project.clone()],
        "created project did not preserve its requested Harness, name and root",
    )?;

    let result = registry
        .run(
            None,
            HarnessRunRequest::new(&project, "selected project").with_placement(
                ConversationPlacement::ExistingProject {
                    harness_id: created.harness_id.clone(),
                    project_id: created.id.clone(),
                },
            ),
        )
        .await?;
    require(
        result.visibility == ConversationVisibility::Client
            && result.native_project_id.as_deref() == Some("created-local")
            && result.client_project_grouping == ClientProjectGrouping::Unverified,
        "selected project was not retained on the provider result",
    )?;
    Ok(())
}

#[tokio::test]
async fn project_ids_cannot_cross_harness_instances() -> TestResult {
    let project = project_directory()?;
    let config = HarnessRegistryConfig {
        schema_version: 1,
        default_harness: Some("local".to_owned()),
        harnesses: vec![
            HarnessDefinition::new("local", "fixture", "fixture://local", vec![project.clone()]),
            HarnessDefinition::new("lan", "fixture", "fixture://lan", vec![project.clone()]),
        ],
    };
    let registry = registry(config, Arc::new(FixtureFactory))?;
    let lan_projects = registry
        .list_projects(Some("lan"), HarnessProjectListRequest::new(&project))
        .await?;
    let lan_project = lan_projects
        .first()
        .ok_or_else(|| io::Error::other("LAN fixture returned no project"))?;

    let error = registry
        .run(
            Some("local"),
            HarnessRunRequest::new(&project, "must not cross Harnesses").with_placement(
                ConversationPlacement::ExistingProject {
                    harness_id: lan_project.harness_id.clone(),
                    project_id: lan_project.id.clone(),
                },
            ),
        )
        .await
        .expect_err("project from another Harness unexpectedly ran");
    require(
        matches!(error, HarnessError::InvalidRequest(message)
            if message.contains("belongs to Harness lan")),
        "cross-Harness project selection returned the wrong error",
    )?;
    Ok(())
}

#[tokio::test]
async fn providers_default_to_structured_project_capability_errors() -> TestResult {
    let project = project_directory()?;
    let registry = registry(
        single_unsupported_project_config(&project),
        Arc::new(UnsupportedProjectFactory),
    )?;

    let list_error = registry
        .list_projects(None, HarnessProjectListRequest::new(&project))
        .await
        .expect_err("provider without discovery support returned projects");
    require(
        matches!(list_error, HarnessError::ProjectDiscoveryUnsupported { harness }
            if harness == "unsupported"),
        "unsupported project discovery returned the wrong error",
    )?;

    let create_error = registry
        .create_project(
            None,
            HarnessProjectCreateRequest::new("Unsupported", &project, "unsupported-key"),
        )
        .await
        .expect_err("provider without creation support created a project");
    require(
        matches!(create_error, HarnessError::ProjectCreationUnsupported { harness }
            if harness == "unsupported"),
        "unsupported project creation returned the wrong error",
    )?;
    Ok(())
}

#[tokio::test]
async fn registry_rejects_empty_or_outside_project_roots() -> TestResult {
    let project = project_directory()?;
    let outside = project
        .parent()
        .ok_or_else(|| io::Error::other("test project has no parent"))?
        .to_path_buf();

    for roots in [
        Vec::new(),
        vec![outside],
        vec![PathBuf::from(
            r"\\recuvora-invalid-host\untrusted-share\project",
        )],
    ] {
        let config = HarnessRegistryConfig {
            schema_version: 1,
            default_harness: Some("malformed".to_owned()),
            harnesses: vec![HarnessDefinition::new(
                "malformed",
                "malformed-projects",
                "fixture://malformed",
                vec![project.clone()],
            )],
        };
        let registry = registry(config, Arc::new(MalformedProjectFactory { roots }))?;
        let error = registry
            .list_projects(None, HarnessProjectListRequest::new(&project))
            .await
            .expect_err("malformed project roots were accepted");
        require(
            matches!(error, HarnessError::ProtocolViolation { harness, .. }
                if harness == "malformed"),
            "malformed project roots returned the wrong error",
        )?;
    }
    Ok(())
}

#[tokio::test]
async fn registry_rejects_wrong_project_or_visibility_from_a_provider() -> TestResult {
    let project = project_directory()?;
    let visibility_registry = registry(
        single_fixture_config("local", &project),
        Arc::new(ForcedResultFactory {
            visibility: ConversationVisibility::Hidden,
            project_id: None,
        }),
    )?;
    let visibility_error = visibility_registry
        .run(None, HarnessRunRequest::new(&project, "wrong visibility"))
        .await
        .expect_err("provider changed visibility without a protocol error");
    require(
        matches!(visibility_error, HarnessError::ConversationOutcomeUnknown(details)
            if details.harness == "local"),
        "wrong provider visibility returned the wrong error",
    )?;

    let project_registry = registry(
        single_fixture_config("local", &project),
        Arc::new(ForcedResultFactory {
            visibility: ConversationVisibility::Client,
            project_id: Some("wrong-project".to_owned()),
        }),
    )?;
    let project_error = project_registry
        .run(None, HarnessRunRequest::new(&project, "wrong project"))
        .await
        .expect_err("provider added a project without a protocol error");
    require(
        matches!(project_error, HarnessError::ConversationOutcomeUnknown(details)
            if details.harness == "local"),
        "wrong provider project returned the wrong error",
    )?;
    Ok(())
}

#[tokio::test]
async fn unknown_or_disabled_instances_do_not_fall_back() -> TestResult {
    let project = project_directory()?;
    let mut disabled = HarnessDefinition::new(
        "disabled",
        "fixture",
        "fixture://disabled",
        vec![project.clone()],
    );
    disabled.enabled = false;
    let config = HarnessRegistryConfig {
        schema_version: 1,
        default_harness: Some("healthy".to_owned()),
        harnesses: vec![
            HarnessDefinition::new(
                "healthy",
                "fixture",
                "fixture://healthy",
                vec![project.clone()],
            ),
            disabled,
        ],
    };
    let registry = registry(config, Arc::new(FixtureFactory))?;

    let unknown = registry
        .run(
            Some("missing"),
            HarnessRunRequest::new(&project, "must not fall back"),
        )
        .await
        .expect_err("unknown Harness unexpectedly ran");
    require(
        matches!(unknown, HarnessError::UnknownHarness(id) if id == "missing"),
        "unknown Harness returned the wrong error",
    )?;

    let disabled = registry
        .run(
            Some("disabled"),
            HarnessRunRequest::new(&project, "must not fall back"),
        )
        .await
        .expect_err("disabled Harness unexpectedly ran");
    require(
        matches!(disabled, HarnessError::HarnessDisabled(id) if id == "disabled"),
        "disabled Harness returned the wrong error",
    )?;
    Ok(())
}

#[test]
fn rejects_duplicate_ids_and_unregistered_adapters() -> TestResult {
    let project = project_directory()?;
    let duplicate = HarnessRegistryConfig {
        schema_version: 1,
        default_harness: None,
        harnesses: vec![
            HarnessDefinition::new("same", "fixture", "fixture://one", vec![project.clone()]),
            HarnessDefinition::new("same", "fixture", "fixture://two", vec![project.clone()]),
        ],
    };
    let error = registry(duplicate, Arc::new(FixtureFactory))
        .err()
        .ok_or_else(|| io::Error::other("duplicate Harness ids were accepted"))?;
    require(
        matches!(error, HarnessError::DuplicateHarness(id) if id == "same"),
        "duplicate Harness ids returned the wrong error",
    )?;

    let unsupported = HarnessRegistryConfig {
        schema_version: 1,
        default_harness: None,
        harnesses: vec![HarnessDefinition::new(
            "other",
            "not-installed",
            "other://node",
            vec![project],
        )],
    };
    let error = registry(unsupported, Arc::new(FixtureFactory))
        .err()
        .ok_or_else(|| io::Error::other("unregistered adapter was accepted"))?;
    require(
        matches!(error, HarnessError::UnsupportedAdapter(id) if id == "not-installed"),
        "unregistered adapter returned the wrong error",
    )?;

    let mut builder = HarnessRegistryBuilder::new();
    builder.register(Arc::new(FixtureFactory))?;
    let error = builder
        .register(Arc::new(FixtureFactory))
        .expect_err("duplicate adapter registration was accepted");
    require(
        matches!(error, HarnessError::DuplicateAdapter(id) if id == "fixture"),
        "duplicate adapter registration returned the wrong error",
    )?;
    Ok(())
}

#[test]
fn loads_the_remote_example_profile_without_resolving_node_workspace_ids() -> TestResult {
    let profile = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("profiles")
        .join("harnesses.remote.example.json");
    let config = HarnessRegistryConfig::load(profile)?;
    require(
        config.default_harness.as_deref() == Some("harness-external"),
        "remote example profile default changed",
    )?;
    require(
        config.harnesses.len() == 1,
        "remote example profile shape changed",
    )?;
    let definition = &config.harnesses[0];
    require(
        definition.id == "harness-external"
            && definition.adapter == "remote-node"
            && definition.address == "node://harness-node"
            && definition.workspace_roots == vec![PathBuf::from("work"), PathBuf::from("review")],
        "remote example profile no longer preserves node workspace identifiers",
    )?;
    Ok(())
}

#[tokio::test]
async fn registry_rejects_projects_outside_the_selected_instance_roots() -> TestResult {
    let project = project_directory()?;
    let docs = std::fs::canonicalize(project.join("docs"))?;
    let config = HarnessRegistryConfig {
        schema_version: 1,
        default_harness: Some("docs-only".to_owned()),
        harnesses: vec![HarnessDefinition::new(
            "docs-only",
            "fixture",
            "fixture://docs",
            vec![docs],
        )],
    };
    let registry = registry(config, Arc::new(FixtureFactory))?;
    let error = registry
        .run(None, HarnessRunRequest::new(&project, "outside root"))
        .await
        .expect_err("project outside the configured root unexpectedly ran");
    require(
        matches!(error, HarnessError::WorkspaceDenied(path) if path == project),
        "workspace denial returned the wrong error",
    )?;
    Ok(())
}

fn fixture_host_tool() -> HarnessTool {
    HarnessTool {
        name: "recuvora_test".to_owned(),
        description: "Exercise a controlled host callback".to_owned(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {"value": {"type": "string"}},
            "required": ["value"],
            "additionalProperties": false
        }),
    }
}

struct NeverCalledToolHandler;

impl HarnessToolHandler for NeverCalledToolHandler {
    fn call<'a>(&'a self, _call: HarnessToolCall) -> HarnessToolFuture<'a> {
        Box::pin(async {
            Err(HarnessError::InvalidRequest(
                "invalid request reached the tool handler".to_owned(),
            ))
        })
    }
}

#[tokio::test]
async fn approval_role_enforces_hidden_tool_free_conversations() -> TestResult {
    let project = project_directory()?;
    let registry = registry(
        single_fixture_config("local", &project),
        Arc::new(FixtureFactory),
    )?;
    let result = registry
        .run(
            None,
            HarnessRunRequest::new(&project, "approval").with_role(HarnessRole::Approval),
        )
        .await?;
    require(
        result.visibility == ConversationVisibility::Hidden,
        "approval role was visible",
    )?;
    for request in [
        HarnessRunRequest::new(&project, "approval")
            .with_role(HarnessRole::Approval)
            .with_visibility(ConversationVisibility::Client),
        HarnessRunRequest::new(&project, "approval")
            .with_role(HarnessRole::Approval)
            .with_tools(vec![fixture_host_tool()], Arc::new(NeverCalledToolHandler)),
        HarnessRunRequest::new(&project, "execution").with_tools(
            vec![fixture_host_tool(), fixture_host_tool()],
            Arc::new(NeverCalledToolHandler),
        ),
    ] {
        require(
            matches!(
                registry.run(None, request).await,
                Err(HarnessError::InvalidRequest(_))
            ),
            "invalid role or tools reached provider",
        )?;
    }
    Ok(())
}

fn registry(
    config: HarnessRegistryConfig,
    factory: Arc<dyn HarnessAdapterFactory>,
) -> Result<recuvora_host::harnesses::HarnessRegistry, HarnessError> {
    let mut builder = HarnessRegistryBuilder::new();
    builder.register(factory)?;
    builder.build(config)
}

fn single_fixture_config(id: &str, project: &Path) -> HarnessRegistryConfig {
    HarnessRegistryConfig {
        schema_version: 1,
        default_harness: Some(id.to_owned()),
        harnesses: vec![HarnessDefinition::new(
            id,
            "fixture",
            format!("fixture://{id}"),
            vec![project.to_path_buf()],
        )],
    }
}

fn single_unsupported_project_config(project: &Path) -> HarnessRegistryConfig {
    HarnessRegistryConfig {
        schema_version: 1,
        default_harness: Some("unsupported".to_owned()),
        harnesses: vec![HarnessDefinition::new(
            "unsupported",
            "unsupported-projects",
            "fixture://unsupported",
            vec![project.to_path_buf()],
        )],
    }
}

struct FixtureFactory;

impl HarnessAdapterFactory for FixtureFactory {
    fn adapter_id(&self) -> &str {
        "fixture"
    }

    fn build(
        &self,
        definition: HarnessDefinition,
    ) -> Result<Arc<dyn HarnessProvider>, HarnessError> {
        Ok(Arc::new(FixtureProvider { definition }))
    }
}

struct FixtureProvider {
    definition: HarnessDefinition,
}

impl HarnessProvider for FixtureProvider {
    fn definition(&self) -> &HarnessDefinition {
        &self.definition
    }

    fn run<'a>(&'a self, request: HarnessRunRequest) -> HarnessRunFuture<'a> {
        Box::pin(async move { fixture_run_result(&self.definition, request, None, None) })
    }

    fn list_projects<'a>(
        &'a self,
        request: HarnessProjectListRequest,
    ) -> HarnessProjectListFuture<'a> {
        Box::pin(async move {
            Ok(vec![HarnessProject {
                harness_id: self.definition.id.clone(),
                id: format!("project-{}", self.definition.id),
                name: format!("Fixture project for {}", self.definition.id),
                roots: vec![request.context_directory().to_path_buf()],
            }])
        })
    }

    fn create_project<'a>(
        &'a self,
        request: HarnessProjectCreateRequest,
    ) -> HarnessProjectCreateFuture<'a> {
        Box::pin(async move {
            Ok(HarnessProject {
                harness_id: self.definition.id.clone(),
                id: format!("created-{}", self.definition.id),
                name: request.name().to_owned(),
                roots: vec![request.root_directory().to_path_buf()],
            })
        })
    }
}

struct UnsupportedProjectFactory;

impl HarnessAdapterFactory for UnsupportedProjectFactory {
    fn adapter_id(&self) -> &str {
        "unsupported-projects"
    }

    fn build(
        &self,
        definition: HarnessDefinition,
    ) -> Result<Arc<dyn HarnessProvider>, HarnessError> {
        Ok(Arc::new(UnsupportedProjectProvider { definition }))
    }
}

struct UnsupportedProjectProvider {
    definition: HarnessDefinition,
}

impl HarnessProvider for UnsupportedProjectProvider {
    fn definition(&self) -> &HarnessDefinition {
        &self.definition
    }

    fn run<'a>(&'a self, request: HarnessRunRequest) -> HarnessRunFuture<'a> {
        Box::pin(async move { fixture_run_result(&self.definition, request, None, None) })
    }
}

struct MalformedProjectFactory {
    roots: Vec<PathBuf>,
}

impl HarnessAdapterFactory for MalformedProjectFactory {
    fn adapter_id(&self) -> &str {
        "malformed-projects"
    }

    fn build(
        &self,
        definition: HarnessDefinition,
    ) -> Result<Arc<dyn HarnessProvider>, HarnessError> {
        Ok(Arc::new(MalformedProjectProvider {
            definition,
            roots: self.roots.clone(),
        }))
    }
}

struct MalformedProjectProvider {
    definition: HarnessDefinition,
    roots: Vec<PathBuf>,
}

impl HarnessProvider for MalformedProjectProvider {
    fn definition(&self) -> &HarnessDefinition {
        &self.definition
    }

    fn run<'a>(&'a self, request: HarnessRunRequest) -> HarnessRunFuture<'a> {
        Box::pin(async move { fixture_run_result(&self.definition, request, None, None) })
    }

    fn list_projects<'a>(
        &'a self,
        _request: HarnessProjectListRequest,
    ) -> HarnessProjectListFuture<'a> {
        Box::pin(async move {
            Ok(vec![HarnessProject {
                harness_id: self.definition.id.clone(),
                id: "malformed-project".to_owned(),
                name: "Malformed project".to_owned(),
                roots: self.roots.clone(),
            }])
        })
    }
}

struct ForcedResultFactory {
    visibility: ConversationVisibility,
    project_id: Option<String>,
}

impl HarnessAdapterFactory for ForcedResultFactory {
    fn adapter_id(&self) -> &str {
        "fixture"
    }

    fn build(
        &self,
        definition: HarnessDefinition,
    ) -> Result<Arc<dyn HarnessProvider>, HarnessError> {
        Ok(Arc::new(ForcedResultProvider {
            definition,
            visibility: self.visibility,
            project_id: self.project_id.clone(),
        }))
    }
}

struct ForcedResultProvider {
    definition: HarnessDefinition,
    visibility: ConversationVisibility,
    project_id: Option<String>,
}

impl HarnessProvider for ForcedResultProvider {
    fn definition(&self) -> &HarnessDefinition {
        &self.definition
    }

    fn run<'a>(&'a self, request: HarnessRunRequest) -> HarnessRunFuture<'a> {
        Box::pin(async move {
            fixture_run_result(
                &self.definition,
                request,
                Some(self.visibility),
                Some(self.project_id.clone()),
            )
        })
    }
}

fn fixture_run_result(
    definition: &HarnessDefinition,
    request: HarnessRunRequest,
    forced_visibility: Option<ConversationVisibility>,
    forced_project_id: Option<Option<String>>,
) -> Result<HarnessRunResult, HarnessError> {
    let project_directory =
        std::fs::canonicalize(request.project_directory()).map_err(|error| {
            HarnessError::InvalidRequest(format!("fixture cannot resolve cwd: {error}"))
        })?;
    let visibility = forced_visibility.unwrap_or(request.visibility());
    let requested_project_id = match request.placement() {
        ConversationPlacement::NoNativeProject => None,
        ConversationPlacement::ExistingProject { project_id, .. } => Some(project_id.clone()),
    };
    Ok(HarnessRunResult {
        harness_id: definition.id.clone(),
        adapter: definition.adapter.clone(),
        address: definition.address.clone(),
        thread_id: format!("thread-{}", definition.id),
        session_id: format!("session-{}", definition.id),
        project_directory,
        visibility,
        native_project_id: forced_project_id.unwrap_or(requested_project_id),
        client_project_grouping: if visibility == ConversationVisibility::Hidden {
            ClientProjectGrouping::NotApplicable
        } else {
            ClientProjectGrouping::Unverified
        },
        final_response: format!("fixture response from {}", definition.id),
    })
}

fn project_directory() -> TestResult<PathBuf> {
    Ok(std::fs::canonicalize(env!("CARGO_MANIFEST_DIR"))?)
}

fn require(condition: bool, message: &str) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(io::Error::other(message).into())
    }
}
