//! A transport onto the lathe's S88 payloads on middleware (`cnc/`): the
//! web UI drives the **process model** and shows the **physical model**'s
//! control modules, in the same `SessionState` shape the virtual transport
//! produces, so the UI, its SSE stream and the egui screen are unchanged.
//!
//! ```text
//!  UI Command ──► MeshTransport ──► app/proc/<process>/cmd       (cnc_model::proc::Cmd)
//!  UI load    ──►               ──► app/proc/<process>/program   (program_chunks)
//!  SessionState ◄──             ◄── app/proc/<process>/state     (ProcState, 1 s heartbeat)
//!                               ◄── app/unit/<unit>/cm           (CmState per control module)
//!                               ◄── app/unit/<unit>/**           (the unit's heartbeats)
//! ```
//!
//! The process model owns the unit: it turns a program into the unit's
//! phase commands and motion segments. This transport never commands the
//! unit's phases itself, so there is one commander, and the process
//! model's reference gate and the screen's preflight stand between the UI
//! and any motion.
//!
//! What maps, and what does not:
//!
//! | UI | process model |
//! |----|---------------|
//! | Home | Reference (after Clear, if the last job failed) |
//! | Run | Run the program last loaded, with the belt the board is configured with ([`MeshConfig::belt`]) |
//! | Hold, Resume | Hold, Resume |
//! | Abort | Stop |
//! | single block, dry run, jog, MDI, touch-off | refused with an `UNSUPPORTED` alarm: the process model has none of them yet |
//! | training faults | refused ([`Transport::fault`] is `false`): the unit's faults are its driver's |
//!
//! The process model is assigned the configured unit whenever its state
//! names another (or none) and it is not busy, so a restarted process
//! payload is reassigned within a heartbeat.
//!
//! Liveness: until the process model's first state the run state is
//! `disconnected`; once it has been silent for [`STALE_MS`] it is
//! `disconnected` again with an alarm. A unit silent for [`STALE_MS`]
//! (its phases report every second) is an alarm.
//!
//! The bus is a [`Bus`]: Zenoh on the board ([`zenoh::ZenohBus`]), a fake in
//! the tests. Nothing here writes the machine's RS-485 bus, and nothing is
//! a safety function: the stop is the cabinet's EMERGENCY-OFF.

pub mod zenoh;

use crate::machine::{Poll, Transport};
use cnc_model::proc::{own_key, program_chunks, Cmd, ProcState, RunState as ProcRun};
use cnc_model::wire::{self, CmState};
use emco_sender_core::gcode::Program;
use emco_sender_core::session::{AlarmInfo, Command, Dro, Event, RunState, SessionState};
use serde_json::{json, Value};

/// A process model or unit silent this long is gone, ms (three heartbeats).
pub const STALE_MS: f64 = 3000.0;
/// At most one Assign this often, ms.
const ASSIGN_EVERY_MS: f64 = 1000.0;
/// Most publications taken from the bus in one poll.
const MAX_PER_POLL: usize = 1024;
/// The id the UI's program is stored under in the process model. One: the
/// UI has one program loaded at a time, and reusing the id keeps the
/// process model's store (16 programs) from filling up.
pub const PROGRAM_ID: u32 = 1;
/// The site's work zero, machine mm: the process payload's G54
/// (`cnc/process/src/payload.rs`), for the work coordinates the DRO shows.
pub const G54_MM: (f64, f64) = (0.0, -50.0);

/// A publish/subscribe bus, polled from the pump thread: both calls return
/// at once.
pub trait Bus {
    /// Publishes `body` on `key`.
    fn publish(&mut self, key: &str, body: &[u8]) -> Result<(), String>;
    /// The next publication received on the subscribed keys, if any.
    fn try_recv(&mut self) -> Option<(String, Vec<u8>)>;
}

/// Which payloads to drive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MeshConfig {
    /// The process payload's node id.
    pub process: [u8; 8],
    /// The physical unit's node id.
    pub unit: [u8; 8],
    /// The belt position fitted on the machine (1, 2 or 3), for the
    /// process model's preflight. The UI has no belt control, so the
    /// board says which one it is set up with.
    pub belt: u8,
}

