//! Trusted provider assembly. User input and output stay in `cli`.
use crate::application::{HostError, HostRuntime};
use crate::cli::{self, HarnessCommand};
use crate::configuration::{self, HostConfig};
use crate::harnesses::{
    HarnessCancellation, HarnessDefinition, HarnessError, HarnessRegistry, HarnessRegistryBuilder,
    HarnessRegistryConfig,
};
use crate::integrations::extensions::ExtensionRegistry;
use crate::integrations::harness::RemoteHarnessFactory;
use serde_json::{Value, json};
use std::ffi::OsString;
use std::io::{self, Write};
use std::process::ExitCode;
use std::sync::Arc;

pub(super) async fn run(args: impl IntoIterator<Item = OsString>) -> ExitCode {
    let args = match cli::parse_harness(args) {
        Ok(Some(args)) => args,
        Ok(None) => {
            return match io::stdout().lock().write_all(cli::HELP.as_bytes()) {
                Ok(()) => ExitCode::SUCCESS,
                Err(_) => ExitCode::FAILURE,
            };
        }
        Err(message) => {
            let _ = write_json(
                io::stderr().lock(),
                &json!({"status":"failed", "category":"invalid_arguments",
                    "message":message, "auto_retry":false}),
            );
            return ExitCode::from(2);
        }
    };
    let config = match configuration::load_host_harness_config(&args.config) {
        Ok(config) => config,
        Err(error) => return report_error(&error),
    };
    let extensions = if let Some(path) = &args.extensions {
        match configuration::load_extensions_config(path) {
            Ok(config) => Some(config),
            Err(error) => {
                return report_error(&HarnessError::InvalidConfiguration(error.to_string()));
            }
        }
    } else {
        None
    };
    let listing = matches!(args.command, HarnessCommand::List);
    let selected = if listing {
        None
    } else {
        match select_configuration(&config, args.harness.as_deref()) {
            Ok(config) => Some(config),
            Err(error) => return report_error(&error),
        }
    };
    let mut host = match HostRuntime::start(HostConfig {
        harnesses: selected,
        extensions,
        monitoring: None,
    })
    .await
    {
        Ok(host) => host,
        Err(error) => return report_error(&host_error(error)),
    };
    if listing {
        let (report, available) = inspect_configuration(config, host.extensions());
        if let Err(error) = host.shutdown().await {
            return report_error(&host_error(error));
        }
        return report_value(&report, available);
    }
    let registry = match host.harnesses() {
        Some(registry) => registry,
        None => {
            let cleanup = host.shutdown().await;
            return report_error(&HarnessError::Unavailable {
                harness: "host".into(),
                message: match cleanup {
                    Ok(()) => "Harness registry was not published".into(),
                    Err(error) => {
                        format!("Harness registry was not published; shutdown failed: {error}")
                    }
                },
            });
        }
    };
    let cancellation = HarnessCancellation::new();
    let operation = cli::execute(&registry, &args, cancellation.clone());
    tokio::pin!(operation);
    // Poll signal registration first, before the operation can dispatch. On
    // Ctrl-C keep polling the operation: its provider owns cleanup and the
    // final outcome, including objects that may already have been created.
    let result = tokio::select! {
        biased;
        signal = tokio::signal::ctrl_c() => {
            cancellation.cancel();
            let result = operation.await;
            match (signal, result) {
                (_, Err(error)) => Err(error),
                (Ok(()), Ok(value)) => Ok(value),
                (Err(error), Ok(_)) => Err(HarnessError::Unavailable {
                    harness: registry.default_harness().unwrap_or("selected").to_owned(),
                    message: format!("cannot listen for Ctrl-C: {error}"),
                }),
            }
        }
        result = &mut operation => result,
    };
    if let Err(error) = host.shutdown().await {
        let outcome = match &result {
            Ok(value) => value.clone(),
            Err(error) => crate::presentation::error_json(error),
        };
        let status = if outcome["status"] == "unknown" {
            "unknown"
        } else {
            "failed"
        };
        return report_value(
            &json!({"status":status,"category":"shutdown_failed","message":error.to_string(),"shutdown_error":error.to_string(),"operation_outcome":outcome,"auto_retry":false}),
            false,
        );
    }
    match result {
        Ok(value) => report_value(&value, true),
        Err(error) => report_error(&error),
    }
}

