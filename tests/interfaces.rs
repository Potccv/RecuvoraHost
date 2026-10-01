//! Consumer tests use only in-memory providers and isolated prompt files.

use super::*;
use crate::harnesses::{
    ClientProjectGrouping, ConversationUncertainty, HarnessAdapterFactory, HarnessDefinition,
    HarnessProject, HarnessProjectCreateFuture, HarnessProjectListFuture, HarnessProvider,
    HarnessRegistryBuilder, HarnessRegistryConfig, HarnessRunFuture, HarnessRunResult,
    ProjectCreationUncertainty,
};
use crate::presentation::{error_json, repair_result_json};
use std::error::Error;
use std::fs;
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

fn parse(values: &[&str]) -> Result<Option<HarnessArguments>, String> {
    parse_harness(values.iter().map(OsString::from))
}

fn parsed(values: &[&str]) -> TestResult<HarnessArguments> {
    parse(values)
        .map_err(io::Error::other)?
        .ok_or_else(|| io::Error::other("unexpected help").into())
}

#[test]
fn requires_explicit_config_visibility_and_project_decision() -> TestResult {
    assert!(parse(&["list"]).is_err());
    let base = [
        "run",
        "--config",
        "config.json",
        "--cwd",
        ".",
        "--prompt",
        "hello",
    ];
    assert!(parse(&base).is_err());
    let mut args = base.to_vec();
    args.extend(["--visibility", "client"]);
    assert!(parse(&args).is_err());
    args.push("--no-project");
    assert!(matches!(
        parsed(&args)?.command,
        HarnessCommand::Run {
            project: None,
            visibility: ConversationVisibility::Client,
            ..
        }
    ));
    args.extend(["--project", "p"]);
    assert!(parse(&args).is_err());
    Ok(())
}

#[test]
fn rejects_duplicate_unknown_inapplicable_and_empty_flags() {
    for invalid in [
        vec!["list", "--config", "a", "--config", "b"],
        vec!["list", "--config", "a", "--harness", "x"],
        vec!["list", "--config", "a", "--typo", "x"],
        vec!["list", "--config", ""],
        vec!["projects", "--config", "a", "--cwd", ".", "--name", "p"],
        vec![
            "projects",
            "--config",
            "a",
            "--cwd",
            ".",
            "--harness",
            "../../other",
        ],
        vec!["projects", "--config", "a", "--cwd", ".", "--harness", "\n"],
    ] {
        assert!(parse(&invalid).is_err(), "accepted {invalid:?}");
    }
}

#[test]
fn timeout_is_a_bounded_positive_integer() -> TestResult {
    for timeout in [
        "0",
        "1801",
        "99999999999999999",
        "-1",
        "1.5",
        "NaN",
        "+1",
        " 1",
        "1s",
    ] {
        assert!(
            parse(&[
                "projects",
                "--config",
                "a",
                "--cwd",
                ".",
                "--timeout-secs",
                timeout
            ])
            .is_err()
        );
    }
    let args = parsed(&[
        "projects",
        "--config",
        "a",
        "--cwd",
        ".",
        "--timeout-secs",
        "1800",
    ])?;
    assert!(
        matches!(args.command, HarnessCommand::Projects { timeout, .. } if timeout == Duration::from_secs(1800))
    );
    Ok(())
}

#[test]
fn hidden_project_and_conflicting_prompt_sources_are_rejected() {
    assert!(
        parse(&[
            "run",
            "--config",
            "a",
            "--cwd",
            ".",
            "--prompt",
            "hello",
            "--visibility",
            "hidden",
            "--project",
            "p"
        ])
        .is_err()
    );
    assert!(
        parse(&[
            "run",
            "--config",
            "a",
            "--cwd",
            ".",
            "--prompt",
            "hello",
            "--prompt-file",
            "unused",
            "--visibility",
            "hidden",
            "--no-project"
        ])
        .is_err()
    );
    assert!(
        parse(&[
            "run",
            "--config",
            "a",
            "--cwd",
            ".",
            "--prompt",
            " \n",
            "--visibility",
            "hidden",
            "--no-project"
        ])
        .is_err()
    );
}

