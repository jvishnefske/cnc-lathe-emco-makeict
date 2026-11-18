//! The process model as a pure state machine: operator commands in, the
//! physical unit's reports and states in, phase commands and segment
//! batches out ([`Process::take`]).
//!
//! It drives one physical unit, the one assigned to it ([`Cmd::Assign`]),
//! and knows its machine only from what that unit announces (its info:
//! travel, speeds, spindle, belts, turret). So the same process model runs
//! unchanged on any physical model: a simulator with the PC Turn 50's
//! numbers, another machine's, the KD240.
//!
//! A phase is run as S88 has it: Reset first if it is in a final state,
//! Start, wait for the report naming the Start's id to say Complete (or
//! Aborted, Stopped: the job fails), then Reset again. A program
//! ([`Cmd::Run`]) must pass the reference gate (both axes referenced since
//! the last e-stop, which the unit reports) and the screen's preflight
//! (`emco_rules::preflight`, the belt and threading rules from the unit's
//! own belts), and compile ([`crate::compile`]); its motion is streamed in
//! batches of at most the queue's free slots, one batch per queue report.

use std::collections::BTreeMap;

use cnc_model::wire::{self, CmState, PHASES};
use cnc_model::{Limits, QueueState, Segment};
use recipe_core::phase::{Command, PhaseState};
use recipe_core::wire::{decode_report, encode_command, Report};

use crate::compile::{compile, Op};

/// Most programs held at once.
pub const MAX_PROGRAMS: usize = 16;
/// Most bytes of program text held, all programs together.
pub const PROGRAM_BYTES: usize = 256 * 1024;

const REFERENCE: usize = 0;
const RUN: usize = 2;
const SPIN: usize = 3;
const SPIN_STOP: usize = 4;
const INDEX: usize = 5;

/// An operator command (`app/proc/<node>/cmd`, [`Cmd::decode`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmd {
    /// Drive the physical unit with this node id: `[1][node 8]`.
    Assign([u8; 8]),
    /// Reference the machine: `[2]`.
    Reference,
    /// Run a stored program with the belt the operator says is fitted:
    /// `[3][program u32][belt u8]`.
    Run { program: u32, belt: u8 },
    /// Feed hold: `[4]`.
    Hold,
    /// Resume after a hold: `[5]`.
    Resume,
    /// Stop the job: `[6]`.
    Stop,
    /// Forget a finished or failed job: `[7]`.
    Clear,
}

impl Cmd {
    /// Decodes a command body.
    pub fn decode(b: &[u8]) -> Option<Cmd> {
        Some(match b {
            [1, node @ ..] if node.len() == 8 => Cmd::Assign(node.try_into().ok()?),
            [2] => Cmd::Reference,
            [3, p0, p1, p2, p3, belt] => Cmd::Run {
                program: u32::from_le_bytes([*p0, *p1, *p2, *p3]),
                belt: *belt,
            },
            [4] => Cmd::Hold,
            [5] => Cmd::Resume,
            [6] => Cmd::Stop,
            [7] => Cmd::Clear,
            _ => return None,
        })
    }

    /// Encodes it.
    pub fn encode(&self) -> Vec<u8> {
        match *self {
            Cmd::Assign(n) => [&[1u8][..], &n].concat(),
            Cmd::Reference => vec![2],
            Cmd::Run { program, belt } => {
                let p = program.to_le_bytes();
                vec![3, p[0], p[1], p[2], p[3], belt]
            }
            Cmd::Hold => vec![4],
            Cmd::Resume => vec![5],
            Cmd::Stop => vec![6],
            Cmd::Clear => vec![7],
        }
    }
}

/// What the process model is doing, as it publishes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum RunState {
    Idle = 0,
    Referencing = 1,
    Running = 2,
    Held = 3,
    Done = 4,
    Failed = 5,
}

/// Where a phase is in its Start / Reset handshake.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Wait {
    /// A Reset (id) to bring it to Idle first.
    Ready(u32),
    /// The Start (id) to complete.
    Done(u32),
    /// The Reset (id) after it completed.
    Reset(u32),
}

