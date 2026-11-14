//! `emco-kd240`: the EMCO PC Turn 50 sender as a Linux service on the AMD
//! Kria KD240, with its web UI served over Ethernet.
//!
//! * [`machine`] — `sender/core`'s `Session` pumped against its
//!   `VirtualMachine` on a thread; the only machine there is, because the
//!   RS-485 protocol is unknown (`docs/30-serial-protocol.md`).
//! * [`capture`] — a listen-only RS-485 capture (read-only tty) in the
//!   capture bridge's record format, decoded by `tools/emco-capture.py`.
//! * [`parmrk`] — the kernel's in-band framing/parity marks, undone.
//! * [`store`] — G-code programs on disk.
//! * [`api`] and [`ui`] — the HTTP API and the embedded RxJS UI.
//!
//! No module writes the machine bus. Nothing here is a safety function: the
//! stop is the cabinet's EMERGENCY-OFF and mains switch, with a person at
//! the machine.
#![forbid(unsafe_code)]

pub mod api;
pub mod capture;
pub mod machine;
pub mod parmrk;
pub mod store;
pub mod ui;
