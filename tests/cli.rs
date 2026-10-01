use std::error::Error;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

#[tokio::test]
async fn repair_help_and_argument_errors_do_not_load_configuration() -> TestResult {
    let output = run(&["repair", "--help"]).await?;
    assert!(output.status.success());
    assert!(String::from_utf8(output.stdout)?.contains("check-result"));
    for args in [
        vec!["repair", "run", "--config", "absent"],
        vec!["repair", "approve", "--config", "absent", "--request", "x"],
        vec!["repair", "inspect", "--config", "absent", "--prompt", "bad"],
        vec![
            "repair",
            "apply",
            "--config",
            "absent",
            "--request",
            "x",
            "--request",
            "y",
        ],
    ] {
        let output = run(&args).await?;
        assert_eq!(output.status.code(), Some(2));
        let json: Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(json["auto_retry"], false);
    }
    Ok(())
}

#[cfg(windows)]
#[tokio::test]
async fn repair_cli_persists_human_decision_and_rejects_a_control_directory_target() -> TestResult {
    use recuvora_core::recovery::approval::{
        ApprovalPolicy, ApprovalStore, ApprovalStoreConfig, ProposedOperation, ReviewerConfig,
    };
    use recuvora_host::repair::{RepairConfig, now};
    let temp = TestDirectory::create()?;
    let target_root = temp.path.join(".control").join("target");
    fs::create_dir_all(&target_root)?;
    for part in ["review", "state"] {
        fs::create_dir(temp.path.join(part))?;
    }
    fs::write(target_root.join("a.txt"), "before")?;
    fs::write(temp.path.join("harness.json"), "{}")?;
    let policy = ApprovalPolicy {
        id: "cli-policy".into(),
        version: 1,
        reviewer: ReviewerConfig::Human,
        delegation: "Allow before to after".into(),
        allowed_targets: vec!["target".into()],
        allowed_action_kinds: vec!["replace_text".into()],
        ttl_secs: 600,
    };
    let config = RepairConfig {
        extensions_config: None,
        execution_workspace: None,
        reviewer_workspace: None,
        schema_version: 1,
        harness_config: temp.path.join("harness.json"),
        execution_harness: "unused".into(),
        target_id: "target".into(),
        target_root,
        reviewer_directory: temp.path.join("review"),
        data_dir: temp.path.join("state"),
        allowed_files: vec!["a.txt".into()],
        policy: policy.clone(),
        timeout_secs: 60,
        max_tool_calls: 4,
    };
    let config_path = temp.path.join("repair.json");
    fs::write(&config_path, serde_json::to_vec(&config)?)?;
    let mut store = ApprovalStore::open(&config.data_dir, ApprovalStoreConfig::default(), now()?)?;
    let record=store.request(ProposedOperation {task_id:"cli-task".into(),task_revision:1,operation_id:"cli-op".into(),target:"target".into(),
        action:json!({"kind":"replace_text","target_root":config.target_root.canonicalize()?,"path":"a.txt","expected":"before","replacement":"after"})},policy,now()?)?;
    let id = record.request.request_id;
    drop(store);
    let config_arg = config_path.to_str().ok_or("non-UTF8 config")?;
    let output = run(&[
        "repair",
        "approve",
        "--config",
        config_arg,
        "--request",
        &id,
        "--reason",
        "reviewed exact replacement",
    ])
    .await?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(
        fs::read_to_string(config.target_root.join("a.txt"))?,
        "before"
    );
    let output = run(&["repair", "apply", "--config", config_arg, "--request", &id]).await?;
    assert_eq!(output.status.code(), Some(1));
    let error: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(error["status"], "failed");
    assert!(
        error["message"]
            .as_str()
            .is_some_and(|message| message.contains("control directory"))
    );
    assert_eq!(
        fs::read_to_string(config.target_root.join("a.txt"))?,
        "before"
    );
    fs::rename(&config.target_root, temp.path.join("moved-target"))?;
    let output = run(&[
        "repair",
        "inspect",
        "--config",
        config_arg,
        "--request",
        &id,
    ])
    .await?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let report: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(report["record"]["state"], "approved");
    temp.clean()?;
    Ok(())
}