#[derive(Debug, Clone)]
struct Program {
    id: u32,
    ops: Vec<Op>,
    at: usize,
    /// Segments of the current motion sent so far.
    sent: usize,
    /// A batch is out; wait for the queue to report before the next.
    awaiting_queue: bool,
    held: bool,
}

#[derive(Debug, Clone)]
enum Job {
    Idle,
    Reference,
    Program(Program),
    Done,
    Failed(String),
}

/// The model (module docs).
pub struct Process {
    node: [u8; 8],
    unit: Option<[u8; 8]>,
    info: Option<Limits>,
    reports: [Option<Report>; 6],
    cms: [Option<CmState>; 6],
    programs: BTreeMap<u32, Vec<u8>>,
    job: Job,
    phase: Option<(usize, Wait)>,
    next_id: u32,
    /// Work zero, machine mm (the site's G54).
    g54: (f64, f64),
    out: Vec<(String, Vec<u8>)>,
    published: Option<Vec<u8>>,
}

impl Process {
    /// A process model on node `node`, the site's work zero at `g54`
    /// (machine mm).
    pub fn new(node: [u8; 8], g54: (f64, f64)) -> Self {
        Process {
            node,
            unit: None,
            info: None,
            reports: [None; 6],
            cms: [None; 6],
            programs: BTreeMap::new(),
            job: Job::Idle,
            phase: None,
            next_id: 0,
            g54,
            out: Vec::new(),
            published: None,
        }
    }

    /// The unit it drives.
    pub fn unit(&self) -> Option<[u8; 8]> {
        self.unit
    }

    /// The machine the unit announced.
    pub fn machine(&self) -> Option<&Limits> {
        self.info.as_ref()
    }

    /// What it is doing.
    pub fn state(&self) -> (RunState, Option<&str>) {
        match &self.job {
            Job::Idle => (RunState::Idle, None),
            Job::Reference => (RunState::Referencing, None),
            Job::Program(p) if p.held => (RunState::Held, None),
            Job::Program(_) => (RunState::Running, None),
            Job::Done => (RunState::Done, None),
            Job::Failed(m) => (RunState::Failed, Some(m)),
        }
    }

    /// The publications to send, in order.
    pub fn take(&mut self) -> Vec<(String, Vec<u8>)> {
        self.publish_state();
        core::mem::take(&mut self.out)
    }

    /// Keys this model listens on: its own command and program keys.
    pub fn own_keys(&self) -> [String; 2] {
        [own_key(self.node, "cmd"), own_key(self.node, "program")]
    }

    /// Keys of the assigned unit it listens on: each phase's report, the
    /// control modules, the info.
    pub fn unit_keys(&self) -> Vec<String> {
        let Some(u) = self.unit else {
            return Vec::new();
        };
        let mut k: Vec<String> = PHASES
            .iter()
            .filter_map(|(em, ph, _)| {
                wire::phase_state_key(u, em, ph).map(|k| k.as_str().to_owned())
            })
            .collect();
        k.extend(wire::cm_key(u).map(|k| k.as_str().to_owned()));
        k.extend(wire::info_key(u).map(|k| k.as_str().to_owned()));
        k
    }

    /// A publication arrived on `key`.
    pub fn publication(&mut self, key: &str, body: &[u8]) {
        let [cmd, program] = self.own_keys();
        if key == cmd {
            if let Some(c) = Cmd::decode(body) {
                self.command(c);
            }
        } else if key == program {
            self.program_chunk(body);
        } else if let Some(u) = self.unit {
            if wire::info_key(u).is_some_and(|k| k.as_str() == key) {
                self.info = Limits::decode_info(body);
            } else if wire::cm_key(u).is_some_and(|k| k.as_str() == key) {
                if let Some((i, s)) = wire::decode_cm(body) {
                    self.cms[i] = Some(s);
                    if i == 5 {
                        if let Job::Program(p) = &mut self.job {
                            p.awaiting_queue = false;
                        }
                    }
                }
            } else if let Some(i) = PHASES.iter().position(|(em, ph, _)| {
                wire::phase_state_key(u, em, ph).is_some_and(|k| k.as_str() == key)
            }) {
                if let Ok(r) = decode_report(body) {
                    self.reports[i] = Some(r);
                }
            }
        }
        self.step();
    }

