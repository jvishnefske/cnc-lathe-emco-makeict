//! The physical model as a pure state machine: what the machine channels
//! said, what the phase commands asked, and what to send in answer.
//!
//! [`Lathe`] holds the latest reading of every control module and one S88
//! phase state per equipment phase ([`PHASES`]). Every call returns
//! [`Outs`]: segments and setpoints for the driver, phase reports and
//! control-module states to publish. No I/O, no clock beyond the `now_ms`
//! the tick passes; the payload glue (`payload.rs`) does the rest.
//!
//! # Coordinates
//!
//! A driver reports step counts and takes segments in its own frame (raw
//! µm, from the counts and [`Limits::counts_per_mm`]). The machine frame is
//! the raw frame plus each axis' offset, which only a reference finds: an
//! axis is referenced once its reference switch has been found since the
//! node started and since the last e-stop. Before that, only the reference
//! phase moves it.
//!
//! # Phases
//!
//! | phase | Start's arguments | runs | completes |
//! |-------|-------------------|------|-----------|
//! | `motion.reference` | — | each axis (X first by default): off its switch if on it, then toward it at the search feed; a flush when it makes | both latched |
//! | `motion.move` | x µm, z µm, feed µm/s (machine) | one segment | it is done |
//! | `motion.run` | — | takes the process model's segments ([`Lathe::segments`]) | the batch marked last is done |
//! | `spindle.run` | rpm (negative: reverse) | commands the speed | measured within 2 % (at least 20 rpm) |
//! | `spindle.stop` | — | commands 0 | measured 0 |
//! | `turret.index` | station | selects it (motion idle) | locked there |
//!
//! A phase that cannot run (the e-stop open, an axis not referenced, motion
//! not idle for the turret, a reference switch never found, a batch that
//! does not fit) aborts: its report carries one value, an [`Abort`] code.
//! The e-stop opening aborts every acting phase and unreferences both
//! axes. Hold, Stop and Abort of a motion phase flush the queue; of the
//! spindle's, Stop and Abort command 0.

use cnc_model::wire::{CmState, PHASES};
use cnc_model::Limits;
use middleware::wasm_abi::machine::{Position, QueueState, Segment};
use recipe_core::phase::{self, Command, Input as PhaseInput, PhaseState};
use recipe_core::wire;

/// Indices into [`PHASES`].
pub const REFERENCE: usize = 0;
/// `motion.move`.
pub const MOVE: usize = 1;
/// `motion.run`.
pub const RUN: usize = 2;
/// `spindle.run`.
pub const SPIN: usize = 3;
/// `spindle.stop`.
pub const SPIN_STOP: usize = 4;
/// `turret.index`.
pub const INDEX: usize = 5;

/// Indices into [`cnc_model::wire::CMS`].
const CM_X: usize = 0;
const CM_SAFETY: usize = 4;
const CM_MOTION: usize = 5;

/// Why a phase aborted: its report's one value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum Abort {
    /// The e-stop chain is open (or opened).
    EStop = 1,
    /// A reference switch was not found within the travel.
    NoSwitch = 2,
    /// An axis is not referenced.
    NotReferenced = 3,
    /// Segments arrived that did not fit the queue, were outside the
    /// travel, or too fast.
    BadSegments = 4,
    /// The turret cannot index while the axes move.
    MotionBusy = 5,
    /// Aborted by command.
    Commanded = 6,
}

/// What a reading says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    /// X's step count.
    XPos(Position),
    /// Z's step count.
    ZPos(Position),
    /// X's reference switch.
    XHome(bool),
    /// Z's reference switch.
    ZHome(bool),
    /// The spindle's speed, rpm.
    SpindleActual(i32),
    /// The turret is locked.
    TurretReady(bool),
    /// The e-stop chain is closed.
    EStop(bool),
    /// The segment queue.
    Queue(QueueState),
}

/// Something to send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Out {
    /// A segment to the motion point.
    Segment(Segment),
    /// A speed to the spindle point.
    Spindle(i32),
    /// A station to the turret point.
    Turret(u8),
    /// Publish phase `i`'s report.
    Report(usize),
    /// Publish control module `i`'s state.
    Cm(usize, CmState),
}

