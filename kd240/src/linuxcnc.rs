//! A LinuxCNC transport: the web UI on LinuxCNC's status, command and error
//! channels (doc 65 §3 S4). **A skeleton.** It's tested against a fake
//! LinuxCNC; nothing in the service constructs it, and there is no LinuxCNC
//! on the board yet.
//!
//! # Design note
//!
//! Under doc 65's option A, LinuxCNC on the A53 owns motion: its own G-code
//! interpreter, trajectory planner and HAL, with HostMot2 in the PL. The
//! service then stops being a sender. It doesn't stream blocks; it forwards
//! the operator's commands to LinuxCNC and shows LinuxCNC's state, in the
//! same `SessionState` shape the virtual transport produces, so the UI is
//! unchanged.
//!
//! LinuxCNC's user-interface API is three channels, which its Python module
//! `linuxcnc` exposes as `stat` (poll the task, interpreter and motion
//! status), `command` (state, mode, MDI, program open/run/pause/resume,
//! abort, home, jog) and `error_channel` (operator errors and messages).
//! [`Channels`] is those three, reduced to what this UI needs. The channels
//! are NML over shared memory, so a Rust process doesn't reach them directly.
//! Three ways to bridge them, best first:
//!
//! 1. **A Python sidecar** started by the same systemd unit: `import
//!    linuxcnc`, poll `stat` at the pump's rate, answer JSON lines on a Unix
//!    socket. It has the whole `stat` structure and the error channel, and
//!    it's the API every LinuxCNC GUI (including this repo's `emco_lathe`
//!    QtVCP screen) uses. The cost is a second process.
//! 2. **`linuxcncrsh`**, LinuxCNC's text-over-TCP shell (port 5007). No
//!    extra code on the LinuxCNC side, but its `get`/`set` vocabulary is
//!    narrower (no error channel as such, coarse interpreter state) and it
//!    needs a hello/enable handshake per connection.
//! 3. **FFI to the NML C++ classes.** Rejected: `#![forbid(unsafe_code)]`
//!    in this crate, and a C++ ABI to track across LinuxCNC versions.
//!
//! So [`Channels`] is shaped like the Python module's API, and the sidecar
//! will implement it over its socket. The mapping below ([`state_of`] and
//! [`LinuxCncTransport::command`]) is the part that's independent of the
//! bridge, and it's what the tests pin.
//!
//! What the transport doesn't do:
//!
//! * **No training faults.** A real controller's faults are real;
//!   [`Transport::fault`] answers `false`.
//! * **No dry run.** LinuxCNC has no equivalent of the sender's "send blocks,
//!   don't move"; the command is refused with an alarm event.
//! * **No safety.** As everywhere in this service: the UI's abort is advisory,
//!   the stop is the cabinet's EMERGENCY-OFF, and there is no remote or
//!   unattended operation (doc 35 §6, doc 65 §3 S4). LinuxCNC's own E-stop
//!   chain (`iocontrol.0.emc-enable-in`) is wired to the cabinet's, not to
//!   this UI.
//!
//! Axis conventions: a lathe configuration (`[TRAJ] COORDINATES = XZ`) has
//! axis indices X = 0 and Z = 2 in LinuxCNC's position arrays. LinuxCNC
//! reports X as a radius whatever G7/G8 is active, and the DRO doubles it in
//! diameter mode, so `x_radius_mm` is LinuxCNC's X as it is.

use crate::machine::{Poll, Transport};
use emco_sender_core::gcode::Program;
use emco_sender_core::session::{AlarmInfo, Axis, Command, Dro, Event, RunState, SessionState};
use serde_json::{json, Value};

/// LinuxCNC's `stat.task_state`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskState {
    Estop,
    EstopReset,
    Off,
    On,
}

/// LinuxCNC's `stat.task_mode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskMode {
    Manual,
    Auto,
    Mdi,
}

