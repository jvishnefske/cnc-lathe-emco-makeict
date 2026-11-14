//! `emco-kd240`: the EMCO PC Turn 50 sender as a Linux service on the AMD
//! Kria KD240, with its web UI served over Ethernet.
//!
//! * [`machine`] — the pump thread and its [`machine::Transport`]: by
//!   default `sender/core`'s `Session` against its `VirtualMachine`, the only
//!   machine the service runs, because the RS-485 protocol is unknown
//!   (`docs/30-serial-protocol.md`).
//! * [`linuxcnc`] — a skeleton [`machine::Transport`] onto LinuxCNC's
//!   status, command and error channels (doc 65 S4); not wired in yet.
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
pub mod linuxcnc;
pub mod machine;
pub mod parmrk;
pub mod store;
pub mod ui;