/// The outs of one call, in order.
#[derive(Debug, Default)]
pub struct Outs {
    items: [Option<Out>; 32],
    n: usize,
}

impl Outs {
    fn push(&mut self, o: Out) {
        // Reports and states are idempotent and re-sent by the heartbeat;
        // nothing else ever comes 32 to a call.
        if self.n < self.items.len() {
            self.items[self.n] = Some(o);
            self.n += 1;
        }
    }

    /// The outs.
    pub fn iter(&self) -> impl Iterator<Item = Out> + '_ {
        self.items[..self.n].iter().flatten().copied()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    None,
    /// Moving `axis` off its switch, segment `seq`.
    Backoff(usize, u32),
    /// Moving `axis` toward its switch, segment `seq`.
    Search(usize, u32),
    /// The switch made and the queue was flushed: latch when idle.
    Latch(usize),
    /// A segment to finish.
    Segment(u32),
    /// Taking segments.
    Run,
    /// The spindle reaching its speed.
    Spin,
    /// The turret unlocking, then locking.
    TurretOff,
    TurretOn,
}

#[derive(Debug, Clone, Copy)]
struct Phase {
    state: PhaseState,
    last_id: u32,
    step: Step,
    abort: Option<Abort>,
    args: [i32; 3],
}

const IDLE_PHASE: Phase = Phase {
    state: PhaseState::Idle,
    last_id: 0,
    step: Step::None,
    abort: None,
    args: [0; 3],
};

/// The model (module docs).
pub struct Lathe {
    lim: Limits,
    counts: [Option<i64>; 2],
    home: [Option<bool>; 2],
    /// machine µm = raw µm + offset, once referenced.
    offset: [Option<i64>; 2],
    spindle_cmd: i32,
    spindle: Option<i32>,
    station: u8,
    ready: Option<bool>,
    estop: Option<bool>,
    queue: Option<QueueState>,
    phases: [Phase; 6],
    own_seq: u32,
    run_last: Option<u32>,
    /// Segments forwarded since the queue last reported (its `free` does
    /// not count them yet).
    in_flight: u16,
    /// Segments dropped (did not fit, out of travel).
    pub dropped: u32,
    cm_sent: [Option<CmState>; 6],
}

/// Own segments are numbered from here, far from a program's.
const OWN_SEQ: u32 = 0x8000_0000;

impl Lathe {
    /// A machine nothing is known about yet.
    pub fn new(lim: Limits) -> Self {
        Lathe {
            lim,
            counts: [None; 2],
            home: [None; 2],
            offset: [None; 2],
            spindle_cmd: 0,
            spindle: None,
            station: 1,
            ready: None,
            estop: None,
            queue: None,
            phases: [IDLE_PHASE; 6],
            own_seq: OWN_SEQ,
            run_last: None,
            in_flight: 0,
            dropped: 0,
            cm_sent: [None; 6],
        }
    }

    /// Phase `i`'s state.
    pub fn phase(&self, i: usize) -> PhaseState {
        self.phases[i].state
    }

    /// Phase `i`'s report: `(state, last id, values)`.
    pub fn report(&self, i: usize) -> (PhaseState, u32, [i32; 1], usize) {
        let p = &self.phases[i];
        let n = usize::from(p.abort.is_some() && p.state == PhaseState::Aborted);
        (p.state, p.last_id, [p.abort.map_or(0, |a| a as i32)], n)
    }

    /// Whether axis `a` (0 X, 1 Z) is referenced.
    pub fn referenced(&self, a: usize) -> bool {
        self.offset[a].is_some()
    }

    /// Start's argument ranges for phase `i`, from the machine's limits.
    fn ranges_for(&self, i: usize) -> ([(i32, i32); 3], usize) {
        let l = &self.lim;
        match i {
            MOVE => (
                [
                    (l.x_min_um, l.x_max_um),
                    (l.z_min_um, l.z_max_um),
                    (1, l.max_feed_um_s as i32),
                ],
                3,
            ),
            SPIN => ([(-l.spindle_max_rpm, l.spindle_max_rpm), (0, 0), (0, 0)], 1),
            INDEX => ([(1, i32::from(l.turret_stations)), (0, 0), (0, 0)], 1),
            _ => ([(0, 0); 3], 0),
        }
    }