    fn command(&mut self, c: Cmd) {
        match c {
            Cmd::Assign(u) => {
                if matches!(self.job, Job::Idle | Job::Done | Job::Failed(_)) {
                    self.unit = Some(u);
                    self.info = None;
                    self.reports = [None; 6];
                    self.cms = [None; 6];
                }
            }
            Cmd::Reference if matches!(self.job, Job::Idle | Job::Done) => {
                if self.unit.is_none() {
                    return self.fail("no unit assigned");
                }
                self.job = Job::Reference;
                self.begin(REFERENCE);
            }
            Cmd::Run { program, belt } if matches!(self.job, Job::Idle | Job::Done) => {
                self.run(program, belt)
            }
            Cmd::Hold => {
                if let Job::Program(p) = &mut self.job {
                    if !p.held && self.phase.is_some_and(|(i, _)| i == RUN) {
                        p.held = true;
                        self.send(RUN, Command::Hold, &[]);
                    }
                }
            }
            Cmd::Resume => {
                let done = self.queue().map_or(0, |q| q.done);
                if let Job::Program(p) = &mut self.job {
                    if p.held {
                        p.held = false;
                        // The queue was flushed: resend from the first
                        // segment not done.
                        if let Some(Op::Motion(segs)) = p.ops.get(p.at) {
                            p.sent = segs.iter().position(|s| s.seq > done).unwrap_or(segs.len());
                        }
                        p.awaiting_queue = false;
                        self.send(RUN, Command::Restart, &[]);
                    }
                }
            }
            Cmd::Stop => {
                if let Some((i, _)) = self.phase {
                    self.send(i, Command::Stop, &[]);
                }
                self.phase = None;
                self.job = Job::Failed("stopped by the operator".into());
            }
            Cmd::Clear if matches!(self.job, Job::Done | Job::Failed(_)) => self.job = Job::Idle,
            _ => {}
        }
    }

    fn program_chunk(&mut self, b: &[u8]) {
        let Some((&head, data)) = b.split_first_chunk::<9>() else {
            return;
        };
        let id = u32::from_le_bytes([head[0], head[1], head[2], head[3]]);
        let offset = u32::from_le_bytes([head[4], head[5], head[6], head[7]]) as usize;
        if !self.programs.contains_key(&id) && self.programs.len() >= MAX_PROGRAMS {
            return;
        }
        // The budget is for all programs together, this one's old text
        // excluded when it is being replaced.
        let others: usize = self
            .programs
            .iter()
            .filter(|(k, _)| **k != id)
            .map(|(_, t)| t.len())
            .sum();
        let text = self.programs.entry(id).or_default();
        if offset == 0 {
            text.clear();
        }
        if offset == text.len() && others + text.len() + data.len() <= PROGRAM_BYTES {
            text.extend_from_slice(data);
        }
    }

    fn queue(&self) -> Option<QueueState> {
        match self.cms[5] {
            Some(CmState::Motion(q)) => Some(q),
            _ => None,
        }
    }

    fn referenced(&self) -> bool {
        matches!(
            self.cms[0],
            Some(CmState::Axis {
                referenced: true,
                ..
            })
        ) && matches!(
            self.cms[1],
            Some(CmState::Axis {
                referenced: true,
                ..
            })
        )
    }

    fn fail(&mut self, why: &str) {
        self.phase = None;
        self.job = Job::Failed(why.to_owned());
    }

