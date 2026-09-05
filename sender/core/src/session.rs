//! `Session`: the protocol driver and single source of UI state.
//!
//! Transport-agnostic and synchronous: [`Session::tick`] is the only pump. It
//! consumes bytes received from the transport, emits bytes to send, and never
//! reads a clock — time comes in as `now_ms` from the caller.
//!
//! Because the (unverified) wire protocol offers no position feedback, the DRO
//! is a **prediction**: the session interprets each block it sends with the
//! same modal machinery as the parser, interpolates along the in-flight move
//! at the commanded rate, and snaps to the block's target when the machine
//! acknowledges it.
//!
//! Deviations from `sender/INTERFACE.md`, all called out in the report:
//! - The session leaves `"disconnected"` on its first `tick`: being pumped is
//!   what "connected" means to a transport-agnostic core.
//! - `setWorkOffset` uses touch-off semantics: after
//!   `{axis: "x", valueMm: v}` the work coordinate of the current position
//!   reads `v`.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

use crate::dialect::{make_dialect, ControlAction, Dialect, DialectEvent};
use crate::gcode::{lex_line, Block, ModalState, MotionCmd, MoveKind, Program, SpindleDir};
use crate::geom::point_along;
use crate::profile::MachineProfile;

// ---------------------------------------------------------------------------
// Public state types (shapes match sender/INTERFACE.md)
// ---------------------------------------------------------------------------

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RunState {
    Disconnected,
    Idle,
    Running,
    Hold,
    Alarm,
    Homing,
    Toolchange,
}

#[derive(Serialize, Clone, Copy, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Dro {
    pub x_radius_mm: f64,
    pub x_diameter_mm: f64,
    pub z_mm: f64,
    pub x_work_mm: f64,
    pub z_work_mm: f64,
    pub x_dtg_mm: f64,
    pub z_dtg_mm: f64,
    pub homed: bool,
}

#[derive(Serialize, Clone, Debug)]
pub struct AlarmInfo {
    pub code: String,
    pub message: String,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SessionState {
    pub run: RunState,
    pub dro: Dro,
    /// -1 when not running a program.
    pub current_line: i32,
    /// Active turret station, 0 = unknown.
    pub tool: u32,
    pub spindle_rpm: f64,
    pub spindle_on: bool,
    pub feed_mm_min: f64,
    pub modal: Vec<String>,
    pub alarms: Vec<AlarmInfo>,
    pub single_block: bool,
    pub dry_run: bool,
}

#[derive(Serialize, Clone, Debug)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Event {
    #[serde(rename_all = "camelCase")]
    Sent { line: i32, text: String, at_ms: f64 },
    #[serde(rename_all = "camelCase")]
    Acked { line: i32, at_ms: f64 },
    #[serde(rename_all = "camelCase")]
    Alarm { code: String, message: String, at_ms: f64 },
    #[serde(rename_all = "camelCase")]
    State { from: RunState, to: RunState, at_ms: f64 },
    #[serde(rename_all = "camelCase")]
    Raw { dir: RawDir, bytes: Vec<u8>, at_ms: f64 },
}

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RawDir {
    Tx,
    Rx,
}

/// Result of one [`Session::tick`].
#[derive(Serialize, Clone, Debug, Default)]
pub struct StepOut {
    /// Bytes to write to the transport.
    pub tx: Vec<u8>,
    pub events: Vec<Event>,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Command {
    Run,
    Hold,
    Resume,
    /// Best-effort stop; advisory, never a safety function.
    Abort,
    SingleBlock { on: bool },
    DryRun { on: bool },
    #[serde(rename_all = "camelCase")]
    Jog { axis: Axis, delta_mm: f64, feed_mm_min: f64 },
    Mdi { text: String },
    #[serde(rename_all = "camelCase")]
    SetWorkOffset { axis: Axis, value_mm: f64 },
    Home,
}

#[derive(Deserialize, Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Axis {
    X,
    Z,
}

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum JobKind {
    Program,
    Mdi,
    Jog,
    Home,
}

struct Inflight {
    /// Source line, -1 for synthesized blocks (MDI, jog, home).
    line: i32,
    job: JobKind,
    motion: Option<MotionCmd>,
    tool_change: Option<u32>,
    end_of_program: bool,
    sent_at_ms: f64,
    /// Dry-run block: never hit the wire; auto-acked on the next tick.
    dry: bool,
}

struct QueuedBlock {
    block: Block,
    line: i32,
    job: JobKind,
}

