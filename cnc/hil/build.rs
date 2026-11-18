//! Builds the payloads for wasm32 in a target directory of their own (a
//! nested cargo on the same directory would wait on its own lock) and
//! tells the tests where they are.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    let ws = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    for dir in [
        "model/src",
        "physical/src",
        "physical/build.rs",
        "model/fixtures",
    ] {
        println!("cargo:rerun-if-changed={}", ws.join(dir).display());
    }
    println!("cargo:rerun-if-env-changed=CNC_PHYSICAL_CONFIG");
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    let fixtures = ws.join("model/fixtures");
    let build = |package: &str, dir: &str, config: Option<PathBuf>| -> PathBuf {
        let target_dir = out.join(dir);
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let mut cmd = Command::new(cargo);
        cmd.current_dir(&ws)
            .args([
                "build",
                "--release",
                "--target",
                "wasm32-unknown-unknown",
                "-p",
                package,
                "--target-dir",
            ])
            .arg(&target_dir)
            // The outer build's flags are for the host.
            .env_remove("RUSTFLAGS")
            .env_remove("CARGO_ENCODED_RUSTFLAGS")
            .env_remove("CARGO_BUILD_TARGET")
            .env_remove("CARGO_TARGET_DIR")
            // Under `cargo clippy` the wrapper would lint the payload in here.
            .env_remove("RUSTC_WORKSPACE_WRAPPER")
            .env_remove("RUSTC_WRAPPER")
            .env_remove("CLIPPY_ARGS")
            .env_remove("CNC_PHYSICAL_CONFIG");
        if let Some(c) = config {
            cmd.env("CNC_PHYSICAL_CONFIG", c);
        }
        let status = cmd.status().expect("cargo");
        assert!(status.success(), "the wasm32 build of {package} failed");
        target_dir.join(format!(
            "wasm32-unknown-unknown/release/{}.wasm",
            package.replace('-', "_")
        ))
    };
    let physical = build("cnc-physical", "wasm", None);
    let alt = build(
        "cnc-physical",
        "wasm-alt",
        Some(fixtures.join("alt_lathe.json")),
    );
    let process = build("cnc-process", "wasm", None);
    println!("cargo:rustc-env=CNC_PHYSICAL_WASM={}", physical.display());
    println!("cargo:rustc-env=CNC_PHYSICAL_ALT_WASM={}", alt.display());
    println!("cargo:rustc-env=CNC_PROCESS_WASM={}", process.display());
}
