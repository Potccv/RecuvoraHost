//! Process-level checks for the simulated recovery core. This is not a
//! production worker supervisor or a real target/Harness integration test.
use recuvora_host::simulation::{Engine, EngineConfig, Simulation, TaskSpec, TaskState};
use std::error::Error;
use std::fs;
use std::io::{self, BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;
const DEADLINE: Duration = Duration::from_secs(10);

#[path = "process_fixture.rs"]
mod process_fixture;

#[tokio::test]
#[ignore = "child entry point for process-death injection"]
async fn fixture_process_entry() -> TestResult {
    // Running ignored tests directly must not inject a fault. Only the parent
    // tests select a fixture mode on their explicitly launched child process.
    let Some(mode) = std::env::var_os(process_fixture::MODE_ENV) else {
        return Ok(());
    };
    let mode = mode
        .to_str()
        .ok_or_else(|| io::Error::other("non-UTF8 process fixture mode"))?;
    process_fixture::run(mode).await
}

#[tokio::test]
async fn killed_host_recovers_unknown_without_replaying_and_accepts_new_target() -> TestResult {
    let temp = TestDirectory::create("killed-host")?;
    let state_dir = temp.path.join("state");
    let mut command = fixture_command("hold-executing")?;
    command.env(process_fixture::STATE_DIR_ENV, &state_dir);
    let mut fixture = ProcessGuard::spawn(&mut command)?;
    fixture.wait_ready("READY executing-persisted")?;
    fixture.child.kill()?;
    let status = fixture.wait_bounded().await?;
    require(
        !status.success(),
        "fault-injected host unexpectedly succeeded",
    )?;

    let engine = Engine::open(&state_dir, EngineConfig::default()).await?;
    let result = async {
        let recovered = engine
            .query("interrupted-task")
            .await?
            .ok_or_else(|| io::Error::other("persisted task is missing after restart"))?;
        require(
            recovered.state == TaskState::Unknown,
            "interrupted execution was not recovered as Unknown",
        )?;
        // The unresolved target stays blocked, while a later submission for
        // an independent target must still pass through the queue.
        engine
            .submit(
                TaskSpec::simulated(
                    "same-target-followup",
                    "interrupted-target",
                    Simulation::Succeed,
                )
                .authorize_simulation(),
            )
            .await?;
        engine
            .submit(
                TaskSpec::simulated("fresh-task", "fresh-target", Simulation::Succeed)
                    .authorize_simulation(),
            )
            .await?;
        require(
            wait_terminal(&engine, "fresh-task").await? == TaskState::Succeeded,
            "a new target did not complete after host recovery",
        )?;
        let unchanged = engine
            .query("interrupted-task")
            .await?
            .ok_or_else(|| io::Error::other("Unknown task disappeared"))?;
        require(
            unchanged.state == TaskState::Unknown && unchanged.revision == recovered.revision,
            "recovery dispatched or changed the unresolved task",
        )?;
        let blocked = engine
            .query("same-target-followup")
            .await?
            .ok_or_else(|| io::Error::other("blocked follow-up task disappeared"))?;
        require(
            blocked.state == TaskState::Queued,
            "a follow-up executed against an unresolved target",
        )
    }
    .await;
    // Always release the journal before the directory guard removes this run.
    let shutdown = engine.shutdown().await;
    result?;
    shutdown?;
    temp.clean()?;
    Ok(())
}

#[tokio::test]
async fn nonzero_child_exit_is_observed_and_same_host_completes_next_task() -> TestResult {
    let temp = TestDirectory::create("child-exit")?;
    let engine = Engine::open(temp.path.join("state"), EngineConfig::default()).await?;
    let result = async {
        let mut command = fixture_command("exit-17")?;
        let mut child = ProcessGuard::spawn(&mut command)?;
        let status = child.wait_bounded().await?;
        require(status.code() == Some(17), "child exit status was lost")?;
        engine
            .submit(
                TaskSpec::simulated("after-exit", "healthy-target", Simulation::Succeed)
                    .authorize_simulation(),
            )
            .await?;
        require(
            wait_terminal(&engine, "after-exit").await? == TaskState::Succeeded,
            "the simulation host did not progress after the child exited",
        )
    }
    .await;
    let shutdown = engine.shutdown().await;
    result?;
    shutdown?;
    temp.clean()?;
    Ok(())
}

async fn wait_terminal(engine: &Engine, task_id: &str) -> TestResult<TaskState> {
    tokio::time::timeout(DEADLINE, async {
        loop {
            let snapshot = engine
                .query(task_id)
                .await?
                .ok_or_else(|| io::Error::other("submitted task disappeared"))?;
            if snapshot.state.is_terminal() {
                return Ok(snapshot.state);
            }
            tokio::task::yield_now().await;
        }
    })
    .await?
}

fn fixture_command(mode: &str) -> TestResult<Command> {
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args([
            "--ignored",
            "--exact",
            "fixture_process_entry",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(process_fixture::MODE_ENV, mode)
        .env_remove(process_fixture::STATE_DIR_ENV)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    Ok(command)
}

struct ProcessGuard {
    child: Child,
}

impl ProcessGuard {
    fn spawn(command: &mut Command) -> TestResult<Self> {
        Ok(Self {
            child: command.spawn()?,
        })
    }

    fn wait_ready(&mut self, expected: &str) -> TestResult {
        let stdout = self
            .child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("fixture stdout was not piped"))?;
        let (sender, receiver) = mpsc::sync_channel(1);
        let expected = expected.to_owned();
        std::thread::spawn(move || {
            // libtest writes a short prelude before the fixture handshake.
            // Bound both the line count and total bytes while skipping it.
            let result = (|| -> io::Result<()> {
                let mut reader = BufReader::new(stdout.take(4096));
                let mut line = String::new();
                for _ in 0..16 {
                    line.clear();
                    if reader.read_line(&mut line)? == 0 {
                        break;
                    }
                    if line.trim_end() == expected {
                        return Ok(());
                    }
                }
                Err(io::Error::other(
                    "fixture readiness handshake was not received",
                ))
            })();
            let _ = sender.send(result);
        });
        receiver.recv_timeout(DEADLINE)??;
        Ok(())
    }

    async fn wait_bounded(&mut self) -> TestResult<ExitStatus> {
        tokio::time::timeout(DEADLINE, async {
            loop {
                if let Some(status) = self.child.try_wait()? {
                    return Ok(status);
                }
                tokio::task::yield_now().await;
            }
        })
        .await?
    }
}

impl Drop for ProcessGuard {
    fn drop(&mut self) {
        // Reap even on an assertion or handshake failure. kill followed by
        // wait also closes the stdout pipe and releases any readiness reader.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct TestDirectory {
    root: PathBuf,
    path: PathBuf,
}

impl TestDirectory {
    fn create(label: &str) -> TestResult<Self> {
        let root = std::env::var_os("RECUVORA_TEST_TEMP")
            .map(PathBuf::from)
            .ok_or_else(|| io::Error::other("set RECUVORA_TEST_TEMP outside the project"))?;
        // The scheduler creates and supplies the shared temporary root. Do not
        // create a fallback directory in the repository or system temp.
        let root = fs::canonicalize(root)?;
        let project = fs::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")))?;
        require(
            root.is_dir() && !root.starts_with(&project),
            "RECUVORA_TEST_TEMP must be a directory outside the project",
        )?;
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let serial = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = root.join(format!(
            "process-{label}-{}-{timestamp}-{serial}",
            std::process::id()
        ));
        fs::create_dir(&path)?;
        let path = fs::canonicalize(path)?;
        Ok(Self { root, path })
    }

    fn clean(self) -> TestResult {
        self.remove_owned()
    }

    fn remove_owned(&self) -> TestResult {
        // Remove only this test's recorded directory after checking its final
        // resolved location. Never enumerate or delete sibling test outputs.
        match fs::canonicalize(&self.path) {
            Ok(resolved) if resolved == self.path && resolved.parent() == Some(&self.root) => {
                fs::remove_dir_all(resolved)?;
                Ok(())
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            _ => Err(io::Error::other(format!(
                "refusing to clean changed test directory {}",
                self.path.display()
            ))
            .into()),
        }
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        if let Err(error) = self.remove_owned() {
            eprintln!(
                "failed to clean test directory {}: {error}",
                self.path.display()
            );
        }
    }
}

fn require(condition: bool, message: &str) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(io::Error::other(message).into())
    }
}
