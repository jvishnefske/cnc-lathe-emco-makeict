//! `emco-egui`: the emco_lathe operator screen as an egui app on wasm32,
//! served by `emco-kd240` at `/egui/`.
//!
//! * [`link`]: the transport-agnostic machine link (status, command and
//!   error channels, as LinuxCNC's), so the screen survives the move from
//!   `sender/core`'s virtual machine to LinuxCNC (doc 65 step S4).
//! * [`kd240`]: the one link there is, the service's HTTP + SSE API.
//! * [`screen`]: the screen's state and decisions (`emco_lathe_handler.py`),
//!   using `emco-rules` for every rule.
//! * [`view`] and [`toolpath`]: drawing, in immediate mode.
//!
//! Nothing here moves the real lathe, and nothing is a safety function: the
//! abort is advisory, and says so next to its button. There is no remote or
//! unattended operation feature, and there must not be one.
#![forbid(unsafe_code)]

pub mod kd240;
pub mod link;
pub mod screen;
pub mod toolpath;
pub mod view;

#[cfg(target_arch = "wasm32")]
mod web;
