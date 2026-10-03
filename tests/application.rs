//! Application lifecycle and receipts without HTTP configuration or handlers.
use super::*;
use crate::boot::application::{ApplicationConfig, open};
use crate::workflow_test_support::TestDir;
use std::future::Future;
use std::path::Path;
use std::task::Poll;

fn config(data_dir: &Path) -> ApplicationConfig {
    ApplicationConfig {
        data_dir: data_dir.to_owned(),
        harness_config: None,
        extensions_config: None,
        repair_config: None,
        monitors_config: None,
        recovery_config: None,
        control_paths: Vec::new(),
    }
}

#[tokio::test]
async fn shutdown_cancels_then_waits_for_receipt_and_rejects_new_work() {
    let directory = TestDir::new("application-drain");
    let (application, simulation) = open(config(&directory.path)).await.unwrap();
    let cancellation = application
        .begin("in-flight".into(), "harness", json!({}))
        .unwrap();
    {
        let shutdown = application.shutdown();
        tokio::pin!(shutdown);
        std::future::poll_fn(|cx| {
            assert!(
                shutdown.as_mut().poll(cx).is_pending(),
                "shutdown discarded a live operation"
            );
            Poll::Ready(())
        })
        .await;
        assert!(cancellation.is_cancelled());
        assert_eq!(
            application.operations().unwrap().records()["in-flight"].status,
            "running"
        );
        assert!(matches!(
            application.begin("late-work".into(), "harness", json!({})),
            Err(ApplicationError {
                kind: ApplicationErrorKind::Unavailable,
                ..
            })
        ));
        application.finish(
            "in-flight",
            Ok(json!({"status":"unknown","reason":"executor acknowledgement lost"})),
        );
        tokio::time::timeout(Duration::from_secs(2), shutdown.as_mut())
            .await
            .unwrap()
            .unwrap();
    }
    let history = application.operations().unwrap();
    let receipt = &history.records()["in-flight"];
    assert_eq!(receipt.status, "unknown");
    assert_eq!(
        receipt.result.as_ref().unwrap()["reason"],
        "executor acknowledgement lost"
    );
    assert!(!receipt.auto_retry);
    assert!(!history.records().contains_key("late-work"));
    drop(history);
    simulation.shutdown().await.unwrap();
    drop(application);
}

#[tokio::test]
async fn interrupted_acceptance_recovers_unknown_without_id_replay() {
    let directory = TestDir::new("application-restart");
    let (application, simulation) = open(config(&directory.path)).await.unwrap();
    application
        .begin(
            "accepted-once".into(),
            "harness",
            json!({"workspaceId":"workspace"}),
        )
        .unwrap();
    // Closing without a completion receipt models an interrupted caller. No external action runs.
    simulation.shutdown().await.unwrap();
    drop(application);

    let (restored, simulation) = open(config(&directory.path)).await.unwrap();
    {
        let history = restored.operations().unwrap();
        let receipt = &history.records()["accepted-once"];
        assert_eq!(receipt.status, "unknown");
        assert!(receipt.result.is_none());
        assert!(!receipt.auto_retry);
        assert_eq!(history.events().len(), 2);
        assert_eq!(history.events()[0].status, "running");
        assert_eq!(history.events()[1].status, "unknown");
    }
    assert!(matches!(
        restored.begin(
            "accepted-once".into(),
            "harness",
            json!({"workspaceId":"workspace"})
        ),
        Err(ApplicationError {
            kind: ApplicationErrorKind::Conflict,
            ..
        })
    ));
    assert_eq!(restored.operations().unwrap().records().len(), 1);
    restored.shutdown().await.unwrap();
    simulation.shutdown().await.unwrap();
    drop(restored);
}