#[test]
fn prompt_file_is_bounded_utf8_and_reads_exact_text() -> TestResult {
    let fixture = PromptFixture::new()?;
    fs::write(&fixture.path, "中文\nquoted \"text\"\n")?;
    let parse_file = || {
        parse_harness([
            OsString::from("run"),
            OsString::from("--config"),
            OsString::from("a"),
            OsString::from("--cwd"),
            OsString::from("."),
            OsString::from("--prompt-file"),
            fixture.path.clone().into_os_string(),
            OsString::from("--visibility"),
            OsString::from("hidden"),
            OsString::from("--no-project"),
        ])
    };
    let args = parse_file()
        .map_err(io::Error::other)?
        .ok_or_else(|| io::Error::other("unexpected help"))?;
    assert!(
        matches!(args.command, HarnessCommand::Run { prompt, .. } if prompt == "中文\nquoted \"text\"\n")
    );
    fs::write(&fixture.path, [0xff, 0xfe])?;
    assert!(parse_file().is_err());
    fs::write(&fixture.path, vec![b'a'; MAX_PROMPT_BYTES + 1])?;
    assert!(parse_file().is_err());
    fixture.cleanup()?;
    Ok(())
}

#[cfg(windows)]
#[test]
fn paths_keep_os_encoding_instead_of_lossy_argument_conversion() -> TestResult {
    use std::os::windows::ffi::OsStringExt;
    let path = OsString::from_wide(&[b'a' as u16, 0xd800, b'b' as u16]);
    let args = parse_harness([
        OsString::from("list"),
        OsString::from("--config"),
        path.clone(),
    ])
    .map_err(io::Error::other)?
    .ok_or_else(|| io::Error::other("unexpected help"))?;
    assert_eq!(args.config.as_os_str(), path.as_os_str());
    Ok(())
}

#[tokio::test]
async fn explicit_selection_routes_discovery_and_run_to_same_harness() -> TestResult {
    let (registry, calls) = fixture_registry(Behavior::Success, true)?;
    let args = run_args(
        Some("alternate"),
        Some("project-alternate"),
        ConversationVisibility::Client,
    )?;
    let output = execute(&registry, &args, HarnessCancellation::new()).await?;
    assert_eq!(output["harness_id"], "alternate");
    assert_eq!(output["thread_id"], "thread-alternate");
    assert_eq!(output["session_id"], "session-alternate");
    assert_eq!(output["native_project_id"], "project-alternate");
    assert_eq!(output["client_project_grouping"]["status"], "unverified");
    assert_eq!(
        *calls
            .lock()
            .map_err(|_| io::Error::other("calls poisoned"))?,
        ["alternate:projects", "alternate:run"]
    );
    let encoded = serde_json::to_string(&output)?;
    assert!(!encoded.contains('\n'));
    assert!(encoded.contains("\\n"));
    Ok(())
}

#[tokio::test]
async fn default_hidden_run_skips_project_discovery_and_preserves_visibility() -> TestResult {
    let (registry, calls) = fixture_registry(Behavior::Success, true)?;
    let args = run_args(None, None, ConversationVisibility::Hidden)?;
    let output = execute(&registry, &args, HarnessCancellation::new()).await?;
    assert_eq!(output["harness_id"], "default");
    assert_eq!(output["visibility"], "hidden");
    assert!(output["native_project_id"].is_null());
    assert_eq!(
        output["client_project_grouping"]["status"],
        "not_applicable"
    );
    assert_eq!(
        *calls
            .lock()
            .map_err(|_| io::Error::other("calls poisoned"))?,
        ["default:run"]
    );
    Ok(())
}

#[tokio::test]
async fn discovery_errors_and_missing_projects_never_fall_back_or_run() -> TestResult {
    for behavior in [Behavior::DiscoveryFailure, Behavior::Success] {
        let (registry, calls) = fixture_registry(behavior, true)?;
        let args = run_args(
            Some("alternate"),
            Some("project-default"),
            ConversationVisibility::Client,
        )?;
        let error = execute(&registry, &args, HarnessCancellation::new())
            .await
            .expect_err("unexpected run");
        assert!(matches!(
            error,
            HarnessError::Unavailable { .. } | HarnessError::UnknownProject { .. }
        ));
        assert_eq!(
            *calls
                .lock()
                .map_err(|_| io::Error::other("calls poisoned"))?,
            ["alternate:projects"]
        );
    }
    Ok(())
}

#[tokio::test]
async fn missing_default_unknown_and_disabled_selection_dispatch_nothing() -> TestResult {
    let (registry, calls) = fixture_registry(Behavior::Success, false)?;
    for harness in [None, Some("missing"), Some("disabled")] {
        let args = run_args(harness, None, ConversationVisibility::Client)?;
        assert!(matches!(
            execute(&registry, &args, HarnessCancellation::new()).await,
            Err(HarnessError::NoDefaultHarness
                | HarnessError::UnknownHarness(_)
                | HarnessError::HarnessDisabled(_))
        ));
    }
    assert!(
        calls
            .lock()
            .map_err(|_| io::Error::other("calls poisoned"))?
            .is_empty()
    );
    Ok(())
}