#[tokio::test]
async fn demo_checks_expected_faults_without_failing_the_process() -> TestResult {
    let temp = TestDirectory::create()?;
    let output = run(&[
        "demo",
        "--data-dir",
        temp.path.to_str().ok_or("non-UTF8 test path")?,
        "--concurrency",
        "1",
    ])
    .await?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout)?;
    assert!(
        stdout.contains("demo: 8 expected outcomes verified"),
        "{stdout}"
    );
    assert!(stdout.contains("runtime: clean shutdown"), "{stdout}");
    for state in ["Succeeded", "Failed", "Unknown", "Denied", "Canceled"] {
        assert!(
            stdout.contains(&format!("state={state}")),
            "missing {state}: {stdout}"
        );
    }
    assert!(output.stderr.is_empty());
    temp.clean()?;
    Ok(())
}

#[cfg(windows)]
#[tokio::test]
async fn repair_cli_rejects_target_owned_extension_configuration_and_tls_trust() -> TestResult {
    use recuvora_core::recovery::approval::{ApprovalPolicy, ReviewerConfig};
    use recuvora_host::repair::RepairConfig;
    let temp = TestDirectory::create()?;
    for directory in ["target", "review", "state"] {
        fs::create_dir(temp.path.join(directory))?;
    }
    fs::write(temp.path.join("target/a.txt"), "before")?;
    fs::write(temp.path.join("harness.json"), "{}")?;
    let mut config = RepairConfig {
        schema_version: 1,
        harness_config: temp.path.join("harness.json"),
        extensions_config: Some(temp.path.join("target/extensions.json")),
        execution_workspace: None,
        reviewer_workspace: None,
        execution_harness: "unused".into(),
        target_id: "target".into(),
        target_root: temp.path.join("target"),
        reviewer_directory: temp.path.join("review"),
        data_dir: temp.path.join("state"),
        allowed_files: vec!["a.txt".into()],
        policy: ApprovalPolicy {
            id: "extension-policy".into(),
            version: 1,
            reviewer: ReviewerConfig::Human,
            delegation: "fixture".into(),
            allowed_targets: vec!["target".into()],
            allowed_action_kinds: vec!["replace_text".into()],
            ttl_secs: 60,
        },
        timeout_secs: 30,
        max_tool_calls: 4,
    };
    fs::write(
        config
            .extensions_config
            .as_ref()
            .ok_or("missing extension config")?,
        "{}",
    )?;
    let repair_path = temp.path.join("repair.json");
    fs::write(&repair_path, serde_json::to_vec(&config)?)?;
    let output = run(&[
        "repair",
        "apply",
        "--config",
        repair_path.to_str().ok_or("non-UTF8 config")?,
        "--request",
        "absent",
    ])
    .await?;
    assert!(!output.status.success());
    let message = String::from_utf8(output.stdout)?;
    assert!(
        message.contains("repair target must be isolated from configuration"),
        "{message}"
    );

    config.extensions_config = Some(temp.path.join("extensions.json"));
    let extension_config = config
        .extensions_config
        .as_ref()
        .ok_or("missing extension config")?;
    fs::write(
        extension_config,
        serde_json::to_vec(&json!({"schema_version":1,"extensions":[{
            "id":"node-test","kind":"node","endpoint":{"url":"wss://node.example.invalid/extension","ca_certificate":config.target_root.join("a.txt")},
            "allow_calls":[],"allow_nodes":[],"namespaces":[]
        }]}))?,
    )?;
    fs::write(&repair_path, serde_json::to_vec(&config)?)?;
    let output = run(&[
        "repair",
        "apply",
        "--config",
        repair_path.to_str().ok_or("non-UTF8 config")?,
        "--request",
        "absent",
    ])
    .await?;
    assert!(!output.status.success());
    let message = String::from_utf8(output.stdout)?;
    assert!(message.contains("overlaps extension"), "{message}");

    fs::write(
        extension_config,
        serde_json::to_vec(&json!({"schema_version":1,"extensions":[{
            "id":"node-test","kind":"node","command":{"program":"legacy-node.exe","args":[],"cwd":config.target_root},
            "allow_calls":[],"allow_nodes":[],"namespaces":[]
        }]}))?,
    )?;
    let output = run(&[
        "repair",
        "apply",
        "--config",
        repair_path.to_str().ok_or("non-UTF8 config")?,
        "--request",
        "absent",
    ])
    .await?;
    assert!(!output.status.success());
    let message = String::from_utf8(output.stdout)?;
    assert!(
        message.contains("command") || message.contains("endpoint"),
        "{message}"
    );
    assert_eq!(
        fs::read_to_string(config.target_root.join("a.txt"))?,
        "before"
    );
    temp.clean()?;
    Ok(())
}

