//! Retain the actual path dependency source boundary in the application binary.
use std::{error::Error, fs, io, path::PathBuf};

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo::rerun-if-changed=Cargo.toml");
    let root = PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR")
            .ok_or_else(|| io::Error::other("Cargo manifest directory is required"))?,
    );
    let manifest: toml::Value = toml::from_str(&fs::read_to_string(root.join("Cargo.toml"))?)?;
    let dependency = manifest
        .get("dependencies")
        .and_then(|dependencies| dependencies.get("recuvora-core"))
        .and_then(|core| core.get("path"))
        .and_then(toml::Value::as_str)
        .ok_or_else(|| io::Error::other("recuvora-core must declare its local source path"))?;
    let source = root.join(dependency).canonicalize()?;
    let source = source.to_str().ok_or_else(|| {
        io::Error::other(
            "Core source directory must be representable in Cargo environment metadata",
        )
    })?;
    println!("cargo::rustc-env=RECUVORA_CORE_SOURCE_DIR={source}");
    Ok(())
}
