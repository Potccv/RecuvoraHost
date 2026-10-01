//! Bounded CLI input and JSON views for the common Harness contracts.
//!
//! Boot owns configuration and provider assembly. This consumer never imports
//! an adapter or treats a provider project as verified desktop client state.

pub(crate) mod repair;

use crate::harnesses::{
    ConversationPlacement, ConversationVisibility, HarnessCancellation, HarnessError,
    HarnessProjectCreateRequest, HarnessProjectListRequest, HarnessRegistry, HarnessRunRequest,
};
use crate::presentation::{grouping_json, path_json, paths_json, project_json, visibility_name};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::time::Instant;

const MAX_PROMPT_BYTES: usize = 64 * 1024;
const DEFAULT_TIMEOUT_SECS: u64 = 180;
const MAX_TIMEOUT_SECS: u64 = 1800;

pub(crate) const HELP: &str = "Harness commands (JSON output):
  recuvora-host harness list --config PATH [--extensions PATH]
  recuvora-host harness projects --config PATH [--harness ID] --cwd PATH [--timeout-secs N]
  recuvora-host harness create-project --config PATH [--harness ID] --cwd PATH --name NAME --idempotency-key KEY [--timeout-secs N]
  recuvora-host harness run --config PATH [--harness ID] --cwd PATH (--prompt TEXT | --prompt-file PATH) [--model MODEL] --visibility client|hidden (--project ID | --no-project) [--timeout-secs N]

Remote commands require --extensions PATH and --workspace ID instead of --cwd PATH.
The remote node, not this host, resolves and validates that workspace directory.
Configuration must be explicit. All paths preserve their operating-system encoding.
Timeouts use one command budget, default 180 seconds, allowed 1..=1800 seconds.
Prompt input must be nonempty UTF-8 and at most 65536 bytes.
create-project registers an existing allowed directory; it does not create directories.
hidden conversations cannot select a native project. Client project grouping is
reported separately from native project assignment and may remain unverified.
Cancellation or timeout after submission may leave a conversation/project outcome
unknown. Commands never automatically retry creation or conversation submission.
";

#[derive(Debug)]
pub(crate) struct HarnessArguments {
    pub config: PathBuf,
    pub extensions: Option<PathBuf>,
    pub workspace: Option<String>,
    pub harness: Option<String>,
    pub command: HarnessCommand,
}

#[derive(Debug)]
pub(crate) enum HarnessCommand {
    List,
    Projects {
        cwd: PathBuf,
        timeout: Duration,
    },
    CreateProject {
        cwd: PathBuf,
        name: String,
        idempotency_key: String,
        timeout: Duration,
    },
    Run {
        cwd: PathBuf,
        prompt: String,
        model: Option<String>,
        visibility: ConversationVisibility,
        project: Option<String>,
        timeout: Duration,
    },
}

