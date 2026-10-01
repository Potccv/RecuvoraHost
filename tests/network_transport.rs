mod network_fixture;
#[path = "workflow_support.rs"]
mod support;

use network_fixture::{Behavior, Fixture, TestResult, TlsMaterial};
use recuvora_host::integrations::extensions::{
    CallbackFuture, CallbackHandler, DispatchGuard, ExtensionCall, ExtensionError,
    ExtensionRegistry, ExtensionsConfig, Outcome, ProtocolSettings,
};
use recuvora_host::runtime::operation::Cancellation;
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::{Notify, Semaphore};

fn call() -> ExtensionCall {
    ExtensionCall {
        contract: "com.example.network".into(),
        version: 1,
        method: "query".into(),
        params: json!({"target":"target-a"}),
        timeout: Duration::from_secs(5),
    }
}

#[tokio::test]
async fn network_endpoint_config_reaches_registry_routes_without_bypassing_allowlists() -> TestResult
{
    for scheme in ["http", "ws"] {
        let fixture = Fixture::start(scheme, Behavior::Echo, None).await?;
        fixture.enable_plugin_contract();
        let config: ExtensionsConfig = serde_json::from_value(json!({
            "schema_version":1,
            "extensions":[{
                "id":"network-plugin",
                "kind":"plugin",
                "endpoint":{"url":fixture.url},
                "namespaces":["com.example.network"],
                "allow_calls":[{"contract":"com.example.network","version":1,"method":"query"}]
            }]
        }))?;
        let registry = ExtensionRegistry::connect(config).await?;
        assert!(registry.metadata("network-plugin").is_some(), "{scheme}");
        assert_eq!(
            registry.contract_owner("com.example.network", 1),
            Some("network-plugin")
        );
        let result = registry
            .call_read_only(
                "network-plugin",
                "com.example.network",
                1,
                "query",
                json!({"target":"target-a"}),
                Duration::from_secs(5),
                Cancellation::new(),
            )
            .await?;
        assert_eq!(result, json!({"echo":{"target":"target-a"}}));
        let denied = registry
            .call_read_only(
                "network-plugin",
                "com.example.network",
                1,
                "unlisted",
                json!({"target":"target-a"}),
                Duration::from_secs(5),
                Cancellation::new(),
            )
            .await;
        assert!(
            matches!(denied, Err(ExtensionError::Rejected(_))),
            "{scheme}: {denied:?}"
        );
        assert_eq!(fixture.state.calls.load(Ordering::SeqCst), 1);
        assert_eq!(fixture.state.handshakes.load(Ordering::SeqCst), 2);
        registry.shutdown().await?;
        fixture.shutdown().await;
    }
    Ok(())
}

struct EchoHandler(AtomicUsize);

impl CallbackHandler for EchoHandler {
    fn call<'a>(
        &'a self,
        method: String,
        params: Value,
        cancellation: Cancellation,
    ) -> CallbackFuture<'a> {
        Box::pin(async move {
            assert_eq!(method, "tool");
            assert_eq!(params, json!({"workload":"target-a"}));
            assert!(!cancellation.is_cancelled());
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(json!({"observation":"checked"}))
        })
    }
}

#[tokio::test]
async fn all_network_schemes_preserve_handshake_and_bidirectional_callbacks() -> TestResult {
    let directory = support::TestDir::new("network-protocol");
    for scheme in ["ws", "http", "wss", "https"] {
        let (tls, ca) = if matches!(scheme, "wss" | "https") {
            let tls = TlsMaterial::new()?;
            let path = directory.path.join(format!("{scheme}-ca.pem"));
            std::fs::write(&path, &tls.pem)?;
            (Some(tls), Some(path))
        } else {
            (None, None)
        };
        let fixture = Fixture::start(scheme, Behavior::Callback, tls).await?;
        let client = fixture.client(ca);
        let expected = client.probe().await?;
        let handler = Arc::new(EchoHandler(AtomicUsize::new(0)));
        let result = tokio::time::timeout(
            Duration::from_secs(15),
            client.call(
                call(),
                expected,
                Cancellation::new(),
                Some(handler.clone()),
                None,
            ),
        )
        .await??;
        assert_eq!(result, json!({"callback":{"observation":"checked"}}));
        assert_eq!(handler.0.load(Ordering::SeqCst), 1, "{scheme}");
        assert_eq!(fixture.state.calls.load(Ordering::SeqCst), 1, "{scheme}");
        assert_eq!(
            fixture.state.handshakes.load(Ordering::SeqCst),
            2,
            "{scheme}"
        );
        fixture.shutdown().await;
    }
    Ok(())
}