/// LinuxCNC's `stat.interp_state`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InterpState {
    Idle,
    Reading,
    Paused,
    Waiting,
}

/// The part of LinuxCNC's `stat` this UI shows. Positions in mm, X as a radius.
#[derive(Clone, Debug, PartialEq)]
pub struct Status {
    pub task_state: TaskState,
    pub task_mode: TaskMode,
    pub interp_state: InterpState,
    /// `stat.homed` for the X and Z joints.
    pub homed: [bool; 2],
    /// True while any joint is homing (`stat.joint[i]["homing"]`).
    pub homing: bool,
    /// `stat.actual_position[0]` and `[2]`: machine coordinates.
    pub machine_xz: [f64; 2],
    /// `stat.g5x_offset[0]` and `[2]` plus `stat.g92_offset`: machine minus work.
    pub offset_xz: [f64; 2],
    /// `stat.dtg[0]` and `[2]`.
    pub dtg_xz: [f64; 2],
    /// `stat.motion_line`, 0-based here; -1 when not running.
    pub line: i32,
    /// `stat.tool_in_spindle`: the turret station.
    pub tool: u32,
    /// `stat.spindle[0]["speed"]` (rpm, signed by direction).
    pub spindle_rpm: f64,
    /// `stat.settings[1]`: the commanded feed.
    pub feed_mm_min: f64,
    /// `stat.gcodes` rendered (`"G7"`, `"G18"`, …).
    pub modal: Vec<String>,
}

/// One message from `error_channel.poll()`.
#[derive(Clone, Debug, PartialEq)]
pub struct ErrorMessage {
    /// `"error"`, `"text"` or `"display"` (LinuxCNC's `OPERATOR_*` kinds).
    pub kind: String,
    pub text: String,
}

/// LinuxCNC's `command` channel, reduced to what the UI sends.
#[derive(Clone, Debug, PartialEq)]
pub enum LcncCommand {
    Mode(TaskMode),
    /// `command.program_open(path)`: the bridge writes the text to a file.
    ProgramOpen {
        name: String,
        text: String,
    },
    /// `command.auto(AUTO_RUN, 0)`.
    AutoRun,
    /// `command.auto(AUTO_STEP)`: one block, for single-block mode.
    AutoStep,
    AutoPause,
    AutoResume,
    Abort,
    Mdi(String),
    /// `command.home(-1)`: all joints in their HOME_SEQUENCE.
    HomeAll,
    /// `command.jog(JOG_INCREMENT, False, axis, vel, incr)`: vel in mm/s.
    JogIncrement {
        axis: usize,
        vel_mm_s: f64,
        incr_mm: f64,
    },
}

/// The bridge to a running LinuxCNC (see the design note).
pub trait Channels {
    /// `stat.poll()`; `None` while LinuxCNC isn't reachable.
    fn status(&mut self) -> Option<Status>;
    /// Sends one command; `Err` with LinuxCNC's or the bridge's reason.
    fn send(&mut self, c: LcncCommand) -> Result<(), String>;
    /// `error_channel.poll()` until empty.
    fn errors(&mut self) -> Vec<ErrorMessage>;
}

/// LinuxCNC's axis index for a lathe's axis (`COORDINATES = XZ`).
pub fn axis_index(a: Axis) -> usize {
    match a {
        Axis::X => 0,
        Axis::Z => 2,
    }
}

/// The run state the UI shows for a LinuxCNC status.
pub fn run_state(s: &Status) -> RunState {
    match s.task_state {
        TaskState::Estop | TaskState::EstopReset | TaskState::Off => RunState::Alarm,
        TaskState::On if s.homing => RunState::Homing,
        TaskState::On => match s.interp_state {
            InterpState::Paused => RunState::Hold,
            InterpState::Reading | InterpState::Waiting => RunState::Running,
            InterpState::Idle => RunState::Idle,
        },
    }
}

