//! Bakes one machine's `PhysicalConfig` into the payload as integer
//! constants (`machine.rs` in OUT_DIR): `CNC_PHYSICAL_CONFIG` names its
//! JSON (default: the PC Turn 50, `../model/fixtures/pc_turn_50.json`).
//! A payload is built for one machine: its image hash, which the node's
//! binding is signed over, covers the numbers it enforces.

use std::path::PathBuf;

fn main() {
    let path = std::env::var_os("CNC_PHYSICAL_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../model/fixtures/pc_turn_50.json")
        });
    println!("cargo:rerun-if-env-changed=CNC_PHYSICAL_CONFIG");
    println!("cargo:rerun-if-changed={}", path.display());
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let cfg: cnc_model::PhysicalConfig =
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    if let Err(e) = cfg.check() {
        panic!("{}: {e}", path.display());
    }
    let l = cfg.limits();
    let out = format!(
        "/// The machine this payload is built for.\n\
         pub const NAME: &str = {name:?};\n\
         /// Its limits ([`cnc_model::Limits`]).\n\
         pub const LIMITS: cnc_model::Limits = cnc_model::Limits {{\n\
         x_min_um: {x_min_um}, x_max_um: {x_max_um}, z_min_um: {z_min_um}, z_max_um: {z_max_um},\n\
         rapid_um_s: {rapid_um_s}, max_feed_um_s: {max_feed_um_s}, spindle_max_rpm: {spindle_max_rpm},\n\
         turret_stations: {turret_stations}, ref_x_um: {ref_x_um}, ref_z_um: {ref_z_um},\n\
         search_um_s: {search_um_s}, ref_x_first: {ref_x_first}, has_limits: {has_limits},\n\
         counts_per_mm: {counts_per_mm}, threading_max_rpm: {threading_max_rpm},\n\
         belt_rpm: {belt_rpm:?}, belts: {belts},\n\
         }};\n",
        name = cfg.name,
        x_min_um = l.x_min_um,
        x_max_um = l.x_max_um,
        z_min_um = l.z_min_um,
        z_max_um = l.z_max_um,
        rapid_um_s = l.rapid_um_s,
        max_feed_um_s = l.max_feed_um_s,
        spindle_max_rpm = l.spindle_max_rpm,
        turret_stations = l.turret_stations,
        ref_x_um = l.ref_x_um,
        ref_z_um = l.ref_z_um,
        search_um_s = l.search_um_s,
        ref_x_first = l.ref_x_first,
        has_limits = l.has_limits,
        counts_per_mm = l.counts_per_mm,
        threading_max_rpm = l.threading_max_rpm,
        belt_rpm = l.belt_rpm,
        belts = l.belts,
    );
    let dest = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR")).join("machine.rs");
    std::fs::write(dest, out).expect("write machine.rs");
}