#[tokio::test]
async fn tls_requires_trust_even_when_the_protocol_identity_matches() -> TestResult {
    let directory = support::TestDir::new("network-tls");
    for scheme in ["wss", "https"] {
        let tls = TlsMaterial::new()?;
        let ca = directory.path.join(format!("{scheme}-ca.pem"));
        std::fs::write(&ca, &tls.pem)?;
        let fixture = Fixture::start(scheme, Behavior::Echo, Some(tls)).await?;
        assert!(fixture.client(None).probe().await.is_err(), "{scheme}");
        assert_eq!(fixture.state.handshakes.load(Ordering::SeqCst), 0);
        let client = fixture.client(Some(ca));
        let expected = client.probe().await?;
        let result = client
            .call(call(), expected, Cancellation::new(), None, None)
            .await?;
        assert_eq!(result, json!({"echo":{"target":"target-a"}}));
        fixture.shutdown().await;
    }
    Ok(())
}

#[tokio::test]
async fn cancellation_after_dispatch_remains_unknown() -> TestResult {
    for scheme in ["ws", "http"] {
        let fixture = Fixture::start(scheme, Behavior::WaitForCancel, None).await?;
        let client = fixture.client(None);
        let expected = client.probe().await?;
        let cancellation = Cancellation::new();
        let token = cancellation.clone();
        let pending =
            tokio::spawn(async move { client.call(call(), expected, token, None, None).await });
        fixture.state.wait_for_dispatch().await;
        cancellation.cancel();
        let result = tokio::time::timeout(Duration::from_secs(15), pending).await??;
        assert!(
            matches!(result, Err(ExtensionError::Unknown { .. })),
            "{scheme}: {result:?}"
        );
        assert_eq!(fixture.state.cancellations.load(Ordering::SeqCst), 1);
        fixture.shutdown().await;
    }
    Ok(())
}

#[tokio::test]
async fn disconnect_and_wrong_correlation_do_not_replay_calls() -> TestResult {
    for scheme in ["ws", "http"] {
        for behavior in [Behavior::Disconnect, Behavior::WrongCorrelation] {
            let fixture = Fixture::start(scheme, behavior, None).await?;
            let client = fixture.client(None);
            let expected = client.probe().await?;
            let result = tokio::time::timeout(
                Duration::from_secs(15),
                client.call(call(), expected, Cancellation::new(), None, None),
            )
            .await?;
            assert!(
                matches!(result, Err(ExtensionError::Unknown { .. })),
                "{scheme}/{behavior:?}: {result:?}"
            );
            assert_eq!(fixture.state.calls.load(Ordering::SeqCst), 1);
            fixture.shutdown().await;
        }
    }
    Ok(())
}

#[tokio::test]
async fn changed_metadata_is_rejected_before_dispatch() -> TestResult {
    for scheme in ["ws", "http"] {
        let fixture = Fixture::start(scheme, Behavior::Echo, None).await?;
        let client = fixture.client(None);
        let expected = client.probe().await?;
        fixture.state.metadata_changed.store(true, Ordering::SeqCst);
        let result = client
            .call(call(), expected, Cancellation::new(), None, None)
            .await;
        assert!(
            matches!(result, Err(ExtensionError::Protocol(_))),
            "{scheme}: {result:?}"
        );
        assert_eq!(fixture.state.calls.load(Ordering::SeqCst), 0);
        fixture.shutdown().await;
    }
    Ok(())
}

#[tokio::test]
async fn http_neither_follows_redirects_nor_retries_dispatched_posts() -> TestResult {
    let redirect = Fixture::start("http", Behavior::Redirect, None).await?;
    assert!(redirect.client(None).probe().await.is_err());
    assert_eq!(redirect.state.redirect_hits.load(Ordering::SeqCst), 0);
    redirect.shutdown().await;

    let unavailable = Fixture::start("http", Behavior::Unavailable, None).await?;
    let client = unavailable.client(None);
    let expected = client.probe().await?;
    let result = client
        .call(call(), expected, Cancellation::new(), None, None)
        .await;
    assert!(matches!(result, Err(ExtensionError::Unknown { .. })));
    assert_eq!(unavailable.state.calls.load(Ordering::SeqCst), 1);
    unavailable.shutdown().await;
    Ok(())
}

