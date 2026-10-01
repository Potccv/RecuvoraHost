use recuvora_host::runtime::{
    FrameworkError, InstanceId, LifecycleFuture, LifecycleOptions, Module, ModuleContext,
    ModuleError, ModuleMetadata, Runtime, RuntimeSnapshot, ServiceKey,
};
use std::future::{Future, pending};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::Duration;
use tokio::sync::oneshot;

#[tokio::test]
async fn dropped_caller_keeps_call_owned_until_cleanup_and_shutdown_can_resume() {
    use recuvora_host::runtime::operation::{CallScope, Cancellation, DispatchError};
    let scope = CallScope::default();
    let token = Cancellation::new();
    let (started_tx, started_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let worker_scope = scope.clone();
    let worker_token = token.clone();
    let caller = tokio::spawn(async move {
        worker_scope
            .run(worker_token.clone(), async move {
                started_tx.send(()).unwrap();
                worker_token.cancelled().await;
                release_rx.await.unwrap();
            })
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), started_rx)
        .await
        .unwrap()
        .unwrap();
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    tokio::time::timeout(Duration::from_secs(2), token.cancelled())
        .await
        .unwrap();
    {
        let shutdown = scope.shutdown();
        tokio::pin!(shutdown);
        std::future::poll_fn(|cx| {
            assert!(
                shutdown.as_mut().poll(cx).is_pending(),
                "shutdown released a live operation"
            );
            Poll::Ready(())
        })
        .await;
    }
    let dispatches = Arc::new(AtomicUsize::new(0));
    let dispatched = dispatches.clone();
    assert!(matches!(
        scope
            .run(Cancellation::new(), async move {
                dispatched.fetch_add(1, Ordering::SeqCst);
            })
            .await,
        Err(DispatchError::Closed)
    ));
    assert_eq!(dispatches.load(Ordering::SeqCst), 0);
    release_tx.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(2), scope.shutdown())
        .await
        .unwrap()
        .unwrap();
    scope.shutdown().await.unwrap();
}

type Log = Arc<Mutex<Vec<String>>>;

type ContextSlot = Arc<Mutex<Option<ModuleContext>>>;

#[derive(Clone, Copy)]
enum Hook {
    Ok,
    Fail,
    Hang,
}

struct OwnedTask {
    live: Arc<AtomicUsize>,
    dropped: Option<oneshot::Sender<()>>,
}

impl Drop for OwnedTask {
    fn drop(&mut self) {
        self.live.fetch_sub(1, Ordering::SeqCst);
        if let Some(sender) = self.dropped.take() {
            let _ = sender.send(());
        }
    }
}

struct Probe {
    metadata: ModuleMetadata,
    log: Log,
    context: ContextSlot,
    value: String,
    start: Hook,
    stop: Hook,
    publish: bool,
    live_tasks: Arc<AtomicUsize>,
    task_dropped: Option<oneshot::Sender<()>>,
}

impl Probe {
    fn new(id: &str, log: &Log) -> Self {
        Self {
            metadata: ModuleMetadata::new(InstanceId::new(id)),
            log: log.clone(),
            context: Arc::new(Mutex::new(None)),
            value: id.into(),
            start: Hook::Ok,
            stop: Hook::Ok,
            publish: true,
            live_tasks: Arc::new(AtomicUsize::new(0)),
            task_dropped: None,
        }
    }
}

impl Module for Probe {
    fn metadata(&self) -> ModuleMetadata {
        self.metadata.clone()
    }

    fn start<'a>(&'a mut self, context: &'a ModuleContext) -> LifecycleFuture<'a> {
        Box::pin(async move {
            let id = self.metadata.instance.0.clone();
            self.log.lock().unwrap().push(format!("start:{id}"));
            *self.context.lock().unwrap() = Some(context.clone());
            let log = self.log.clone();
            context.on_dispose(move || log.lock().unwrap().push(format!("dispose:{id}")))?;
            self.live_tasks.fetch_add(1, Ordering::SeqCst);
            let guard = OwnedTask {
                live: self.live_tasks.clone(),
                dropped: self.task_dropped.take(),
            };
            context.spawn(async move {
                let _guard = guard;
                pending::<()>().await;
            })?;
            for key in &self.metadata.requires {
                let value = context.service::<String>(key)?;
                self.log.lock().unwrap().push(format!("read:{value}"));
            }
            if self.publish {
                for key in &self.metadata.provides {
                    context.publish(key.clone(), Arc::new(self.value.clone()))?;
                }
            }
            match self.start {
                Hook::Ok => Ok(()),
                Hook::Fail => Err(ModuleError::new("injected start failure")),
                Hook::Hang => pending().await,
            }
        })
    }