#[tokio::test]
async fn invalid_arguments_and_source_data_directory_fail_cleanly() -> TestResult {
    for args in [
        vec!["demo"],
        vec!["demo", "--data-dir", ".", "--concurrency", "0"],
        vec!["demo", "--data-dir", env!("CARGO_MANIFEST_DIR")],
        vec!["unknown-command"],
    ] {
        let output = run(&args).await?;
        assert!(!output.status.success());
        let stderr = String::from_utf8(output.stderr)?;
        assert!(stderr.contains("recuvora-host:"), "{stderr}");
        assert!(!stderr.contains("panicked"), "{stderr}");
    }
    let help = run(&["--help"]).await?;
    assert!(help.status.success());
    assert!(String::from_utf8(help.stdout)?.contains("simulation only"));
    Ok(())
}

#[tokio::test]
async fn repair_cli_rejects_host_source_target_before_creating_approval_state() -> TestResult {
    use recuvora_core::recovery::approval::{ApprovalPolicy, ReviewerConfig};
    use recuvora_host::repair::RepairConfig;
    let temp = TestDirectory::create()?;
    fs::create_dir(temp.path.join("review"))?;
    fs::write(temp.path.join("harness.json"), "{}")?;
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).canonicalize()?;
    let manifest = fs::read(source.join("Cargo.toml"))?;
    let config = RepairConfig {
        schema_version: 1,
        harness_config: temp.path.join("harness.json"),
        extensions_config: None,
        execution_workspace: None,
        reviewer_workspace: None,
        execution_harness: "unused".into(),
        target_id: "source-target".into(),
        target_root: source.clone(),
        reviewer_directory: temp.path.join("review"),
        data_dir: temp.path.join("must-not-create-state"),
        allowed_files: vec!["Cargo.toml".into()],
        policy: ApprovalPolicy {
            id: "source-policy".into(),
            version: 1,
            reviewer: ReviewerConfig::Human,
            delegation: "fixture denial only".into(),
            allowed_targets: vec!["source-target".into()],
            allowed_action_kinds: vec!["replace_text".into()],
            ttl_secs: 60,
        },
        timeout_secs: 30,
        max_tool_calls: 4,
    };
    let path = temp.path.join("repair.json");
    fs::write(&path, serde_json::to_vec(&config)?)?;
    let output = run(&[
        "repair",
        "apply",
        "--config",
        path_text(&path)?,
        "--request",
        "absent",
    ])
    .await?;
    assert_eq!(output.status.code(), Some(1));
    let error: Value = serde_json::from_slice(&output.stdout)?;
    assert!(
        error["message"]
            .as_str()
            .is_some_and(|message| message.contains("source")),
        "{error}"
    );
    assert!(
        !config.data_dir.exists(),
        "denied target created approval state"
    );
    assert_eq!(fs::read(source.join("Cargo.toml"))?, manifest);
    temp.clean()?;
    Ok(())
}

#[tokio::test]
async fn harness_help_describes_the_available_commands_without_loading_configuration() -> TestResult
{
    for args in [vec!["--help"], vec!["harness", "--help"]] {
        let output = run(&args).await?;
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        let help = String::from_utf8(output.stdout)?;
        for command in ["harness", "list", "projects", "create-project", "run"] {
            assert!(help.contains(command), "missing {command}: {help}");
        }
    }
    Ok(())
}

