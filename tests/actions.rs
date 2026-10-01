use super::path_policy::validate_relative;
#[cfg(windows)]
use super::platform::{open_pinned_file, path_within, verify_handle_path, windows_components};
use super::*;
#[cfg(windows)]
use std::path::{Path, PathBuf};

#[cfg(windows)]
fn fixture(name: &str) -> (crate::workflow_test_support::TestDir, ScopedFiles) {
    let dir = crate::workflow_test_support::TestDir::new(name);
    std::fs::create_dir(dir.path.join("target")).unwrap();
    std::fs::write(dir.path.join("target/a.txt"), "before").unwrap();
    let files = ScopedFiles::open(&dir.path.join("target"), &["a.txt".into()], &[]).unwrap();
    (dir, files)
}

#[test]
fn rejects_path_aliases_and_control_paths() {
    for path in [
        "../a",
        "/a",
        "C:/a",
        "a//b",
        "a\\b",
        "a:stream",
        "a/../b",
        "a.",
        "a ",
        ".git/config",
        ".provider-state/config.toml",
        "NUL",
        "COM1.txt",
    ] {
        assert!(validate_relative(path).is_err(), "{path}");
    }
    assert!(validate_relative("src/main.rs").is_ok());
}

#[cfg(windows)]
#[test]
fn approved_exact_replacement_is_flushed_and_verified() {
    let (_dir, files) = fixture("write");
    let edit = TextEdit {
        path: "a.txt".into(),
        expected: "before".into(),
        replacement: "after".into(),
    };
    let prepared = files.prepare(&edit).unwrap();
    let receipt = prepared.execute().unwrap();
    assert!(receipt.content_verified);
    assert_eq!(files.read("a.txt").unwrap(), "after");
    assert!(matches!(files.prepare(&edit), Err(ActionError::Changed)));
}

#[cfg(windows)]
#[test]
fn locks_target_against_writes_and_renames_and_rechecks_new_hard_links() {
    let (dir, files) = fixture("locks");
    let prepared = files
        .prepare(&TextEdit {
            path: "a.txt".into(),
            expected: "before".into(),
            replacement: "after".into(),
        })
        .unwrap();
    let path = dir.path.join("target/a.txt");
    assert!(std::fs::write(&path, "racing writer").is_err());
    assert!(std::fs::rename(&path, dir.path.join("target/moved.txt")).is_err());
    // Windows sharing does not forbid creating a new hard link. The next
    // execution check must reject it instead of claiming exclusive identity.
    std::fs::hard_link(&path, dir.path.join("link.txt")).unwrap();
    assert!(prepared.execute().is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "before");
}

#[cfg(windows)]
#[test]
fn handle_identity_rejects_a_different_file_and_normalizes_dos_prefixes() {
    let (dir, files) = fixture("handle-identity");
    let expected = files.root().join("a.txt");
    let outside = dir.path.join("outside.txt");
    std::fs::write(&outside, "before").unwrap();
    let file = open_pinned_file(&outside, true).unwrap();
    assert!(verify_handle_path(&file, &expected).is_err());
    let matching = open_pinned_file(&expected, false).unwrap();
    let actual = verify_handle_path(&matching, &expected).unwrap();
    assert!(path_within(&actual, files.root()).unwrap());
    assert_eq!(
        windows_components(Path::new(r"\\?\C:\Target\File.txt")).unwrap(),
        windows_components(Path::new(r"c:\target\file.TXT")).unwrap()
    );
    assert!(
        !path_within(
            Path::new(r"C:\target-other\file.txt"),
            Path::new(r"C:\target")
        )
        .unwrap()
    );
}

#[cfg(windows)]
#[test]
fn protected_paths_compare_components_across_windows_casing_and_prefixes() {
    let (dir, files) = fixture("protected-identity");
    drop(files);
    let target = dir.path.join("target");
    let protected = PathBuf::from(target.join("a.txt").to_string_lossy().to_ascii_uppercase());
    assert!(ScopedFiles::open(&target, &["a.txt".into()], &[protected]).is_err());
}

#[cfg(windows)]
#[test]
fn control_directories_cannot_be_selected_as_the_absolute_target_root() {
    let dir = crate::workflow_test_support::TestDir::new("protected-root");
    for name in [".git", ".control", ".hidden", ".PROVIDER-STATE"] {
        let root = dir.path.join(name).join("nested");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.txt"), "before").unwrap();
        assert!(ScopedFiles::open(&root, &["a.txt".into()], &[]).is_err());
    }
}

#[cfg(windows)]
#[test]
fn controlled_external_test_root_can_contain_an_ordinary_target() {
    let dir = crate::workflow_test_support::TestDir::new("external-test-root");
    let root = dir.path.join("target");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("a.txt"), "before").unwrap();
    assert!(ScopedFiles::open(&root, &["a.txt".into()], &[]).is_ok());
}

#[cfg(windows)]
#[test]
fn rejects_hardlinked_or_protected_files_and_unlisted_paths() {
    let (dir, files) = fixture("scope");
    assert!(files.read("other.txt").is_err());
    let target = dir.path.join("target");
    assert!(ScopedFiles::open(&target, &["a.txt".into()], &[target.join("a.txt")]).is_err());
    assert!(ScopedFiles::open(&target, &["a.txt".into()], std::slice::from_ref(&target)).is_err());
    std::fs::hard_link(target.join("a.txt"), dir.path.join("link.txt")).unwrap();
    assert!(files.read("a.txt").is_err());
    assert!(
        files
            .prepare(&TextEdit {
                path: "a.txt".into(),
                expected: "before".into(),
                replacement: "after".into()
            })
            .is_err()
    );
}