pub struct Session {
    profile: MachineProfile,
    dialect: Box<dyn Dialect>,
    run: RunState,
    program: Option<Program>,
    /// True while a `run` command is actually draining the loaded program
    /// (distinguishes a program run from MDI/jog activity in `running` state).
    program_active: bool,
    /// Index of the next program block to send.
    next_block: usize,
    /// Synthesized blocks (MDI, jog, home) waiting to be sent.
    pending: VecDeque<QueuedBlock>,
    inflight: Option<Inflight>,
    cmds: VecDeque<Command>,
    /// Predicted modal state (mirrors what the machine has been told).
    modal: ModalState,
    /// Committed position: last acknowledged target (radius mm, mm).
    x: f64,
    z: f64,
    /// Interpolated display position for the DRO.
    disp_x: f64,
    disp_z: f64,
    work_offset_x: f64,
    work_offset_z: f64,
    homed: bool,
    tool: u32,
    alarms: Vec<AlarmInfo>,
    single_block: bool,
    /// Waiting for cycle start (resume) between blocks in single-block mode.
    single_block_gate: bool,
    dry_run: bool,
    current_line: i32,
    now_ms: f64,
}

impl Session {
    pub fn new(profile: MachineProfile) -> Session {
        let dialect = make_dialect(profile.dialect);
        Session {
            profile,
            dialect,
            run: RunState::Disconnected,
            program: None,
            program_active: false,
            next_block: 0,
            pending: VecDeque::new(),
            inflight: None,
            cmds: VecDeque::new(),
            modal: ModalState::default(),
            x: 0.0,
            z: 0.0,
            disp_x: 0.0,
            disp_z: 0.0,
            work_offset_x: 0.0,
            work_offset_z: 0.0,
            homed: false,
            tool: 0,
            alarms: Vec::new(),
            single_block: false,
            single_block_gate: false,
            dry_run: false,
            current_line: -1,
            now_ms: 0.0,
        }
    }

    pub fn load_program(&mut self, program: &Program) {
        if self.run == RunState::Running || self.run == RunState::Toolchange {
            return; // never swap the program mid-run
        }
        self.program = Some(program.clone());
        self.next_block = 0;
        self.current_line = -1;
    }

    /// Queue an operator command; effects appear on the next tick.
    pub fn command(&mut self, cmd: Command) {
        self.cmds.push_back(cmd);
    }

    /// Report hardware CTS state when the transport exposes it.
    pub fn set_cts(&mut self, asserted: bool) {
        self.dialect.set_cts(asserted);
    }

    /// The pump: advance to `now_ms`, consume `rx`, return bytes to send and
    /// events that happened.
    pub fn tick(&mut self, now_ms: f64, rx: &[u8]) -> StepOut {
        self.now_ms = now_ms;
        let mut out = StepOut::default();

        // Being pumped is what "connected" means to a transport-agnostic core.
        if self.run == RunState::Disconnected {
            self.set_run(RunState::Idle, &mut out);
        }

        // 1. Dry-run auto-ack (the block never hit the wire).
        if self.inflight.as_ref().is_some_and(|i| i.dry) {
            self.handle_ack(&mut out);
        }

        // 2. Received bytes -> dialect events.
        if !rx.is_empty() {
            out.events.push(Event::Raw { dir: RawDir::Rx, bytes: rx.to_vec(), at_ms: now_ms });
            for ev in self.dialect.feed_rx(rx) {
                match ev {
                    DialectEvent::Ack => self.handle_ack(&mut out),
                    DialectEvent::Nak { message } => {
                        self.inflight = None;
                        self.raise_alarm("NAK", &message, &mut out);
                    }
                    DialectEvent::Alarm { code, message } => {
                        self.raise_alarm(&code, &message, &mut out);
                    }
                    DialectEvent::XOn | DialectEvent::XOff | DialectEvent::Text(_) => {}
                }
            }
        }

        // 3. Operator commands.
        while let Some(cmd) = self.cmds.pop_front() {
            self.handle_command(cmd, &mut out);
        }

        // 4. Send phase.
        self.pump_tx(&mut out);

        // 5. DRO interpolation along the in-flight move.
        self.interpolate();

        out
    }

    pub fn state(&self) -> SessionState {
        SessionState {
            run: self.run,
            dro: self.dro(),
            current_line: self.current_line,
            tool: self.tool,
            spindle_rpm: if self.modal.spindle == SpindleDir::Off {
                0.0
            } else {
                self.modal.spindle_rpm
            },
            spindle_on: self.modal.spindle != SpindleDir::Off,
            feed_mm_min: self.modal.feed_mm_min(),
            modal: self.modal.modal_display(),
            alarms: self.alarms.clone(),
            single_block: self.single_block,
            dry_run: self.dry_run,
        }
    }

    pub fn dro(&self) -> Dro {
        let (dtg_x, dtg_z) = match self.inflight.as_ref().and_then(|i| i.motion.as_ref()) {
            Some(m) => (m.x1 - self.disp_x, m.z1 - self.disp_z),
            None => (0.0, 0.0),
        };
        Dro {
            x_radius_mm: self.disp_x,
            x_diameter_mm: self.disp_x * 2.0,
            z_mm: self.disp_z,
            x_work_mm: self.disp_x - self.work_offset_x,
            z_work_mm: self.disp_z - self.work_offset_z,
            x_dtg_mm: dtg_x,
            z_dtg_mm: dtg_z,
            homed: self.homed,
        }
    }