#[tokio::test]
async fn canceled_command_dispatches_nothing_and_active_token_reaches_provider() -> TestResult {
    let (registry, calls) = fixture_registry(Behavior::Success, true)?;
    let args = run_args(None, None, ConversationVisibility::Client)?;
    let cancellation = HarnessCancellation::new();
    cancellation.cancel();
    let error = execute(&registry, &args, cancellation)
        .await
        .expect_err("canceled command ran");
    assert_eq!(error_json(&error)["status"], "canceled");
    assert!(
        calls
            .lock()
            .map_err(|_| io::Error::other("calls poisoned"))?
            .is_empty()
    );
    let (registry, _) = fixture_registry(Behavior::CancelRun, true)?;
    let cancellation = HarnessCancellation::new();
    let error = execute(&registry, &args, cancellation.clone())
        .await
        .expect_err("cancellation ignored");
    assert!(cancellation.is_cancelled());
    assert!(matches!(error, HarnessError::Interrupted { .. }));
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn discovery_consumes_the_same_deadline_as_the_following_run() -> TestResult {
    let (registry, calls) = fixture_registry(Behavior::DiscoveryConsumesBudget, true)?;
    let mut args = run_args(
        None,
        Some("project-default"),
        ConversationVisibility::Client,
    )?;
    if let HarnessCommand::Run { timeout, .. } = &mut args.command {
        *timeout = Duration::from_secs(1);
    }
    let error = execute(&registry, &args, HarnessCancellation::new())
        .await
        .expect_err("deadline was reset");
    assert!(matches!(error, HarnessError::DeadlineExceeded { .. }));
    assert_eq!(
        *calls
            .lock()
            .map_err(|_| io::Error::other("calls poisoned"))?,
        ["default:projects"]
    );
    Ok(())
}

#[tokio::test]
async fn list_is_configuration_only_and_create_is_one_explicit_operation() -> TestResult {
    let (registry, calls) = fixture_registry(Behavior::Success, true)?;
    let args = HarnessArguments {
        config: PathBuf::from("unused.json"),
        extensions: None,
        workspace: None,
        harness: None,
        command: HarnessCommand::List,
    };
    let output = execute(&registry, &args, HarnessCancellation::new()).await?;
    assert_eq!(output["default_harness"], "default");
    assert!(
        output["harnesses"]
            .as_array()
            .ok_or_else(|| io::Error::other("missing harnesses"))?
            .iter()
            .all(|entry| entry["authentication"] == "not_checked")
    );
    assert!(
        calls
            .lock()
            .map_err(|_| io::Error::other("calls poisoned"))?
            .is_empty()
    );
    let args = HarnessArguments {
        command: HarnessCommand::CreateProject {
            cwd: existing_directory()?,
            name: "New Project".to_owned(),
            idempotency_key: "stable-key".to_owned(),
            timeout: Duration::from_secs(1),
        },
        ..args
    };
    let output = execute(&registry, &args, HarnessCancellation::new()).await?;
    assert_eq!(output["project"]["name"], "New Project");
    assert_eq!(output["idempotency_key"], "stable-key");
    assert_eq!(
        *calls
            .lock()
            .map_err(|_| io::Error::other("calls poisoned"))?,
        ["default:create"]
    );
    Ok(())
}

#[test]
fn unknown_outputs_preserve_all_known_correlations_and_disable_retry() -> TestResult {
    let conversation = error_json(&HarnessError::ConversationOutcomeUnknown(Box::new(
        ConversationUncertainty {
            harness: "h".to_owned(),
            thread_id: Some("thread".to_owned()),
            project_directory: PathBuf::from("cwd"),
            visibility: ConversationVisibility::Client,
            native_project_id: Some("project".to_owned()),
            message: "lost\nreply\"".to_owned(),
        },
    )));
    assert_eq!(conversation["status"], "unknown");
    assert_eq!(conversation["code"], "conversation_outcome_unknown");
    assert_eq!(conversation["thread_id"], "thread");
    assert_eq!(conversation["native_project_id"], "project");
    assert_eq!(conversation["auto_retry"], false);
    assert_eq!(
        conversation["client_project_grouping"]["status"],
        "unverified"
    );
    let project = error_json(&HarnessError::ProjectCreationOutcomeUnknown(Box::new(
        ProjectCreationUncertainty {
            harness: "h".to_owned(),
            name: "name".to_owned(),
            root_directory: PathBuf::from("cwd"),
            idempotency_key: "key".to_owned(),
            project_id: Some("known-id".to_owned()),
            message: "lost reply".to_owned(),
        },
    )));
    assert_eq!(project["status"], "unknown");
    assert_eq!(project["idempotency_key"], "key");
    assert_eq!(project["native_project_id"], "known-id");
    assert_eq!(project["auto_retry"], false);
    let encoded = serde_json::to_string(&conversation)?;
    assert!(!encoded.contains('\n'));
    assert_eq!(serde_json::from_str::<Value>(&encoded)?, conversation);
    Ok(())
}

#[test]
fn repair_presentation_retains_unknown_identity_and_never_claims_business_health() {
    use crate::repair::{RepairResult, RepairStatus};
    let result = RepairResult {
        status: RepairStatus::Unknown,
        task_id: "repair-task".into(),
        final_response: None,
        harness_error: Some(HarnessError::ConversationOutcomeUnknown(Box::new(
            ConversationUncertainty {
                harness: "remote".into(),
                thread_id: Some("existing-thread".into()),
                project_directory: PathBuf::from("node://node/workspace"),
                visibility: ConversationVisibility::Hidden,
                native_project_id: None,
                message: "reply lost".into(),
            },
        ))),
        operations: Vec::new(),
        execution_context: None,
    };
    let wire = repair_result_json(&result);
    assert_eq!(wire["status"], "unknown");
    assert_eq!(wire["task_id"], "repair-task");
    assert_eq!(wire["harness_error"]["status"], "unknown");
    assert_eq!(wire["harness_error"]["thread_id"], "existing-thread");
    assert_eq!(wire["harness_error"]["auto_retry"], false);
    assert_eq!(wire["auto_retry"], false);
    assert_eq!(wire["business_verified"], false);
}

#[derive(Clone, Copy)]
enum Behavior {
    Success,
    DiscoveryFailure,
    DiscoveryConsumesBudget,
    CancelRun,
}

struct FixtureFactory {
    behavior: Behavior,
    calls: Arc<Mutex<Vec<String>>>,
}
struct FixtureProvider {
    definition: HarnessDefinition,
    behavior: Behavior,
    calls: Arc<Mutex<Vec<String>>>,
}

impl HarnessAdapterFactory for FixtureFactory {
    fn adapter_id(&self) -> &str {
        "fixture"
    }
    fn build(
        &self,
        definition: HarnessDefinition,
    ) -> Result<Arc<dyn HarnessProvider>, HarnessError> {
        Ok(Arc::new(FixtureProvider {
            definition,
            behavior: self.behavior,
            calls: self.calls.clone(),
        }))
    }
}

impl FixtureProvider {
    fn record(&self, operation: &str) -> Result<(), HarnessError> {
        self.calls
            .lock()
            .map_err(|_| HarnessError::Unavailable {
                harness: self.definition.id.clone(),
                message: "fixture log poisoned".to_owned(),
            })?
            .push(format!("{}:{operation}", self.definition.id));
        Ok(())
    }
}

impl HarnessProvider for FixtureProvider {
    fn definition(&self) -> &HarnessDefinition {
        &self.definition
    }
    fn run<'a>(&'a self, request: HarnessRunRequest) -> HarnessRunFuture<'a> {
        Box::pin(async move {
            self.record("run")?;
            if matches!(self.behavior, Behavior::CancelRun) {
                request.cancellation().cancel();
                return Err(HarnessError::Interrupted {
                    harness: self.definition.id.clone(),
                });
            }
            Ok(HarnessRunResult {
                harness_id: self.definition.id.clone(),
                adapter: self.definition.adapter.clone(),
                address: self.definition.address.clone(),
                thread_id: format!("thread-{}", self.definition.id),
                session_id: format!("session-{}", self.definition.id),
                project_directory: request.project_directory().to_owned(),
                visibility: request.visibility(),
                native_project_id: match request.placement() {
                    ConversationPlacement::NoNativeProject => None,
                    ConversationPlacement::ExistingProject { project_id, .. } => {
                        Some(project_id.clone())
                    }
                },
                client_project_grouping: if request.visibility() == ConversationVisibility::Hidden {
                    ClientProjectGrouping::NotApplicable
                } else {
                    ClientProjectGrouping::Unverified
                },
                final_response: "response\nquoted \"content\"".to_owned(),
            })
        })
    }
    fn list_projects<'a>(
        &'a self,
        request: HarnessProjectListRequest,
    ) -> HarnessProjectListFuture<'a> {
        Box::pin(async move {
            self.record("projects")?;
            if matches!(self.behavior, Behavior::DiscoveryFailure) {
                return Err(HarnessError::Unavailable {
                    harness: self.definition.id.clone(),
                    message: "injected discovery failure".to_owned(),
                });
            }
            if matches!(self.behavior, Behavior::DiscoveryConsumesBudget) {
                tokio::time::advance(Duration::from_secs(2)).await;
            }
            Ok(vec![HarnessProject {
                harness_id: self.definition.id.clone(),
                id: format!("project-{}", self.definition.id),
                name: "Existing".to_owned(),
                roots: vec![request.context_directory().to_owned()],
            }])
        })
    }
    fn create_project<'a>(
        &'a self,
        request: HarnessProjectCreateRequest,
    ) -> HarnessProjectCreateFuture<'a> {
        Box::pin(async move {
            self.record("create")?;
            Ok(HarnessProject {
                harness_id: self.definition.id.clone(),
                id: format!("created-{}", self.definition.id),
                name: request.name().to_owned(),
                roots: vec![request.root_directory().to_owned()],
            })
        })
    }
}