    fn run(&mut self, program: u32, belt: u8) {
        let (Some(_), Some(info)) = (self.unit, self.info) else {
            return self.fail("no unit, or it has not announced its machine");
        };
        if !self.referenced() {
            return self.fail("not referenced: reference the machine first (doc 40 §5)");
        }
        let Some(text) = self
            .programs
            .get(&program)
            .and_then(|t| String::from_utf8(t.clone()).ok())
        else {
            return self.fail("no such program");
        };
        // The screen's preflight, with this machine's belts and limits. It
        // fails closed: a run it cannot check does not run.
        let Some(b) = emco_rules::Belt::new(belt) else {
            return self.fail("no such belt (1, 2 or 3)");
        };
        let Some(machine) = rules_machine(&info) else {
            return self.fail("the preflight knows machines with three belt positions only");
        };
        if let Some(f) = emco_rules::preflight(&text, b, &machine)
            .into_iter()
            .find(|f| f.severity == emco_rules::Severity::Error)
        {
            return self.fail(&format!("preflight, line {}: {}", f.line, f.message));
        }
        match compile(&text, &info, self.g54.0, self.g54.1) {
            Ok(c) => {
                self.job = Job::Program(Program {
                    id: program,
                    ops: c.ops,
                    at: 0,
                    sent: 0,
                    awaiting_queue: false,
                    held: false,
                });
                self.next_op();
            }
            Err(e) => self.fail(&format!("line {}: {}", e.line + 1, e.message)),
        }
    }

    fn next_op(&mut self) {
        let Job::Program(p) = &mut self.job else {
            return;
        };
        let Some(op) = p.ops.get(p.at).cloned() else {
            self.job = Job::Done;
            self.phase = None;
            return;
        };
        p.sent = 0;
        p.awaiting_queue = false;
        match op {
            Op::Spindle(0) => self.begin(SPIN_STOP),
            Op::Spindle(_) => self.begin(SPIN),
            Op::Index(_) => self.begin(INDEX),
            Op::Motion(_) => self.begin(RUN),
        }
    }

    fn args(&self, i: usize) -> Vec<i32> {
        let Job::Program(p) = &self.job else {
            return Vec::new();
        };
        match (i, p.ops.get(p.at)) {
            (SPIN, Some(Op::Spindle(rpm))) => vec![*rpm],
            (INDEX, Some(Op::Index(s))) => vec![i32::from(*s)],
            _ => Vec::new(),
        }
    }

    fn id(&mut self) -> u32 {
        self.next_id += 1;
        self.next_id
    }

    fn send(&mut self, i: usize, c: Command, args: &[i32]) -> u32 {
        let id = self.id();
        let (Some(u), (em, ph, _)) = (self.unit, PHASES[i]) else {
            return id;
        };
        let mut buf = [0u8; recipe_core::wire::MAX_BODY];
        if let (Some(k), Ok(body)) = (
            wire::phase_key(u, em, ph),
            encode_command(c, id, args, &mut buf),
        ) {
            self.out.push((k.as_str().to_owned(), body.to_vec()));
        }
        id
    }

    /// Starts phase `i`: a Reset first if it is not Idle.
    fn begin(&mut self, i: usize) {
        let state = self.reports[i].map_or(PhaseState::Idle, |r| r.state);
        let wait = if state == PhaseState::Idle {
            let args = self.args(i);
            Wait::Done(self.send(i, Command::Start, &args))
        } else {
            Wait::Ready(self.send(i, Command::Reset, &[]))
        };
        self.phase = Some((i, wait));
    }

