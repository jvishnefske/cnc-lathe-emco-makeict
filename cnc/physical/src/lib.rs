//! The lathe's physical model: a WASM payload for `mw-posix-node` (ABI 8).
//!
//! It is the lathe **unit** of the S88 physical model: equipment modules
//! motion (X, Z), spindle and turret, and the safety chain, each over the
//! node's control-module points (machine channels bound to a driver: the
//! simulator, the KD240's inputs, later LinuxCNC). It serves the unit's
//! equipment phases to a process model and publishes every control
//! module's state ([`cnc_model::wire`]). It never generates steps: motion
//! leaves as whole segments for the driver to run.
//!
//! * [`lathe`]: the model as a pure state machine (host-tested).
//! * [`machine`]: the machine it is built for (`build.rs`, from a
//!   `PhysicalConfig`).
//! * `payload` (wasm32 only): the glue to the ABI.
#![no_std]
#![forbid(unsafe_code)]

#[cfg(test)]
extern crate std;

pub mod lathe;

/// The machine this build is for.
pub mod machine {
    include!(concat!(env!("OUT_DIR"), "/machine.rs"));
}

#[cfg(target_arch = "wasm32")]
mod payload;