type FixtureRegistry = (HarnessRegistry, Arc<Mutex<Vec<String>>>);

fn fixture_registry(behavior: Behavior, default: bool) -> TestResult<FixtureRegistry> {
    let cwd = existing_directory()?;
    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut builder = HarnessRegistryBuilder::new();
    builder.register(Arc::new(FixtureFactory {
        behavior,
        calls: calls.clone(),
    }))?;
    let mut disabled = HarnessDefinition::new(
        "disabled",
        "fixture",
        "fixture://disabled",
        vec![cwd.clone()],
    );
    disabled.enabled = false;
    let registry = builder.build(HarnessRegistryConfig {
        schema_version: 1,
        default_harness: default.then(|| "default".to_owned()),
        harnesses: vec![
            HarnessDefinition::new("default", "fixture", "fixture://default", vec![cwd.clone()]),
            HarnessDefinition::new("alternate", "fixture", "fixture://alternate", vec![cwd]),
            disabled,
        ],
    })?;
    Ok((registry, calls))
}

fn run_args(
    harness: Option<&str>,
    project: Option<&str>,
    visibility: ConversationVisibility,
) -> TestResult<HarnessArguments> {
    Ok(HarnessArguments {
        config: PathBuf::from("unused.json"),
        extensions: None,
        workspace: None,
        harness: harness.map(str::to_owned),
        command: HarnessCommand::Run {
            cwd: existing_directory()?,
            prompt: "hello".to_owned(),
            model: Some("fixture-model".to_owned()),
            visibility,
            project: project.map(str::to_owned),
            timeout: Duration::from_secs(30),
        },
    })
}