impl MeshConfig {
    /// The key prefixes a [`Bus`] subscribes to for this transport.
    pub fn prefixes(&self) -> [String; 2] {
        let hex = cnc_model::proc::node_hex;
        [
            format!("app/proc/{}", hex(self.process)),
            format!("app/unit/{}", hex(self.unit)),
        ]
    }
}

/// The UI on the payloads, through any [`Bus`].
pub struct MeshTransport<B: Bus> {
    bus: B,
    cfg: MeshConfig,
    state_key: String,
    cm_key: String,
    unit_prefix: String,
    proc_state: Option<ProcState>,
    proc_seen_ms: Option<f64>,
    unit_seen_ms: Option<f64>,
    cms: [Option<CmState>; wire::CMS.len()],
    loaded: bool,
    last_assign_ms: Option<f64>,
    last_run: Option<RunState>,
    pending: Vec<Event>,
    now_ms: f64,
}

impl<B: Bus> MeshTransport<B> {
    pub fn new(bus: B, cfg: MeshConfig) -> Self {
        let [_, unit_prefix] = cfg.prefixes();
        MeshTransport {
            bus,
            state_key: own_key(cfg.process, "state"),
            cm_key: wire::cm_key(cfg.unit)
                .map(|k| k.as_str().to_owned())
                .unwrap_or_default(),
            unit_prefix: format!("{unit_prefix}/"),
            cfg,
            proc_state: None,
            proc_seen_ms: None,
            unit_seen_ms: None,
            cms: [None; wire::CMS.len()],
            loaded: false,
            last_assign_ms: None,
            last_run: None,
            pending: Vec::new(),
            now_ms: 0.0,
        }
    }

    /// The bus, for tests.
    pub fn bus(&mut self) -> &mut B {
        &mut self.bus
    }

    fn alarm(&mut self, code: &str, message: impl Into<String>) {
        self.pending.push(Event::Alarm {
            code: code.into(),
            message: message.into(),
            at_ms: self.now_ms,
        });
    }

    fn publish(&mut self, what: &str, body: &[u8]) -> bool {
        let key = own_key(self.cfg.process, what);
        match self.bus.publish(&key, body) {
            Ok(()) => true,
            Err(e) => {
                self.alarm("MESH", format!("publish {key}: {e}"));
                false
            }
        }
    }

    fn send(&mut self, cmds: &[Cmd]) {
        for c in cmds {
            if !self.publish("cmd", &c.encode()) {
                return;
            }
        }
    }

    fn proc_alive(&self) -> bool {
        self.proc_seen_ms
            .is_some_and(|t| self.now_ms - t <= STALE_MS)
    }

    fn unit_alive(&self) -> bool {
        self.unit_seen_ms
            .is_some_and(|t| self.now_ms - t <= STALE_MS)
    }

    /// The process model's run state while it is alive.
    fn proc_run(&self) -> Option<ProcRun> {
        self.proc_alive()
            .then(|| self.proc_state.as_ref().map(|s| s.run))
            .flatten()
    }

    /// Clear first when the last job failed: the process model takes a
    /// new job only when idle or done.
    fn clear_if_failed(&self) -> Vec<Cmd> {
        match self.proc_run() {
            Some(ProcRun::Failed) => vec![Cmd::Clear],
            _ => vec![],
        }
    }

    fn assigned(&self) -> bool {
        self.proc_state
            .as_ref()
            .is_some_and(|s| s.unit == Some(self.cfg.unit))
    }

    fn receive(&mut self, key: &str, body: &[u8]) {
        if key == self.state_key {
            if let Some(s) = ProcState::decode(body) {
                if s.run == ProcRun::Failed
                    && self.proc_state.as_ref().map(|p| p.run) != Some(ProcRun::Failed)
                {
                    self.alarm("PROCESS", s.message.clone());
                }
                self.proc_state = Some(s);
                self.proc_seen_ms = Some(self.now_ms);
            }
        } else if key.starts_with(&self.unit_prefix) {
            self.unit_seen_ms = Some(self.now_ms);
            if key == self.cm_key {
                if let Some((i, s)) = wire::decode_cm(body) {
                    self.cms[i] = Some(s);
                }
            }
        }
    }