    fn stop<'a>(&'a mut self, context: &'a ModuleContext) -> LifecycleFuture<'a> {
        Box::pin(async move {
            assert!(!context.is_accepting(), "stop must first refuse new work");
            self.log
                .lock()
                .unwrap()
                .push(format!("stop:{}", self.metadata.instance));
            match self.stop {
                Hook::Ok => Ok(()),
                Hook::Fail => Err(ModuleError::new("injected stop failure")),
                Hook::Hang => pending().await,
            }
        })
    }
}

fn service() -> ServiceKey {
    ServiceKey::new("example", 1, "local")
}

fn context(slot: &ContextSlot) -> ModuleContext {
    slot.lock().unwrap().clone().unwrap()
}

#[tokio::test(start_paused = true)]
async fn starts_dependencies_first_and_stops_in_reverse_with_complete_cleanup() {
    let log = Log::default();
    let mut provider = Probe::new("provider", &log);
    provider.metadata = provider.metadata.provides(service());
    let live = provider.live_tasks.clone();
    let mut consumer = Probe::new("consumer", &log);
    consumer.metadata = consumer.metadata.requires(service());
    let mut runtime = Runtime::default();
    runtime.add(Box::new(consumer)).unwrap();
    runtime.add(Box::new(provider)).unwrap();
    runtime.start().await.unwrap();
    assert_eq!(runtime.snapshot().unwrap().active_instances, 2);
    assert!(runtime.shutdown().await.is_clean());
    assert_eq!(
        *log.lock().unwrap(),
        [
            "start:provider",
            "start:consumer",
            "read:provider",
            "stop:consumer",
            "dispose:consumer",
            "stop:provider",
            "dispose:provider",
        ]
    );
    assert_eq!(runtime.snapshot().unwrap(), RuntimeSnapshot::default());
    assert_eq!(live.load(Ordering::SeqCst), 0);
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test(start_paused = true)]
async fn provider_conflict_is_rejected_before_any_hook() {
    let log = Log::default();
    let mut runtime = Runtime::default();
    for id in ["first", "second"] {
        let mut module = Probe::new(id, &log);
        module.metadata = module.metadata.provides(service());
        runtime.add(Box::new(module)).unwrap();
    }
    assert!(matches!(
        runtime.start().await.unwrap_err().cause,
        FrameworkError::ProviderConflict(_)
    ));
    assert!(log.lock().unwrap().is_empty());
}

#[tokio::test(start_paused = true)]
async fn wrong_scope_or_major_does_not_satisfy_a_dependency() {
    for wrong in [
        ServiceKey::new("example", 2, "local"),
        ServiceKey::new("example", 1, "local/child"),
    ] {
        let log = Log::default();
        let mut runtime = Runtime::default();
        let mut provider = Probe::new("provider", &log);
        provider.metadata = provider.metadata.provides(wrong);
        let mut consumer = Probe::new("consumer", &log);
        consumer.metadata = consumer.metadata.requires(service());
        runtime.add(Box::new(provider)).unwrap();
        runtime.add(Box::new(consumer)).unwrap();
        assert!(matches!(
            runtime.start().await.unwrap_err().cause,
            FrameworkError::MissingDependency { .. }
        ));
        assert!(log.lock().unwrap().is_empty());
    }
}

