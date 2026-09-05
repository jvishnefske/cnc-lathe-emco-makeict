//! `emco-sender-core`: the pure, synchronous core of the browser G-code sender
//! for the EMCO PC Turn 50 lathe.
//!
//! All logic lives in plain Rust modules that are unit-testable natively; the
//! thin `wasm-bindgen` layer in [`wasm`] only exists on the `wasm32` target.
//! The core performs no I/O and never reads a clock — bytes and time come from
//! the caller. See `sender/INTERFACE.md` for the contract with the UI.
//!
//! Safety note: nothing in this crate is a safety function. The abort command
//! is advisory; the real stop is the mains switch at the machine.

pub mod dialect;
pub mod gcode;
pub mod geom;
pub mod machine;
pub mod profile;
pub mod session;

#[cfg(target_arch = "wasm32")]
pub mod wasm;
