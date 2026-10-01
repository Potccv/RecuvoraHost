//! Output protection and fail-fast behavior of the Rust development check tool.
#[allow(dead_code)]
#[path = "../scripts/windows/check.rs"]
mod check_script;
mod workflow_support;

use check_script::{Options, external_directory, parse, prepare, run_checks, source_roots};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use workflow_support::TestDir;

fn options(build_dir: PathBuf, test_temp: PathBuf) -> Options {
    Options {
        build_dir,
        test_temp,
        cargo: "cargo".into(),
    }
}

#[test]
fn arguments_require_outputs_and_reject_ambiguous_options() {
    for args in [
        vec![],
        vec!["--build-dir", "output"],
        vec!["--build-dir", "--test-temp", "output"],
        vec!["--unknown"],
        vec!["--test-temp", "a", "--temp-root", "b"],
        vec!["--help", "--build-dir", "a"],
    ] {
        assert!(parse(args.into_iter().map(OsString::from)).is_err());
    }
    assert!(parse([OsString::from("--help")]).unwrap().is_none());
    let config = parse(
        [
            "--build-dir",
            "build space",
            "--temp-root",
            "temporary space",
            "--cargo-path",
            "cargo space",
        ]
        .map(OsString::from),
    )
    .unwrap()
    .unwrap();
    assert_eq!(config.test_temp, Path::new("temporary space"));
    assert_eq!(config.cargo, "cargo space");
}

#[test]
fn metadata_protects_host_and_local_dependency_sources() {
    let dir = TestDir::new("check-metadata");
    let host = dir.path.join("host");
    let core = dir.path.join("core");
    std::fs::create_dir(&host).unwrap();
    std::fs::create_dir(&core).unwrap();
    let metadata = serde_json::json!({"packages":[{"manifest_path":host.join("Cargo.toml"),"dependencies":[{"path":core},{"path":null}]}]});
    let roots = source_roots(&host, &serde_json::to_vec(&metadata).unwrap()).unwrap();
    for path in [&host, &core] {
        assert!(external_directory(&path.join("nested/output"), &roots).is_err());
        assert!(!path.join("nested").exists());
    }
    assert!(external_directory(&dir.path.join("host-output"), &roots).is_ok());
    assert!(source_roots(&host, b"{}").is_err());
}

#[test]
fn invalid_second_output_does_not_create_first_output() {
    let dir = TestDir::new("check-prepare");
    let build = dir.path.join("new-build");
    let mut config = options(build.clone(), PathBuf::from("relative-test-root"));
    assert!(prepare(&mut config, &[]).is_err());
    assert!(!build.exists());
    let file = dir.path.join("file");
    std::fs::write(&file, "existing file").unwrap();
    assert!(external_directory(&file, &[]).is_err());
}

#[test]
fn external_outputs_preserve_existing_data_and_normalize_parent_segments() {
    let dir = TestDir::new("check-existing");
    let build = dir.path.join("build with spaces");
    std::fs::create_dir(&build).unwrap();
    std::fs::write(build.join("keep.txt"), "preserved").unwrap();
    let mut config = options(build.clone(), dir.path.join("temporary"));
    prepare(&mut config, &[]).unwrap();
    #[cfg(windows)]
    {
        assert!(!config.build_dir.to_string_lossy().starts_with(r"\\?\"));
        let canonical = build.canonicalize().unwrap();
        let ordinary = external_directory(&canonical, &[]).unwrap();
        assert!(!ordinary.to_string_lossy().starts_with(r"\\?\"));
        assert_eq!(ordinary.canonicalize().unwrap(), canonical);
    }
    assert_eq!(
        std::fs::read_to_string(build.join("keep.txt")).unwrap(),
        "preserved"
    );
    assert!(config.test_temp.is_dir());
    let source = dir.path.join("source");
    std::fs::create_dir(&source).unwrap();
    assert!(
        external_directory(
            &build.join("../source/output"),
            &[source.canonicalize().unwrap()]
        )
        .is_err()
    );
}

#[test]
fn first_failed_check_stops_dispatch_and_keeps_parent_settings() {
    let dir = TestDir::new("check-dispatch");
    let config = options(dir.path.join("build"), dir.path.join("test"));
    let cwd = std::env::current_dir().unwrap();
    let keys = [
        "CARGO_TARGET_DIR",
        "RECUVORA_TEST_TEMP",
        "RUST_TEST_THREADS",
    ];
    let saved = keys.map(std::env::var_os);
    let mut calls = Vec::new();
    let result = run_checks(&config, &dir.path, |command| {
        assert_eq!(command.get_current_dir(), Some(dir.path.as_path()));
        assert_eq!(command.get_program(), "cargo");
        let child_env: std::collections::HashMap<_, _> = command.get_envs().collect();
        assert_eq!(
            child_env[std::ffi::OsStr::new("CARGO_TARGET_DIR")],
            Some(config.build_dir.as_os_str())
        );
        assert_eq!(
            child_env[std::ffi::OsStr::new("RECUVORA_TEST_TEMP")],
            Some(config.test_temp.as_os_str())
        );
        assert_eq!(
            child_env[std::ffi::OsStr::new("RUST_TEST_THREADS")],
            Some(std::ffi::OsStr::new("4"))
        );
        calls.push(
            command
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
        );
        if calls.len() == 3 {
            Err("injected failure".into())
        } else {
            Ok(())
        }
    });
    assert!(result.unwrap_err().contains("injected failure"));
    assert_eq!(calls.len(), 3);
    assert_eq!(
        calls[0],
        ["fmt", "--package", "recuvora-host", "--", "--check"]
    );
    assert_eq!(calls[2], ["test", "--all-targets", "--locked"]);
    assert_eq!(std::env::current_dir().unwrap(), cwd);
    assert_eq!(keys.map(std::env::var_os), saved);
}

#[test]
fn successful_dispatch_includes_doc_tests_and_clippy() {
    let dir = TestDir::new("check-success");
    let config = options(dir.path.join("build"), dir.path.join("test"));
    let mut calls = Vec::new();
    run_checks(&config, &dir.path, |command| {
        calls.push(
            command
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
        );
        Ok(())
    })
    .unwrap();
    assert_eq!(calls.len(), 5);
    assert_eq!(calls[3], ["test", "--doc", "--locked"]);
    assert_eq!(
        calls[4],
        [
            "clippy",
            "--all-targets",
            "--locked",
            "--",
            "-D",
            "warnings"
        ]
    );
}

#[test]
fn linked_output_ancestors_are_rejected() {
    let dir = TestDir::new("check-linked");
    let target = dir.path.join("target");
    let link = dir.path.join("link");
    std::fs::create_dir(&target).unwrap();
    #[cfg(windows)]
    {
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(&target)
            .status()
            .unwrap();
        assert!(status.success());
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(&target, &link).unwrap();
    assert!(external_directory(&link.join("new/output"), &[]).is_err());
    assert!(!target.join("new").exists());
}