    /// Assigns the configured unit when the process model names another
    /// and is not busy (it refuses Assign while busy anyway).
    fn maybe_assign(&mut self) {
        let Some(run) = self.proc_run() else {
            return;
        };
        if self.assigned() || run.busy() {
            return;
        }
        if self
            .last_assign_ms
            .is_some_and(|t| self.now_ms - t < ASSIGN_EVERY_MS)
        {
            return;
        }
        self.last_assign_ms = Some(self.now_ms);
        self.send(&[Cmd::Assign(self.cfg.unit)]);
    }

    fn axis(&self, i: usize) -> (bool, f64) {
        match self.cms[i] {
            Some(CmState::Axis {
                referenced,
                position_um,
            }) => (referenced, f64::from(position_um) / 1000.0),
            _ => (false, 0.0),
        }
    }

    /// The UI's view of the payloads.
    pub fn session_state(&self) -> SessionState {
        let mut alarms = Vec::new();
        let mut alarm = |code: &str, message: &str| {
            alarms.push(AlarmInfo {
                code: code.into(),
                message: message.into(),
            })
        };
        let run = match self.proc_run() {
            None => {
                if self.proc_seen_ms.is_some() {
                    alarm("MESH", "the process model is silent");
                }
                RunState::Disconnected
            }
            Some(p) => {
                let mut run = match p {
                    ProcRun::Idle | ProcRun::Done => RunState::Idle,
                    ProcRun::Referencing => RunState::Homing,
                    ProcRun::Running => RunState::Running,
                    ProcRun::Held => RunState::Hold,
                    ProcRun::Failed => RunState::Alarm,
                };
                if p == ProcRun::Failed {
                    let m = self.proc_state.as_ref().map_or("", |s| s.message.as_str());
                    alarm("PROCESS", m);
                }
                if !self.unit_alive() {
                    alarm("UNIT", "the physical model is silent");
                    run = RunState::Alarm;
                }
                if let Some(CmState::Safety {
                    chain_closed,
                    stopped,
                }) = self.cms[4]
                {
                    if !chain_closed {
                        alarm("ESTOP", "the e-stop chain is open");
                        run = RunState::Alarm;
                    } else if stopped {
                        alarm("STOPPED", "motion stopped (an e-stop or a limit)");
                        run = RunState::Alarm;
                    }
                }
                run
            }
        };
        let (x_ref, x) = self.axis(0);
        let (z_ref, z) = self.axis(1);
        let (spindle_rpm, spindle_on) = match self.cms[2] {
            Some(CmState::Spindle { commanded, actual }) => {
                (f64::from(actual.unsigned_abs()), commanded != 0)
            }
            _ => (0.0, false),
        };
        let tool = match self.cms[3] {
            Some(CmState::Turret { station, .. }) => u32::from(station),
            _ => 0,
        };
        SessionState {
            run,
            dro: Dro {
                x_radius_mm: x,
                x_diameter_mm: 2.0 * x,
                z_mm: z,
                x_work_mm: x - G54_MM.0,
                z_work_mm: z - G54_MM.1,
                x_dtg_mm: 0.0,
                z_dtg_mm: 0.0,
                homed: x_ref && z_ref,
            },
            current_line: -1,
            tool,
            spindle_rpm,
            spindle_on,
            feed_mm_min: 0.0,
            modal: vec![],
            alarms,
            single_block: false,
            dry_run: false,
        }
    }
}