struct DrainingHandler {
    started: Notify,
    cancelled: Notify,
    finish: Notify,
    unknown: bool,
}

impl CallbackHandler for DrainingHandler {
    fn call<'a>(
        &'a self,
        _method: String,
        _params: Value,
        cancellation: Cancellation,
    ) -> CallbackFuture<'a> {
        Box::pin(async move {
            self.started.notify_one();
            cancellation.cancelled().await;
            self.cancelled.notify_one();
            self.finish.notified().await;
            if self.unknown {
                return Err(ExtensionError::Unknown {
                    call_id: "drained-action".into(),
                    message: "callback receipt lost during cancellation".into(),
                });
            }
            Ok(json!({"persistent_result":"completed_pending"}))
        })
    }
}

#[tokio::test]
async fn dropped_network_caller_keeps_capacity_until_its_callback_finishes() -> TestResult {
    for scheme in ["ws", "http"] {
        let fixture = Fixture::start(scheme, Behavior::Callback, None).await?;
        let client = fixture.client(None);
        let expected = client.probe().await?;
        let capacity = Arc::new(Semaphore::new(1));
        let permit = capacity.clone().acquire_owned().await?;
        let handler = Arc::new(DrainingHandler {
            started: Notify::new(),
            cancelled: Notify::new(),
            finish: Notify::new(),
            unknown: false,
        });
        let callback = handler.clone();
        let caller = tokio::spawn(async move {
            client
                .call(
                    call(),
                    expected,
                    Cancellation::new(),
                    Some(callback),
                    Some(permit),
                )
                .await
        });
        tokio::time::timeout(Duration::from_secs(5), handler.started.notified()).await?;
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());
        tokio::time::timeout(Duration::from_secs(5), handler.cancelled.notified()).await?;
        assert_eq!(capacity.available_permits(), 0, "{scheme}");
        handler.finish.notify_one();
        let returned = tokio::time::timeout(Duration::from_secs(15), capacity.acquire()).await??;
        drop(returned);
        assert_eq!(capacity.available_permits(), 1, "{scheme}");
        fixture.shutdown().await;
    }
    Ok(())
}

struct FailingHandler {
    outcome: Outcome,
    calls: AtomicUsize,
}

impl CallbackHandler for FailingHandler {
    fn call<'a>(
        &'a self,
        _method: String,
        _params: Value,
        _token: Cancellation,
    ) -> CallbackFuture<'a> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(match self.outcome {
                Outcome::Rejected => ExtensionError::Rejected("not dispatched".into()),
                Outcome::Cancelled => ExtensionError::Cancelled,
                Outcome::Unknown => ExtensionError::Unknown {
                    call_id: "external-action-1".into(),
                    message: "external action dispatched; receipt lost".into(),
                },
            })
        })
    }
}

#[tokio::test]
async fn callback_classification_survives_wire_and_unknown_survives_top_level_success() -> TestResult
{
    for scheme in ["ws", "http"] {
        for outcome in [Outcome::Rejected, Outcome::Cancelled, Outcome::Unknown] {
            let fixture = Fixture::start(scheme, Behavior::Callback, None).await?;
            let client = fixture.client(None);
            let expected = client.probe().await?;
            let handler = Arc::new(FailingHandler {
                outcome,
                calls: AtomicUsize::new(0),
            });
            let result = client
                .call(
                    call(),
                    expected,
                    Cancellation::new(),
                    Some(handler.clone()),
                    None,
                )
                .await;
            assert_eq!(
                *fixture.state.callback_outcomes.lock().unwrap(),
                vec![outcome]
            );
            if outcome == Outcome::Unknown {
                assert!(
                    matches!(&result, Err(ExtensionError::Unknown { call_id, .. }) if call_id == "external-action-1"),
                    "{result:?}"
                );
            } else {
                assert_eq!(result?, json!({"completed":true}));
            }
            assert_eq!(handler.calls.load(Ordering::SeqCst), 1);
            assert_eq!(fixture.state.calls.load(Ordering::SeqCst), 1);
            fixture.shutdown().await;
        }
    }
    Ok(())
}