fn build_one(
    definition: HarnessDefinition,
    extensions: Option<Arc<ExtensionRegistry>>,
) -> Result<HarnessRegistry, HarnessError> {
    let mut builder = HarnessRegistryBuilder::new();
    if let Some(extensions) = extensions {
        builder.register(Arc::new(RemoteHarnessFactory::new(extensions)))?;
    }
    builder.build(HarnessRegistryConfig {
        schema_version: crate::harnesses::CONFIG_SCHEMA_VERSION,
        default_harness: definition.enabled.then(|| definition.id.clone()),
        harnesses: vec![definition],
    })
}

fn select_configuration(
    config: &HarnessRegistryConfig,
    selected: Option<&str>,
) -> Result<HarnessRegistryConfig, HarnessError> {
    let id = selected
        .or(config.default_harness.as_deref())
        .ok_or(HarnessError::NoDefaultHarness)?;
    let definition = config
        .harnesses
        .iter()
        .find(|definition| definition.id == id)
        .ok_or_else(|| HarnessError::UnknownHarness(id.to_owned()))?;
    if !definition.enabled {
        return Err(HarnessError::HarnessDisabled(id.to_owned()));
    }
    Ok(HarnessRegistryConfig {
        schema_version: config.schema_version,
        default_harness: Some(id.to_owned()),
        harnesses: vec![definition.clone()],
    })
}

fn inspect_configuration(
    config: HarnessRegistryConfig,
    extensions: Option<Arc<ExtensionRegistry>>,
) -> (Value, bool) {
    let mut all_enabled_available = true;
    let mut harnesses = Vec::with_capacity(config.harnesses.len());
    for definition in config.harnesses {
        let assembly = build_one(definition.clone(), extensions.clone());
        let (roots, reason) = match assembly {
            Ok(registry) => (
                registry.definitions().remove(0).workspace_roots,
                Value::Null,
            ),
            Err(error) => (
                definition.workspace_roots.clone(),
                crate::presentation::error_json(&error),
            ),
        };
        let availability = if !definition.enabled {
            "disabled"
        } else if reason.is_null() {
            "configured"
        } else {
            all_enabled_available = false;
            "unavailable"
        };
        harnesses.push(json!({
            "id": definition.id,
            "adapter": definition.adapter,
            "address": definition.address,
            "enabled": definition.enabled,
            "is_default": config.default_harness.as_deref() == Some(&definition.id),
            "workspace_roots": roots.iter().map(|path| path.to_string_lossy()).collect::<Vec<_>>(),
            "availability": availability,
            "authentication": "not_checked",
            "runtime_status": "not_probed",
            "reason": reason,
        }));
    }
    (
        json!({
            "status": if all_enabled_available { "ok" } else { "unavailable" },
            "default_harness": config.default_harness,
            "harnesses": harnesses,
        }),
        all_enabled_available,
    )
}

fn host_error(error: HostError) -> HarnessError {
    match error {
        HostError::Harness(error) => error,
        error => HarnessError::Unavailable {
            harness: "host".into(),
            message: error.to_string(),
        },
    }
}

fn report_error(error: &HarnessError) -> ExitCode {
    let report = crate::presentation::error_json(error);
    if write_json(io::stderr().lock(), &report).is_err() {
        let _ = write_json(io::stdout().lock(), &report);
    }
    ExitCode::from(match error {
        HarnessError::ConversationOutcomeUnknown(_)
        | HarnessError::ProjectCreationOutcomeUnknown(_) => 3,
        HarnessError::Interrupted { .. } => 130,
        _ => 1,
    })
}

fn report_value(value: &Value, success: bool) -> ExitCode {
    match write_json(io::stdout().lock(), value) {
        Ok(()) if value["status"] == "unknown" => ExitCode::from(3),
        Ok(()) if success => ExitCode::SUCCESS,
        Ok(()) => ExitCode::FAILURE,
        Err(error) => {
            // The operation has finished. Preserve its known outcome even if
            // its normal output pipe was closed; never rerun it for delivery.
            let _ = write_json(
                io::stderr().lock(),
                &json!({
                    "status": "failed",
                    "category": "output_delivery_failed",
                    "message": format!("operation completed but stdout could not be written: {error}"),
                    "auto_retry": false,
                    "operation_outcome": value,
                }),
            );
            ExitCode::FAILURE
        }
    }
}

// JSON encoding escapes terminal controls in provider text and errors. Avoid
// println!: broken pipes must return failure rather than panic after a call.
fn write_json(mut writer: impl Write, value: &Value) -> io::Result<()> {
    serde_json::to_writer(&mut writer, value)?;
    writer.write_all(b"\n")?;
    writer.flush()
}