#[tokio::test]
async fn unsupported_human_then_harness_policy_rejects_execution_before_state_creation()
-> TestResult {
    use recuvora_core::recovery::approval::{ApprovalPolicy, ReviewerConfig};
    use recuvora_host::boot::host::load_repair_config;
    use recuvora_host::repair::RepairConfig;

    let temp = TestDirectory::create()?;
    for name in ["target", "review"] {
        fs::create_dir(temp.path.join(name))?;
    }
    fs::write(temp.path.join("target/a.txt"), "before")?;
    fs::write(temp.path.join("harness.json"), "{}")?;
    let config = RepairConfig {
        schema_version: 1,
        harness_config: temp.path.join("harness.json"),
        extensions_config: None,
        execution_workspace: None,
        reviewer_workspace: None,
        execution_harness: "unused".into(),
        target_id: "target".into(),
        target_root: temp.path.join("target"),
        reviewer_directory: temp.path.join("review"),
        data_dir: temp.path.join("must-not-create-state"),
        allowed_files: vec!["a.txt".into()],
        policy: ApprovalPolicy {
            id: "deferred-review-policy".into(),
            version: 1,
            reviewer: ReviewerConfig::HumanThenHarness {
                harness_id: "reviewer".into(),
                human_wait_secs: 20,
                review_timeout_secs: 15,
            },
            delegation: "fixture policy compatibility only".into(),
            allowed_targets: vec!["target".into()],
            allowed_action_kinds: vec!["replace_text".into()],
            ttl_secs: 60,
        },
        timeout_secs: 30,
        max_tool_calls: 4,
    };
    config.validate()?;
    let path = temp.path.join("repair.json");
    fs::write(&path, serde_json::to_vec(&config)?)?;
    let error =
        load_repair_config(&path, true).expect_err("unsupported execution policy was accepted");
    assert!(error.to_string().contains("HumanThenHarness"), "{error}");
    assert!(!error.to_string().contains("control directory"), "{error}");
    assert!(!config.data_dir.exists());

    for args in [
        vec![
            "repair",
            "run",
            "--config",
            path_text(&path)?,
            "--task",
            "policy-task",
            "--prompt",
            "diagnose",
        ],
        vec![
            "repair",
            "apply",
            "--config",
            path_text(&path)?,
            "--request",
            "absent",
        ],
    ] {
        let output = run(&args).await?;
        assert_eq!(output.status.code(), Some(1));
        let error: Value = serde_json::from_slice(&output.stdout)?;
        assert!(
            error["message"]
                .as_str()
                .is_some_and(|message| message.contains("HumanThenHarness")
                    && !message.contains("control directory")),
            "{error}"
        );
        assert!(
            !config.data_dir.exists(),
            "unsupported policy created approval state"
        );
    }

    // Record inspection/management remains available without opening an action session.
    let (loaded, _) = load_repair_config(&path, false)?;
    assert_eq!(loaded.policy.reviewer, config.policy.reviewer);
    assert_eq!(
        fs::read_to_string(config.target_root.join("a.txt"))?,
        "before"
    );
    temp.clean()?;
    Ok(())
}

#[tokio::test]
async fn demo_rejects_new_runtime_paths_inside_host_and_core_sources_before_creation() -> TestResult
{
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    for root in [env!("CARGO_MANIFEST_DIR"), env!("RECUVORA_CORE_SOURCE_DIR")] {
        let path = Path::new(root).join(format!(
            "must-not-create-host-runtime-{}-{stamp}",
            std::process::id()
        ));
        assert!(!path.exists(), "isolated denial path unexpectedly exists");
        let output = run(&["demo", "--data-dir", path_text(&path)?]).await?;
        assert!(
            !output.status.success(),
            "source runtime path was accepted: {}",
            path.display()
        );
        assert!(
            !path.exists(),
            "denied source path was created: {}",
            path.display()
        );
        assert!(String::from_utf8(output.stderr)?.contains("source"));
    }
    Ok(())
}