#[tokio::test]
async fn predispatch_cancellation_never_contacts_node() -> TestResult {
    let fixture = Fixture::start("http", Behavior::Echo, None).await?;
    let client = fixture.client(None);
    let expected = client.probe().await?;
    let token = Cancellation::new();
    token.cancel();
    assert!(matches!(
        client.call(call(), expected, token, None, None).await,
        Err(ExtensionError::Cancelled)
    ));
    assert_eq!(fixture.state.calls.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.state.handshakes.load(Ordering::SeqCst), 1);
    fixture.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn callback_transport_cleanup_failure_is_unknown_without_replay() -> TestResult {
    let fixture = Fixture::start("http", Behavior::CallbackCleanupFailure, None).await?;
    let client = fixture.client(None);
    let expected = client.probe().await?;
    let handler = Arc::new(EchoHandler(AtomicUsize::new(0)));
    let capacity = Arc::new(Semaphore::new(1));
    let result = client
        .call(
            call(),
            expected,
            Cancellation::new(),
            Some(handler.clone()),
            Some(capacity.clone().acquire_owned().await?),
        )
        .await;
    assert!(matches!(result, Err(ExtensionError::Unknown { .. })));
    assert_eq!(handler.0.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.state.calls.load(Ordering::SeqCst), 1);
    assert_eq!(capacity.available_permits(), 1);
    fixture.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn unknown_callback_during_cancel_drain_retains_evidence_and_capacity() -> TestResult {
    for scheme in ["ws", "http"] {
        let fixture = Fixture::start(scheme, Behavior::Callback, None).await?;
        let client = fixture.client(None);
        let expected = client.probe().await?;
        let handler = Arc::new(DrainingHandler {
            started: Notify::new(),
            cancelled: Notify::new(),
            finish: Notify::new(),
            unknown: true,
        });
        let token = Cancellation::new();
        let cancellation = token.clone();
        let capacity = Arc::new(Semaphore::new(1));
        let permit = capacity.clone().acquire_owned().await?;
        let callback = handler.clone();
        let pending = tokio::spawn(async move {
            client
                .call(call(), expected, token, Some(callback), Some(permit))
                .await
        });
        tokio::time::timeout(Duration::from_secs(5), handler.started.notified()).await?;
        cancellation.cancel();
        tokio::time::timeout(Duration::from_secs(5), handler.cancelled.notified()).await?;
        assert_eq!(capacity.available_permits(), 0);
        handler.finish.notify_one();
        let result = tokio::time::timeout(Duration::from_secs(15), pending).await??;
        assert!(
            matches!(&result, Err(ExtensionError::Unknown { message, .. }) if message.contains("callback receipt lost during cancellation")),
            "{result:?}"
        );
        assert_eq!(capacity.available_permits(), 1);
        assert_eq!(fixture.state.calls.load(Ordering::SeqCst), 1);
        fixture.shutdown().await;
    }
    Ok(())
}

struct PanickingHandler(AtomicUsize);

impl CallbackHandler for PanickingHandler {
    fn call<'a>(
        &'a self,
        _method: String,
        _params: Value,
        _token: Cancellation,
    ) -> CallbackFuture<'a> {
        Box::pin(async move {
            self.0.fetch_add(1, Ordering::SeqCst);
            panic!("injected callback failure after dispatch");
        })
    }
}

#[tokio::test]
async fn failed_trusted_callback_is_unknown_and_is_not_replayed() -> TestResult {
    let fixture = Fixture::start("http", Behavior::Callback, None).await?;
    let client = fixture.client(None);
    let expected = client.probe().await?;
    let handler = Arc::new(PanickingHandler(AtomicUsize::new(0)));
    let capacity = Arc::new(Semaphore::new(1));
    let result = client
        .call(
            call(),
            expected,
            Cancellation::new(),
            Some(handler.clone()),
            Some(capacity.clone().acquire_owned().await?),
        )
        .await;
    assert!(matches!(result, Err(ExtensionError::Unknown { .. })));
    assert_eq!(handler.0.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.state.calls.load(Ordering::SeqCst), 1);
    assert_eq!(capacity.available_permits(), 1);
    fixture.shutdown().await;
    Ok(())
}