/// LinuxCNC's status as `sender/core`'s `SessionState`, so the UI draws it
/// unchanged. `alarms` are the error-channel errors still standing.
pub fn state_of(
    s: &Status,
    alarms: &[AlarmInfo],
    single_block: bool,
    diameter_mode: bool,
) -> SessionState {
    let [x, z] = s.machine_xz;
    let [ox, oz] = s.offset_xz;
    let mut alarms = alarms.to_vec();
    match s.task_state {
        TaskState::Estop => alarms.insert(
            0,
            AlarmInfo {
                code: "ESTOP".into(),
                message: "LinuxCNC is in E-stop".into(),
            },
        ),
        TaskState::EstopReset | TaskState::Off => alarms.insert(
            0,
            AlarmInfo {
                code: "MACHINE-OFF".into(),
                message: "LinuxCNC: machine off".into(),
            },
        ),
        TaskState::On => {}
    }
    let xw = x - ox;
    SessionState {
        run: run_state(s),
        dro: Dro {
            x_radius_mm: x,
            x_diameter_mm: 2.0 * x,
            z_mm: z,
            x_work_mm: if diameter_mode { 2.0 * xw } else { xw },
            z_work_mm: z - oz,
            x_dtg_mm: s.dtg_xz[0],
            z_dtg_mm: s.dtg_xz[1],
            homed: s.homed.iter().all(|h| *h),
        },
        current_line: s.line,
        tool: s.tool,
        spindle_rpm: s.spindle_rpm.abs(),
        spindle_on: s.spindle_rpm != 0.0,
        feed_mm_min: s.feed_mm_min,
        modal: s.modal.clone(),
        alarms,
        single_block,
        dry_run: false,
    }
}

/// The UI on LinuxCNC, through any [`Channels`].
pub struct LinuxCncTransport<C: Channels> {
    ch: C,
    single_block: bool,
    alarms: Vec<AlarmInfo>,
    last_run: Option<RunState>,
    pending: Vec<Event>,
    now_ms: f64,
}

impl<C: Channels> LinuxCncTransport<C> {
    pub fn new(ch: C) -> Self {
        LinuxCncTransport {
            ch,
            single_block: false,
            alarms: Vec::new(),
            last_run: None,
            pending: Vec::new(),
            now_ms: 0.0,
        }
    }

    /// The channels, for tests and for the bridge's own housekeeping.
    pub fn channels(&mut self) -> &mut C {
        &mut self.ch
    }

    fn alarm(&mut self, code: &str, message: String) {
        self.pending.push(Event::Alarm {
            code: code.into(),
            message,
            at_ms: self.now_ms,
        });
    }

    fn send_all(&mut self, cmds: Vec<LcncCommand>) {
        for c in cmds {
            if let Err(e) = self.ch.send(c) {
                self.alarm("LINUXCNC", e);
                return;
            }
        }
    }

    fn diameter_mode(s: &Status) -> bool {
        s.modal.iter().any(|g| g == "G7")
    }
}

