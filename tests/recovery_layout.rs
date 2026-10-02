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
fn retired_generation_cannot_open_as_a_new_root() {
    let dir = TestDir::new("layout-retired");
    assert!(RootStorageLock::acquire(&dir.path.join(".core02-1-2-3")).is_err());
    assert!(RootStorageLock::acquire(&dir.path.join(".CORE02-1-2-3")).is_err());
}