struct OversizedHandler(AtomicUsize);
impl CallbackHandler for OversizedHandler {
    fn call<'a>(
        &'a self,
        _method: String,
        _params: Value,
        _token: Cancellation,
    ) -> CallbackFuture<'a> {
        Box::pin(async move {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(json!({"content":"x".repeat(recuvora_host::protocol::MAX_FRAME_BYTES)}))
        })
    }
}

#[tokio::test]
async fn oversized_completed_callback_reply_is_unknown_and_never_replayed() -> TestResult {
    for scheme in ["http", "ws"] {
        let fixture = Fixture::start(scheme, Behavior::Callback, None).await?;
        let client = fixture.client(None);
        let expected = client.probe().await?;
        let handler = Arc::new(OversizedHandler(AtomicUsize::new(0)));
        let capacity = Arc::new(Semaphore::new(1));
        let result = client
            .call(
                call(),
                expected,
                Cancellation::new(),
                Some(handler.clone()),
                Some(capacity.clone().acquire_owned().await?),
            )
            .await;
        assert!(
            matches!(&result, Err(ExtensionError::Unknown { call_id, message }) if call_id == "callback-1" && message.contains("outgoing frame exceeds")),
            "{scheme}: {result:?}"
        );
        assert_eq!(handler.0.load(Ordering::SeqCst), 1);
        assert_eq!(fixture.state.calls.load(Ordering::SeqCst), 1);
        assert_eq!(capacity.available_permits(), 1);
        fixture.shutdown().await;
    }
    Ok(())
}

struct DispatchGate {
    state: Arc<network_fixture::State>,
    gate: std::sync::Mutex<Option<tokio::sync::OwnedMutexGuard<()>>>,
    deny: bool,
    released: Notify,
    validations: AtomicUsize,
}
impl DispatchGuard for DispatchGate {
    fn validate(&self) -> Result<(), ExtensionError> {
        assert_eq!(
            self.state.handshakes.load(Ordering::SeqCst),
            2,
            "validate after the call handshake"
        );
        assert!(self.gate.lock().unwrap().is_some());
        self.validations.fetch_add(1, Ordering::SeqCst);
        if self.deny {
            Err(ExtensionError::Rejected(
                "authority changed before send".into(),
            ))
        } else {
            Ok(())
        }
    }
    fn release(&self) {
        self.gate.lock().unwrap().take();
        self.released.notify_one();
    }
}

#[tokio::test]
async fn dispatch_gate_rechecks_after_handshake_and_releases_after_send_not_execution() -> TestResult
{
    for scheme in ["http", "ws"] {
        for deny in [false, true] {
            let fixture = Fixture::start(scheme, Behavior::WaitForCancel, None).await?;
            let client = fixture.client(None);
            let expected = client.probe().await?;
            let mutex = Arc::new(tokio::sync::Mutex::new(()));
            let guard = Arc::new(DispatchGate {
                state: fixture.state.clone(),
                gate: std::sync::Mutex::new(Some(mutex.clone().lock_owned().await)),
                deny,
                released: Notify::new(),
                validations: AtomicUsize::new(0),
            });
            let token = Cancellation::new();
            let cancel = token.clone();
            let dispatched = guard.clone();
            let pending = tokio::spawn(async move {
                client
                    .call_with_dispatch_guard(
                        call(),
                        expected,
                        token,
                        None,
                        None,
                        &ProtocolSettings::default(),
                        Some(dispatched),
                    )
                    .await
            });
            tokio::time::timeout(Duration::from_secs(5), guard.released.notified()).await?;
            assert!(
                mutex.try_lock().is_ok(),
                "authority must release immediately after sending or preflight rejection"
            );
            if !deny {
                assert!(
                    !pending.is_finished(),
                    "node has not returned an execution result"
                );
                fixture.state.wait_for_dispatch().await;
                cancel.cancel();
            }
            let result = tokio::time::timeout(Duration::from_secs(5), pending).await??;
            if deny {
                assert!(matches!(result, Err(ExtensionError::Rejected(_))));
            } else {
                assert!(matches!(result, Err(ExtensionError::Unknown { .. })));
            }
            assert_eq!(
                fixture.state.calls.load(Ordering::SeqCst),
                usize::from(!deny)
            );
            assert_eq!(guard.validations.load(Ordering::SeqCst), 1);
            fixture.shutdown().await;
        }
    }
    Ok(())
}
