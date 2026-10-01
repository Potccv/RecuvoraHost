//! Test-only host used for explicit process-death injection.
use recuvora_host::simulation::{Engine, EngineConfig, Simulation, TaskSpec, TaskState};
use std::error::Error;
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::Duration;

type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

pub const MODE_ENV: &str = "RECUVORA_PROCESS_FIXTURE_MODE";
pub const STATE_DIR_ENV: &str = "RECUVORA_PROCESS_FIXTURE_STATE_DIR";

pub async fn run(mode: &str) -> TestResult {
    match mode {
        "exit-17" => std::process::exit(17),
        "hold-executing" => {
            let state_dir = std::env::var_os(STATE_DIR_ENV)
                .map(PathBuf::from)
                .ok_or_else(|| io::Error::other("missing test state directory"))?;
            hold_executing(state_dir).await
        }
        _ => Err(io::Error::other("unknown process fixture mode").into()),
    }
}

async fn hold_executing(state_dir: PathBuf) -> TestResult {
    let engine = Engine::open(state_dir, EngineConfig::default()).await?;
    engine
        .submit(
            TaskSpec::simulated("interrupted-task", "interrupted-target", Simulation::Hang)
                .authorize_simulation()
                .with_timeout(Duration::from_secs(60)),
        )
        .await?;

    // Executing is acknowledged only after the execution intent is durable.
    // The parent uses this event, rather than timing guesses, to inject death.
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let snapshot = engine
                .query("interrupted-task")
                .await?
                .ok_or_else(|| io::Error::other("submitted task disappeared"))?;
            if snapshot.state == TaskState::Executing {
                return Ok::<(), Box<dyn Error + Send + Sync>>(());
            }
            if snapshot.state.is_terminal() {
                return Err(io::Error::other(format!(
                    "task became terminal before the injection point: {:?}",
                    snapshot.state
                ))
                .into());
            }
            tokio::task::yield_now().await;
        }
    })
    .await??;
    // Start a fresh line even if libtest has printed its test-name prefix.
    println!("\nREADY executing-persisted");
    io::stdout().flush()?;

    // A guard deadline prevents an orphaned fixture from living indefinitely
    // if the parent itself fails. Successful tests kill this process at READY.
    let _ = tokio::time::timeout(Duration::from_secs(30), std::future::pending::<()>()).await;
    engine.shutdown().await?;
    Err(io::Error::other("parent did not terminate the fixture before its deadline").into())
}
