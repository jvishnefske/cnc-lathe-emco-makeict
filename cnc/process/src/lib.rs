//! The lathe's process model: a WASM payload for `mw-posix-node`.
//!
//! The CNC business logic, separate from any machine: operator commands,
//! the reference gate, the screen's preflight, and G-code (the browser
//! sender's own interpreter) turned into the physical unit's phase
//! commands and motion segments. It learns the machine from the unit it is
//! assigned, so one build runs on every physical model.
//!
//! * [`compile`]: a program as the unit's operations.
//! * [`process`]: the model as a pure state machine (host-tested).
//! * `payload` (wasm32 only): the glue to the ABI.
//!
//! Unlike the physical model this is a `std` crate (the sender's core and
//! the screen's rules are), built for wasm32-unknown-unknown with std's
//! own allocator and panic handler; it imports nothing beyond the ABI.
#![forbid(unsafe_code)]

pub mod compile;
pub mod process;

#[cfg(target_arch = "wasm32")]
mod payload;