fn existing_directory() -> io::Result<PathBuf> {
    fs::canonicalize(env!("CARGO_MANIFEST_DIR"))
}

static NEXT_PROMPT: AtomicU64 = AtomicU64::new(0);
struct PromptFixture {
    path: PathBuf,
}
impl PromptFixture {
    fn new() -> TestResult<Self> {
        let root = std::env::var_os("RECUVORA_TEST_TEMP")
            .map(PathBuf::from)
            .ok_or_else(|| {
                io::Error::other("RECUVORA_TEST_TEMP must name the external temporary directory")
            })?;
        let root = fs::canonicalize(root)?;
        if root.starts_with(existing_directory()?) {
            return Err(io::Error::other("test temporary directory is inside project").into());
        }
        let path = root.join(format!(
            "interfaces-prompt-{}-{}.txt",
            std::process::id(),
            NEXT_PROMPT.fetch_add(1, Ordering::Relaxed)
        ));
        File::options().write(true).create_new(true).open(&path)?;
        Ok(Self { path })
    }
    fn cleanup(self) -> io::Result<()> {
        fs::remove_file(self.path)
    }
}

#[test]
fn remote_workspace_arguments_are_explicit_and_exclude_local_paths() -> TestResult {
    let args = parsed(&[
        "projects",
        "--config",
        "harness.json",
        "--extensions",
        "extensions.json",
        "--workspace",
        "work",
    ])?;
    assert_eq!(args.workspace.as_deref(), Some("work"));
    assert!(args.extensions.is_some());
    assert!(
        parse(&[
            "projects",
            "--config",
            "harness.json",
            "--workspace",
            "work",
            "--cwd",
            "."
        ])
        .is_err()
    );
    assert!(
        parse(&[
            "projects",
            "--config",
            "harness.json",
            "--workspace",
            "../work"
        ])
        .is_err()
    );
    Ok(())
}
