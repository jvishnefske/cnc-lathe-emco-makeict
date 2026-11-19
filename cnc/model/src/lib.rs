//! The lathe's model, shared by its payloads and its tools.
//!
//! * [`config`]: [`PhysicalConfig`], one machine's hardware (the physical
//!   half of the sender's `MachineProfile`, plus the I/O map), and the
//!   integer [`Limits`] the physical payload enforces.
//! * [`cm`]: the control modules (axes, the spindle, switches, the turret,
//!   the e-stop) as traits.
//! * [`wire`]: what the process model sends the physical model beside S88
//!   phase commands (motion segments), and the control modules' published
//!   states.
//! * [`proc`]: the process model's own wire, for the tools that drive it
//!   (operator commands, programs, its published state).
//!
//! `no_std`. The configuration's serde types and the batch encoder need
//! `alloc` (the default feature); a payload without a heap turns it off
//! and keeps [`Limits`], the traits and the wire codecs.
#![no_std]
#![forbid(unsafe_code)]

#[cfg(feature = "alloc")]
extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod cm;
#[cfg(feature = "alloc")]
pub mod config;
pub mod limits;
#[cfg(feature = "alloc")]
pub mod proc;
pub mod wire;

#[cfg(feature = "alloc")]
pub use config::{BeltStep, IoMap, PhysicalConfig, Reference};
pub use limits::Limits;
/// The motion messages, from middleware's ABI.
pub use middleware::wasm_abi::machine::{QueueState, Segment};
