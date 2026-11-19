//! The machine link: what the screen needs from whatever runs the machine.
//!
//! Shaped like LinuxCNC's three channels, so that the screen does not
//! change when `docs/65-kd240-controller-plan.md` step S4 puts LinuxCNC
//! behind it instead of `sender/core`'s virtual machine:
//!
//! | LinuxCNC | here |
//! |---|---|
//! | `linuxcnc.stat` (polled status) | [`Incoming::Status`], plus [`Incoming::Event`] for things a snapshot would miss |
//! | `linuxcnc.command` | [`MachineLink::send`] with a [`Request`] |
//! | `linuxcnc.error_channel` | [`Incoming::Error`] |
//!
//! Requests are fire-and-forget; what happened comes back through
//! [`MachineLink::poll`] as a [`Reply`] or an error, in the order the
//! requests were sent. A link says what it cannot do in its
//! [`Capabilities`], and the screen shows that rather than pretending.
//!
//! The one implementation is [`crate::kd240`]: the `emco-kd240` service's
//! HTTP + SSE API.

use emco_sender_core::profile::MachineProfile;

/// The machine's run state, as the screen needs to tell them apart.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunState {
    Disconnected,
    Idle,
    Running,
    Hold,
    Alarm,
    Homing,
    Toolchange,
    /// Anything a link reports that is not one of the above.
    Other(String),
}

impl RunState {
    pub fn from_name(s: &str) -> RunState {
        match s {
            "disconnected" => RunState::Disconnected,
            "idle" => RunState::Idle,
            "running" => RunState::Running,
            "hold" => RunState::Hold,
            "alarm" => RunState::Alarm,
            "homing" => RunState::Homing,
            "toolchange" => RunState::Toolchange,
            other => RunState::Other(other.to_string()),
        }
    }

    pub fn name(&self) -> &str {
        match self {
            RunState::Disconnected => "disconnected",
            RunState::Idle => "idle",
            RunState::Running => "running",
            RunState::Hold => "hold",
            RunState::Alarm => "alarm",
            RunState::Homing => "homing",
            RunState::Toolchange => "toolchange",
            RunState::Other(s) => s,
        }
    }

    /// Something is executing or waiting to continue (LinuxCNC: the
    /// interpreter is not idle).
    pub fn busy(&self) -> bool {
        matches!(
            self,
            RunState::Running | RunState::Hold | RunState::Toolchange | RunState::Homing
        )
    }
}

/// Positions, mm. X is radius throughout; diameter is a display matter.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Position {
    /// Machine coordinates.
    pub x: f64,
    pub z: f64,
    /// Work coordinates (the active work offset applied).
    pub x_work: f64,
    pub z_work: f64,
    /// Distance to go on the current move.
    pub x_dtg: f64,
    pub z_dtg: f64,
}

impl Position {
    /// The work offset: the machine coordinate of work zero.
    pub fn work_offset(&self) -> (f64, f64) {
        (self.x - self.x_work, self.z - self.z_work)
    }
}

/// An active alarm.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Alarm {
    pub code: String,
    pub message: String,
}

/// One status snapshot (LinuxCNC's `stat`, as much of it as the screen
/// reads).
#[derive(Clone, Debug, PartialEq)]
pub struct Status {
    pub run: RunState,
    /// E-stop is active, until reset.
    pub estop: bool,
    /// The machine is on: not in E-stop, and the control is up.
    pub machine_on: bool,
    /// Per joint.
    pub homed: Vec<bool>,
    pub pos: Position,
    /// Where the simulator says the carriage really is (machine X radius,
    /// Z), when the link has a simulator behind it. It differs from `pos`
    /// after a stall.
    pub sim: Option<(f64, f64)>,
    /// 0-based program line executing, if a program is running.
    pub current_line: Option<usize>,
    /// Tool in position, 0 = none or unknown.
    pub tool: u32,
    /// Actual spindle speed, rpm (speed times override where the link has
    /// one).
    pub spindle_rpm: f64,
    pub spindle_on: bool,
    pub feed_mm_min: f64,
    /// Active modal codes for display, e.g. `G21 G90 G95 G01 G54`.
    pub modal: Vec<String>,
    /// G7 (diameter mode) is in force.
    pub diameter_mode: bool,
    /// False when `diameter_mode` is an assumption (the link could not
    /// tell).
    pub diameter_mode_known: bool,
    /// A threading move (G33, G33.1, G76) is in force.
    pub threading: bool,
    pub alarms: Vec<Alarm>,
    pub single_block: bool,
    pub dry_run: bool,
    /// A short name for what is on the other end, e.g. `"virtual"`.
    pub transport: String,
}

impl Status {
    pub fn all_homed(&self) -> bool {
        !self.homed.is_empty() && self.homed.iter().all(|h| *h)
    }

    /// A program (not MDI, not a jog) is executing or held.
    pub fn program_running(&self) -> bool {
        self.current_line.is_some()
            && matches!(
                self.run,
                RunState::Running | RunState::Hold | RunState::Toolchange
            )
    }

    /// LinuxCNC's `is_auto_running`.
    pub fn auto_running(&self) -> bool {
        self.program_running()
    }

