//! Application assembly for simulation, Harness text and approved text repair.
pub mod application;
mod harness_cli;
pub mod host;
mod repair_cli;

use crate::runtime::{
    InstanceId, LifecycleFuture, LifecycleOptions, Module, ModuleContext, ModuleError,
    ModuleMetadata, Runtime, RuntimeSnapshot, ServiceKey,
};
use crate::simulation::{Engine, EngineConfig, EngineHandle, Simulation, TaskSpec, TaskState};
use std::error::Error;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

type AppResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

const HELP: &str = "Recuvora Host: simulation, Harness text and approval-controlled repair

Usage:
  recuvora-host demo --data-dir <external-directory> [--concurrency <1..32>]
  recuvora-host inspect --data-dir <external-directory> --task <task-id>
  recuvora-host harness --help
  recuvora-host repair --help
  recuvora-host serve --config <external-config.json>
    Harness commands: list, projects, create-project, run (explicit --config).

The demo is simulation only: success, failure, unavailable result, timeout,
denied authorization, and cancellation. It does not invoke a Harness, commands,
network services, or desktop input. State is retained in the supplied directory.
The authenticated HTTP API serves the separately built official UI. Harness
providers run as independent network nodes selected by extension configuration.
";

/// Runs CLI composition inside the runtime owned by the executable entry point.
pub async fn run_cli() -> ExitCode {
    if std::env::args_os().nth(1).is_some_and(|arg| arg == "serve") {
        return match crate::server::run_cli(std::env::args_os().skip(2)).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("recuvora-host serve: {error}");
                ExitCode::FAILURE
            }
        };
    }
    if std::env::args_os()
        .nth(1)
        .is_some_and(|arg| arg == "repair")
    {
        return repair_cli::run(std::env::args_os().skip(2)).await;
    }
    if std::env::args_os()
        .nth(1)
        .is_some_and(|arg| arg == "harness")
    {
        return harness_cli::run(std::env::args_os().skip(2)).await;
    }
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("recuvora-host: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> AppResult {
    let Some(args) = Arguments::parse()? else {
        print!("{HELP}");
        return Ok(());
    };
    let data_dir = external_directory(&args.data_dir)?;
    let config = EngineConfig {
        max_concurrency: args.concurrency,
        ..EngineConfig::default()
    };
    let mut runtime = Runtime::new(LifecycleOptions {
        start_timeout: Duration::from_secs(30),
        stop_timeout: Duration::from_secs(10),
        ..LifecycleOptions::default()
    });
    // Register the consumer first: the dependency graph determines startup order.
    runtime.add(Box::new(ConsoleModule {
        command: args.command,
    }))?;
    runtime.add(Box::new(RecoveryModule {
        data_dir,
        config,
        engine: None,
    }))?;
    runtime.start().await?;
    let report = runtime.shutdown().await;
    if !report.is_clean() {
        return Err(io::Error::other(format!("shutdown reported: {:?}", report.issues)).into());
    }
    let remaining = runtime.snapshot()?;
    if remaining != RuntimeSnapshot::default() {
        return Err(io::Error::other(format!("shutdown left resources: {remaining:?}")).into());
    }
    println!("runtime: clean shutdown");
    Ok(())
}

fn task_service() -> ServiceKey {
    ServiceKey::new("recovery.simulation", 1, "local")
}

struct RecoveryModule {
    data_dir: PathBuf,
    config: EngineConfig,
    engine: Option<Engine>,
}

impl Module for RecoveryModule {
    fn metadata(&self) -> ModuleMetadata {
        ModuleMetadata::new(InstanceId::new("simulation-engine")).provides(task_service())
    }

    fn start<'a>(&'a mut self, context: &'a ModuleContext) -> LifecycleFuture<'a> {
        Box::pin(async move {
            let engine = Engine::open(&self.data_dir, self.config.clone())
                .await
                .map_err(module_error)?;
            let handle = engine.handle();
            // Retain ownership before publishing, so partial-start cleanup can stop it.
            self.engine = Some(engine);
            context.publish(task_service(), Arc::new(handle))?;
            Ok(())
        })
    }

    fn stop<'a>(&'a mut self, _context: &'a ModuleContext) -> LifecycleFuture<'a> {
        Box::pin(async move {
            if let Some(engine) = self.engine.take() {
                engine.shutdown().await.map_err(module_error)?;
            }
            Ok(())
        })
    }
}

enum ConsoleCommand {
    Demo,
    Inspect(String),
}

struct ConsoleModule {
    command: ConsoleCommand,
}

impl Module for ConsoleModule {
    fn metadata(&self) -> ModuleMetadata {
        ModuleMetadata::new(InstanceId::new("console")).requires(task_service())
    }

    fn start<'a>(&'a mut self, context: &'a ModuleContext) -> LifecycleFuture<'a> {
        Box::pin(async move {
            let service = context.service::<EngineHandle>(&task_service())?;
            match &self.command {
                ConsoleCommand::Demo => demonstrate(&service).await.map_err(module_error),
                ConsoleCommand::Inspect(id) => {
                    let snapshot = service
                        .query(id)
                        .await
                        .map_err(module_error)?
                        .ok_or_else(|| ModuleError::new(format!("unknown task: {id}")))?;
                    println!(
                        "task={} target={} state={:?} revision={}",
                        snapshot.spec.id, snapshot.spec.target, snapshot.state, snapshot.revision
                    );
                    Ok(())
                }
            }
        })
    }
}