#[tokio::test]
async fn harness_arguments_are_rejected_before_configuration_or_provider_access() -> TestResult {
    // None of these requests may reach configuration loading or a provider.
    // An absent file distinguishes argument validation from a later I/O failure.
    let missing = "definitely-absent-cli-config.json";
    let cases = [
        vec!["unknown"],
        vec!["list"],
        vec!["list", "--config"],
        vec!["list", "--config", missing, "--config", missing],
        vec!["list", "--config", missing, "--cwd", "."],
        vec!["list", "--config", missing, "--harness", "alpha"],
        vec!["list", "--config", missing, "--unrecognized", "value"],
        vec!["projects", "--config", missing],
        vec![
            "projects", "--config", missing, "--cwd", ".", "--prompt", "text",
        ],
        vec![
            "projects",
            "--config",
            missing,
            "--cwd",
            ".",
            "--timeout-secs",
            "0",
        ],
        vec![
            "projects",
            "--config",
            missing,
            "--cwd",
            ".",
            "--timeout-secs",
            "1801",
        ],
        vec!["projects", "--config", missing, "--cwd", ".", "--cwd", "."],
        vec![
            "create-project",
            "--config",
            missing,
            "--cwd",
            ".",
            "--name",
            "name",
        ],
        vec![
            "create-project",
            "--config",
            missing,
            "--cwd",
            ".",
            "--idempotency-key",
            "key",
        ],
        vec!["run", "--config", missing, "--cwd", ".", "--prompt", "text"],
        vec![
            "run",
            "--config",
            missing,
            "--cwd",
            ".",
            "--prompt",
            "text",
            "--visibility",
            "invalid",
            "--no-project",
        ],
        vec![
            "run",
            "--config",
            missing,
            "--cwd",
            ".",
            "--prompt",
            "text",
            "--visibility",
            "hidden",
            "--project",
            "native-project",
        ],
        vec![
            "run",
            "--config",
            missing,
            "--cwd",
            ".",
            "--prompt",
            "text",
            "--visibility",
            "client",
            "--project",
            "native-project",
            "--no-project",
        ],
        vec![
            "run",
            "--config",
            missing,
            "--cwd",
            ".",
            "--prompt",
            "text",
            "--prompt-file",
            "prompt.txt",
            "--visibility",
            "hidden",
            "--no-project",
        ],
        vec![
            "run",
            "--config",
            missing,
            "--cwd",
            ".",
            "--prompt",
            "text",
            "--visibility",
            "hidden",
            "--no-project",
            "--no-project",
        ],
    ];
    for case in cases {
        let mut args = vec!["harness"];
        args.extend(case);
        let output = run(&args).await?;
        let error = json_error(&output, 2)?;
        assert_eq!(error["category"], "invalid_arguments", "{args:?}: {error}");
    }
    Ok(())
}

#[cfg(windows)]
#[tokio::test]
async fn harness_list_preserves_remote_workspace_ids_without_connecting_to_a_node() -> TestResult {
    let temp = TestDirectory::create()?;
    let config = write_harness_config(
        &temp,
        json!({
            "schema_version": 1,
            "default_harness": "alpha",
            "harnesses": [
                definition("alpha", "remote-node", "node://harness-node", true, "workspace"),
                definition("beta", "remote-node", "node://harness-node", true, "workspace"),
                definition("off", "not-installed", "https://example.invalid", false, "workspace")
            ]
        }),
    )?;
    let output = run_harness("list", &config, &[]).await?;
    let report = json_stdout(&output, 1)?;
    assert_eq!(report["status"], "unavailable");
    assert_eq!(report["default_harness"], "alpha");
    assert_eq!(
        report["harnesses"]
            .as_array()
            .ok_or("missing harnesses")?
            .len(),
        3
    );
    for id in ["alpha", "beta", "off"] {
        let entry = listed_harness(&report, id)?;
        assert_eq!(entry["is_default"], id == "alpha");
        assert_eq!(entry["enabled"], id != "off");
        assert_eq!(
            entry["availability"],
            if id == "off" {
                "disabled"
            } else {
                "unavailable"
            }
        );
        assert_eq!(entry["authentication"], "not_checked");
        assert_eq!(entry["runtime_status"], "not_probed");
        let expected_roots = if id == "off" {
            json!([temp.path.join("workspace").to_string_lossy()])
        } else {
            json!(["workspace"])
        };
        assert_eq!(entry["workspace_roots"], expected_roots);
    }
    temp.clean()?;
    Ok(())
}