#[tokio::test(start_paused = true)]
async fn cycle_is_rejected_before_any_hook() {
    let log = Log::default();
    let mut runtime = Runtime::default();
    let a = ServiceKey::new("a", 1, "local");
    let b = ServiceKey::new("b", 1, "local");
    let mut first = Probe::new("first", &log);
    first.metadata = first.metadata.provides(a.clone()).requires(b.clone());
    let mut second = Probe::new("second", &log);
    second.metadata = second.metadata.provides(b).requires(a);
    runtime.add(Box::new(first)).unwrap();
    runtime.add(Box::new(second)).unwrap();
    assert!(matches!(
        runtime.start().await.unwrap_err().cause,
        FrameworkError::DependencyCycle(_)
    ));
    assert!(log.lock().unwrap().is_empty());
}

#[tokio::test(start_paused = true)]
async fn partial_failure_stops_failing_instance_and_started_dependencies() {
    let log = Log::default();
    let mut runtime = Runtime::default();
    let mut provider = Probe::new("provider", &log);
    provider.metadata = provider.metadata.provides(service());
    let mut failure = Probe::new("failure", &log);
    failure.metadata = failure.metadata.requires(service());
    failure.start = Hook::Fail;
    let context_slot = failure.context.clone();
    runtime.add(Box::new(provider)).unwrap();
    runtime.add(Box::new(failure)).unwrap();
    runtime
        .add(Box::new(Probe::new("never-started", &log)))
        .unwrap();
    let error = runtime.start().await.unwrap_err();
    assert!(matches!(
        error.cause,
        FrameworkError::LifecycleFailure { phase: "start", .. }
    ));
    assert!(error.cleanup.is_clean());
    assert_eq!(
        *log.lock().unwrap(),
        [
            "start:provider",
            "start:failure",
            "read:provider",
            "stop:failure",
            "dispose:failure",
            "stop:provider",
            "dispose:provider",
        ]
    );
    assert_eq!(runtime.snapshot().unwrap(), RuntimeSnapshot::default());
    assert!(!context(&context_slot).is_accepting());
}