impl<C: Channels + 'static> Transport for LinuxCncTransport<C> {
    fn name(&self) -> &'static str {
        "linuxcnc"
    }

    fn command(&mut self, c: Command) {
        use LcncCommand as L;
        let cmds = match c {
            Command::Run if self.single_block => vec![L::Mode(TaskMode::Auto), L::AutoStep],
            Command::Run => vec![L::Mode(TaskMode::Auto), L::AutoRun],
            // In single-block mode, "resume" runs the next block.
            Command::Resume if self.single_block => vec![L::AutoStep],
            Command::Resume => vec![L::AutoResume],
            Command::Hold => vec![L::AutoPause],
            Command::Abort => vec![L::Abort],
            Command::SingleBlock { on } => {
                self.single_block = on;
                vec![]
            }
            Command::DryRun { on: true } => {
                self.alarm(
                    "UNSUPPORTED",
                    "LinuxCNC has no dry run; use its own simulator".into(),
                );
                vec![]
            }
            Command::DryRun { on: false } => vec![],
            Command::Jog {
                axis,
                delta_mm,
                feed_mm_min,
            } => vec![
                L::Mode(TaskMode::Manual),
                L::JogIncrement {
                    axis: axis_index(axis),
                    vel_mm_s: feed_mm_min / 60.0,
                    incr_mm: delta_mm,
                },
            ],
            Command::Mdi { text } => vec![L::Mode(TaskMode::Mdi), L::Mdi(text)],
            Command::SetWorkOffset { axis, value_mm } => {
                // Touch-off, as the session does it: afterwards the current
                // position reads value_mm (a radius for X) in G54. G10 L20
                // takes X in the active G7/G8 mode, so double it in G7.
                let g7 = self
                    .ch
                    .status()
                    .map(|s| Self::diameter_mode(&s))
                    .unwrap_or(false);
                let text = match axis {
                    Axis::X => {
                        let v = if g7 { 2.0 * value_mm } else { value_mm };
                        format!("G10 L20 P1 X{v:.4}")
                    }
                    Axis::Z => format!("G10 L20 P1 Z{value_mm:.4}"),
                };
                vec![L::Mode(TaskMode::Mdi), L::Mdi(text)]
            }
            Command::Home => vec![L::Mode(TaskMode::Manual), L::HomeAll],
        };
        self.send_all(cmds);
    }

    fn load(&mut self, p: &Program) {
        let text: String = p.lines.iter().map(|l| format!("{l}\n")).collect();
        self.send_all(vec![
            LcncCommand::Mode(TaskMode::Auto),
            LcncCommand::ProgramOpen {
                name: "kd240-ui.ngc".into(),
                text,
            },
        ]);
    }

    fn fault(&mut self, _kind: &'static str) -> bool {
        false
    }

    fn poll(&mut self, now_ms: f64) -> Poll {
        self.now_ms = now_ms;
        for e in self.ch.errors() {
            if e.kind == "error" {
                self.alarms.push(AlarmInfo {
                    code: "LINUXCNC".into(),
                    message: e.text.clone(),
                });
            }
            self.alarm(&e.kind.to_uppercase(), e.text);
        }
        let state = match self.ch.status() {
            Some(s) => {
                // An error stands until the machine is idle again after it.
                if run_state(&s) == RunState::Idle && self.last_run != Some(RunState::Idle) {
                    self.alarms.clear();
                }
                state_of(&s, &self.alarms, self.single_block, Self::diameter_mode(&s))
            }
            None => SessionState {
                run: RunState::Disconnected,
                dro: Dro::default(),
                current_line: -1,
                tool: 0,
                spindle_rpm: 0.0,
                spindle_on: false,
                feed_mm_min: 0.0,
                modal: vec![],
                alarms: self.alarms.clone(),
                single_block: self.single_block,
                dry_run: false,
            },
        };
        if self.last_run != Some(state.run) {
            if let Some(from) = self.last_run {
                self.pending.push(Event::State {
                    from,
                    to: state.run,
                    at_ms: now_ms,
                });
            }
            self.last_run = Some(state.run);
        }
        Poll {
            state: json!({ "session": state, "transport": self.name() }),
            events: self
                .pending
                .drain(..)
                .filter_map(|e| serde_json::to_value(&e).ok())
                .collect::<Vec<Value>>(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A LinuxCNC that records commands and answers with a settable status.
    #[derive(Default)]
    struct Fake {
        status: Option<Status>,
        sent: Vec<LcncCommand>,
        errors: Vec<ErrorMessage>,
        refuse: Option<String>,
    }

    impl Channels for Fake {
        fn status(&mut self) -> Option<Status> {
            self.status.clone()
        }
        fn send(&mut self, c: LcncCommand) -> Result<(), String> {
            if let Some(r) = &self.refuse {
                return Err(r.clone());
            }
            self.sent.push(c);
            Ok(())
        }
        fn errors(&mut self) -> Vec<ErrorMessage> {
            std::mem::take(&mut self.errors)
        }
    }

    fn on_idle() -> Status {
        Status {
            task_state: TaskState::On,
            task_mode: TaskMode::Manual,
            interp_state: InterpState::Idle,
            homed: [true, true],
            homing: false,
            machine_xz: [10.0, -5.0],
            offset_xz: [2.0, -20.0],
            dtg_xz: [0.0, 0.0],
            line: -1,
            tool: 3,
            spindle_rpm: -800.0,
            feed_mm_min: 100.0,
            modal: vec!["G7".into(), "G18".into(), "G21".into()],
        }
    }

    fn transport(s: Option<Status>) -> LinuxCncTransport<Fake> {
        LinuxCncTransport::new(Fake {
            status: s,
            ..Fake::default()
        })
    }

    #[test]
    fn run_states_follow_task_and_interpreter() {
        let mut s = on_idle();
        assert_eq!(run_state(&s), RunState::Idle);
        s.interp_state = InterpState::Reading;
        assert_eq!(run_state(&s), RunState::Running);
        s.interp_state = InterpState::Waiting;
        assert_eq!(run_state(&s), RunState::Running);
        s.interp_state = InterpState::Paused;
        assert_eq!(run_state(&s), RunState::Hold);
        s.homing = true;
        assert_eq!(run_state(&s), RunState::Homing);
        for t in [TaskState::Estop, TaskState::EstopReset, TaskState::Off] {
            s.task_state = t;
            assert_eq!(run_state(&s), RunState::Alarm);
        }
    }

    #[test]
    fn the_dro_is_linuxcncs_position_in_the_sessions_shape() {
        let st = state_of(&on_idle(), &[], false, true);
        assert_eq!(st.dro.x_radius_mm, 10.0);
        assert_eq!(st.dro.x_diameter_mm, 20.0);
        assert_eq!(
            st.dro.x_work_mm, 16.0,
            "work X in diameter mode: 2 * (10 - 2)"
        );
        assert_eq!(st.dro.z_work_mm, 15.0);
        assert!(st.dro.homed);
        assert_eq!((st.tool, st.spindle_rpm, st.spindle_on), (3, 800.0, true));
        assert!(st.alarms.is_empty());
        let st = state_of(&on_idle(), &[], false, false);
        assert_eq!(st.dro.x_work_mm, 8.0, "radius mode");
    }

    #[test]
    fn estop_shows_as_an_alarm() {
        let mut s = on_idle();
        s.task_state = TaskState::Estop;
        let st = state_of(&s, &[], false, false);
        assert_eq!(st.run, RunState::Alarm);
        assert_eq!(st.alarms[0].code, "ESTOP");
    }

    #[test]
    fn commands_become_mode_switches_and_linuxcnc_commands() {
        let mut t = transport(Some(on_idle()));
        t.command(Command::Home);
        t.command(Command::Jog {
            axis: Axis::Z,
            delta_mm: -0.5,
            feed_mm_min: 120.0,
        });
        t.command(Command::Mdi {
            text: "M3 S500".into(),
        });
        t.command(Command::Run);
        t.command(Command::Hold);
        t.command(Command::Resume);
        t.command(Command::Abort);
        use LcncCommand as L;
        assert_eq!(
            t.channels().sent,
            vec![
                L::Mode(TaskMode::Manual),
                L::HomeAll,
                L::Mode(TaskMode::Manual),
                L::JogIncrement {
                    axis: 2,
                    vel_mm_s: 2.0,
                    incr_mm: -0.5
                },
                L::Mode(TaskMode::Mdi),
                L::Mdi("M3 S500".into()),
                L::Mode(TaskMode::Auto),
                L::AutoRun,
                L::AutoPause,
                L::AutoResume,
                L::Abort,
            ]
        );
    }

    #[test]
    fn single_block_steps_instead_of_running() {
        let mut t = transport(Some(on_idle()));
        t.command(Command::SingleBlock { on: true });
        t.command(Command::Run);
        t.command(Command::Resume);
        use LcncCommand as L;
        assert_eq!(
            t.channels().sent,
            vec![L::Mode(TaskMode::Auto), L::AutoStep, L::AutoStep]
        );
        let p = t.poll(0.0);
        assert_eq!(p.state["session"]["singleBlock"], true);
    }

    #[test]
    fn touch_off_doubles_x_in_diameter_mode() {
        let mut t = transport(Some(on_idle()));
        t.command(Command::SetWorkOffset {
            axis: Axis::X,
            value_mm: 1.5,
        });
        t.command(Command::SetWorkOffset {
            axis: Axis::Z,
            value_mm: 0.0,
        });
        let mdi: Vec<_> = t
            .channels()
            .sent
            .iter()
            .filter_map(|c| match c {
                LcncCommand::Mdi(s) => Some(s.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(mdi, ["G10 L20 P1 X3.0000", "G10 L20 P1 Z0.0000"]);
    }

    #[test]
    fn load_opens_the_program_text() {
        let mut t = transport(Some(on_idle()));
        t.load(&Program::parse("G21\nG00 X5 Z1\nM30\n"));
        match &t.channels().sent[1] {
            LcncCommand::ProgramOpen { text, .. } => assert_eq!(text, "G21\nG00 X5 Z1\nM30\n"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn what_linuxcnc_cannot_do_is_refused_visibly() {
        let mut t = transport(Some(on_idle()));
        assert!(!t.fault("estop"));
        t.command(Command::DryRun { on: true });
        let p = t.poll(5.0);
        assert!(t.channels().sent.is_empty());
        assert_eq!(p.events[0]["kind"], "alarm");
        assert_eq!(p.events[0]["code"], "UNSUPPORTED");
        assert_eq!(p.state["session"]["dryRun"], false);
    }

    #[test]
    fn a_refused_command_stops_its_sequence_and_raises_an_alarm() {
        let mut t = transport(Some(on_idle()));
        t.channels().refuse = Some("machine not on".into());
        t.command(Command::Run);
        let p = t.poll(1.0);
        assert!(t.channels().sent.is_empty());
        assert_eq!(p.events[0]["message"], "machine not on");
    }

    #[test]
    fn errors_stand_until_idle_and_state_changes_are_events() {
        let mut t = transport(Some(Status {
            interp_state: InterpState::Reading,
            ..on_idle()
        }));
        let p = t.poll(0.0);
        assert_eq!(p.state["session"]["run"], "running");
        assert_eq!(p.state["transport"], "linuxcnc");
        t.channels().errors.push(ErrorMessage {
            kind: "error".into(),
            text: "joint 0 following error".into(),
        });
        t.channels().status.as_mut().unwrap().interp_state = InterpState::Paused;
        let p = t.poll(10.0);
        assert_eq!(
            p.state["session"]["alarms"][0]["message"],
            "joint 0 following error"
        );
        let kinds: Vec<_> = p.events.iter().map(|e| e["kind"].clone()).collect();
        assert_eq!(kinds, [json!("alarm"), json!("state")]);
        t.channels().status.as_mut().unwrap().interp_state = InterpState::Idle;
        let p = t.poll(20.0);
        assert_eq!(p.state["session"]["run"], "idle");
        assert_eq!(p.state["session"]["alarms"], json!([]));
    }

    #[test]
    fn no_linuxcnc_means_disconnected() {
        let mut t = transport(None);
        let p = t.poll(0.0);
        assert_eq!(p.state["session"]["run"], "disconnected");
    }

    #[test]
    fn the_pump_runs_it() {
        use crate::machine::Machine;
        let m = Machine::spawn_with(|| Box::new(transport(Some(on_idle()))));
        for _ in 0..200 {
            if m.run_state() == "idle" {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(m.run_state(), "idle");
        assert_eq!(m.state.borrow()["transport"], "linuxcnc");
    }
}