#[tokio::test]
async fn harness_list_keeps_all_instances_when_one_cannot_be_assembled() -> TestResult {
    let temp = TestDirectory::create()?;
    let config = write_harness_config(
        &temp,
        json!({
            "schema_version": 1,
            "default_harness": "missing-node",
            "harnesses": [
                definition("missing-node", "remote-node", "node://harness-node", true, "."),
                definition("missing-adapter", "not-installed", "https://example.invalid", true, "."),
                definition("off", "remote-node", "node://harness-node", false, ".")
            ]
        }),
    )?;
    let output = run_harness("list", &config, &[]).await?;
    let report = json_stdout(&output, 1)?;
    assert_eq!(report["status"], "unavailable");
    assert_eq!(report["default_harness"], "missing-node");
    assert_eq!(
        report["harnesses"]
            .as_array()
            .ok_or("missing harnesses")?
            .len(),
        3
    );
    for id in ["missing-node", "missing-adapter"] {
        let entry = listed_harness(&report, id)?;
        assert_eq!(entry["availability"], "unavailable");
        assert!(
            entry["reason"]["message"]
                .as_str()
                .is_some_and(|reason| !reason.is_empty())
        );
        assert_eq!(entry["authentication"], "not_checked");
        assert_eq!(entry["runtime_status"], "not_probed");
    }
    assert_eq!(listed_harness(&report, "off")?["availability"], "disabled");
    temp.clean()?;
    Ok(())
}

#[tokio::test]
async fn harness_list_with_only_disabled_instances_does_not_require_a_node_connection() -> TestResult
{
    let temp = TestDirectory::create()?;
    let config = write_harness_config(
        &temp,
        json!({
            "schema_version": 1,
            "harnesses": [definition("off", "remote-node", "node://harness-node", false, ".")]
        }),
    )?;
    let output = run_harness("list", &config, &[]).await?;
    let report = json_stdout(&output, 0)?;
    assert!(report["default_harness"].is_null());
    let entry = listed_harness(&report, "off")?;
    assert_eq!(entry["availability"], "disabled");
    assert_eq!(entry["is_default"], false);
    temp.clean()?;
    Ok(())
}

#[tokio::test]
async fn harness_configuration_rejects_missing_oversized_and_source_tree_files() -> TestResult {
    let temp = TestDirectory::create()?;
    let missing = temp.path.join("missing.json");
    json_error(&run_harness("list", &missing, &[]).await?, 1)?;
    assert!(!missing.exists());

    let oversized = temp.path.join("oversized.json");
    fs::write(&oversized, vec![b' '; 64 * 1024 + 1])?;
    let error = json_error(&run_harness("list", &oversized, &[]).await?, 1)?;
    assert!(
        error["message"]
            .as_str()
            .is_some_and(|message| message.contains("65536")
                || message.contains("64")
                || message.contains("limit")),
        "{error}"
    );

    let source =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("profiles/harnesses.remote.example.json");
    let error = json_error(&run_harness("list", &source, &[]).await?, 1)?;
    assert!(
        error["message"]
            .as_str()
            .is_some_and(|message| message.contains("source") || message.contains("project")),
        "{error}"
    );
    temp.clean()?;
    Ok(())
}

#[tokio::test]
async fn harness_configuration_rejects_invalid_schema_duplicates_and_default_selection()
-> TestResult {
    let temp = TestDirectory::create()?;
    let valid = definition("alpha", "remote-node", "node://harness-node", true, ".");
    let configs = [
        json!({"schema_version": 2, "harnesses": [valid.clone()]}),
        json!({"schema_version": 1, "harnesses": [valid.clone(), valid.clone()]}),
        json!({"schema_version": 1, "default_harness": "missing", "harnesses": [valid.clone()]}),
        json!({"schema_version": 1, "default_harness": "off", "harnesses": [definition("off", "remote-node", "node://harness-node", false, ".")]}),
        json!({"schema_version": 1, "unexpected": true, "harnesses": [valid]}),
    ];
    for config in configs {
        let path = write_harness_config(&temp, config)?;
        json_error(&run_harness("list", &path, &[]).await?, 1)?;
    }
    temp.clean()?;
    Ok(())
}

