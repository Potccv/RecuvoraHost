use super::*;
use crate::workflow_test_support::TestDir;

#[test]
fn root_lock_excludes_another_writer_and_keeps_legacy_history() {
    let dir = TestDir::new("layout-legacy");
    let original = b"{\"format\":2,\"sequence\":1,\"task\":{}}\n";
    std::fs::write(dir.path.join("recovery.jsonl"), original).unwrap();
    let mut lock = RootStorageLock::acquire(&dir.path).unwrap();
    assert!(RootStorageLock::acquire(&dir.path).is_err());
    assert_eq!(lock.resolve(&dir.path).unwrap(), dir.path);
    assert_eq!(
        std::fs::read(dir.path.join("recovery.jsonl")).unwrap(),
        original
    );
    drop(lock);
    RootStorageLock::acquire(&dir.path).unwrap();
}

#[test]
fn incomplete_or_escaping_generation_never_creates_missing_journals() {
    let dir = TestDir::new("layout-incomplete");
    for generation in ["../outside", ".core02-1/child", ".core02-1-2-3"] {
        let marker = serde_json::json!({"host_recovery_layout":1,"generation":generation});
        std::fs::write(dir.path.join("recovery.jsonl"), format!("{marker}\n")).unwrap();
        let mut lock = RootStorageLock::acquire(&dir.path).unwrap();
        assert!(lock.resolve(&dir.path).is_err());
        assert!(!dir.path.join(".core02-1-2-3").exists());
    }
}

#[test]
fn installed_generation_uses_original_lock_and_cannot_open_as_a_new_root() {
    let dir = TestDir::new("layout-installed");
    let generation = dir.path.join(".core02-1-2-3");
    std::fs::create_dir_all(generation.join("approvals")).unwrap();
    for name in [
        "recovery.jsonl",
        "dispatch.jsonl",
        "knowledge.jsonl",
        "approvals/approvals.jsonl",
        "legacy-recovery.jsonl",
    ] {
        std::fs::write(generation.join(name), b"fixture\n").unwrap();
    }
    std::fs::write(
        dir.path.join("recovery.jsonl"),
        b"{\"host_recovery_layout\":1,\"generation\":\".core02-1-2-3\"}\n",
    )
    .unwrap();
    let mut lock = RootStorageLock::acquire(&dir.path).unwrap();
    assert_eq!(lock.resolve(&dir.path).unwrap(), generation);
    assert!(RootStorageLock::acquire(&dir.path).is_err());
    assert!(RootStorageLock::acquire(&generation).is_err());
    assert!(RootStorageLock::acquire(&dir.path.join(".CORE02-1-2-3")).is_err());
    lock.validate().unwrap();
    // A same-length rewrite cannot redirect a running owner to another generation.
    std::fs::write(
        dir.path.join("recovery.jsonl"),
        b"{\"host_recovery_layout\":1,\"generation\":\".core02-1-2-4\"}\n",
    )
    .unwrap();
    assert!(lock.validate().is_err());
}
