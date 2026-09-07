//! Which pins, on which board. No logic lives here.
//!
//! `no_std` needs pins as compile-time types, so "the board as data" means one
//! module per board containing a table and nothing else, selected by a cargo
//! feature. Correcting a pin is then a one-line edit in a file with nothing
//! else in it, and getting one wrong produces "the field `de` has type PA1,
//! not PA5" rather than a device that enumerates and never receives.
//!
//! A `build.rs` reading a TOML would be data in the stricter sense. It was
//! considered and rejected for ONE board: it costs a build script, and it
//! turns "that pin does not exist on this part" from a compile error into a
//! runtime surprise. Worth revisiting at board number two.
//!
//! ## Adding a board
//!
//! Copy `f407_discovery.rs`, change the table, add a feature to `Cargo.toml`,
//! and add an arm below. The rest of the firmware does not change, because it
//! only ever names `Board`'s fields.

#[cfg(feature = "board-f407-discovery")]
mod f407_discovery;
#[cfg(feature = "board-f407-discovery")]
pub use f407_discovery::*;

// Exactly one board, or the pin table below is somebody's guess.
#[cfg(not(any(feature = "board-f407-discovery")))]
compile_error!(
    "no board selected. Pass exactly one board feature, e.g. \
     --features board-f407-discovery"
);