#[tokio::test]
async fn harness_selection_rejects_unknown_disabled_and_missing_default_before_startup()
-> TestResult {
    let temp = TestDirectory::create()?;
    let config = write_harness_config(
        &temp,
        json!({
            "schema_version": 1,
            "harnesses": [definition("off", "remote-node", "node://harness-node", false, ".")]
        }),
    )?;
    for (selection, expected) in [
        (Some("missing"), "unknown"),
        (Some("off"), "disabled"),
        (None, "default"),
    ] {
        for command in ["projects", "run"] {
            let mut extras = vec!["--cwd", path_text(&temp.path)?];
            if let Some(id) = selection {
                extras.extend(["--harness", id]);
            }
            if command == "run" {
                extras.extend([
                    "--prompt",
                    "test must never reach a provider",
                    "--visibility",
                    "hidden",
                    "--no-project",
                ]);
            }
            let error = json_error(&run_harness(command, &config, &extras).await?, 1)?;
            let message = error["message"]
                .as_str()
                .ok_or("missing error message")?
                .to_ascii_lowercase();
            assert!(
                message.contains(expected),
                "{command} {selection:?}: {error}"
            );
        }
    }
    temp.clean()?;
    Ok(())
}

#[tokio::test]
async fn harness_prompt_files_are_bounded_utf8_input_and_valid_text_reaches_selection() -> TestResult
{
    let temp = TestDirectory::create()?;
    let config = write_harness_config(
        &temp,
        json!({
            "schema_version": 1,
            "harnesses": [definition("off", "remote-node", "node://harness-node", false, ".")]
        }),
    )?;
    let prompt = temp.path.join("prompt.txt");
    let extras = [
        "--harness",
        "off",
        "--cwd",
        path_text(&temp.path)?,
        "--prompt-file",
        path_text(&prompt)?,
        "--visibility",
        "hidden",
        "--no-project",
    ];
    let missing = json_error(&run_harness("run", &config, &extras).await?, 2)?;
    assert_eq!(missing["category"], "invalid_arguments");
    for bytes in [
        Vec::new(),
        b" \n\t ".to_vec(),
        vec![0xff, 0xfe],
        vec![b'a'; 64 * 1024 + 1],
    ] {
        fs::write(&prompt, bytes)?;
        let error = json_error(&run_harness("run", &config, &extras).await?, 2)?;
        assert_eq!(error["category"], "invalid_arguments");
    }
    fs::write(&prompt, "第一行\nSecond line\n")?;
    let error = json_error(&run_harness("run", &config, &extras).await?, 1)?;
    assert!(
        error["message"]
            .as_str()
            .is_some_and(|message| message.contains("disabled")),
        "{error}"
    );
    temp.clean()?;
    Ok(())
}

#[cfg(windows)]
#[tokio::test]
async fn harness_list_preserves_its_outcome_on_stderr_when_stdout_is_closed() -> TestResult {
    let temp = TestDirectory::create()?;
    let config = write_harness_config(
        &temp,
        json!({
            "schema_version": 1,
            "default_harness": "alpha",
            "harnesses": [definition("alpha", "remote-node", "node://harness-node", true, ".")]
        }),
    )?;
    let (reader, writer) = io::pipe()?;
    // Close the only read handle before the child exists, so its first stdout
    // write deterministically fails without a sleep or process scheduling race.
    drop(reader);
    let mut child = ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_recuvora-host"))
            .args(["harness", "list", "--config"])
            .arg(&config)
            .stdin(Stdio::null())
            .stdout(writer)
            .stderr(Stdio::piped())
            .spawn()?,
    );
    let stderr = child.0.stderr.take().ok_or("missing stderr")?;
    let read_err = tokio::task::spawn_blocking(move || read_output(stderr));
    let status = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Some(status) = child.0.try_wait()? {
                return Ok::<_, io::Error>(status);
            }
            tokio::task::yield_now().await;
        }
    })
    .await??;
    let stderr = tokio::time::timeout(Duration::from_secs(2), read_err).await???;
    let error = json_error(
        &Output {
            status,
            stdout: Vec::new(),
            stderr,
        },
        1,
    )?;
    assert_eq!(error["category"], "output_delivery_failed");
    let outcome = &error["operation_outcome"];
    assert_eq!(outcome["status"], "unavailable");
    assert_eq!(outcome["default_harness"], "alpha");
    assert_eq!(
        outcome["harnesses"]
            .as_array()
            .ok_or("missing harnesses")?
            .len(),
        1
    );
    let entry = listed_harness(outcome, "alpha")?;
    assert_eq!(entry["availability"], "unavailable");
    assert_eq!(entry["authentication"], "not_checked");
    assert_eq!(entry["runtime_status"], "not_probed");
    temp.clean()?;
    Ok(())
}