    /// Advances the phase handshake and the job.
    fn step(&mut self) {
        for _ in 0..4 {
            let Some((i, wait)) = self.phase else { return };
            let Some(r) = self.reports[i] else { return };
            match wait {
                Wait::Ready(id) if r.last_id == id && r.state == PhaseState::Idle => {
                    let args = self.args(i);
                    let id = self.send(i, Command::Start, &args);
                    self.phase = Some((i, Wait::Done(id)));
                }
                Wait::Done(id) if r.last_id >= id => match r.state {
                    PhaseState::Complete => {
                        let id = self.send(i, Command::Reset, &[]);
                        self.phase = Some((i, Wait::Reset(id)));
                    }
                    PhaseState::Aborted | PhaseState::Stopped => {
                        let (em, ph, _) = PHASES[i];
                        let why = r.values[..r.n]
                            .first()
                            .map(|c| format!(" (code {c})"))
                            .unwrap_or_default();
                        return self.fail(&format!("{em}.{ph} {}{why}", r.state.name()));
                    }
                    PhaseState::Idle if r.last_id == id => {
                        let (em, ph, _) = PHASES[i];
                        return self.fail(&format!("{em}.{ph} refused its start"));
                    }
                    PhaseState::Running if i == RUN => {
                        self.stream();
                        return;
                    }
                    _ => return,
                },
                Wait::Reset(id) if r.last_id == id && r.state == PhaseState::Idle => {
                    self.phase = None;
                    match &mut self.job {
                        Job::Reference => self.job = Job::Done,
                        Job::Program(p) => {
                            p.at += 1;
                            self.next_op();
                        }
                        _ => {}
                    }
                }
                _ => return,
            }
        }
    }

    /// Sends the next batch of the current motion, if the queue has room.
    fn stream(&mut self) {
        let free = self.queue().map_or(0, |q| usize::from(q.free));
        let Job::Program(p) = &mut self.job else {
            return;
        };
        if p.held || p.awaiting_queue {
            return;
        }
        let Some(Op::Motion(segs)) = p.ops.get(p.at) else {
            return;
        };
        let left = segs.len() - p.sent;
        let n = left.min(free).min(wire::MAX_BATCH);
        if n == 0 && left > 0 {
            return;
        }
        let batch: Vec<Segment> = segs[p.sent..p.sent + n].to_vec();
        let last = p.sent + n == segs.len();
        if n == 0 && !last {
            return;
        }
        p.sent += n;
        p.awaiting_queue = true;
        if let (Some(u), Some(body)) = (self.unit, wire::encode_segments(&batch, last)) {
            if let Some(k) = wire::segments_key(u) {
                self.out.push((k.as_str().to_owned(), body));
            }
        }
    }

    fn publish_state(&mut self) {
        let (state, msg) = self.state();
        let (program, at, ops) = match &self.job {
            Job::Program(p) => (p.id, p.at as u16, p.ops.len() as u16),
            _ => (0, 0, 0),
        };
        let msg = msg.unwrap_or("").as_bytes();
        let msg = &msg[..msg.len().min(200)];
        let mut b = vec![state as u8];
        b.extend_from_slice(&program.to_le_bytes());
        b.extend_from_slice(&at.to_le_bytes());
        b.extend_from_slice(&ops.to_le_bytes());
        b.push(msg.len() as u8);
        b.extend_from_slice(msg);
        if self.published.as_ref() != Some(&b) {
            self.published = Some(b.clone());
            self.out.push((own_key(self.node, "state"), b));
        }
    }
}

/// `app/proc/<node>/<what>`.
pub fn own_key(node: [u8; 8], what: &str) -> String {
    let hex: String = node.iter().map(|b| format!("{b:02x}")).collect();
    format!("app/proc/{hex}/{what}")
}

/// A program as publications on `app/proc/<node>/program`: `[id u32][offset
/// u32][last u8]` then up to 400 bytes each.
pub fn program_chunks(id: u32, text: &str) -> Vec<Vec<u8>> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut at = 0;
    loop {
        let n = (bytes.len() - at).min(400);
        let mut b = Vec::with_capacity(9 + n);
        b.extend_from_slice(&id.to_le_bytes());
        b.extend_from_slice(&(at as u32).to_le_bytes());
        b.push(u8::from(at + n == bytes.len()));
        b.extend_from_slice(&bytes[at..at + n]);
        out.push(b);
        at += n;
        if at == bytes.len() {
            return out;
        }
    }
}

/// The screen rules' view of a machine from its info (three belts only).
fn rules_machine(info: &Limits) -> Option<emco_rules::Machine> {
    if info.belts != 3 {
        return None;
    }
    let b = |i: usize| (info.belt_rpm[i].0 as u32, info.belt_rpm[i].1 as u32);
    Some(emco_rules::Machine {
        x_retract: f64::from(info.ref_x_um) / 1000.0,
        z_park: f64::from(info.ref_z_um) / 1000.0,
        tool_change_retracts: false,
        belts: [b(0), b(1), b(2)],
        threading_max_rpm: info.threading_max_rpm as u32,
    })
}