/// Parses only the arguments following `harness`. Invalid input is rejected
/// before configuration loading, executable probing, or provider calls.
pub(crate) fn parse_harness(
    args: impl IntoIterator<Item = OsString>,
) -> Result<Option<HarnessArguments>, String> {
    let mut args = args.into_iter();
    let Some(command) = args.next() else {
        return Ok(None);
    };
    let command = command
        .to_str()
        .ok_or("Harness command must be valid UTF-8")?;
    if matches!(command, "help" | "--help" | "-h") {
        return if args.next().is_none() {
            Ok(None)
        } else {
            Err("help does not accept additional arguments".to_owned())
        };
    }
    if !matches!(command, "list" | "projects" | "create-project" | "run") {
        return Err(format!("unknown Harness command: {command}"));
    }
    let mut options = BTreeMap::new();
    while let Some(flag) = args.next() {
        let flag = flag.to_str().ok_or("option names must be valid UTF-8")?;
        if matches!(flag, "--help" | "-h") && options.is_empty() {
            return if args.next().is_none() {
                Ok(None)
            } else {
                Err("help does not accept additional arguments".to_owned())
            };
        }
        let allowed = matches!(flag, "--config" | "--extensions")
            || (command != "list"
                && matches!(
                    flag,
                    "--harness" | "--cwd" | "--workspace" | "--timeout-secs"
                ))
            || (command == "create-project" && matches!(flag, "--name" | "--idempotency-key"))
            || (command == "run"
                && matches!(
                    flag,
                    "--prompt"
                        | "--prompt-file"
                        | "--model"
                        | "--visibility"
                        | "--project"
                        | "--no-project"
                ));
        if !allowed {
            return Err(format!(
                "unknown or inapplicable option for {command}: {flag}"
            ));
        }
        if options.contains_key(flag) {
            return Err(format!("duplicate option: {flag}"));
        }
        let value = if flag == "--no-project" {
            OsString::new()
        } else {
            let value = args
                .next()
                .ok_or_else(|| format!("missing value for {flag}"))?;
            if value.is_empty() || value.to_str().is_some_and(|value| value.starts_with("--")) {
                return Err(format!("missing or empty value for {flag}"));
            }
            value
        };
        options.insert(flag.to_owned(), value);
    }
    let config = required_path(&mut options, "--config")?;
    let extensions = options.remove("--extensions").map(PathBuf::from);
    let workspace = optional_text(&mut options, "--workspace", 128, false)?;
    if workspace
        .as_ref()
        .is_some_and(|id| !crate::protocol::valid_id(id))
    {
        return Err("--workspace must be a workspace identifier".into());
    }
    let harness = optional_text(&mut options, "--harness", 64, false)?;
    if let Some(id) = &harness
        && !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err("--harness must use ASCII letters, digits, '.', '_' or '-'".to_owned());
    }
    let command = if command == "list" {
        HarnessCommand::List
    } else {
        let cwd = if workspace.is_some() {
            if options.contains_key("--cwd") {
                return Err("choose --workspace ID or --cwd PATH".into());
            }
            PathBuf::new()
        } else {
            required_path(&mut options, "--cwd")?
        };
        let timeout = parse_timeout(options.remove("--timeout-secs"))?;
        match command {
            "projects" => HarnessCommand::Projects { cwd, timeout },
            "create-project" => HarnessCommand::CreateProject {
                cwd,
                name: required_text(&mut options, "--name", 256, false)?,
                idempotency_key: required_text(&mut options, "--idempotency-key", 512, false)?,
                timeout,
            },
            _ => {
                let visibility =
                    match required_text(&mut options, "--visibility", 16, false)?.as_str() {
                        "client" => ConversationVisibility::Client,
                        "hidden" => ConversationVisibility::Hidden,
                        _ => return Err("--visibility must be client or hidden".to_owned()),
                    };
                let project = optional_text(&mut options, "--project", 512, false)?;
                let no_project = options.remove("--no-project").is_some();
                if project.is_some() == no_project {
                    return Err(
                        "run requires exactly one of --project ID or --no-project".to_owned()
                    );
                }
                if visibility == ConversationVisibility::Hidden && project.is_some() {
                    return Err("hidden conversations cannot select --project".to_owned());
                }
                let model = optional_text(&mut options, "--model", 128, false)?;
                let prompt = optional_text(&mut options, "--prompt", MAX_PROMPT_BYTES, true)?;
                let prompt_file = options.remove("--prompt-file").map(PathBuf::from);
                let prompt = match (prompt, prompt_file) {
                    (Some(prompt), None) => prompt,
                    (None, Some(path)) => read_prompt_file(&path)?,
                    _ => {
                        return Err(
                            "run requires exactly one of --prompt or --prompt-file".to_owned()
                        );
                    }
                };
                HarnessCommand::Run {
                    cwd,
                    prompt,
                    model,
                    visibility,
                    project,
                    timeout,
                }
            }
        }
    };
    Ok(Some(HarnessArguments {
        config,
        extensions,
        workspace,
        harness,
        command,
    }))
}

fn required_path(options: &mut BTreeMap<String, OsString>, flag: &str) -> Result<PathBuf, String> {
    options
        .remove(flag)
        .map(PathBuf::from)
        .ok_or_else(|| format!("missing required option {flag}"))
}

fn optional_text(
    options: &mut BTreeMap<String, OsString>,
    flag: &str,
    limit: usize,
    allow_controls: bool,
) -> Result<Option<String>, String> {
    options
        .remove(flag)
        .map(|value| {
            let value = value
                .into_string()
                .map_err(|_| format!("{flag} must be valid UTF-8"))?;
            validate_text(&value, flag, limit, allow_controls)?;
            Ok(value)
        })
        .transpose()
}