fn definition(id: &str, adapter: &str, address: &str, enabled: bool, root: &str) -> Value {
    json!({
        "id": id,
        "adapter": adapter,
        "address": address,
        "enabled": enabled,
        "workspace_roots": [root]
    })
}

fn write_harness_config(temp: &TestDirectory, config: Value) -> TestResult<PathBuf> {
    let path = temp.path.join("harnesses.json");
    fs::write(&path, serde_json::to_vec(&config)?)?;
    Ok(path)
}

fn path_text(path: &Path) -> TestResult<&str> {
    path.to_str().ok_or_else(|| "non-UTF8 test path".into())
}

fn listed_harness<'a>(report: &'a Value, id: &str) -> TestResult<&'a Value> {
    report["harnesses"]
        .as_array()
        .and_then(|entries| entries.iter().find(|entry| entry["id"] == id))
        .ok_or_else(|| format!("missing Harness {id}: {report}").into())
}

fn json_stdout(output: &Output, expected_code: i32) -> TestResult<Value> {
    assert_eq!(
        output.status.code(),
        Some(expected_code),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout)?;
    assert!(report.is_object(), "{report}");
    Ok(report)
}

fn json_error(output: &Output, expected_code: i32) -> TestResult<Value> {
    assert_eq!(
        output.status.code(),
        Some(expected_code),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let error: Value = serde_json::from_slice(&output.stderr)?;
    assert_eq!(error["status"], "failed", "{error}");
    assert!(
        error["category"]
            .as_str()
            .is_some_and(|category| !category.is_empty()),
        "{error}"
    );
    assert!(
        error["message"]
            .as_str()
            .is_some_and(|message| !message.is_empty()),
        "{error}"
    );
    assert_eq!(error["auto_retry"], false, "{error}");
    Ok(error)
}

async fn run_harness(command: &str, config: &Path, extras: &[&str]) -> TestResult<Output> {
    let mut args = vec!["harness", command, "--config", path_text(config)?];
    args.extend_from_slice(extras);
    run(&args).await
}

async fn run(args: &[&str]) -> TestResult<Output> {
    let mut child = ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_recuvora-host"))
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?,
    );
    // The CLI emits a fixed small report. Bound both reading and retained bytes;
    // readers also prevent stdout/stderr pipe backpressure from hanging the child.
    let stdout = child.0.stdout.take().ok_or("missing stdout")?;
    let stderr = child.0.stderr.take().ok_or("missing stderr")?;
    let read_out = tokio::task::spawn_blocking(move || read_output(stdout));
    let read_err = tokio::task::spawn_blocking(move || read_output(stderr));
    let status = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Some(status) = child.0.try_wait()? {
                return Ok::<_, io::Error>(status);
            }
            tokio::task::yield_now().await;
        }
    })
    .await??;
    let stdout = tokio::time::timeout(Duration::from_secs(2), read_out).await???;
    let stderr = tokio::time::timeout(Duration::from_secs(2), read_err).await???;
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

fn read_output(reader: impl Read) -> io::Result<Vec<u8>> {
    let mut output = Vec::new();
    reader.take(64 * 1024).read_to_end(&mut output)?;
    Ok(output)
}

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct TestDirectory {
    path: PathBuf,
    root: PathBuf,
}
impl TestDirectory {
    fn create() -> TestResult<Self> {
        let root = fs::canonicalize(
            std::env::var_os("RECUVORA_TEST_TEMP").ok_or("set RECUVORA_TEST_TEMP")?,
        )?;
        let project = fs::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")))?;
        if root.starts_with(project) {
            return Err("test data must be outside the project".into());
        }
        let path = root.join(format!(
            "cli-{}-{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
        ));
        fs::create_dir(&path)?;
        Ok(Self { path, root })
    }
    fn clean(&self) -> io::Result<()> {
        let resolved = match fs::canonicalize(&self.path) {
            Ok(path) => path,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        if resolved.parent() != Some(self.root.as_path()) {
            return Err(io::Error::other("test path changed"));
        }
        fs::remove_dir_all(resolved)
    }
}
impl Drop for TestDirectory {
    fn drop(&mut self) {
        if let Err(error) = self.clean() {
            eprintln!("test cleanup failed: {error}");
        }
    }
}