impl<B: Bus + 'static> Transport for MeshTransport<B> {
    fn name(&self) -> &'static str {
        "mesh"
    }

    fn command(&mut self, c: Command) {
        // Starting something needs a process model that is answering: a
        // command it cannot hear now must not reach it later. Hold and
        // Abort are always sent (best effort, never a safety function).
        let starts = matches!(c, Command::Home | Command::Run | Command::Resume);
        if starts && self.proc_run().is_none() {
            return self.alarm("MESH", "the process model is not answering");
        }
        match c {
            Command::Home => {
                let mut cmds = self.clear_if_failed();
                cmds.push(Cmd::Reference);
                self.send(&cmds);
            }
            Command::Run => {
                if !self.loaded {
                    return self.alarm("NO-PROGRAM", "load a program first");
                }
                if !self.assigned() {
                    return self.alarm(
                        "MESH",
                        "the process model has not been assigned the unit yet; try again",
                    );
                }
                let mut cmds = self.clear_if_failed();
                cmds.push(Cmd::Run {
                    program: PROGRAM_ID,
                    belt: self.cfg.belt,
                });
                self.send(&cmds);
            }
            Command::Hold => self.send(&[Cmd::Hold]),
            Command::Resume => self.send(&[Cmd::Resume]),
            Command::Abort => self.send(&[Cmd::Stop]),
            Command::SingleBlock { on: false } | Command::DryRun { on: false } => {}
            Command::SingleBlock { on: true }
            | Command::DryRun { on: true }
            | Command::Jog { .. }
            | Command::Mdi { .. }
            | Command::SetWorkOffset { .. } => self.alarm(
                "UNSUPPORTED",
                "the process model runs programs and references; it has no single block, dry run, jog, MDI or touch-off yet",
            ),
        }
    }

    fn load(&mut self, p: &Program) {
        let text: String = p.lines.iter().map(|l| format!("{l}\n")).collect();
        self.loaded = false;
        for chunk in program_chunks(PROGRAM_ID, &text) {
            if !self.publish("program", &chunk) {
                return;
            }
        }
        self.loaded = true;
    }

    fn fault(&mut self, _kind: &'static str) -> bool {
        false
    }

    fn poll(&mut self, now_ms: f64) -> Poll {
        self.now_ms = now_ms;
        for _ in 0..MAX_PER_POLL {
            let Some((k, b)) = self.bus.try_recv() else {
                break;
            };
            self.receive(&k, &b);
        }
        self.maybe_assign();
        let state = self.session_state();
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
        let hex = cnc_model::proc::node_hex;
        let proc_ = self.proc_state.as_ref().map(|s| {
            json!({
                "run": format!("{:?}", s.run).to_lowercase(),
                "program": s.program,
                "op": s.op,
                "ops": s.ops,
                "message": s.message,
                "unit": s.unit.map(hex),
            })
        });
        Poll {
            state: json!({
                "session": state,
                "transport": self.name(),
                "mesh": {
                    "process": hex(self.cfg.process),
                    "unit": hex(self.cfg.unit),
                    "belt": self.cfg.belt,
                    "processState": proc_,
                },
            }),
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
    use std::collections::VecDeque;

    const PROC: [u8; 8] = [0xa1; 8];
    const UNIT: [u8; 8] = [0xb2; 8];

    /// A bus that records what is published and delivers what a test
    /// queues.
    #[derive(Default)]
    struct Fake {
        sent: Vec<(String, Vec<u8>)>,
        inbox: VecDeque<(String, Vec<u8>)>,
        refuse: bool,
    }

    impl Bus for Fake {
        fn publish(&mut self, key: &str, body: &[u8]) -> Result<(), String> {
            if self.refuse {
                return Err("no router".into());
            }
            self.sent.push((key.into(), body.into()));
            Ok(())
        }
        fn try_recv(&mut self) -> Option<(String, Vec<u8>)> {
            self.inbox.pop_front()
        }
    }

    fn cfg() -> MeshConfig {
        MeshConfig {
            process: PROC,
            unit: UNIT,
            belt: 2,
        }
    }

    fn mesh() -> MeshTransport<Fake> {
        MeshTransport::new(Fake::default(), cfg())
    }

    fn proc_state(run: ProcRun, unit: Option<[u8; 8]>, message: &str) -> (String, Vec<u8>) {
        let s = ProcState {
            run,
            program: 0,
            op: 0,
            ops: 0,
            message: message.into(),
            unit,
        };
        (own_key(PROC, "state"), s.encode())
    }

    fn cm(i: usize, s: CmState) -> (String, Vec<u8>) {
        let mut b = [0u8; 1 + CmState::MAX_LEN];
        let n = wire::encode_cm(i, &s, &mut b).unwrap();
        (wire::cm_key(UNIT).unwrap().as_str().into(), b[..n].to_vec())
    }

    fn info() -> (String, Vec<u8>) {
        (wire::info_key(UNIT).unwrap().as_str().into(), vec![0])
    }

    /// The unit and the process model up, assigned and idle.
    fn up(m: &mut MeshTransport<Fake>, at: f64) {
        m.bus().inbox.push_back(info());
        m.bus()
            .inbox
            .push_back(proc_state(ProcRun::Idle, Some(UNIT), ""));
        m.poll(at);
    }

    fn cmds(m: &mut MeshTransport<Fake>) -> Vec<Cmd> {
        let key = own_key(PROC, "cmd");
        m.bus()
            .sent
            .drain(..)
            .filter(|(k, _)| *k == key)
            .filter_map(|(_, b)| Cmd::decode(&b))
            .collect()
    }

    fn run_of(p: &Poll) -> String {
        p.state["session"]["run"].as_str().unwrap().into()
    }

    #[test]
    fn it_subscribes_to_the_process_model_and_the_unit() {
        assert_eq!(
            cfg().prefixes(),
            [
                "app/proc/a1a1a1a1a1a1a1a1".to_string(),
                "app/unit/b2b2b2b2b2b2b2b2".to_string()
            ]
        );
    }

    #[test]
    fn no_process_model_is_disconnected_and_a_silent_one_is_an_alarm() {
        let mut m = mesh();
        assert_eq!(run_of(&m.poll(0.0)), "disconnected");
        up(&mut m, 10.0);
        let p = m.poll(20.0);
        assert_eq!(run_of(&p), "idle");
        assert_eq!(p.state["transport"], "mesh");
        // Heartbeats from the unit only: the process model goes stale.
        m.bus().inbox.push_back(info());
        let p = m.poll(10.0 + STALE_MS + 1.0);
        assert_eq!(run_of(&p), "disconnected");
        assert_eq!(p.state["session"]["alarms"][0]["code"], "MESH");
        assert!(p.events.iter().any(|e| e["kind"] == "state"));
    }

    #[test]
    fn a_silent_unit_is_an_alarm() {
        let mut m = mesh();
        up(&mut m, 0.0);
        m.bus()
            .inbox
            .push_back(proc_state(ProcRun::Idle, Some(UNIT), ""));
        let p = m.poll(STALE_MS + 1.0);
        assert_eq!(run_of(&p), "alarm");
        assert_eq!(p.state["session"]["alarms"][0]["code"], "UNIT");
    }

    #[test]
    fn the_process_model_is_assigned_the_unit_when_it_is_not_busy() {
        let mut m = mesh();
        m.bus().inbox.push_back(proc_state(ProcRun::Idle, None, ""));
        m.poll(0.0);
        assert_eq!(cmds(&mut m), vec![Cmd::Assign(UNIT)]);
        // Not again within a second.
        m.bus().inbox.push_back(proc_state(ProcRun::Idle, None, ""));
        m.poll(500.0);
        assert!(cmds(&mut m).is_empty());
        // Another unit, and busy: left alone.
        m.bus()
            .inbox
            .push_back(proc_state(ProcRun::Running, Some([7; 8]), ""));
        m.poll(2000.0);
        assert!(cmds(&mut m).is_empty());
        // Assigned: nothing more.
        m.bus()
            .inbox
            .push_back(proc_state(ProcRun::Idle, Some(UNIT), ""));
        m.poll(4000.0);
        assert!(cmds(&mut m).is_empty());
    }

    #[test]
    fn commands_become_process_commands() {
        let mut m = mesh();
        up(&mut m, 0.0);
        cmds(&mut m);
        m.command(Command::Home);
        assert_eq!(cmds(&mut m), vec![Cmd::Reference]);
        m.command(Command::Hold);
        m.command(Command::Resume);
        m.command(Command::Abort);
        assert_eq!(cmds(&mut m), vec![Cmd::Hold, Cmd::Resume, Cmd::Stop]);
        // After a failure, Home clears it first.
        m.bus()
            .inbox
            .push_back(proc_state(ProcRun::Failed, Some(UNIT), "stopped"));
        let p = m.poll(10.0);
        assert_eq!(run_of(&p), "alarm");
        assert_eq!(p.state["session"]["alarms"][0]["message"], "stopped");
        assert!(p.events.iter().any(|e| e["kind"] == "alarm"));
        m.command(Command::Home);
        assert_eq!(cmds(&mut m), vec![Cmd::Clear, Cmd::Reference]);
    }

    #[test]
    fn a_program_is_uploaded_and_run_with_the_configured_belt() {
        let mut m = mesh();
        up(&mut m, 0.0);
        m.command(Command::Run);
        assert!(cmds(&mut m).is_empty(), "nothing loaded");
        let p = Program::parse(&"G0 X10 Z1\n".repeat(80));
        m.load(&p);
        let key = own_key(PROC, "program");
        let chunks: Vec<Vec<u8>> = m
            .bus()
            .sent
            .iter()
            .filter(|(k, _)| *k == key)
            .map(|(_, b)| b.clone())
            .collect();
        let text: String = p.lines.iter().map(|l| format!("{l}\n")).collect();
        assert_eq!(chunks, program_chunks(PROGRAM_ID, &text));
        m.command(Command::Run);
        assert_eq!(
            cmds(&mut m),
            vec![Cmd::Run {
                program: PROGRAM_ID,
                belt: 2
            }]
        );
        let p = m.poll(5.0);
        assert!(p.events.iter().any(|e| e["code"] == "NO-PROGRAM"));
    }

    #[test]
    fn run_waits_for_the_assignment() {
        let mut m = mesh();
        m.bus().inbox.push_back(info());
        m.bus().inbox.push_back(proc_state(ProcRun::Idle, None, ""));
        m.poll(0.0);
        m.load(&Program::parse("M30\n"));
        cmds(&mut m);
        m.command(Command::Run);
        assert!(cmds(&mut m).is_empty());
    }

    #[test]
    fn what_the_process_model_cannot_do_is_refused_visibly() {
        let mut m = mesh();
        up(&mut m, 0.0);
        cmds(&mut m);
        m.command(Command::Mdi {
            text: "G0 X0".into(),
        });
        m.command(Command::DryRun { on: true });
        m.command(Command::DryRun { on: false });
        assert!(cmds(&mut m).is_empty());
        assert!(!m.fault("estop"));
        let p = m.poll(1.0);
        let refused = p
            .events
            .iter()
            .filter(|e| e["code"] == "UNSUPPORTED")
            .count();
        assert_eq!(refused, 2);
    }

    #[test]
    fn the_dro_and_spindle_come_from_the_control_modules() {
        let mut m = mesh();
        for s in [
            cm(
                0,
                CmState::Axis {
                    referenced: true,
                    position_um: 12_500,
                },
            ),
            cm(
                1,
                CmState::Axis {
                    referenced: true,
                    position_um: -60_000,
                },
            ),
            cm(
                2,
                CmState::Spindle {
                    commanded: -800,
                    actual: -790,
                },
            ),
            cm(
                3,
                CmState::Turret {
                    station: 4,
                    ready: true,
                },
            ),
            cm(
                4,
                CmState::Safety {
                    chain_closed: true,
                    stopped: false,
                },
            ),
        ] {
            m.bus().inbox.push_back(s);
        }
        up(&mut m, 0.0);
        let s = &m.poll(1.0).state["session"];
        assert_eq!(s["run"], "idle");
        assert_eq!(s["dro"]["xRadiusMm"], 12.5);
        assert_eq!(s["dro"]["xDiameterMm"], 25.0);
        assert_eq!(s["dro"]["zMm"], -60.0);
        assert_eq!(s["dro"]["zWorkMm"], -10.0);
        assert_eq!(s["dro"]["homed"], true);
        assert_eq!(s["spindleRpm"], 790.0);
        assert_eq!(s["spindleOn"], true);
        assert_eq!(s["tool"], 4);
    }

    #[test]
    fn an_open_e_stop_chain_is_an_alarm() {
        let mut m = mesh();
        m.bus().inbox.push_back(cm(
            4,
            CmState::Safety {
                chain_closed: false,
                stopped: true,
            },
        ));
        up(&mut m, 0.0);
        let s = &m.poll(1.0).state["session"];
        assert_eq!(s["run"], "alarm");
        assert_eq!(s["alarms"][0]["code"], "ESTOP");
    }

    #[test]
    fn a_bus_that_refuses_raises_an_alarm_and_loads_nothing() {
        let mut m = mesh();
        up(&mut m, 0.0);
        m.bus().refuse = true;
        m.load(&Program::parse("M30\n"));
        m.command(Command::Home);
        m.bus().refuse = false;
        m.command(Command::Run);
        let p = m.poll(1.0);
        let codes: Vec<&str> = p.events.iter().filter_map(|e| e["code"].as_str()).collect();
        assert_eq!(codes, ["MESH", "MESH", "NO-PROGRAM"]);
    }

    #[test]
    fn other_nodes_publications_are_ignored() {
        let mut m = mesh();
        up(&mut m, 0.0);
        let other = ProcState {
            run: ProcRun::Running,
            program: 1,
            op: 0,
            ops: 1,
            message: String::new(),
            unit: Some(UNIT),
        };
        m.bus()
            .inbox
            .push_back((own_key([9; 8], "state"), other.encode()));
        m.bus()
            .inbox
            .push_back((own_key(PROC, "state"), vec![1, 2, 3]));
        assert_eq!(run_of(&m.poll(1.0)), "idle");
    }

    #[test]
    fn nothing_starts_while_the_process_model_is_silent() {
        let mut m = mesh();
        m.load(&Program::parse("M30\n"));
        m.command(Command::Home);
        m.command(Command::Run);
        m.command(Command::Resume);
        m.command(Command::Hold);
        m.command(Command::Abort);
        assert_eq!(cmds(&mut m), vec![Cmd::Hold, Cmd::Stop]);
        let p = m.poll(1.0);
        assert_eq!(p.events.iter().filter(|e| e["code"] == "MESH").count(), 3);
    }

    #[test]
    fn the_pump_runs_it() {
        use crate::machine::Machine;
        use std::sync::{Arc, Mutex};

        /// A shared fake, so the test can feed the pump thread's bus.
        #[derive(Clone, Default)]
        struct Shared(Arc<Mutex<Fake>>);
        impl Bus for Shared {
            fn publish(&mut self, key: &str, body: &[u8]) -> Result<(), String> {
                self.0.lock().unwrap().publish(key, body)
            }
            fn try_recv(&mut self) -> Option<(String, Vec<u8>)> {
                self.0.lock().unwrap().try_recv()
            }
        }
        let bus = Shared::default();
        {
            let mut f = bus.0.lock().unwrap();
            f.inbox.push_back(info());
            f.inbox.push_back(proc_state(ProcRun::Idle, Some(UNIT), ""));
        }
        let b = bus.clone();
        let m = Machine::spawn_with(move || Box::new(MeshTransport::new(b, cfg())));
        for _ in 0..200 {
            if m.run_state() == "idle" {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(m.run_state(), "idle");
        m.command(Command::Home);
        for _ in 0..200 {
            if bus.0.lock().unwrap().sent.iter().any(|(_, b)| b == &[2]) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("Home was not sent as Reference");
    }
}