fn required_text(
    options: &mut BTreeMap<String, OsString>,
    flag: &str,
    limit: usize,
    allow_controls: bool,
) -> Result<String, String> {
    optional_text(options, flag, limit, allow_controls)?
        .ok_or_else(|| format!("missing required option {flag}"))
}

fn validate_text(
    value: &str,
    label: &str,
    limit: usize,
    allow_controls: bool,
) -> Result<(), String> {
    if value.trim().is_empty()
        || value.len() > limit
        || (!allow_controls && value.chars().any(char::is_control))
    {
        return Err(format!(
            "{label} must contain 1..={limit} bytes of nonempty UTF-8{}",
            if allow_controls {
                ""
            } else {
                " without control characters"
            }
        ));
    }
    Ok(())
}

fn read_prompt_file(path: &Path) -> Result<String, String> {
    let file = File::open(path).map_err(|error| format!("cannot open --prompt-file: {error}"))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("cannot inspect --prompt-file: {error}"))?;
    if !metadata.is_file() {
        return Err("--prompt-file must be a regular file".to_owned());
    }
    if metadata.len() > MAX_PROMPT_BYTES as u64 {
        return Err(format!("--prompt-file exceeds {MAX_PROMPT_BYTES} bytes"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_PROMPT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read --prompt-file: {error}"))?;
    let prompt =
        String::from_utf8(bytes).map_err(|_| "--prompt-file must be valid UTF-8".to_owned())?;
    validate_text(&prompt, "--prompt-file", MAX_PROMPT_BYTES, true)?;
    Ok(prompt)
}

fn parse_timeout(value: Option<OsString>) -> Result<Duration, String> {
    let Some(value) = value else {
        return Ok(Duration::from_secs(DEFAULT_TIMEOUT_SECS));
    };
    let value = value.to_str().ok_or("--timeout-secs must be valid UTF-8")?;
    if value.is_empty() || value.len() > 4 || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(format!(
            "--timeout-secs must be an integer from 1 to {MAX_TIMEOUT_SECS}"
        ));
    }
    let seconds: u64 = value
        .parse()
        .map_err(|_| "invalid --timeout-secs".to_owned())?;
    if !(1..=MAX_TIMEOUT_SECS).contains(&seconds) {
        return Err(format!(
            "--timeout-secs must be between 1 and {MAX_TIMEOUT_SECS}"
        ));
    }
    Ok(Duration::from_secs(seconds))
}

pub(crate) async fn execute(
    registry: &HarnessRegistry,
    args: &HarnessArguments,
    cancellation: HarnessCancellation,
) -> Result<Value, HarnessError> {
    if matches!(args.command, HarnessCommand::List) {
        return Ok(json!({
            "status": "ok",
            "default_harness": registry.default_harness(),
            "harnesses": registry.definitions().iter().map(|definition| json!({
                "id": definition.id,
                "adapter": definition.adapter,
                "address": definition.address,
                "enabled": definition.enabled,
                "default": registry.default_harness() == Some(definition.id.as_str()),
                "workspace_roots": paths_json(&definition.workspace_roots),
                "availability": if definition.enabled { "configured" } else { "disabled" },
                "authentication": "not_checked",
            })).collect::<Vec<_>>(),
        }));
    }
    let harness = selected_harness(registry, args.harness.as_deref())?;
    let definition = registry
        .definitions()
        .into_iter()
        .find(|d| d.id == harness)
        .ok_or_else(|| HarnessError::UnknownHarness(harness.clone()))?;
    let remote = if definition.adapter == crate::harnesses::REMOTE_NODE_ADAPTER {
        let workspace = args.workspace.as_ref().ok_or_else(|| {
            HarnessError::InvalidRequest("remote Harness requires --workspace ID".into())
        })?;
        Some((
            definition.address.trim_start_matches("node://").to_owned(),
            workspace.clone(),
        ))
    } else {
        if args.workspace.is_some() {
            return Err(HarnessError::InvalidRequest(
                "local Harness requires --cwd PATH".into(),
            ));
        }
        None
    };
    let list_request = |cwd: &Path| match &remote {
        Some((node, workspace)) => HarnessProjectListRequest::remote(node, workspace),
        None => HarnessProjectListRequest::new(cwd),
    };
    let timeout = match &args.command {
        HarnessCommand::List => unreachable!(),
        HarnessCommand::Projects { timeout, .. }
        | HarnessCommand::CreateProject { timeout, .. }
        | HarnessCommand::Run { timeout, .. } => *timeout,
    };
    let deadline = Instant::now() + timeout;
    let remaining = || {
        if cancellation.is_cancelled() {
            return Err(HarnessError::Interrupted {
                harness: harness.clone(),
            });
        }
        deadline
            .checked_duration_since(Instant::now())
            .filter(|duration| !duration.is_zero())
            .ok_or_else(|| HarnessError::DeadlineExceeded {
                harness: harness.clone(),
                seconds: timeout.as_secs(),
            })
    };
    match &args.command {
        HarnessCommand::List => unreachable!(),
        HarnessCommand::Projects { cwd, .. } => {
            let projects = registry
                .list_projects(
                    Some(&harness),
                    list_request(cwd)
                        .with_timeout(remaining()?)
                        .with_cancellation(cancellation.clone()),
                )
                .await?;
            Ok(
                json!({ "status": "ok", "harness_id": harness, "projects": projects.iter().map(project_json).collect::<Vec<_>>() }),
            )
        }
        HarnessCommand::CreateProject {
            cwd,
            name,
            idempotency_key,
            ..
        } => {
            let project = registry
                .create_project(
                    Some(&harness),
                    (match &remote {
                        Some((node, workspace)) => HarnessProjectCreateRequest::remote(
                            node,
                            workspace,
                            name,
                            idempotency_key,
                        ),
                        None => HarnessProjectCreateRequest::new(name, cwd, idempotency_key),
                    })
                    .with_timeout(remaining()?)
                    .with_cancellation(cancellation.clone()),
                )
                .await?;
            Ok(
                json!({ "status": "ok", "harness_id": harness, "idempotency_key": idempotency_key, "project": project_json(&project), "client_project_grouping": { "status": "unverified" } }),
            )
        }
        HarnessCommand::Run {
            cwd,
            prompt,
            model,
            visibility,
            project,
            ..
        } => {
            let placement = if let Some(project_id) = project {
                let projects = registry
                    .list_projects(
                        Some(&harness),
                        list_request(cwd)
                            .with_timeout(remaining()?)
                            .with_cancellation(cancellation.clone()),
                    )
                    .await?;
                if !projects
                    .iter()
                    .any(|project| project.id == *project_id && project.harness_id == harness)
                {
                    return Err(HarnessError::UnknownProject {
                        harness,
                        project_id: project_id.clone(),
                    });
                }
                ConversationPlacement::ExistingProject {
                    harness_id: harness.clone(),
                    project_id: project_id.clone(),
                }
            } else {
                ConversationPlacement::NoNativeProject
            };
            let mut request = (match &remote {
                Some((node, workspace)) => HarnessRunRequest::remote(node, workspace, prompt),
                None => HarnessRunRequest::new(cwd, prompt),
            })
            .with_timeout(remaining()?)
            .with_visibility(*visibility)
            .with_placement(placement)
            .with_cancellation(cancellation.clone());
            if let Some(model) = model {
                request = request.with_model(model);
            }
            let result = registry.run(Some(&harness), request).await?;
            Ok(json!({
                "status": "ok", "harness_id": result.harness_id, "adapter": result.adapter,
                "address": result.address, "session_id": result.session_id, "thread_id": result.thread_id,
                "cwd": path_json(&result.project_directory), "visibility": visibility_name(result.visibility),
                "native_project_id": result.native_project_id, "client_project_grouping": grouping_json(&result.client_project_grouping),
                "final_response": result.final_response,
            }))
        }
    }
}

fn selected_harness(
    registry: &HarnessRegistry,
    requested: Option<&str>,
) -> Result<String, HarnessError> {
    let harness = requested
        .or_else(|| registry.default_harness())
        .ok_or(HarnessError::NoDefaultHarness)?;
    let definition = registry
        .definitions()
        .into_iter()
        .find(|definition| definition.id == harness)
        .ok_or_else(|| HarnessError::UnknownHarness(harness.to_owned()))?;
    if !definition.enabled {
        return Err(HarnessError::HarnessDisabled(harness.to_owned()));
    }
    Ok(harness.to_owned())
}

#[cfg(test)]
#[path = "../../tests/interfaces.rs"]
mod tests;