    pub fn paused(&self) -> bool {
        self.run == RunState::Hold
    }

    /// LinuxCNC's `interp_state == INTERP_IDLE`.
    pub fn interp_idle(&self) -> bool {
        !self.run.busy()
    }
}

/// Things that happened, which a snapshot can miss.
#[derive(Clone, Debug, PartialEq)]
pub enum MachineEvent {
    /// A block went to the machine. `line` is the 0-based program line,
    /// `None` for MDI, jog and home blocks.
    Sent {
        line: Option<usize>,
        text: String,
    },
    Acked {
        line: Option<usize>,
    },
    State {
        from: RunState,
        to: RunState,
    },
    /// The link dropped this many events; the screen cannot trust what it
    /// inferred from them.
    Lagged(u64),
}

/// What kind of error (LinuxCNC's error channel kinds, roughly).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    /// The machine reported an alarm or fault (`NML_ERROR`).
    Machine,
    /// A request was refused (`OPERATOR_ERROR`).
    Operator,
    /// The link itself failed (network, server). Not the machine's doing.
    Link,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ErrorMsg {
    pub kind: ErrorKind,
    pub text: String,
}

/// The link's own state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkState {
    Connecting,
    Connected,
    /// Lost; the link is trying again.
    Lost(String),
}

/// A machine axis the operator can jog or touch off.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    X,
    Z,
}

/// Why an MDI was sent, so its reply can be matched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MdiTag {
    Operator,
    Wizard,
    Spindle,
}

/// The command channel.
#[derive(Clone, Debug, PartialEq)]
pub enum Request {
    /// Cycle start: run the loaded program from the top, or continue from
    /// hold.
    Run,
    Hold,
    Resume,
    /// Best-effort stop. Advisory: never a safety function.
    Abort,
    Home,
    SingleBlock(bool),
    DryRun(bool),
    /// Incremental jog, `delta_mm` of radius for X.
    Jog {
        axis: Axis,
        delta_mm: f64,
        feed_mm_min: f64,
    },
    /// MDI lines, executed in order.
    Mdi {
        lines: Vec<String>,
        tag: MdiTag,
    },
    /// Feed override, percent. Only if [`Capabilities::feed_override`].
    FeedOverride(u32),
    /// A training fault on a simulated machine: `"estop"`, `"limit_x"`,
    /// `"limit_z"`, `"turret_jam"`.
    Fault(String),
    Profile,
    ListPrograms,
    ReadProgram(String),
    WriteProgram {
        name: String,
        text: String,
    },
    DeleteProgram(String),
    LoadProgram(String),
    /// Which program the machine has loaded.
    LoadedProgram,
}

/// A stored program.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProgramInfo {
    pub name: String,
    pub bytes: u64,
}

/// A diagnostic from the machine's own program parser (0-based line).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub line: usize,
    pub column: usize,
    pub error: bool,
    pub message: String,
}

/// The machine profile (`sender/INTERFACE.md`), comparable.
#[derive(Clone, Debug)]
pub struct Profile(pub MachineProfile);

impl PartialEq for Profile {
    /// `MachineProfile` has no `PartialEq`; its JSON is the comparison.
    fn eq(&self, other: &Self) -> bool {
        serde_json::to_value(&self.0).ok() == serde_json::to_value(&other.0).ok()
    }
}

/// What a request produced.
#[derive(Clone, Debug, PartialEq)]
pub enum Reply {
    Profile(Box<Profile>),
    Programs(Vec<ProgramInfo>),
    ProgramText {
        name: String,
        text: String,
    },
    Written(String),
    Deleted(String),
    /// The machine parsed and (if `loaded`) loaded the program. It refuses
    /// a program its own parser finds errors in.
    Loaded {
        name: String,
        loaded: bool,
        diagnostics: Vec<Diagnostic>,
    },
    /// The loaded program's name, if one is loaded.
    LoadedProgram(Option<String>),
    /// An MDI was accepted (queued, not yet finished).
    MdiAccepted(MdiTag),
    /// A command was accepted.
    Accepted(String),
}

/// One thing from [`MachineLink::poll`].
#[derive(Clone, Debug, PartialEq)]
pub enum Incoming {
    Link(LinkState),
    Status(Box<Status>),
    Event(MachineEvent),
    Error(ErrorMsg),
    Reply(Reply),
}

/// What a link can and cannot do. The screen greys out, and says why,
/// anything a link lacks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capabilities {
    pub feed_override: bool,
    /// Tool offsets (G10 L10, G43).
    pub tool_table: bool,
    /// Jog until released (otherwise increments only).
    pub continuous_jog: bool,
    /// A simulator that takes training faults.
    pub training_faults: bool,
    /// Abort is also how an E-stop is reset.
    pub abort_resets: bool,
}

/// A machine link. See the module documentation.
pub trait MachineLink {
    /// Queues a request. Requests reach the machine in the order sent.
    fn send(&mut self, req: Request);
    /// Everything that arrived since the last call.
    fn poll(&mut self) -> Vec<Incoming>;
    fn capabilities(&self) -> Capabilities;
    /// The credential for requests that change things. Never logged.
    fn set_token(&mut self, token: Option<String>);
}