#[tokio::test(start_paused = true)]
async fn start_timeout_is_structured_and_does_not_prevent_cleanup() {
    let log = Log::default();
    let mut hanging = Probe::new("hanging", &log);
    hanging.start = Hook::Hang;
    let live = hanging.live_tasks.clone();
    let mut runtime = Runtime::default();
    runtime.add(Box::new(hanging)).unwrap();
    let error = runtime.start().await.unwrap_err();
    assert!(matches!(
        error.cause,
        FrameworkError::LifecycleTimeout { phase: "start", .. }
    ));
    assert!(error.cleanup.is_clean());
    assert_eq!(
        *log.lock().unwrap(),
        ["start:hanging", "stop:hanging", "dispose:hanging"]
    );
    assert_eq!(live.load(Ordering::SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn stop_timeout_and_failure_do_not_skip_other_modules() {
    let log = Log::default();
    let mut runtime = Runtime::default();
    for (id, hook) in [
        ("healthy", Hook::Ok),
        ("failing", Hook::Fail),
        ("hanging", Hook::Hang),
    ] {
        let mut module = Probe::new(id, &log);
        module.stop = hook;
        runtime.add(Box::new(module)).unwrap();
    }
    runtime.start().await.unwrap();
    let report = runtime.shutdown().await;
    assert_eq!(report.issues.len(), 2);
    assert!(matches!(
        report.issues[0],
        FrameworkError::LifecycleTimeout { phase: "stop", .. }
    ));
    assert!(matches!(
        report.issues[1],
        FrameworkError::LifecycleFailure { phase: "stop", .. }
    ));
    assert_eq!(runtime.snapshot().unwrap(), RuntimeSnapshot::default());
    assert_eq!(
        &log.lock().unwrap()[3..],
        [
            "stop:hanging",
            "dispose:hanging",
            "stop:failing",
            "dispose:failing",
            "stop:healthy",
            "dispose:healthy",
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn missing_publication_fails_before_consumer_start() {
    let log = Log::default();
    let mut module = Probe::new("provider", &log);
    module.metadata = module.metadata.provides(service());
    module.publish = false;
    let mut consumer = Probe::new("consumer", &log);
    consumer.metadata = consumer.metadata.requires(service());
    let mut runtime = Runtime::default();
    runtime.add(Box::new(consumer)).unwrap();
    runtime.add(Box::new(module)).unwrap();
    let error = runtime.start().await.unwrap_err();
    assert!(matches!(
        error.cause,
        FrameworkError::UnpublishedService { .. }
    ));
    assert_eq!(
        *log.lock().unwrap(),
        ["start:provider", "stop:provider", "dispose:provider"]
    );
}

#[tokio::test(start_paused = true)]
async fn service_resolution_is_typed_declared_and_revoked_on_stop() {
    let log = Log::default();
    let mut module = Probe::new("provider", &log);
    module.metadata = module.metadata.provides(service());
    let slot = module.context.clone();
    let mut runtime = Runtime::default();
    runtime.add(Box::new(module)).unwrap();
    runtime.start().await.unwrap();
    let ctx = context(&slot);
    assert_eq!(&*ctx.service::<String>(&service()).unwrap(), "provider");
    assert!(matches!(
        ctx.service::<u32>(&service()),
        Err(FrameworkError::ServiceTypeMismatch(_))
    ));
    assert!(matches!(
        ctx.service::<String>(&ServiceKey::new("unknown", 1, "local")),
        Err(FrameworkError::UndeclaredService { .. })
    ));
    assert!(runtime.shutdown().await.is_clean());
    assert!(matches!(
        ctx.service::<String>(&service()),
        Err(FrameworkError::InstanceInactive(_))
    ));
    assert!(matches!(
        ctx.on_dispose(|| {}),
        Err(FrameworkError::InstanceInactive(_))
    ));
}

#[tokio::test(start_paused = true)]
async fn events_are_bounded_nonblocking_and_subscriptions_close_on_shutdown() {
    let log = Log::default();
    let module = Probe::new("events", &log);
    let slot = module.context.clone();
    let mut runtime = Runtime::new(LifecycleOptions {
        max_event_bytes: 8,
        max_event_queue: 1,
        max_subscriptions_per_module: 1,
        ..LifecycleOptions::default()
    });
    runtime.add(Box::new(module)).unwrap();
    runtime.start().await.unwrap();
    let ctx = context(&slot);
    let mut subscription = ctx.subscribe("tick", 1).unwrap();
    assert!(matches!(
        ctx.subscribe("tick", 1),
        Err(FrameworkError::CapacityExceeded(_))
    ));
    assert_eq!(ctx.emit("tick", "one").unwrap().delivered, 1);
    assert_eq!(ctx.emit("tick", "two").unwrap().full, 1);
    assert!(matches!(
        ctx.emit("tick", "oversized"),
        Err(FrameworkError::CapacityExceeded(_))
    ));
    assert_eq!(subscription.recv().await.unwrap().data, "one");
    assert!(runtime.shutdown().await.is_clean());
    assert!(subscription.recv().await.is_none());
    assert!(matches!(
        ctx.emit("tick", "late"),
        Err(FrameworkError::InstanceInactive(_))
    ));
    assert_eq!(runtime.snapshot().unwrap(), RuntimeSnapshot::default());
}

#[tokio::test(start_paused = true)]
async fn configuration_selects_either_compiled_provider_without_resource_growth() {
    let live = Arc::new(AtomicUsize::new(0));
    for selected in ["builtin-a", "builtin-b"].into_iter().cycle().take(50) {
        let log = Log::default();
        let mut provider = Probe::new(selected, &log);
        provider.metadata = provider.metadata.provides(service());
        provider.live_tasks = live.clone();
        let mut consumer = Probe::new("consumer", &log);
        consumer.metadata = consumer.metadata.requires(service());
        consumer.live_tasks = live.clone();
        let mut runtime = Runtime::default();
        runtime.add(Box::new(consumer)).unwrap();
        runtime.add(Box::new(provider)).unwrap();
        runtime.start().await.unwrap();
        assert!(log.lock().unwrap().contains(&format!("read:{selected}")));
        assert!(runtime.shutdown().await.is_clean());
        assert_eq!(live.load(Ordering::SeqCst), 0);
        assert_eq!(runtime.snapshot().unwrap(), RuntimeSnapshot::default());
    }
}

#[tokio::test(start_paused = true)]
async fn caller_cancelled_start_can_be_explicitly_cleaned_up() {
    let log = Log::default();
    let mut module = Probe::new("cancelled", &log);
    module.start = Hook::Hang;
    let live = module.live_tasks.clone();
    let mut runtime = Runtime::default();
    runtime.add(Box::new(module)).unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(1), runtime.start())
            .await
            .is_err()
    );
    assert!(runtime.shutdown().await.is_clean());
    assert_eq!(
        *log.lock().unwrap(),
        ["start:cancelled", "stop:cancelled", "dispose:cancelled"]
    );
    assert_eq!(live.load(Ordering::SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn dropping_runtime_revokes_context_and_aborts_owned_task() {
    let log = Log::default();
    let mut module = Probe::new("drop", &log);
    let (sender, receiver) = oneshot::channel();
    module.task_dropped = Some(sender);
    let slot = module.context.clone();
    let mut runtime = Runtime::default();
    runtime.add(Box::new(module)).unwrap();
    runtime.start().await.unwrap();
    let (disposed_sender, disposed_receiver) = oneshot::channel();
    context(&slot)
        .on_dispose(move || {
            let _ = disposed_sender.send(());
        })
        .unwrap();
    drop(runtime);
    tokio::time::timeout(Duration::from_secs(1), receiver)
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), disposed_receiver)
        .await
        .unwrap()
        .unwrap();
    assert!(!context(&slot).is_accepting());
    assert_eq!(*log.lock().unwrap(), ["start:drop", "dispose:drop"]);
}

#[tokio::test(start_paused = true)]
async fn cancelled_shutdown_retains_handles_and_disposes_only_after_task_drop() {
    let log = Log::default();
    let module = Probe::new("cancel-cleanup", &log);
    let slot = module.context.clone();
    let live = module.live_tasks.clone();
    let checked = Arc::new(AtomicUsize::new(0));
    let mut runtime = Runtime::default();
    runtime.add(Box::new(module)).unwrap();
    runtime.start().await.unwrap();
    let checked_by_disposer = checked.clone();
    let live_at_dispose = live.clone();
    context(&slot)
        .on_dispose(move || {
            assert_eq!(
                live_at_dispose.load(Ordering::SeqCst),
                0,
                "task future must have dropped before any owned resource is disposed"
            );
            checked_by_disposer.fetch_add(1, Ordering::SeqCst);
        })
        .unwrap();
    let mut shutdown = Box::pin(runtime.shutdown());
    // Poll exactly once, before Tokio can process the task's abort request.
    // This stops at the join await without sleeping or racing another thread.
    std::future::poll_fn(|cx| {
        assert!(shutdown.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    drop(shutdown);
    let pending = runtime.snapshot().unwrap();
    assert_eq!(pending.pending_cleanup_instances, 1);
    assert_eq!(pending.background_tasks, 1);
    assert_eq!(pending.disposers, 2);
    assert_eq!(checked.load(Ordering::SeqCst), 0);
    assert!(runtime.shutdown().await.is_clean());
    assert_eq!(checked.load(Ordering::SeqCst), 1);
    assert_eq!(runtime.snapshot().unwrap(), RuntimeSnapshot::default());
    assert_eq!(
        *log.lock().unwrap(),
        [
            "start:cancel-cleanup",
            "stop:cancel-cleanup",
            "dispose:cancel-cleanup"
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn resource_capacity_failure_cleans_partial_start() {
    let log = Log::default();
    let module = Probe::new("limited", &log);
    let live = module.live_tasks.clone();
    let mut runtime = Runtime::new(LifecycleOptions {
        max_resources_per_module: 1,
        ..LifecycleOptions::default()
    });
    runtime.add(Box::new(module)).unwrap();
    let error = runtime.start().await.unwrap_err();
    assert!(matches!(
        error.cause,
        FrameworkError::LifecycleFailure { phase: "start", .. }
    ));
    assert!(error.cleanup.is_clean());
    assert_eq!(live.load(Ordering::SeqCst), 0);
    assert_eq!(runtime.snapshot().unwrap(), RuntimeSnapshot::default());
}
