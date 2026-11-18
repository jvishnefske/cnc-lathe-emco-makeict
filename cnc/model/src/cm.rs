//! The control modules of a turning machine, as the physical model sees
//! them: what each one reads and what it can be told to do.
//!
//! The physical payload implements these over its machine channels (and
//! so over whatever driver a node binds them to); tests implement them
//! with fakes. `None` from a reading means nothing has been read yet: the
//! physical model never acts on a value it has not seen.

/// A switch: a reference switch, a limit switch, the turret's in-position
/// signal.
pub trait SwitchInput {
    /// Whether it is made.
    fn made(&self) -> Option<bool>;
}

/// One linear axis.
pub trait Axis {
    /// The step count the driver reports.
    fn counts(&self) -> Option<i64>;
    /// Whether the axis has found its reference since it last lost it.
    fn referenced(&self) -> bool;
    /// Machine position, µm: only once referenced.
    fn position_um(&self) -> Option<i32>;
}

/// The spindle.
pub trait Spindle {
    /// What it was told, rpm (negative: reverse).
    fn commanded(&self) -> i32;
    /// What it turns, rpm.
    fn actual(&self) -> Option<i32>;
    /// Tell it a speed.
    fn command(&mut self, rpm: i32);
}

/// The tool turret.
pub trait Turret {
    /// The station selected (the one it is on, or going to).
    fn station(&self) -> u8;
    /// Whether it is locked on that station.
    fn ready(&self) -> Option<bool>;
    /// Index to a station.
    fn select(&mut self, station: u8);
}

/// The e-stop chain.
pub trait EStop {
    /// Closed: the machine may move.
    fn chain_closed(&self) -> Option<bool>;
}