    // -- internals ----------------------------------------------------------

    fn set_run(&mut self, to: RunState, out: &mut StepOut) {
        if self.run != to {
            out.events.push(Event::State { from: self.run, to, at_ms: self.now_ms });
            self.run = to;
        }
    }

    fn raise_alarm(&mut self, code: &str, message: &str, out: &mut StepOut) {
        self.alarms.push(AlarmInfo { code: code.to_string(), message: message.to_string() });
        out.events.push(Event::Alarm {
            code: code.to_string(),
            message: message.to_string(),
            at_ms: self.now_ms,
        });
        self.set_run(RunState::Alarm, out);
    }

    fn handle_ack(&mut self, out: &mut StepOut) {
        let Some(inf) = self.inflight.take() else { return };
        out.events.push(Event::Acked { line: inf.line, at_ms: self.now_ms });
        if let Some(m) = &inf.motion {
            self.x = m.x1;
            self.z = m.z1;
            self.disp_x = m.x1;
            self.disp_z = m.z1;
        }
        if let Some(t) = inf.tool_change {
            self.tool = t;
            if self.run == RunState::Toolchange {
                self.set_run(RunState::Running, out);
            }
        }
        if inf.job == JobKind::Home {
            // Home position: the reference switches at the positive travel end.
            self.x = self.profile.x_max_mm;
            self.z = self.profile.z_max_mm;
            self.disp_x = self.x;
            self.disp_z = self.z;
            self.modal.x = self.x;
            self.modal.z = self.z;
            self.homed = true;
            self.set_run(RunState::Idle, out);
            return;
        }
        if inf.end_of_program {
            self.current_line = -1;
            self.next_block = usize::MAX; // job over even if blocks remain after M30
            self.program_active = false;
            self.set_run(RunState::Idle, out);
            return;
        }
        // Program exhausted?
        if self.run == RunState::Running && self.job_finished() {
            self.current_line = -1;
            self.program_active = false;
            self.set_run(RunState::Idle, out);
            return;
        }
        if self.single_block && self.run == RunState::Running {
            self.single_block_gate = true;
            self.set_run(RunState::Hold, out);
        }
    }

    fn job_finished(&self) -> bool {
        let program_left = self.program_active
            && matches!(&self.program, Some(p) if self.next_block < p.blocks.len());
        !program_left && self.pending.is_empty() && self.inflight.is_none()
    }

    fn handle_command(&mut self, cmd: Command, out: &mut StepOut) {
        match cmd {
            Command::Run => match self.run {
                RunState::Idle => {
                    if self.program.is_some() {
                        self.next_block = 0;
                        self.program_active = true;
                        self.modal = ModalState::default();
                        self.modal.x = self.x;
                        self.modal.z = self.z;
                        self.single_block_gate = false;
                        self.set_run(RunState::Running, out);
                    }
                }
                RunState::Hold => {
                    // Cycle start from hold behaves like resume.
                    self.single_block_gate = false;
                    self.set_run(RunState::Running, out);
                }
                _ => {}
            },
            Command::Hold => {
                if matches!(self.run, RunState::Running | RunState::Toolchange) {
                    out.tx.extend(self.dialect.control_bytes(ControlAction::FeedHold));
                    self.set_run(RunState::Hold, out);
                }
            }
            Command::Resume => {
                if self.run == RunState::Hold {
                    out.tx.extend(self.dialect.control_bytes(ControlAction::Resume));
                    self.single_block_gate = false;
                    self.set_run(RunState::Running, out);
                }
            }
            Command::Abort => {
                // Advisory stop; the real stop is the mains switch at the machine.
                out.tx.extend(self.dialect.control_bytes(ControlAction::Abort));
                self.dialect.reset();
                self.inflight = None;
                self.pending.clear();
                self.next_block = usize::MAX;
                self.program_active = false;
                self.current_line = -1;
                self.alarms.clear();
                self.set_run(RunState::Disconnected, out);
            }
            Command::SingleBlock { on } => {
                self.single_block = on;
                if !on {
                    self.single_block_gate = false;
                }
            }
            Command::DryRun { on } => {
                if !matches!(self.run, RunState::Running | RunState::Toolchange) {
                    self.dry_run = on;
                }
            }
            Command::Jog { axis, delta_mm, feed_mm_min } => {
                if self.run == RunState::Idle && self.inflight.is_none() {
                    let feed = feed_mm_min.clamp(1.0, self.profile.max_feed_mm_min);
                    // U/W are incremental regardless of G90/G91; U takes a
                    // diameter value when the machine is in diameter mode.
                    let text = match axis {
                        Axis::X => {
                            let word = if self.modal.diameter_mode { delta_mm * 2.0 } else { delta_mm };
                            format!("G94 G1 U{word:.4} F{feed:.1}")
                        }
                        Axis::Z => format!("G94 G1 W{delta_mm:.4} F{feed:.1}"),
                    };
                    self.queue_text(&text, JobKind::Jog, out);
                }
            }
            Command::Mdi { text } => {
                if self.run == RunState::Idle {
                    let mut queued = false;
                    for line in text.lines() {
                        if self.queue_text(line, JobKind::Mdi, out) {
                            queued = true;
                        }
                    }
                    if queued {
                        self.set_run(RunState::Running, out);
                        self.current_line = -1;
                    }
                }
            }
            Command::SetWorkOffset { axis, value_mm } => {
                // Touch-off semantics: current position reads `value_mm` afterwards.
                match axis {
                    Axis::X => self.work_offset_x = self.disp_x - value_mm,
                    Axis::Z => self.work_offset_z = self.disp_z - value_mm,
                }
            }
            Command::Home => {
                if matches!(self.run, RunState::Idle | RunState::Alarm) {
                    self.alarms.clear();
                    self.queue_text("G28", JobKind::Home, out);
                    self.set_run(RunState::Homing, out);
                }
            }
        }
    }

