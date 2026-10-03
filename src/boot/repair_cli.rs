//! Trusted assembly and external configuration for approval-controlled repair.
use crate::application::HostRuntime;
use crate::cli::repair::{self, Arguments};
use crate::configuration::{self, HostConfig};
use crate::harnesses::HarnessCancellation;
use crate::persistence::approval::{
    ApprovalDecision, ApprovalStore, ApprovalStoreConfig, ReviewerConfig,
};
use crate::repair::{RepairSession, WorkflowError, now};
use serde_json::{Value, json};
use std::ffi::OsString;
use std::io::{self, Write};
use std::process::ExitCode;

pub(super) async fn run(args: impl IntoIterator<Item = OsString>) -> ExitCode {
    let args = match repair::parse(args) {
        Ok(Some(args)) => args,
        Ok(None) => {
            return if io::stdout()
                .lock()
                .write_all(repair::HELP.as_bytes())
                .is_ok()
            {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            };
        }
        Err(message) => {
            return report(
                json!({"status":"failed","message":message,"auto_retry":false}),
                2,
            );
        }
    };
    let token = HarnessCancellation::new();
    let operation = execute(args, token.clone());
    tokio::pin!(operation);
    let result = tokio::select! {
        biased;
        signal = tokio::signal::ctrl_c() => {
            token.cancel();
            let result = operation.await;
            if let Err(error) = signal { Err(WorkflowError::Io(error)) } else {result}
        }
        result = &mut operation => result,
    };
    match result {
        Ok(value) => {
            let code = match value.get("status").and_then(Value::as_str) {
                Some("unknown") => 3,
                Some("waiting_human") => 4,
                Some("canceled") => 130,
                Some("failed" | "blocked") => 1,
                _ => 0,
            };
            report(value, code)
        }
        Err(error) => report(
            workflow_error_json(&error),
            if matches!(error, WorkflowError::OutcomeUnknown { .. }) {
                3
            } else {
                1
            },
        ),
    }
}

async fn execute(args: Arguments, token: HarnessCancellation) -> Result<Value, WorkflowError> {
    let needs_target = matches!(args.command.as_str(), "run" | "apply" | "check-result");
    let (config, protected) = configuration::load_host_repair_config(&args.config, needs_target)?;
    // Inspection and decisions remain available even if a target was deleted,
    // became binary, or no longer satisfies execution preconditions.
    if matches!(
        args.command.as_str(),
        "inspect" | "approve" | "deny" | "revoke"
    ) {
        let mut store =
            ApprovalStore::open(&config.data_dir, ApprovalStoreConfig::default(), now()?)?;
        if args.command == "inspect" {
            return Ok(match args.request {
                Some(id) => {
                    json!({"status":"ok","record":store.get(&id).ok_or(crate::persistence::approval::ApprovalError::NotFound)?})
                }
                None => json!({"status":"ok","records":store.list()}),
            });
        }
        let id = args
            .request
            .as_deref()
            .ok_or_else(|| invalid("request required"))?;
        let reason = args.reason.ok_or_else(|| invalid("reason required"))?;
        let record = if args.command == "revoke" {
            store.revoke(id, reason, now()?)?
        } else {
            store.decide_human(
                id,
                if args.command == "approve" {
                    ApprovalDecision::Approve
                } else {
                    ApprovalDecision::Deny
                },
                reason,
                "local-cli-operator".into(),
                &config.policy,
                now()?,
            )?
        };
        return Ok(json!({"status":"ok","record":record,"executed":false}));
    }
    let mut host = if args.command == "run" {
        let mut registry_config = configuration::load_host_harness_config(&config.harness_config)?;
        registry_config.validate()?;
        let mut ids = vec![config.execution_harness.clone()];
        if let ReviewerConfig::Harness { harness_id }
        | ReviewerConfig::HumanThenHarness { harness_id, .. } = &config.policy.reviewer
        {
            ids.push(harness_id.clone());
        }
        for id in &ids {
            if !registry_config
                .harnesses
                .iter()
                .any(|h| h.id == *id && h.enabled)
            {
                return Err(invalid(format!(
                    "selected Harness {id} missing or disabled"
                )));
            }
        }
        registry_config.harnesses.retain(|h| ids.contains(&h.id));
        registry_config.default_harness = Some(config.execution_harness.clone());
        let extensions = config
            .extensions_config
            .as_ref()
            .map(|path| configuration::load_extensions_config(path))
            .transpose()
            .map_err(|error| invalid(error.to_string()))?;
        Some(
            HostRuntime::start(HostConfig {
                harnesses: Some(registry_config),
                extensions,
                monitoring: None,
            })
            .await
            .map_err(|error| invalid(error.to_string()))?,
        )
    } else {
        None
    };
    let result = async {
    let registry = host.as_ref().and_then(HostRuntime::harnesses);
    let session = RepairSession::open(config, registry, &protected)?;
    match args.command.as_str() {
        "run" => {
            let result = session
                .run(
                    args.task.ok_or_else(|| invalid("task required"))?,
                    args.prompt.ok_or_else(|| invalid("prompt required"))?,
                    token,
                )
                .await?;
            Ok(crate::presentation::repair_result_json(&result))
        }
        "apply" => {
            let record = session.apply(
                args.request
                    .as_deref()
                    .ok_or_else(|| invalid("request required"))?,
                &token,
            )?;
            Ok(
                json!({"status":match record.state {crate::persistence::approval::ApprovalState::Executed=>"completed",crate::persistence::approval::ApprovalState::Unknown=>"unknown",_=>"failed"},"record":record,"business_verified":false,"auto_retry":false}),
            )
        }
        "check-result" => Ok(
            json!({"status":"ok","record":session.check_result(args.request.as_deref().ok_or_else(||invalid("request required"))?)?,"write_repeated":false}),
        ),
        _ => Err(invalid("unsupported command")),
    }
    }.await;
    if let Some(host) = &mut host
        && let Err(error) = host.shutdown().await
    {
        let outcome = match result {
            Ok(value) => value,
            Err(original) => workflow_error_json(&original),
        };
        let status = if outcome["status"] == "unknown" {
            "unknown"
        } else {
            "failed"
        };
        return Ok(json!({"status":status,"category":"shutdown_failed",
            "message":error.to_string(),"shutdown_error":error.to_string(),
            "operation_outcome":outcome,"auto_retry":false}));
    }
    result
}

fn invalid(message: impl Into<String>) -> WorkflowError {
    WorkflowError::Invalid(message.into())
}

fn workflow_error_json(error: &WorkflowError) -> Value {
    match error {
        WorkflowError::OutcomeUnknown {
            request_id,
            message,
        } => {
            json!({"status":"unknown","request_id":request_id,"message":message,"auto_retry":false})
        }
        error => json!({"status":"failed","message":error.to_string(),"auto_retry":false,
            "recovery":"Inspect durable approval records before retrying any execution."}),
    }
}

fn report(value: Value, code: u8) -> ExitCode {
    let result = (|| -> io::Result<()> {
        let mut out = io::stdout().lock();
        serde_json::to_writer(&mut out, &value)?;
        out.write_all(b"\n")?;
        out.flush()
    })();
    if let Err(error) = result {
        let fallback = json!({"status":"failed","message":format!("output delivery failed: {error}"),"operation_outcome":value,"auto_retry":false});
        let mut err = io::stderr().lock();
        let _ = serde_json::to_writer(&mut err, &fallback);
        let _ = err.write_all(b"\n");
        ExitCode::FAILURE
    } else {
        ExitCode::from(code)
    }
}