    fn raw_um(&self, a: usize) -> Option<i64> {
        let per_mm = i64::from(self.lim.counts_per_mm.max(1));
        self.counts[a].map(|c| c * 1000 / per_mm)
    }

    fn machine_um(&self, a: usize) -> Option<i64> {
        Some(self.raw_um(a)? + self.offset[a]?)
    }

    fn idle(&self) -> bool {
        self.queue.is_some_and(|q| q.flags & QueueState::IDLE != 0)
    }

    fn next_seq(&mut self) -> u32 {
        self.own_seq = self.own_seq.wrapping_add(1) | OWN_SEQ;
        self.own_seq
    }

    /// A raw-frame segment to `(x, z)` raw µm.
    fn raw_segment(&mut self, x: i64, z: i64, feed: u32, flags: u8, out: &mut Outs) -> u32 {
        let seq = self.next_seq();
        out.push(Out::Segment(Segment {
            seq,
            x_um: x as i32,
            z_um: z as i32,
            feed,
            flags,
        }));
        seq
    }

    fn flush(&mut self, out: &mut Outs) {
        let seq = self.next_seq();
        out.push(Out::Segment(Segment::flush(seq)));
    }

    // ------------------------------------------------------------ phases

    fn set(&mut self, i: usize, state: PhaseState, out: &mut Outs) {
        if self.phases[i].state != state {
            self.phases[i].state = state;
            out.push(Out::Report(i));
        }
    }

    /// The acting phase `i` finished its logic.
    fn done(&mut self, i: usize, out: &mut Outs) {
        let (to, _) = phase::step(self.phases[i].state, PhaseInput::Done);
        self.phases[i].step = Step::None;
        self.set(i, to, out);
        self.enter(i, out);
    }

    /// Aborts phase `i` for `why` (if it can be aborted).
    fn abort(&mut self, i: usize, why: Abort, out: &mut Outs) {
        let (to, effect) = phase::step(self.phases[i].state, PhaseInput::Command(Command::Abort));
        if effect == phase::Effect::Rejected {
            return;
        }
        self.phases[i].abort = Some(why);
        self.set(i, to, out);
        self.enter(i, out);
    }

    /// Runs the logic the phase's new state starts.
    fn enter(&mut self, i: usize, out: &mut Outs) {
        let state = self.phases[i].state;
        // Only a motion phase with motion of its own under way empties the
        // queue: one refused at its start never sent any, and the queue
        // may hold another phase's.
        let moving = matches!(i, REFERENCE | MOVE | RUN) && self.phases[i].step != Step::None;
        match state {
            PhaseState::Running => self.start(i, out),
            PhaseState::Holding => {
                if moving {
                    self.flush(out);
                }
                self.done(i, out);
            }
            PhaseState::Restarting => {
                // The run resumes taking segments; the process model resends
                // from the queue's `done`.
                if i == RUN {
                    self.run_last = None;
                }
                let (to, _) = phase::step(state, PhaseInput::Done);
                self.set(i, to, out);
                if i == RUN {
                    self.phases[i].step = Step::Run;
                } else {
                    self.start(i, out);
                }
            }
            PhaseState::Stopping | PhaseState::Aborting => {
                if moving {
                    self.flush(out);
                }
                if matches!(i, SPIN | SPIN_STOP) && self.spindle_cmd != 0 {
                    self.command_spindle(0, out);
                }
                self.phases[i].step = Step::None;
                self.done(i, out);
            }
            PhaseState::Resetting => {
                self.phases[i].abort = None;
                self.done(i, out);
            }
            _ => {}
        }
    }

    fn command_spindle(&mut self, rpm: i32, out: &mut Outs) {
        self.spindle_cmd = rpm;
        out.push(Out::Spindle(rpm));
    }

    /// Whether a phase other than `i` that moves the axes or the turret
    /// is still doing something (not Idle and not in a final state).
    fn other_mover_active(&self, i: usize) -> bool {
        [REFERENCE, MOVE, RUN, INDEX].iter().any(|&j| {
            let s = self.phases[j].state;
            j != i && s != PhaseState::Idle && !s.is_final()
        })
    }