/// A decoded state publication: `(state, program, op, ops, message)`.
pub fn decode_state(b: &[u8]) -> Option<(u8, u32, u16, u16, String)> {
    let (&[s, p0, p1, p2, p3, a0, a1, o0, o1, n], msg) = b.split_first_chunk::<10>()?;
    (msg.len() == usize::from(n)).then(|| {
        (
            s,
            u32::from_le_bytes([p0, p1, p2, p3]),
            u16::from_le_bytes([a0, a1]),
            u16::from_le_bytes([o0, o1]),
            String::from_utf8_lossy(msg).into_owned(),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_round_trip() {
        for c in [
            Cmd::Assign([1, 2, 3, 4, 5, 6, 7, 8]),
            Cmd::Reference,
            Cmd::Run {
                program: 7,
                belt: 2,
            },
            Cmd::Hold,
            Cmd::Resume,
            Cmd::Stop,
            Cmd::Clear,
        ] {
            assert_eq!(Cmd::decode(&c.encode()), Some(c));
        }
        assert_eq!(Cmd::decode(&[3, 0]), None);
    }

    #[test]
    fn programs_arrive_in_chunks() {
        let mut p = Process::new([9; 8], (0.0, -50.0));
        let text = "G0 X10 Z0\n".repeat(100);
        for c in program_chunks(4, &text) {
            p.publication(&own_key([9; 8], "program"), &c);
        }
        assert_eq!(p.programs.get(&4).map(Vec::len), Some(text.len()));
    }

    #[test]
    fn the_program_store_has_a_budget() {
        let mut p = Process::new([9; 8], (0.0, -50.0));
        let key = own_key([9; 8], "program");
        let big = "G0 X10 Z0\n".repeat(PROGRAM_BYTES / 10 / 2);
        for id in 0..3 {
            for c in program_chunks(id, &big) {
                p.publication(&key, &c);
            }
        }
        let total: usize = p.programs.values().map(Vec::len).sum();
        assert!(total <= PROGRAM_BYTES, "{total}");
        assert_eq!(p.programs[&0].len(), big.len());
        assert!(p.programs[&2].len() < big.len(), "the third did not fit");
        // Replacing one does not count its old text.
        for c in program_chunks(0, &big) {
            p.publication(&key, &c);
        }
        assert_eq!(p.programs[&0].len(), big.len());
        // And there are only so many ids.
        let mut q = Process::new([9; 8], (0.0, -50.0));
        for id in 0..(MAX_PROGRAMS as u32 + 4) {
            q.publication(&key, &program_chunks(id, "M30\n")[0]);
        }
        assert_eq!(q.programs.len(), MAX_PROGRAMS);
    }

    #[test]
    fn a_run_needs_a_unit_its_machine_and_a_reference() {
        let mut p = Process::new([9; 8], (0.0, -50.0));
        let cmd = own_key([9; 8], "cmd");
        p.publication(
            &cmd,
            &Cmd::Run {
                program: 1,
                belt: 2,
            }
            .encode(),
        );
        assert_eq!(p.state().0, RunState::Failed);
        p.publication(&cmd, &Cmd::Clear.encode());
        p.publication(&cmd, &Cmd::Assign([1; 8]).encode());
        let info_key = wire::info_key([1; 8]).unwrap();
        p.publication(
            info_key.as_str(),
            &cnc_model::PhysicalConfig::pc_turn_50().limits().encode(),
        );
        assert!(p.machine().is_some());
        p.publication(
            &cmd,
            &Cmd::Run {
                program: 1,
                belt: 2,
            }
            .encode(),
        );
        let (s, m) = p.state();
        assert_eq!(s, RunState::Failed);
        assert!(m.unwrap().contains("not referenced"), "{m:?}");
    }
}
