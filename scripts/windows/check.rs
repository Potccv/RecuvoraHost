//! Windows development checks with external output paths and child-local environment settings.
use serde::Deserialize;
use std::ffi::OsString;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, ExitCode};

const HELP: &str = "Usage: recuvora-host-check --build-dir PATH --test-temp PATH [--cargo-path PATH]\n\
                   --temp-root is an alias for --test-temp. All output paths must be absolute and outside source trees.";

#[derive(Debug)]
pub(crate) struct Options {
    pub build_dir: PathBuf,
    pub test_temp: PathBuf,
    pub cargo: OsString,
}

pub(crate) fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Option<Options>, String> {
    let mut args = args.into_iter();
    let mut build_dir = None;
    let mut test_temp = None;
    let mut cargo = None;
    while let Some(arg) = args.next() {
        if arg == "--help" || arg == "-h" {
            if build_dir.is_some()
                || test_temp.is_some()
                || cargo.is_some()
                || args.next().is_some()
            {
                return Err("Help must be used alone".into());
            }
            return Ok(None);
        }
        let slot = match arg.to_str() {
            Some("--build-dir") => &mut build_dir,
            Some("--test-temp" | "--temp-root") => &mut test_temp,
            Some("--cargo-path") => &mut cargo,
            _ => return Err(format!("Unknown argument: {}", arg.to_string_lossy())),
        };
        if slot.is_some() {
            return Err(format!("Duplicate option: {}", arg.to_string_lossy()));
        }
        let value = args
            .next()
            .ok_or_else(|| format!("Missing value for {}", arg.to_string_lossy()))?;
        if value.is_empty() || value.to_string_lossy().starts_with("--") {
            return Err(format!("Missing value for {}", arg.to_string_lossy()));
        }
        *slot = Some(value);
    }
    Ok(Some(Options {
        build_dir: build_dir.ok_or("--build-dir is required")?.into(),
        test_temp: test_temp.ok_or("--test-temp is required")?.into(),
        cargo: cargo.unwrap_or_else(|| OsString::from("cargo")),
    }))
}

#[derive(Deserialize)]
struct Metadata {
    packages: Vec<Package>,
}
#[derive(Deserialize)]
struct Package {
    manifest_path: PathBuf,
    dependencies: Vec<Dependency>,
}
#[derive(Deserialize)]
struct Dependency {
    path: Option<PathBuf>,
}

pub(crate) fn source_roots(project: &Path, metadata: &[u8]) -> Result<Vec<PathBuf>, String> {
    let metadata: Metadata =
        serde_json::from_slice(metadata).map_err(|e| format!("Invalid Cargo metadata: {e}"))?;
    let mut roots = vec![project.to_path_buf()];
    for package in metadata.packages {
        roots.push(
            package
                .manifest_path
                .parent()
                .ok_or("Manifest has no parent")?
                .to_path_buf(),
        );
        roots.extend(
            package
                .dependencies
                .into_iter()
                .filter_map(|dependency| dependency.path),
        );
    }
    roots
        .into_iter()
        .map(|root| {
            root.canonicalize()
                .map_err(|e| format!("Cannot resolve source directory {}: {e}", root.display()))
        })
        .collect()
}