    /// Running's logic.
    fn start(&mut self, i: usize, out: &mut Outs) {
        if self.estop != Some(true) {
            return self.abort(i, Abort::EStop, out);
        }
        // One thing moves the axes or the turret at a time.
        if matches!(i, REFERENCE | MOVE | RUN | INDEX) && self.other_mover_active(i) {
            return self.abort(i, Abort::MotionBusy, out);
        }
        match i {
            REFERENCE => {
                let first = if self.lim.ref_x_first { 0 } else { 1 };
                self.offset[first] = None;
                self.offset[1 - first] = None;
                self.reference_axis(first, out);
            }
            MOVE | RUN if !(self.referenced(0) && self.referenced(1)) => {
                self.abort(i, Abort::NotReferenced, out)
            }
            MOVE => {
                let [x, z, feed] = self.phases[i].args();
                let (Some(ox), Some(oz)) = (self.offset[0], self.offset[1]) else {
                    return;
                };
                let seq =
                    self.raw_segment(i64::from(x) - ox, i64::from(z) - oz, feed as u32, 0, out);
                self.phases[i].step = Step::Segment(seq);
            }
            RUN => {
                self.run_last = None;
                self.phases[i].step = Step::Run;
            }
            SPIN => {
                let [rpm, ..] = self.phases[i].args();
                self.command_spindle(rpm, out);
                self.phases[i].step = Step::Spin;
                self.check_spin(i, out);
            }
            SPIN_STOP => {
                self.command_spindle(0, out);
                self.phases[i].step = Step::Spin;
                self.check_spin(i, out);
            }
            INDEX => {
                if !self.idle() {
                    return self.abort(i, Abort::MotionBusy, out);
                }
                let [station, ..] = self.phases[i].args();
                let station = station as u8;
                if station == self.station && self.ready == Some(true) {
                    return self.done(i, out);
                }
                self.station = station;
                out.push(Out::Turret(station));
                self.phases[i].step = Step::TurretOff;
            }
            _ => {}
        }
    }

    /// Starts referencing axis `a`: off the switch first if it is made.
    fn reference_axis(&mut self, a: usize, out: &mut Outs) {
        let (Some(x), Some(z)) = (self.raw_um(0), self.raw_um(1)) else {
            // No position yet: nothing can move.
            return self.abort(REFERENCE, Abort::NoSwitch, out);
        };
        let toward = if self.home_at_max(a) { 1 } else { -1 };
        let mut target = [x, z];
        let step = if self.home[a] == Some(true) {
            target[a] -= toward * 3_000;
            let seq = self.raw_segment(target[0], target[1], self.lim.search_um_s, 0, out);
            Step::Backoff(a, seq)
        } else {
            let span = i64::from(self.span_um(a)) + 10_000;
            target[a] += toward * span;
            let seq = self.raw_segment(target[0], target[1], self.lim.search_um_s, 0, out);
            Step::Search(a, seq)
        };
        self.phases[REFERENCE].step = step;
    }

    fn span_um(&self, a: usize) -> i32 {
        if a == 0 {
            self.lim.x_max_um - self.lim.x_min_um
        } else {
            self.lim.z_max_um - self.lim.z_min_um
        }
    }

    fn home_at_max(&self, a: usize) -> bool {
        let l = &self.lim;
        let (min, max, r) = if a == 0 {
            (l.x_min_um, l.x_max_um, l.ref_x_um)
        } else {
            (l.z_min_um, l.z_max_um, l.ref_z_um)
        };
        i64::from(r) * 2 >= i64::from(min) + i64::from(max)
    }

    fn check_spin(&mut self, i: usize, out: &mut Outs) {
        let Some(actual) = self.spindle else { return };
        let target = self.spindle_cmd;
        let tol = (target.abs() / 50).max(20);
        let there = if target == 0 {
            actual == 0
        } else {
            (actual - target).abs() <= tol
        };
        if there {
            self.done(i, out);
        }
    }

