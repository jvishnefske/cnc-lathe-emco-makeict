//! Embeds the egui screen (`web/`, served at `/egui/`) when it has been
//! built, and only then, so building the service never needs a wasm
//! toolchain.
//!
//! The screen is built separately (`web/build.sh`, or the Nix package) into
//! a directory holding `index.html`, `emco_egui.js` and `emco_egui_bg.wasm`:
//! `$EMCO_EGUI_DIST` if set, else `web/dist`. With all three present this
//! sets `cfg(egui_dist)` and `/egui/` serves them; without, `/egui/` says
//! how to build it, and cargo prints a warning.

use std::path::PathBuf;

const FILES: [&str; 3] = ["index.html", "emco_egui.js", "emco_egui_bg.wasm"];

fn main() {
    println!("cargo::rustc-check-cfg=cfg(egui_dist)");
    println!("cargo::rerun-if-env-changed=EMCO_EGUI_DIST");
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets it"));
    let dir = match std::env::var_os("EMCO_EGUI_DIST") {
        Some(d) => PathBuf::from(d),
        None => manifest.join("web").join("dist"),
    };
    // Watch the directory too, so a first build of the screen is noticed.
    println!("cargo::rerun-if-changed={}", dir.display());
    for f in FILES {
        println!("cargo::rerun-if-changed={}", dir.join(f).display());
    }
    if FILES.iter().all(|f| dir.join(f).is_file()) {
        println!("cargo::rustc-cfg=egui_dist");
        println!("cargo::rustc-env=EMCO_EGUI_DIST_DIR={}", dir.display());
    } else {
        println!(
            "cargo::warning=the egui screen is not built ({} missing): /egui/ will say so. Run web/build.sh, then rebuild.",
            dir.display()
        );
    }
}
