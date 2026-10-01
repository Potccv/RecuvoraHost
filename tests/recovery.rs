use std::{
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use recuvora_host::simulation::{
    Engine, EngineConfig, EngineError, Simulation, TaskSnapshot, TaskSpec, TaskState,
};

static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

struct TestDir {
    path: PathBuf,
    root: PathBuf,
}

impl TestDir {
    fn new(name: &str) -> Self {
        let root = PathBuf::from(
            std::env::var_os("RECUVORA_TEST_TEMP")
                .expect("set RECUVORA_TEST_TEMP to an external test directory"),
        );
        assert!(root.is_absolute(), "test root must be absolute");
        std::fs::create_dir_all(&root).expect("create external test root");
        let root = root.canonicalize().expect("resolve test root");
        let project = Path::new(env!("CARGO_MANIFEST_DIR"))
            .canonicalize()
            .expect("resolve project root");
        assert!(
            !root.starts_with(project),
            "test data cannot be inside the project"
        );
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let path = root.join(format!(
            "recovery-{name}-{}-{stamp}-{}",
            std::process::id(),
            NEXT_DIR.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).expect("create unique test directory");
        Self { path, root }
    }

    fn journal(&self) -> PathBuf {
        self.path.join("tasks.jsonl")
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        // Only the unique, recorded child created above is ever removed.
        if self.path.parent() == Some(self.root.as_path())
            && self.path.starts_with(&self.root)
            && let Err(error) = std::fs::remove_dir_all(&self.path)
            && !std::thread::panicking()
        {
            panic!("test directory cleanup failed: {error}");
        }
    }
}

fn task(id: &str, target: &str, simulation: Simulation) -> TaskSpec {
    TaskSpec::simulated(id, target, simulation).authorize_simulation()
}

async fn until(engine: &Engine, id: &str, expected: impl Fn(TaskState) -> bool) -> TaskSnapshot {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let snapshot = engine
                .query(id)
                .await
                .expect("query live engine")
                .expect("task exists");
            if expected(snapshot.state) {
                return snapshot;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("task made progress within deadline")
}

#[tokio::test]
async fn success_is_durable_and_duplicate_ids_do_not_repeat_work() {
    let dir = TestDir::new("success");
    let engine = Engine::open(&dir.path, EngineConfig::default())
        .await
        .expect("open");
    let spec = task("first", "target", Simulation::Succeed);
    let handle = engine.handle();
    let (first, duplicate) = tokio::join!(engine.submit(spec.clone()), handle.submit(spec.clone()));
    assert_eq!(
        first.expect("first accepted").spec,
        duplicate.expect("duplicate accepted").spec
    );
    let completed = until(&engine, "first", TaskState::is_terminal).await;
    assert_eq!(completed.state, TaskState::Succeeded);
    assert_eq!(completed.revision, 4);
    assert_eq!(
        engine
            .submit(spec.clone())
            .await
            .expect("durable duplicate"),
        completed
    );
    assert!(matches!(
        engine
            .submit(task("first", "different", Simulation::Succeed))
            .await,
        Err(EngineError::Conflict)
    ));
    engine.shutdown().await.expect("shutdown");
    assert!(matches!(
        handle.query("first").await,
        Err(EngineError::Unavailable)
    ));
    let reopened = Engine::open(&dir.path, EngineConfig::default())
        .await
        .expect("reopen");
    assert_eq!(
        reopened
            .submit(spec)
            .await
            .expect("duplicate after restart"),
        completed
    );
    reopened.shutdown().await.expect("shutdown reopened");
    assert_eq!(
        std::fs::read_to_string(dir.journal())
            .expect("journal")
            .lines()
            .count(),
        5
    );
}

#[tokio::test]
async fn missing_simulation_authorization_is_a_durable_denial() {
    let dir = TestDir::new("denied");
    let engine = Engine::open(&dir.path, EngineConfig::default())
        .await
        .expect("open");
    let denied = engine
        .submit(TaskSpec::simulated("denied", "target", Simulation::Succeed))
        .await
        .expect("denial recorded");
    assert_eq!(denied.state, TaskState::Denied);
    assert_eq!(denied.revision, 0);
    engine.shutdown().await.expect("shutdown");
    let reopened = Engine::open(&dir.path, EngineConfig::default())
        .await
        .expect("reopen");
    assert_eq!(reopened.query("denied").await.expect("query"), Some(denied));
    reopened.shutdown().await.expect("shutdown reopened");
    assert_eq!(
        std::fs::read_to_string(dir.journal())
            .expect("journal")
            .lines()
            .count(),
        1
    );
}

#[tokio::test]
async fn failed_verification_and_lost_receipts_do_not_poison_later_tasks() {
    let dir = TestDir::new("failures");
    let config = EngineConfig {
        max_concurrency: 1,
        ..EngineConfig::default()
    };
    let engine = Engine::open(&dir.path, config).await.expect("open");
    for (id, simulation, expected) in [
        ("reported-failure", Simulation::Fail, TaskState::Failed),
        (
            "bad-verification",
            Simulation::VerificationFailed,
            TaskState::Failed,
        ),
        ("lost-receipt", Simulation::Exit, TaskState::Unknown),
        ("healthy", Simulation::Succeed, TaskState::Succeeded),
    ] {
        engine
            .submit(task(
                id,
                if id == "healthy" {
                    "new-target"
                } else {
                    "shared-target"
                },
                simulation,
            ))
            .await
            .expect("submit");
        assert_eq!(
            until(&engine, id, TaskState::is_terminal).await.state,
            expected
        );
    }
    engine.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn target_waiters_do_not_block_other_targets_and_unknown_stays_blocked() {
    let dir = TestDir::new("fairness");
    let config = EngineConfig {
        max_concurrency: 2,
        ..EngineConfig::default()
    };
    let engine = Engine::open(&dir.path, config).await.expect("open");
    engine
        .submit(task("holding", "a", Simulation::Hang))
        .await
        .expect("holding accepted");
    until(&engine, "holding", |state| state == TaskState::Executing).await;
    engine
        .submit(task("waiting", "a", Simulation::Succeed))
        .await
        .expect("same-target waiter");
    engine
        .submit(task("independent", "b", Simulation::Succeed))
        .await
        .expect("other target");
    assert_eq!(
        until(&engine, "independent", TaskState::is_terminal)
            .await
            .state,
        TaskState::Succeeded
    );
    assert_eq!(
        engine
            .query("waiting")
            .await
            .expect("query")
            .expect("waiting exists")
            .state,
        TaskState::Queued
    );
    let canceled = tokio::time::timeout(Duration::from_secs(5), engine.cancel("holding"))
        .await
        .expect("cancel not blocked")
        .expect("cancel")
        .expect("holding exists");
    assert_eq!(canceled.state, TaskState::Unknown);
    engine
        .submit(task("after-cancel", "b", Simulation::Succeed))
        .await
        .expect("submit other target after cancel");
    assert_eq!(
        until(&engine, "after-cancel", TaskState::is_terminal)
            .await
            .state,
        TaskState::Succeeded
    );
    assert_eq!(
        engine
            .query("waiting")
            .await
            .expect("query")
            .expect("waiting exists")
            .state,
        TaskState::Queued
    );
    assert_eq!(
        engine
            .cancel("waiting")
            .await
            .expect("cancel queued")
            .expect("waiting exists")
            .state,
        TaskState::Canceled
    );
    engine.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn running_timeout_is_unknown_and_does_not_hold_the_execution_slot() {
    let dir = TestDir::new("timeout");
    let config = EngineConfig {
        max_concurrency: 1,
        ..EngineConfig::default()
    };
    let engine = Engine::open(&dir.path, config).await.expect("open");
    engine
        .submit(task("timeout", "target", Simulation::Hang).with_timeout(Duration::from_secs(1)))
        .await
        .expect("submit hang");
    until(&engine, "timeout", |state| state == TaskState::Executing).await;
    engine
        .submit(task("next", "different-target", Simulation::Succeed))
        .await
        .expect("submit next");
    assert_eq!(
        until(&engine, "timeout", TaskState::is_terminal)
            .await
            .state,
        TaskState::Unknown
    );
    assert_eq!(
        until(&engine, "next", TaskState::is_terminal).await.state,
        TaskState::Succeeded
    );
    engine.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn queue_and_history_are_bounded_and_queued_cancel_never_executes() {
    let dir = TestDir::new("capacity");
    let config = EngineConfig {
        max_concurrency: 1,
        max_queued: 1,
        max_tasks: 3,
        ..EngineConfig::default()
    };
    let engine = Engine::open(&dir.path, config).await.expect("open");
    engine
        .submit(task("running", "a", Simulation::Hang))
        .await
        .expect("running");
    until(&engine, "running", |state| state == TaskState::Executing).await;
    engine
        .submit(task("queued", "b", Simulation::Succeed))
        .await
        .expect("queued");
    assert!(matches!(
        engine
            .submit(task("overflow", "c", Simulation::Succeed))
            .await,
        Err(EngineError::Capacity)
    ));
    assert_eq!(
        engine
            .cancel("queued")
            .await
            .expect("cancel queued")
            .expect("queued exists")
            .state,
        TaskState::Canceled
    );
    engine
        .submit(TaskSpec::simulated("denied", "d", Simulation::Succeed))
        .await
        .expect("third history record");
    assert!(matches!(
        engine
            .submit(TaskSpec::simulated(
                "history-overflow",
                "e",
                Simulation::Succeed
            ))
            .await,
        Err(EngineError::Capacity)
    ));
    assert!(
        engine
            .cancel("absent")
            .await
            .expect("absent cancel")
            .is_none()
    );
    tokio::time::timeout(Duration::from_secs(5), engine.shutdown())
        .await
        .expect("shutdown reaps hang")
        .expect("shutdown");
    let reopened = Engine::open(&dir.path, EngineConfig::default())
        .await
        .expect("reopen");
    assert_eq!(
        reopened
            .query("running")
            .await
            .expect("query")
            .expect("running exists")
            .state,
        TaskState::Unknown
    );
    assert_eq!(
        reopened
            .query("queued")
            .await
            .expect("query")
            .expect("queued exists")
            .state,
        TaskState::Canceled
    );
    reopened.shutdown().await.expect("shutdown reopened");
}

#[tokio::test]
async fn runtime_directory_has_one_writer_and_unlocks_after_shutdown() {
    let dir = TestDir::new("writer-lock");
    let engine = Engine::open(&dir.path, EngineConfig::default())
        .await
        .expect("first writer");
    assert!(matches!(
        Engine::open(&dir.path, EngineConfig::default()).await,
        Err(EngineError::Locked(_))
    ));
    engine.shutdown().await.expect("shutdown");
    let reopened = Engine::open(&dir.path, EngineConfig::default())
        .await
        .expect("lock released");
    reopened.shutdown().await.expect("shutdown reopened");
}

#[tokio::test]
async fn unacknowledged_tail_is_truncated_but_complete_corruption_is_rejected() {
    let dir = TestDir::new("corruption");
    let engine = Engine::open(&dir.path, EngineConfig::default())
        .await
        .expect("open");
    engine
        .submit(TaskSpec::simulated("denied", "target", Simulation::Succeed))
        .await
        .expect("record");
    engine.shutdown().await.expect("shutdown");
    let original_len = std::fs::metadata(dir.journal()).expect("metadata").len();
    {
        let mut file = OpenOptions::new()
            .append(true)
            .open(dir.journal())
            .expect("open tail");
        file.write_all(b"{\"format\":1,\"sequence\":2")
            .expect("partial write");
        file.sync_all().expect("sync test fixture");
    }
    let reopened = Engine::open(&dir.path, EngineConfig::default())
        .await
        .expect("recover incomplete tail");
    reopened.shutdown().await.expect("shutdown reopened");
    assert_eq!(
        std::fs::metadata(dir.journal()).expect("metadata").len(),
        original_len
    );
    {
        let mut file = OpenOptions::new()
            .append(true)
            .open(dir.journal())
            .expect("open tail");
        file.write_all(b"not-json\n")
            .expect("complete corrupt record");
        file.sync_all().expect("sync test fixture");
    }
    assert!(matches!(
        Engine::open(&dir.path, EngineConfig::default()).await,
        Err(EngineError::Corrupt(_))
    ));
    assert_eq!(
        std::fs::metadata(dir.journal()).expect("metadata").len(),
        original_len + 9
    );
}

#[tokio::test]
async fn exhausted_journal_does_not_acknowledge_or_execute_new_work() {
    let dir = TestDir::new("journal-limit");
    let config = EngineConfig {
        max_journal_bytes: 128,
        ..EngineConfig::default()
    };
    let engine = Engine::open(&dir.path, config).await.expect("open");
    assert!(matches!(
        engine
            .submit(task("unaccepted", "target", Simulation::Succeed))
            .await,
        Err(EngineError::JournalFull)
    ));
    assert!(matches!(
        engine.query("unaccepted").await,
        Err(EngineError::Unavailable)
    ));
    assert!(engine.shutdown().await.is_err());
    let reopened = Engine::open(&dir.path, EngineConfig::default())
        .await
        .expect("open healthy store");
    assert!(reopened.query("unaccepted").await.expect("query").is_none());
    reopened
        .submit(task("healthy", "target", Simulation::Succeed))
        .await
        .expect("submit healthy");
    assert_eq!(
        until(&reopened, "healthy", TaskState::is_terminal)
            .await
            .state,
        TaskState::Succeeded
    );
    reopened.shutdown().await.expect("shutdown reopened");
}

#[tokio::test]
async fn storage_limit_during_dispatch_preserves_acceptance_without_replaying() {
    let dir = TestDir::new("dispatch-storage-limit");
    let spec = task("accepted", "target", Simulation::Hang);
    let first_line = serde_json::to_vec(&serde_json::json!({
        "format": 1,
        "sequence": 1,
        "snapshot": TaskSnapshot { spec: spec.clone(), state: TaskState::Queued, revision: 0 },
    }))
    .expect("encode size fixture");
    let config = EngineConfig {
        max_journal_bytes: first_line.len() as u64 + 1,
        ..EngineConfig::default()
    };
    let engine = Engine::open(&dir.path, config).await.expect("open");
    assert_eq!(
        engine
            .submit(spec)
            .await
            .expect("acceptance is durable")
            .state,
        TaskState::Queued
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        while engine.query("accepted").await.is_ok() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("dispatch stops after storage failure");
    assert!(engine.shutdown().await.is_err());
    let reopened = Engine::open(&dir.path, EngineConfig::default())
        .await
        .expect("open with sufficient journal capacity");
    assert_eq!(
        reopened
            .query("accepted")
            .await
            .expect("query")
            .expect("durable task exists")
            .state,
        TaskState::Canceled
    );
    reopened
        .submit(task("followup", "target", Simulation::Succeed))
        .await
        .expect("unexecuted task did not reserve target");
    assert_eq!(
        until(&reopened, "followup", TaskState::is_terminal)
            .await
            .state,
        TaskState::Succeeded
    );
    reopened.shutdown().await.expect("shutdown reopened");
}

#[tokio::test]
async fn illegal_complete_journal_transition_is_rejected_without_repair() {
    let dir = TestDir::new("transition-corruption");
    let spec = TaskSpec::simulated("denied", "target", Simulation::Succeed);
    let engine = Engine::open(&dir.path, EngineConfig::default())
        .await
        .expect("open");
    engine.submit(spec.clone()).await.expect("denial recorded");
    engine.shutdown().await.expect("shutdown");
    let mut forged = serde_json::to_vec(&serde_json::json!({
        "format": 1,
        "sequence": 2,
        "snapshot": TaskSnapshot { spec, state: TaskState::Executing, revision: 1 },
    }))
    .expect("encode corrupt fixture");
    forged.push(b'\n');
    {
        let mut file = OpenOptions::new()
            .append(true)
            .open(dir.journal())
            .expect("open journal");
        file.write_all(&forged).expect("append invalid transition");
        file.sync_all().expect("sync test fixture");
    }
    let before = std::fs::read(dir.journal()).expect("read corruption fixture");
    assert!(matches!(
        Engine::open(&dir.path, EngineConfig::default()).await,
        Err(EngineError::Corrupt(_))
    ));
    assert_eq!(
        std::fs::read(dir.journal()).expect("read unchanged fixture"),
        before
    );
}

#[tokio::test]
async fn invalid_inputs_do_not_stop_a_healthy_engine() {
    let dir = TestDir::new("validation");
    assert!(matches!(
        Engine::open(
            &dir.path,
            EngineConfig {
                max_concurrency: 0,
                ..EngineConfig::default()
            }
        )
        .await,
        Err(EngineError::Invalid(_))
    ));
    let engine = Engine::open(&dir.path, EngineConfig::default())
        .await
        .expect("open");
    assert!(matches!(
        engine.submit(task("", "target", Simulation::Succeed)).await,
        Err(EngineError::Invalid(_))
    ));
    assert!(matches!(
        engine
            .submit(task("timeout", "target", Simulation::Succeed).with_timeout(Duration::ZERO))
            .await,
        Err(EngineError::Invalid(_))
    ));
    assert!(matches!(
        engine.query(&"x".repeat(129)).await,
        Err(EngineError::Invalid(_))
    ));
    engine
        .submit(task("healthy", "target", Simulation::Succeed))
        .await
        .expect("submit healthy");
    assert_eq!(
        until(&engine, "healthy", TaskState::is_terminal)
            .await
            .state,
        TaskState::Succeeded
    );
    engine.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn racing_stage_timeouts_and_cancels_preserve_scheduler_and_journal() {
    let dir = TestDir::new("stage-races");
    let config = EngineConfig {
        max_concurrency: 8,
        ..EngineConfig::default()
    };
    let engine = Engine::open(&dir.path, config).await.expect("open");
    let mut cancellations = tokio::task::JoinSet::new();
    for index in 0..32 {
        let id = format!("race-{index}");
        engine
            .submit(
                task(&id, &id, Simulation::Delay { millis: 1 })
                    .with_timeout(Duration::from_millis(1)),
            )
            .await
            .expect("accept racing task");
        if index % 2 == 0 {
            let handle = engine.handle();
            cancellations.spawn(async move { handle.cancel(&id).await });
        }
    }
    while let Some(canceled) = cancellations.join_next().await {
        assert!(
            canceled
                .expect("cancel caller stayed alive")
                .expect("cancel engine stayed alive")
                .expect("task exists")
                .state
                .is_terminal()
        );
    }
    for index in 0..32 {
        assert!(matches!(
            until(&engine, &format!("race-{index}"), TaskState::is_terminal)
                .await
                .state,
            TaskState::Succeeded | TaskState::Canceled | TaskState::TimedOut | TaskState::Unknown
        ));
    }
    engine
        .submit(task("probe", "fresh-target", Simulation::Succeed))
        .await
        .expect("scheduler accepts after races");
    assert_eq!(
        until(&engine, "probe", TaskState::is_terminal).await.state,
        TaskState::Succeeded
    );
    engine.shutdown().await.expect("shutdown");
    let reopened = Engine::open(&dir.path, EngineConfig::default())
        .await
        .expect("all racing transitions form a valid journal");
    assert_eq!(
        reopened
            .query("probe")
            .await
            .expect("query probe")
            .expect("probe exists")
            .state,
        TaskState::Succeeded
    );
    reopened.shutdown().await.expect("shutdown reopened");
}