    /// After a reading: advance whatever waits on it.
    fn advance(&mut self, out: &mut Outs) {
        for i in 0..PHASES.len() {
            let p = self.phases[i];
            match p.step {
                Step::Backoff(a, seq)
                    if self.queue.is_some_and(|q| q.done == seq) && self.idle() =>
                {
                    if self.home[a] == Some(true) {
                        // Still on it after 3 mm: the switch is stuck.
                        self.abort(i, Abort::NoSwitch, out);
                    } else {
                        self.reference_axis(a, out);
                    }
                }
                Step::Search(a, _) if self.home[a] == Some(true) => {
                    self.flush(out);
                    self.phases[i].step = Step::Latch(a);
                }
                Step::Search(_, seq) if self.queue.is_some_and(|q| q.done == seq) => {
                    self.abort(i, Abort::NoSwitch, out)
                }
                Step::Latch(a) if self.idle() => {
                    let Some(raw) = self.raw_um(a) else { continue };
                    let r = if a == 0 {
                        self.lim.ref_x_um
                    } else {
                        self.lim.ref_z_um
                    };
                    self.offset[a] = Some(i64::from(r) - raw);
                    let first = if self.lim.ref_x_first { 0 } else { 1 };
                    if a == first {
                        self.reference_axis(1 - first, out);
                    } else {
                        self.done(i, out);
                    }
                }
                Step::Segment(seq) if self.queue.is_some_and(|q| q.done == seq) && self.idle() => {
                    self.done(i, out)
                }
                Step::Run => {
                    let finished = self
                        .run_last
                        .is_some_and(|l| self.queue.is_some_and(|q| q.done == l))
                        && self.idle();
                    if finished {
                        self.done(i, out);
                    }
                }
                Step::Spin => self.check_spin(i, out),
                Step::TurretOff if self.ready == Some(false) => {
                    self.phases[i].step = Step::TurretOn
                }
                Step::TurretOn if self.ready == Some(true) => self.done(i, out),
                _ => {}
            }
        }
    }

    // ------------------------------------------------------------ inputs

    /// A reading arrived.
    pub fn input(&mut self, input: Input) -> Outs {
        let mut out = Outs::default();
        match input {
            Input::XPos(p) => self.counts[0] = Some(p.counts),
            Input::ZPos(p) => self.counts[1] = Some(p.counts),
            Input::XHome(on) => self.home[0] = Some(on),
            Input::ZHome(on) => self.home[1] = Some(on),
            Input::SpindleActual(rpm) => self.spindle = Some(rpm),
            Input::TurretReady(on) => self.ready = Some(on),
            Input::Queue(q) => {
                self.queue = Some(q);
                self.in_flight = 0;
            }
            Input::EStop(closed) => {
                let was = self.estop;
                self.estop = Some(closed);
                if !closed && was != Some(false) {
                    // The position is no longer trusted (doc 40 §5).
                    self.offset = [None; 2];
                    // Told to the driver, not only remembered: a driver
                    // must not depend on its own e-stop handling.
                    self.command_spindle(0, &mut out);
                    for i in 0..PHASES.len() {
                        self.abort(i, Abort::EStop, &mut out);
                    }
                }
                if closed && was == Some(false) {
                    // Acknowledge the stop so motion may resume.
                    self.flush(&mut out);
                }
            }
        }
        self.advance(&mut out);
        self.states(&mut out);
        out
    }

    /// A command for phase `i` (a recipe-core command body). Malformed,
    /// out of range or repeated (same id) commands change nothing; the
    /// report is sent again either way.
    pub fn command(&mut self, i: usize, body: &[u8]) -> Outs {
        let mut out = Outs::default();
        let (ranges, n) = self.ranges_for(i);
        let state = self.phases[i].state;
        match wire::serve(state, &ranges[..n], body) {
            Ok(s) if s.command.id != self.phases[i].last_id || s.command.id == 0 => {
                self.phases[i].last_id = s.command.id;
                if s.accepted {
                    if s.command.command == Command::Start {
                        self.phases[i].set_args(&s.command.args[..s.command.n]);
                    }
                    if s.command.command == Command::Abort {
                        self.phases[i].abort = Some(Abort::Commanded);
                    }
                    self.set(i, s.state, &mut out);
                    self.enter(i, &mut out);
                }
            }
            _ => {}
        }
        out.push(Out::Report(i));
        self.advance(&mut out);
        self.states(&mut out);
        out
    }