    /// Lex one synthesized line and queue it. Returns false (with an alarm
    /// event, but no alarm state) when it does not parse.
    fn queue_text(&mut self, text: &str, job: JobKind, out: &mut StepOut) -> bool {
        let mut diags = Vec::new();
        match lex_line(text, 0, &mut diags) {
            Some(b) if diags.iter().all(|d| d.severity != crate::gcode::Severity::Error) => {
                self.pending.push_back(QueuedBlock { block: b, line: -1, job });
                true
            }
            _ => {
                let msg = diags
                    .first()
                    .map(|d| d.message.clone())
                    .unwrap_or_else(|| "empty block".to_string());
                out.events.push(Event::Alarm {
                    code: "parse".to_string(),
                    message: format!("{text}: {msg}"),
                    at_ms: self.now_ms,
                });
                false
            }
        }
    }

    fn next_to_send(&mut self) -> Option<QueuedBlock> {
        // Synthesized blocks (jog, MDI, home) go first; they only exist in
        // states where the program queue is not being drained.
        if let Some(q) = self.pending.pop_front() {
            return Some(q);
        }
        if self.run != RunState::Running || !self.program_active {
            return None;
        }
        let program = self.program.as_ref()?;
        while self.next_block < program.blocks.len() {
            let b = program.blocks[self.next_block].clone();
            self.next_block += 1;
            if b.words.is_empty() {
                continue; // e.g. a bare block-skip line
            }
            return Some(QueuedBlock { line: b.line as i32, block: b, job: JobKind::Program });
        }
        None
    }

    fn pump_tx(&mut self, out: &mut StepOut) {
        {
            if self.inflight.is_some() || self.single_block_gate {
                return;
            }
            match self.run {
                RunState::Running | RunState::Homing => {}
                RunState::Idle if !self.pending.is_empty() => {} // jog
                _ => return,
            }
            if !self.dry_run && !self.dialect.wants_next_block() {
                return;
            }
            let Some(q) = self.next_to_send() else {
                return;
            };

            // Interpret the block so the DRO prediction and modal display track
            // what the machine was told. Parse-time diagnostics were already
            // reported; they are not re-raised here.
            let mut diags = Vec::new();
            self.modal.x = self.x;
            self.modal.z = self.z;
            let fx = self.modal.apply_block(&q.block, &mut diags);

            // Sender-side soft-limit safety net. The real machine has no
            // end-of-travel limit switches - an overtravel move just stalls
            // the stepper and silently loses position - so the *sender*
            // refuses to transmit a move whose swept extent leaves the
            // profile's travel. Always on: it is the only limit this machine
            // has.
            if let Some(m) = &fx.motion {
                if self.motion_exceeds_travel(m) {
                    self.modal.x = self.x;
                    self.modal.z = self.z;
                    self.raise_alarm(
                        "SOFTLIMIT",
                        &format!(
                            "move exceeds machine travel (line {}); block not sent — \
                             the machine has no limit switches and would stall",
                            q.line
                        ),
                        out,
                    );
                    return;
                }
            }

            let dry = self.dry_run && q.job == JobKind::Program;
            if !dry {
                let bytes = self.dialect.encode_block(&q.block);
                out.tx.extend_from_slice(&bytes);
                self.dialect.note_block_sent();
            }
            out.events.push(Event::Sent {
                line: q.line,
                text: q.block.clean.clone(),
                at_ms: self.now_ms,
            });
            if q.job == JobKind::Program {
                self.current_line = q.line;
            }
            self.inflight = Some(Inflight {
                line: q.line,
                job: q.job,
                motion: fx.motion,
                tool_change: fx.tool_change,
                end_of_program: fx.end_of_program,
                sent_at_ms: self.now_ms,
                dry,
            });
            if fx.tool_change.is_some() && self.run == RunState::Running {
                self.set_run(RunState::Toolchange, out);
            }
            // At most one block per tick in dry-run so progress is visible;
            // otherwise a block is now in flight and the pump stops anyway.
        }
    }