fn linked(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

fn within(path: &Path, root: &Path) -> bool {
    #[cfg(windows)]
    {
        let path: Vec<_> = path.components().collect();
        let root: Vec<_> = root.components().collect();
        path.len() >= root.len()
            && path.iter().zip(root).all(|(a, b)| {
                a.as_os_str().to_string_lossy().to_lowercase()
                    == b.as_os_str().to_string_lossy().to_lowercase()
            })
    }
    #[cfg(not(windows))]
    {
        path.starts_with(root)
    }
}

/// Validate without creating directories; resolve existing ancestors before comparing sources.
pub(crate) fn external_directory(path: &Path, roots: &[PathBuf]) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err("Output paths must be absolute".into());
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            #[cfg(windows)]
            Component::Prefix(prefix) => match prefix.kind() {
                std::path::Prefix::VerbatimDisk(drive) => {
                    normalized.push(format!("{}:", char::from(drive)));
                }
                std::path::Prefix::VerbatimUNC(server, share) => {
                    let mut unc = OsString::from("\\\\");
                    unc.push(server);
                    unc.push("\\");
                    unc.push(share);
                    normalized.push(unc);
                }
                _ => normalized.push(prefix.as_os_str()),
            },
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return Err("Output path escapes its root".into());
                }
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    let mut existing = None;
    for ancestor in normalized.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) => {
                if linked(&metadata) {
                    return Err(format!(
                        "Linked output path is not supported: {}",
                        ancestor.display()
                    ));
                }
                if !metadata.is_dir() {
                    return Err(format!(
                        "Output ancestor is not a directory: {}",
                        ancestor.display()
                    ));
                }
                existing.get_or_insert(ancestor);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("Cannot inspect {}: {error}", ancestor.display())),
        }
    }
    let existing = existing.ok_or("Output path has no existing parent")?;
    let resolved = existing.canonicalize().map_err(|e| e.to_string())?.join(
        normalized
            .strip_prefix(existing)
            .map_err(|e| e.to_string())?,
    );
    if roots.iter().any(|root| within(&resolved, root)) {
        return Err(format!(
            "Output must be outside source trees: {}",
            resolved.display()
        ));
    }
    // Canonical Windows paths may use a verbatim prefix unsupported by external linkers.
    // Keep the canonical comparison above, but pass the validated ordinary path to Cargo.
    Ok(normalized)
}

pub(crate) fn prepare(options: &mut Options, roots: &[PathBuf]) -> Result<(), String> {
    let build_dir = external_directory(&options.build_dir, roots)?;
    let test_temp = external_directory(&options.test_temp, roots)?;
    for path in [&build_dir, &test_temp] {
        fs::create_dir_all(path).map_err(|e| format!("Cannot create {}: {e}", path.display()))?;
    }
    options.build_dir = build_dir;
    options.test_temp = test_temp;
    Ok(())
}

/// Stop on the first failure. Only child commands receive the check environment and directory.
pub(crate) fn run_checks(
    options: &Options,
    project: &Path,
    mut run: impl FnMut(&mut Command) -> Result<(), String>,
) -> Result<(), String> {
    let checks: &[&[&str]] = &[
        &["fmt", "--package", "recuvora-host", "--", "--check"],
        &["check", "--all-targets", "--locked"],
        &["test", "--all-targets", "--locked"],
        &["test", "--doc", "--locked"],
        &[
            "clippy",
            "--all-targets",
            "--locked",
            "--",
            "-D",
            "warnings",
        ],
    ];
    for args in checks {
        println!("Running cargo {}", args.join(" "));
        let mut command = Command::new(&options.cargo);
        command
            .args(*args)
            .current_dir(project)
            .env("CARGO_TARGET_DIR", &options.build_dir)
            .env("RECUVORA_TEST_TEMP", &options.test_temp)
            .env("RUST_TEST_THREADS", "4");
        run(&mut command).map_err(|e| format!("cargo {} failed: {e}", args.join(" ")))?;
    }
    Ok(())
}

fn execute(mut options: Options) -> Result<(), String> {
    let project = Path::new(env!("CARGO_MANIFEST_DIR"));
    let metadata = Command::new(&options.cargo)
        .args(["metadata", "--manifest-path"])
        .arg(project.join("Cargo.toml"))
        .args(["--no-deps", "--format-version", "1", "--locked"])
        .current_dir(project)
        .output()
        .map_err(|e| format!("Cannot run Cargo metadata: {e}"))?;
    if !metadata.status.success() {
        return Err(format!(
            "Cargo metadata failed ({}): {}",
            metadata.status,
            String::from_utf8_lossy(&metadata.stderr)
        ));
    }
    let roots = source_roots(project, &metadata.stdout)?;
    prepare(&mut options, &roots)?;
    run_checks(&options, project, |command| {
        let status = command.status().map_err(|e| e.to_string())?;
        if status.success() {
            Ok(())
        } else {
            Err(status.to_string())
        }
    })
}

fn main() -> ExitCode {
    match parse(std::env::args_os().skip(1)) {
        Ok(None) => {
            println!("{HELP}");
            ExitCode::SUCCESS
        }
        Ok(Some(options)) => match execute(options) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("{error}");
                ExitCode::FAILURE
            }
        },
        Err(error) => {
            eprintln!("{error}\n{HELP}");
            ExitCode::from(2)
        }
    }
}
