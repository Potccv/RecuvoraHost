use crate::{
    integrations::recovery::*,
    persistence::{approval::*, incidents::IncidentStoreConfig, knowledge::*, legacy::*},
    workflow_test_support::TestDir,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
#[path = "legacy_bundle_fixture.rs"]
mod fixture;
use fixture::*;

#[test]
fn activation_failure_retains_original_history_and_can_retry() {
    let fixture = Fixture::new("pending", 2);
    let authority = fixture.authority();
    let original = std::fs::read(fixture.root.join("recovery.jsonl")).unwrap();
    let result = super::import_with_activation(
        &fixture.root,
        &fixture.incidents,
        fixture.config.clone(),
        &authority,
        10_000,
        |source, target| {
            assert!(source.is_file());
            assert_eq!(std::fs::read(target).unwrap(), original);
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "injected rename failure",
            ))
        },
    );
    assert!(result.is_err());
    assert_eq!(
        std::fs::read(fixture.root.join("recovery.jsonl")).unwrap(),
        original
    );
    assert!(!std::fs::read_dir(&fixture.root).unwrap().any(|item| {
        item.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".core02-")
    }));
    fixture.import(&authority).unwrap();
}

#[test]
fn activation_probe() {
    let Some(root) = std::env::var_os("RECUVORA_IMPORT_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let directory = root.parent().unwrap();
    let authority = FileTargetOwnership::open(directory.join("ownership")).unwrap();
    import_legacy_recovery_bundle(
        &root,
        &directory.join("incidents.jsonl"),
        configuration(),
        &authority,
        10_000,
    )
    .unwrap();
    panic!("activation crash boundary not reached");
}

fn crash(fixture: &Fixture, boundary: &str) {
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "persistence::legacy::bundle::tests::activation_probe",
            "--nocapture",
        ])
        .env("RECUVORA_IMPORT_ROOT", &fixture.root)
        .env("RECUVORA_IMPORT_CRASH", boundary)
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert_eq!(status.code(), Some(94));
            break;
        }
        if std::time::Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("migration subprocess timed out");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[test]
fn process_exit_before_marker_leaves_legacy_source_active_and_retryable() {
    let fixture = Fixture::new("pending", 2);
    let original = std::fs::read(fixture.root.join("recovery.jsonl")).unwrap();
    crash(&fixture, "before_marker");
    assert_eq!(
        std::fs::read(fixture.root.join("recovery.jsonl")).unwrap(),
        original
    );
    // An unactivated generation is retained by a process crash, but neither
    // a service opener nor retry may treat it as the selected source of truth.
    assert!(std::fs::read_dir(&fixture.root).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".core02-")
    }));
    let report = fixture.import(&fixture.authority()).unwrap();
    assert_eq!(report.recovery_revisions, fixture.revisions.len());
    assert_eq!(
        std::fs::read(report.generation_directory.join("legacy-recovery.jsonl")).unwrap(),
        original
    );
}

#[test]
fn process_exit_after_marker_has_a_complete_replayable_generation() {
    let fixture = Fixture::new("executing", 2);
    let original = std::fs::read(fixture.root.join("recovery.jsonl")).unwrap();
    crash(&fixture, "after_marker");
    let marker: Value =
        serde_json::from_slice(&std::fs::read(fixture.root.join("recovery.jsonl")).unwrap())
            .unwrap();
    assert_eq!(marker["host_recovery_layout"], 1);
    assert!(
        marker.get("format").is_none() && marker.get("task").is_none(),
        "old workflow format cannot parse or execute a redirect"
    );
    let generation = fixture.root.join(marker["generation"].as_str().unwrap());
    assert_eq!(
        std::fs::read(generation.join("legacy-recovery.jsonl")).unwrap(),
        original
    );
    let approval = ApprovalStore::open(
        generation.join("approvals"),
        fixture.config.approval.clone(),
        11,
    )
    .unwrap();
    assert_eq!(
        approval.get(APPROVAL).unwrap().state,
        ApprovalState::Unknown
    );
    let journal = crate::persistence::journal::Journal::open(
        generation.join("recovery.jsonl"),
        "recovery",
        serde_json::to_value(&fixture.config.recovery).unwrap(),
        256 * 1024 * 1024,
    )
    .unwrap();
    let entries = journal
        .records()
        .iter()
        .map(|entry| {
            serde_json::from_value::<recuvora_core::recovery::workflow::RecoveryEntry>(
                entry.payload.clone(),
            )
            .unwrap()
        })
        .collect::<Vec<_>>();
    let recovered = recuvora_core::recovery::workflow::RecoveryState::restore(
        fixture.config.recovery.clone(),
        entries,
    )
    .unwrap();
    assert_eq!(
        recovered
            .task(TASK)
            .unwrap()
            .operation
            .as_ref()
            .unwrap()
            .operation_id,
        OPERATION
    );
    assert_eq!(recovered.task(TASK).unwrap().stage, RecoveryStage::Unknown);
    assert!(generation.join("dispatch.jsonl").is_file());
    assert!(generation.join("incidents.jsonl").is_file());
    assert!(generation.join("knowledge.jsonl").is_file());
}