    /// Does a move's swept extent (endpoint, and arc extrema for arcs) leave
    /// the profile's travel box?
    fn motion_exceeds_travel(&self, m: &MotionCmd) -> bool {
        let seg = crate::geom::Segment {
            kind: match m.kind {
                MoveKind::Arc { .. } => crate::geom::SegKind::Arc,
                _ => crate::geom::SegKind::Feed,
            },
            x0: m.x0,
            z0: m.z0,
            x1: m.x1,
            z1: m.z1,
            cx: match m.kind { MoveKind::Arc { cx, .. } => Some(cx), _ => None },
            cz: match m.kind { MoveKind::Arc { cz, .. } => Some(cz), _ => None },
            ccw: match m.kind { MoveKind::Arc { ccw, .. } => Some(ccw), _ => None },
            line: 0,
            feed_mm_min: None,
            tool: None,
        };
        let b = crate::geom::segment_bounds(&seg);
        let p = &self.profile;
        let eps = 1e-6;
        b.x_min < p.x_min_mm - eps
            || b.x_max > p.x_max_mm + eps
            || b.z_min < p.z_min_mm - eps
            || b.z_max > p.z_max_mm + eps
    }

    /// Move the display position along the in-flight move at the commanded
    /// rate. A prediction only — it snaps to truth on ack.
    fn interpolate(&mut self) {
        let Some(inf) = &self.inflight else {
            self.disp_x = self.x;
            self.disp_z = self.z;
            return;
        };
        let Some(m) = &inf.motion else { return };
        let v_mm_min = match m.kind {
            MoveKind::Rapid => self.profile.rapid_mm_min,
            _ => m.feed_mm_min.min(self.profile.max_feed_mm_min),
        };
        let length = crate::geom::segment_length(m.kind, m.x0, m.z0, m.x1, m.z1);
        if length <= 1e-9 || v_mm_min <= 0.0 {
            return;
        }
        let elapsed_min = (self.now_ms - inf.sent_at_ms) / 60_000.0;
        let t = (elapsed_min * v_mm_min / length).clamp(0.0, 1.0);
        let (x, z) = point_along(m.kind, m.x0, m.z0, m.x1, m.z1, t);
        self.disp_x = x;
        self.disp_z = z;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> Session {
        let mut p = MachineProfile::pc_turn_50_default();
        p.dialect = crate::profile::DialectKind::PlainGcode;
        // Clearance travel above the work face for test programs' positive-Z
        // approach moves (the real z_max is the reference at 0).
        p.z_max_mm = 50.0;
        Session::new(p)
    }

    fn states(out: &StepOut) -> Vec<(RunState, RunState)> {
        out.events
            .iter()
            .filter_map(|e| match e {
                Event::State { from, to, .. } => Some((*from, *to)),
                _ => None,
            })
            .collect()
    }

    fn sent_lines(out: &StepOut) -> Vec<i32> {
        out.events
            .iter()
            .filter_map(|e| match e {
                Event::Sent { line, .. } => Some(*line),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn first_tick_connects() {
        let mut s = session();
        assert_eq!(s.state().run, RunState::Disconnected);
        let out = s.tick(0.0, &[]);
        assert_eq!(s.state().run, RunState::Idle);
        assert_eq!(states(&out), vec![(RunState::Disconnected, RunState::Idle)]);
    }

    #[test]
    fn run_without_program_stays_idle() {
        let mut s = session();
        s.tick(0.0, &[]);
        s.command(Command::Run);
        s.tick(1.0, &[]);
        assert_eq!(s.state().run, RunState::Idle);
    }

    #[test]
    fn run_sends_first_block_and_waits_for_ack() {
        let mut s = session();
        s.tick(0.0, &[]);
        s.load_program(&Program::parse("G8 G94 F100\nG1 X5 Z-5\nM30"));
        s.command(Command::Run);
        let out = s.tick(1.0, &[]);
        assert_eq!(sent_lines(&out), vec![0], "exactly one block until acked");
        assert!(out.tx.ends_with(b"\r"));
        assert_eq!(s.state().current_line, 0);
        // No ack: nothing more goes out.
        let out2 = s.tick(2.0, &[]);
        assert!(out2.tx.is_empty());
        // Ack: next block flows.
        let out3 = s.tick(3.0, b"ok\r\n");
        assert_eq!(sent_lines(&out3), vec![1]);
        assert!(matches!(out3.events[1], Event::Acked { line: 0, .. }));
    }

    #[test]
    fn program_completion_returns_to_idle() {
        let mut s = session();
        s.tick(0.0, &[]);
        s.load_program(&Program::parse("G8 G94 F100\nG1 X5 Z-5\nM30"));
        s.command(Command::Run);
        let mut t = 1.0;
        let mut acked = 0;
        let mut pending_ok = false;
        for _ in 0..50 {
            let rx: &[u8] = if pending_ok { b"ok\r\n" } else { b"" };
            let out = s.tick(t, rx);
            pending_ok = !sent_lines(&out).is_empty();
            acked += out
                .events
                .iter()
                .filter(|e| matches!(e, Event::Acked { .. }))
                .count();
            t += 10.0;
            if s.state().run == RunState::Idle && acked > 0 {
                break;
            }
        }
        assert_eq!(acked, 3, "every block acknowledged");
        assert_eq!(s.state().run, RunState::Idle);
        assert_eq!(s.state().current_line, -1);
    }

    #[test]
    fn hold_stops_sending_resume_continues() {
        let mut s = session();
        s.tick(0.0, &[]);
        s.load_program(&Program::parse("G8 G94 F100\nG1 X5\nG1 Z-5\nM30"));
        s.command(Command::Run);
        s.tick(1.0, &[]); // block 0 out
        s.command(Command::Hold);
        let out = s.tick(2.0, b"ok\r\n"); // ack arrives while holding
        assert_eq!(s.state().run, RunState::Hold);
        assert!(sent_lines(&out).is_empty(), "hold must stop the queue");
        let out = s.tick(3.0, &[]);
        assert!(out.tx.is_empty());
        s.command(Command::Resume);
        let out = s.tick(4.0, &[]);
        assert_eq!(s.state().run, RunState::Running);
        assert_eq!(sent_lines(&out), vec![1]);
    }

    #[test]
    fn abort_drops_link_and_sends_stop_bytes() {
        let mut s = session();
        s.tick(0.0, &[]);
        s.load_program(&Program::parse("G8 G94 F100\nG1 X5\nM30"));
        s.command(Command::Run);
        s.tick(1.0, &[]);
        s.command(Command::Abort);
        let out = s.tick(2.0, &[]);
        assert!(out.tx.contains(&0x18), "abort control byte expected");
        assert_eq!(s.state().run, RunState::Disconnected);
        assert_eq!(s.state().current_line, -1);
        // Ticking again reconnects to idle; the job does not resume.
        let out = s.tick(3.0, &[]);
        assert!(sent_lines(&out).is_empty());
        assert_eq!(s.state().run, RunState::Idle);
    }

    #[test]
    fn nak_raises_alarm_state() {
        let mut s = session();
        s.tick(0.0, &[]);
        s.load_program(&Program::parse("G8 G94 F100\nG1 X5\nM30"));
        s.command(Command::Run);
        s.tick(1.0, &[]);
        let out = s.tick(2.0, b"error: unparseable block\r\n");
        assert_eq!(s.state().run, RunState::Alarm);
        assert!(out.events.iter().any(|e| matches!(e, Event::Alarm { .. })));
        assert_eq!(s.state().alarms.len(), 1);
        // Alarmed: nothing else is sent.
        let out = s.tick(3.0, &[]);
        assert!(out.tx.is_empty());
    }

    #[test]
    fn async_alarm_line_raises_alarm() {
        let mut s = session();
        s.tick(0.0, &[]);
        s.tick(1.0, b"alarm: ESTOP emergency stop\r\n");
        let st = s.state();
        assert_eq!(st.run, RunState::Alarm);
        assert_eq!(st.alarms[0].code, "ESTOP");
    }

    #[test]
    fn single_block_holds_after_each_block() {
        let mut s = session();
        s.tick(0.0, &[]);
        s.load_program(&Program::parse("G8 G94 F100\nG1 X5\nG1 Z-5\nM30"));
        s.command(Command::SingleBlock { on: true });
        s.command(Command::Run);
        s.tick(1.0, &[]); // block 0 sent
        s.tick(2.0, b"ok\r\n"); // acked -> hold
        assert_eq!(s.state().run, RunState::Hold);
        let out = s.tick(3.0, &[]);
        assert!(sent_lines(&out).is_empty());
        s.command(Command::Resume);
        let out = s.tick(4.0, &[]);
        assert_eq!(sent_lines(&out), vec![1]);
    }

    #[test]
    fn dry_run_emits_no_tx_but_walks_the_program() {
        let mut s = session();
        s.tick(0.0, &[]);
        s.load_program(&Program::parse("G8 G94 F100\nG1 X5 Z-5\nM30"));
        s.command(Command::DryRun { on: true });
        s.command(Command::Run);
        let mut t = 1.0;
        let mut sent = 0;
        for _ in 0..20 {
            let out = s.tick(t, &[]);
            assert!(out.tx.is_empty(), "dry run must not touch the transport");
            sent += sent_lines(&out).len();
            t += 10.0;
            if s.state().run == RunState::Idle && sent > 0 {
                break;
            }
        }
        assert_eq!(sent, 3);
        assert_eq!(s.state().run, RunState::Idle);
        // DRO followed the program even though nothing was transmitted.
        assert!((s.dro().x_radius_mm - 5.0).abs() < 1e-9);
    }

    #[test]
    fn jog_only_when_idle() {
        let mut s = session();
        s.tick(0.0, &[]);
        s.command(Command::Jog { axis: Axis::Z, delta_mm: -1.0, feed_mm_min: 200.0 });
        let out = s.tick(1.0, &[]);
        let txt = String::from_utf8_lossy(&out.tx).to_string();
        assert!(txt.contains("W-1.0000"), "{txt}");
        assert!(txt.contains("G94"));
        // While the jog is in flight (still Idle), a second jog queues fine,
        // but a jog during Running must be ignored.
        s.tick(2.0, b"ok\r\n");
        s.load_program(&Program::parse("G8 G94 F100\nG1 X5\nM30"));
        s.command(Command::Run);
        s.tick(3.0, &[]);
        s.command(Command::Jog { axis: Axis::X, delta_mm: 1.0, feed_mm_min: 100.0 });
        let out = s.tick(4.0, &[]);
        assert!(out.tx.is_empty(), "jog while running must be ignored");
    }

    #[test]
    fn jog_x_uses_diameter_u_word_in_diameter_mode() {
        let mut s = session();
        s.tick(0.0, &[]);
        s.command(Command::Jog { axis: Axis::X, delta_mm: 0.5, feed_mm_min: 100.0 });
        let out = s.tick(1.0, &[]);
        let txt = String::from_utf8_lossy(&out.tx).to_string();
        assert!(txt.contains("U1.0000"), "0.5 mm radius = 1.0 diameter: {txt}");
    }

    #[test]
    fn mdi_runs_then_returns_to_idle() {
        let mut s = session();
        s.tick(0.0, &[]);
        s.command(Command::Mdi { text: "S1000 M3\nG8 G94 G1 X5 F100".to_string() });
        let out = s.tick(1.0, &[]);
        assert_eq!(s.state().run, RunState::Running);
        assert_eq!(s.state().current_line, -1, "MDI has no program line");
        assert_eq!(sent_lines(&out), vec![-1]);
        s.tick(2.0, b"ok\r\n");
        s.tick(3.0, b"ok\r\n");
        assert_eq!(s.state().run, RunState::Idle);
        assert!(s.state().spindle_on);
    }

    #[test]
    fn mdi_parse_error_reports_and_does_not_hang() {
        let mut s = session();
        s.tick(0.0, &[]);
        s.command(Command::Mdi { text: "G1 X".to_string() });
        let out = s.tick(1.0, &[]);
        assert!(out.tx.is_empty());
        assert!(out.events.iter().any(|e| matches!(e, Event::Alarm { .. })));
        assert_eq!(s.state().run, RunState::Idle, "bad MDI must not wedge the session");
    }

    #[test]
    fn home_command_homes_and_clears_alarm() {
        let mut s = session();
        s.tick(0.0, &[]);
        s.tick(1.0, b"alarm: LIMITX limit\r\n");
        assert_eq!(s.state().run, RunState::Alarm);
        s.command(Command::Home);
        let out = s.tick(2.0, &[]);
        assert_eq!(s.state().run, RunState::Homing);
        assert_eq!(String::from_utf8_lossy(&out.tx), "G28\r");
        s.tick(3.0, b"ok\r\n");
        let st = s.state();
        assert_eq!(st.run, RunState::Idle);
        assert!(st.dro.homed);
        assert!((st.dro.x_radius_mm - 48.0).abs() < 1e-9, "home = reference at X48");
    }

    #[test]
    fn toolchange_state_during_t_block() {
        let mut s = session();
        s.tick(0.0, &[]);
        s.load_program(&Program::parse("T0202\nG8 G94 F100 G1 X5\nM30"));
        s.command(Command::Run);
        s.tick(1.0, &[]);
        assert_eq!(s.state().run, RunState::Toolchange);
        s.tick(2.0, b"ok\r\n");
        assert_eq!(s.state().run, RunState::Running);
        assert_eq!(s.state().tool, 2);
    }

    #[test]
    fn work_offset_touch_off() {
        let mut s = session();
        s.tick(0.0, &[]);
        // Jog to x=2 radius, then touch off X to 0.
        s.command(Command::Jog { axis: Axis::X, delta_mm: 2.0, feed_mm_min: 100.0 });
        s.tick(1.0, &[]);
        s.tick(2.0, b"ok\r\n");
        assert!((s.dro().x_radius_mm - 2.0).abs() < 1e-9);
        s.command(Command::SetWorkOffset { axis: Axis::X, value_mm: 0.0 });
        s.tick(3.0, &[]);
        let d = s.dro();
        assert!((d.x_work_mm - 0.0).abs() < 1e-9);
        assert!((d.x_radius_mm - 2.0).abs() < 1e-9, "machine coord unchanged");
    }

    #[test]
    fn dro_interpolates_along_inflight_move() {
        let mut s = session();
        s.tick(0.0, &[]);
        // 10 mm Z move at 600 mm/min = 10 mm/s -> 1 s total.
        s.command(Command::Mdi { text: "G8 G94 G1 W-10 F600".to_string() });
        s.tick(0.0, &[]);
        s.tick(500.0, &[]); // halfway, no ack yet
        let d = s.dro();
        // F600 (within the 750 max) -> 10 mm/s -> ~5 mm at 500 ms.
        assert!(d.z_mm < -2.0 && d.z_mm > -8.0, "should be mid-move, got {}", d.z_mm);
        assert!(d.z_dtg_mm < -1.0, "distance to go should be negative toward target");
        s.tick(1500.0, b"ok\r\n");
        assert!((s.dro().z_mm + 10.0).abs() < 1e-9, "snap to target on ack");
        assert_eq!(s.dro().z_dtg_mm, 0.0);
    }

    #[test]
    fn xoff_pauses_transmission() {
        let mut s = session();
        s.tick(0.0, &[]);
        s.load_program(&Program::parse("G8 G94 F100\nG1 X5\nG1 Z-5\nM30"));
        s.command(Command::Run);
        s.tick(1.0, &[]);
        // Ack + XOFF in the same rx: block acked but no new block may go out.
        let out = s.tick(2.0, &[b'o', b'k', b'\r', 0x13]);
        assert!(sent_lines(&out).is_empty(), "XOFF must gate the next block");
        assert_eq!(s.state().run, RunState::Running);
        let out = s.tick(3.0, &[0x11]);
        assert_eq!(sent_lines(&out), vec![1], "XON releases the queue");
    }

    #[test]
    fn cts_gates_transmission() {
        let mut s = session();
        s.tick(0.0, &[]);
        s.load_program(&Program::parse("G8 G94 F100\nG1 X5\nM30"));
        s.set_cts(false);
        s.command(Command::Run);
        let out = s.tick(1.0, &[]);
        assert!(out.tx.is_empty());
        s.set_cts(true);
        let out = s.tick(2.0, &[]);
        assert_eq!(sent_lines(&out), vec![0]);
    }

    #[test]
    fn sender_refuses_overtravel_block() {
        // The machine has no limit switches, so the sender must be the net:
        // an overtravel move is never transmitted.
        let mut s = session();
        s.tick(0.0, &[]);
        s.load_program(&Program::parse("G8 G94 F100\nG1 X100\nM30")); // x_max 48
        s.command(Command::Run);
        s.tick(1.0, &[]); // modal block goes out
        let out = s.tick(2.0, b"ok\r\n"); // acked; overtravel block is next
        assert!(out.tx.is_empty(), "overtravel block must not hit the wire");
        assert_eq!(s.state().run, RunState::Alarm);
        assert!(s.state().alarms[0].code.contains("SOFT"));
        assert!(s.state().alarms[0].message.contains("no limit switches"));
    }

    #[test]
    fn sender_refuses_overtravel_jog() {
        let mut s = session();
        s.tick(0.0, &[]);
        s.command(Command::Jog { axis: Axis::X, delta_mm: 100.0, feed_mm_min: 100.0 });
        let out = s.tick(1.0, &[]);
        assert!(out.tx.is_empty());
        assert_eq!(s.state().run, RunState::Alarm);
    }

    #[test]
    fn session_starts_unreferenced() {
        // requiresReferenceAfterPowerOn: the UI must be able to see homed=false
        // and insist on a reference cycle.
        let mut s = session();
        s.tick(0.0, &[]);
        assert!(!s.state().dro.homed);
    }

    #[test]
    fn command_json_shapes_deserialize() {
        let cases = [
            r#"{"kind":"run"}"#,
            r#"{"kind":"hold"}"#,
            r#"{"kind":"resume"}"#,
            r#"{"kind":"abort"}"#,
            r#"{"kind":"singleBlock","on":true}"#,
            r#"{"kind":"dryRun","on":false}"#,
            r#"{"kind":"jog","axis":"x","deltaMm":0.5,"feedMmMin":100}"#,
            r#"{"kind":"mdi","text":"G0 X0"}"#,
            r#"{"kind":"setWorkOffset","axis":"z","valueMm":0}"#,
            r#"{"kind":"home"}"#,
        ];
        for c in cases {
            serde_json::from_str::<Command>(c).unwrap_or_else(|e| panic!("{c}: {e}"));
        }
    }

    #[test]
    fn state_and_event_json_shapes() {
        let mut s = session();
        let out = s.tick(0.0, &[]);
        let ev = serde_json::to_string(&out.events[0]).unwrap();
        assert!(ev.contains("\"kind\":\"state\""));
        assert!(ev.contains("\"from\":\"disconnected\""));
        assert!(ev.contains("\"atMs\""));
        let st = serde_json::to_string(&s.state()).unwrap();
        assert!(st.contains("\"currentLine\":-1"));
        assert!(st.contains("\"xRadiusMm\""));
        assert!(st.contains("\"singleBlock\":false"));
        assert!(st.contains("\"run\":\"idle\""));
    }
}
