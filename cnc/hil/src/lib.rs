//! Host-in-the-loop tests (`tests/`): the payloads exactly as built for
//! wasm32 (`build.rs` builds them and exports their paths as
//! `CNC_PHYSICAL_WASM`), installed, bound and started in mw-posix-node's
//! own controller with its simulated lathe, and driven through the unit's
//! keys as a process model would.

/// The built physical payload, for the PC Turn 50.
pub const PHYSICAL_WASM: &str = env!("CNC_PHYSICAL_WASM");
/// The physical payload built for `model/fixtures/alt_lathe.json`.
pub const PHYSICAL_ALT_WASM: &str = env!("CNC_PHYSICAL_ALT_WASM");
/// The built process payload (one build for every machine).
pub const PROCESS_WASM: &str = env!("CNC_PROCESS_WASM");