// This is a CLI demonstration, not a second scheduler or task state machine.
async fn demonstrate(service: &EngineHandle) -> AppResult {
    let run_id = format!(
        "{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    );
    let scenarios = [
        ("success", Simulation::Succeed, TaskState::Succeeded),
        ("failure", Simulation::Fail, TaskState::Failed),
        ("unknown", Simulation::Exit, TaskState::Unknown),
        (
            "verification-failure",
            Simulation::VerificationFailed,
            TaskState::Failed,
        ),
        ("timeout", Simulation::Hang, TaskState::Unknown),
    ];
    let mut expectations = Vec::new();
    for (label, simulation, expected) in scenarios {
        let id = format!("{run_id}-{label}");
        service
            .submit(
                TaskSpec::simulated(&id, format!("{run_id}-{label}-target"), simulation)
                    .authorize_simulation()
                    .with_timeout(Duration::from_millis(500)),
            )
            .await?;
        expectations.push((id, expected));
    }
    let denied_id = format!("{run_id}-denied");
    service
        .submit(TaskSpec::simulated(
            &denied_id,
            format!("{run_id}-denied-target"),
            Simulation::Succeed,
        ))
        .await?;
    expectations.push((denied_id, TaskState::Denied));
    let cancel_id = format!("{run_id}-canceled");
    // Queue behind the hanging/Unknown target so this cancellation is pre-execution.
    service
        .submit(
            TaskSpec::simulated(
                &cancel_id,
                format!("{run_id}-timeout-target"),
                Simulation::Hang,
            )
            .authorize_simulation()
            .with_timeout(Duration::from_secs(5)),
        )
        .await?;
    service.cancel(&cancel_id).await?;
    expectations.push((cancel_id, TaskState::Canceled));

    for (id, expected) in expectations {
        let state = wait_terminal(service, &id).await?;
        println!("task={id} state={state:?}");
        if state != expected && !(id.ends_with("-timeout") && state == TaskState::TimedOut) {
            return Err(
                io::Error::other(format!("{id}: expected {expected:?}, got {state:?}")).into(),
            );
        }
    }
    // Demonstrate that completed fault cases do not prevent subsequent work.
    let final_id = format!("{run_id}-after-faults");
    service
        .submit(
            TaskSpec::simulated(
                &final_id,
                format!("{run_id}-fresh-target"),
                Simulation::Succeed,
            )
            .authorize_simulation(),
        )
        .await?;
    if wait_terminal(service, &final_id).await? != TaskState::Succeeded {
        return Err(io::Error::other("task after simulated faults did not succeed").into());
    }
    println!("task={final_id} state=Succeeded\ndemo: 8 expected outcomes verified");
    Ok(())
}

async fn wait_terminal(service: &EngineHandle, id: &str) -> AppResult<TaskState> {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let snapshot = service
                .query(id)
                .await?
                .ok_or_else(|| io::Error::other("accepted task disappeared"))?;
            if snapshot.state.is_terminal() {
                return Ok(snapshot.state);
            }
            // User-facing observation cadence; the scheduler is event-driven.
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?
}

fn module_error(error: impl std::fmt::Display) -> ModuleError {
    ModuleError::new(error.to_string())
}

struct Arguments {
    command: ConsoleCommand,
    data_dir: PathBuf,
    concurrency: usize,
}

impl Arguments {
    fn parse() -> AppResult<Option<Self>> {
        let mut values = std::env::args_os().skip(1);
        let Some(command) = values.next() else {
            return Ok(None);
        };
        if command == "--help" || command == "-h" {
            return Ok(None);
        }
        let mut data_dir = None;
        let mut task = None;
        let mut concurrency = 2;
        while let Some(flag) = values.next() {
            let value = values.next().ok_or_else(|| {
                io::Error::other(format!("missing value for {}", flag.to_string_lossy()))
            })?;
            match flag.to_str() {
                Some("--data-dir") if data_dir.is_none() => data_dir = Some(PathBuf::from(value)),
                Some("--task") if task.is_none() => {
                    task = Some(
                        value
                            .into_string()
                            .map_err(|_| io::Error::other("task id must be UTF-8"))?,
                    )
                }
                Some("--concurrency") => {
                    concurrency = value
                        .to_str()
                        .ok_or_else(|| io::Error::other("invalid concurrency"))?
                        .parse()?
                }
                _ => {
                    return Err(io::Error::other(format!(
                        "unknown or repeated option: {}",
                        flag.to_string_lossy()
                    ))
                    .into());
                }
            }
        }
        if !(1..=32).contains(&concurrency) {
            return Err(io::Error::other("concurrency must be between 1 and 32").into());
        }
        let command = match command.to_str() {
            Some("demo") if task.is_none() => ConsoleCommand::Demo,
            Some("inspect") => ConsoleCommand::Inspect(
                task.ok_or_else(|| io::Error::other("inspect requires --task"))?,
            ),
            _ => return Err(io::Error::other("expected demo or inspect; see --help").into()),
        };
        Ok(Some(Self {
            command,
            data_dir: data_dir.ok_or_else(|| io::Error::other("--data-dir is required"))?,
            concurrency,
        }))
    }
}

fn external_directory(path: &Path) -> AppResult<PathBuf> {
    Ok(host::prepare_runtime_directory(path)?)
}