    /// A batch of segments from the process model ([`cnc_model::wire`]).
    pub fn segments(&mut self, body: &[u8]) -> Outs {
        let mut out = Outs::default();
        if self.phases[RUN].step != Step::Run || self.phases[RUN].state != PhaseState::Running {
            self.dropped += 1;
            return out;
        }
        let ok = self.take_batch(body, &mut out);
        if !ok {
            self.dropped += 1;
            self.abort(RUN, Abort::BadSegments, &mut out);
        }
        self.advance(&mut out);
        self.states(&mut out);
        out
    }

    fn take_batch(&mut self, body: &[u8], out: &mut Outs) -> bool {
        let Some(batch) = cnc_model::wire::decode_segments(body) else {
            return false;
        };
        let free = self
            .queue
            .map_or(0, |q| usize::from(q.free.saturating_sub(self.in_flight)));
        let (Some(ox), Some(oz)) = (self.offset[0], self.offset[1]) else {
            return false;
        };
        if batch.len() > free {
            return false;
        }
        let max_feed = u64::from(self.lim.max_feed_um_s);
        // A synchronised move's speed is its feed per revolution times the
        // spindle's speed: checked at the faster of the commanded and the
        // measured speed, which must also be within the threading limit.
        let rpm = u64::from(
            self.spindle_cmd
                .unsigned_abs()
                .max(self.spindle.unwrap_or(0).unsigned_abs()),
        );
        let threading_ok = rpm > 0 && rpm <= u64::from(self.lim.threading_max_rpm.unsigned_abs());
        let ok = batch.segments().all(|s| {
            let feed_ok = if s.flags & Segment::RAPID != 0 {
                true
            } else if s.flags & Segment::SYNC != 0 {
                threading_ok && u64::from(s.feed) * rpm / 60 <= max_feed
            } else {
                u64::from(s.feed) <= max_feed
            };
            self.lim.contains(s.x_um, s.z_um) && feed_ok && s.flags & Segment::FLUSH == 0
        });
        if !ok {
            return false;
        }
        let mut last = None;
        for s in batch.segments() {
            last = Some(s.seq);
            self.in_flight = self.in_flight.saturating_add(1);
            out.push(Out::Segment(Segment {
                x_um: (i64::from(s.x_um) - ox) as i32,
                z_um: (i64::from(s.z_um) - oz) as i32,
                ..s
            }));
        }
        if batch.last {
            self.run_last = last.or(self.queue.map(|q| q.done));
        }
        true
    }

    /// The periodic tick: every phase's report again (a heartbeat).
    pub fn heartbeat(&mut self) -> Outs {
        let mut out = Outs::default();
        for i in 0..PHASES.len() {
            out.push(Out::Report(i));
        }
        self.cm_sent = [None; 6];
        self.states(&mut out);
        out
    }

    fn cm_state(&self, i: usize) -> Option<CmState> {
        Some(match i {
            CM_X | 1 => {
                let referenced = self.referenced(i);
                let pos = if referenced {
                    self.machine_um(i)
                } else {
                    self.raw_um(i)
                };
                CmState::Axis {
                    referenced,
                    position_um: pos? as i32,
                }
            }
            2 => CmState::Spindle {
                commanded: self.spindle_cmd,
                actual: self.spindle.unwrap_or(0),
            },
            3 => CmState::Turret {
                station: self.station,
                ready: self.ready.unwrap_or(false),
            },
            CM_SAFETY => CmState::Safety {
                chain_closed: self.estop.unwrap_or(false),
                stopped: self
                    .queue
                    .is_some_and(|q| q.flags & QueueState::STOPPED != 0),
            },
            CM_MOTION => CmState::Motion(self.queue?),
            _ => return None,
        })
    }

    /// The control modules whose state changed.
    fn states(&mut self, out: &mut Outs) {
        for i in 0..self.cm_sent.len() {
            if let Some(state) = self.cm_state(i) {
                if self.cm_sent[i] != Some(state) {
                    self.cm_sent[i] = Some(state);
                    out.push(Out::Cm(i, state));
                }
            }
        }
    }
}

impl Phase {
    /// Start's arguments (zero past the ones given).
    fn args(&self) -> [i32; 3] {
        self.args
    }

    fn set_args(&mut self, args: &[i32]) {
        self.args = [0; 3];
        for (slot, v) in self.args.iter_mut().zip(args) {
            *slot = *v;
        }
    }
}
