//! The machine pump: a [`Transport`] driven on its own thread.
//!
//! One thread owns the transport and polls it every [`TICK`], as the
//! browser sender's rAF loop does. Commands arrive over a channel; the state
//! is published on a `watch` (latest value wins) and the transport's events
//! on a `broadcast` (a slow web client lags and is told so, it never stalls
//! the pump).
//!
//! A transport is whatever stands behind the operator's commands and the
//! state snapshot the UI draws. Two are planned (`docs/65-kd240-controller-plan.md`
//! §3 S4, and the design note in [`crate::linuxcnc`]):
//!
//! * [`VirtualTransport`]: `sender/core`'s `Session` streaming G-code to its
//!   `VirtualMachine`. The default, and the only one this service runs.
//! * `LinuxCncTransport` ([`crate::linuxcnc`]): LinuxCNC's status, command
//!   and error channels, for when LinuxCNC drives the axes (doc 65 D3 A).
//!   A skeleton tested against a fake LinuxCNC; nothing connects it yet.
//!
//! **No transport writes the machine's RS-485 bus.** That protocol is unknown
//! (`docs/30-serial-protocol.md`); the dialect seam in `sender/core` is where
//! a recovered `emco-485` dialect would land, behind a decision recorded in
//! the docs, not before.

use emco_sender_core::gcode::Program;
use emco_sender_core::machine::VirtualMachine;
use emco_sender_core::profile::MachineProfile;
use emco_sender_core::session::{Command, Session};
use serde_json::{json, Value};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, watch};

/// Pump period.
pub const TICK: Duration = Duration::from_millis(10);

/// The faults `VirtualMachine::inject_fault` knows.
pub const FAULTS: [&str; 4] = ["estop", "limit_x", "limit_z", "turret_jam"];

enum Cmd {
    Session(Command),
    Load(Box<Program>),
    Fault(&'static str),
}

/// What one [`Transport::poll`] produces.
#[derive(Debug, Default)]
pub struct Poll {
    /// The snapshot the UI draws: `{"session": SessionState, "transport": name, …}`.
    /// Every transport keeps `session` in `sender/core`'s `SessionState`
    /// shape, so the UI doesn't know which one it is talking to.
    pub state: Value,
    /// `sender/core` `Event`s, serialised.
    pub events: Vec<Value>,
}

/// What stands behind the operator's commands.
///
/// A transport lives on the pump thread and is built there (see
/// [`Machine::spawn_with`]), so it needs neither `Send` nor `Sync`:
/// `sender/core`'s `Session` is neither. `poll` is called every [`TICK`] and must not block for long: a
/// transport that talks to another process keeps its I/O non-blocking or on
/// its own thread.
pub trait Transport {
    /// Shown as the snapshot's `transport` (`"virtual"`, `"linuxcnc"`).
    fn name(&self) -> &'static str;
    fn command(&mut self, c: Command);
    fn load(&mut self, p: &Program);
    /// Injects a training fault from [`FAULTS`]; `false` where the transport
    /// has no such thing (a real controller's faults are real).
    fn fault(&mut self, kind: &'static str) -> bool;
    /// Advances the transport to `now_ms` (milliseconds since the pump
    /// started) and returns its state and the events since the last poll.
    fn poll(&mut self, now_ms: f64) -> Poll;
}

/// `sender/core`'s `Session` against its `VirtualMachine`: the bytes the
/// session emits go to the virtual machine, and the machine's answer is fed
/// to the session on the next poll.
pub struct VirtualTransport {
    vm: VirtualMachine,
    session: Session,
    from_machine: Vec<u8>,
}

impl VirtualTransport {
    pub fn new(profile: MachineProfile) -> Self {
        VirtualTransport {
            vm: VirtualMachine::new(profile.clone()),
            session: Session::new(profile),
            from_machine: Vec::new(),
        }
    }
}

impl Transport for VirtualTransport {
    fn name(&self) -> &'static str {
        "virtual"
    }

    fn command(&mut self, c: Command) {
        self.session.command(c);
    }

    fn load(&mut self, p: &Program) {
        self.session.load_program(p);
    }

    fn fault(&mut self, kind: &'static str) -> bool {
        self.vm.inject_fault(kind);
        true
    }

    fn poll(&mut self, now_ms: f64) -> Poll {
        let out = self.session.tick(now_ms, &self.from_machine);
        self.from_machine = self.vm.step(now_ms, &out.tx);
        Poll {
            state: json!({
                "session": self.session.state(),
                // The simulator's own truth: where the carriage really is,
                // which differs from the session's predicted DRO after a
                // stall.
                "sim": self.vm.dro(),
                "transport": self.name(),
            }),
            events: out
                .events
                .iter()
                .filter_map(|e| serde_json::to_value(e).ok())
                .collect(),
        }
    }
}

/// The handle the API holds.
#[derive(Clone)]
pub struct Machine {
    cmd: mpsc::Sender<Cmd>,
    pub state: watch::Receiver<Value>,
    pub events: broadcast::Sender<Value>,
}

impl Machine {
    /// Starts the pump thread on the virtual machine.
    pub fn spawn(profile: MachineProfile) -> Machine {
        Machine::spawn_with(move || Box::new(VirtualTransport::new(profile)))
    }

    /// Starts the pump thread on the transport `make` builds there.
    pub fn spawn_with<F>(make: F) -> Machine
    where
        F: FnOnce() -> Box<dyn Transport> + Send + 'static,
    {
        let (cmd, rx) = mpsc::channel();
        let (state_tx, state) = watch::channel(Value::Null);
        let (events, _) = broadcast::channel(1024);
        let ev = events.clone();
        std::thread::Builder::new()
            .name("machine-pump".into())
            .spawn(move || pump(make(), rx, state_tx, ev))
            .expect("spawn the machine pump");
        Machine { cmd, state, events }
    }

    pub fn command(&self, c: Command) {
        let _ = self.cmd.send(Cmd::Session(c));
    }

    pub fn load(&self, p: Program) {
        let _ = self.cmd.send(Cmd::Load(Box::new(p)));
    }

    /// Injects a training fault; `false` for an unknown kind.
    pub fn fault(&self, kind: &str) -> bool {
        match FAULTS.iter().find(|f| **f == kind) {
            Some(f) => {
                let _ = self.cmd.send(Cmd::Fault(f));
                true
            }
            None => false,
        }
    }

    /// The run state (`"idle"`, `"running"`, …) from the latest snapshot.
    pub fn run_state(&self) -> String {
        self.state.borrow()["session"]["run"]
            .as_str()
            .unwrap_or("disconnected")
            .to_owned()
    }

    /// True while a program is running or held.
    pub fn busy(&self) -> bool {
        matches!(
            self.run_state().as_str(),
            "running" | "hold" | "homing" | "toolchange"
        )
    }
}

fn pump(
    mut transport: Box<dyn Transport>,
    rx: mpsc::Receiver<Cmd>,
    state: watch::Sender<Value>,
    events: broadcast::Sender<Value>,
) {
    let origin = Instant::now();
    loop {
        loop {
            match rx.try_recv() {
                Ok(Cmd::Session(c)) => transport.command(c),
                Ok(Cmd::Load(p)) => transport.load(&p),
                Ok(Cmd::Fault(f)) => {
                    transport.fault(f);
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => return,
            }
        }
        let now = origin.elapsed().as_secs_f64() * 1000.0;
        let out = transport.poll(now);
        for e in out.events {
            let _ = events.send(e);
        }
        let snapshot = out.state;
        state.send_if_modified(|cur| {
            if *cur == snapshot {
                false
            } else {
                *cur = snapshot;
                true
            }
        });
        std::thread::sleep(TICK);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wait_for(m: &Machine, run: &str) -> bool {
        for _ in 0..500 {
            if m.run_state() == run {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    #[test]
    fn the_pump_runs_a_program_on_the_virtual_machine_and_reports_events() {
        let m = Machine::spawn(MachineProfile::pc_turn_50_default());
        let mut events = m.events.subscribe();
        assert!(wait_for(&m, "idle"), "{}", m.run_state());
        m.command(Command::Home);
        std::thread::sleep(Duration::from_millis(300));
        let p = Program::parse("G21 G90\nG00 X5 Z1\nG01 Z-1 F100\nM30\n");
        assert!(p
            .diagnostics
            .iter()
            .all(|d| !matches!(d.severity, emco_sender_core::gcode::Severity::Error)));
        m.load(p);
        m.command(Command::Run);
        let mut saw_sent = false;
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            while let Ok(e) = events.try_recv() {
                saw_sent |= e["kind"] == "sent";
            }
            if saw_sent && m.run_state() == "idle" {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(saw_sent, "no block was sent");
        assert_eq!(m.state.borrow()["transport"], "virtual");
    }

    #[test]
    fn unknown_faults_are_refused() {
        let m = Machine::spawn(MachineProfile::pc_turn_50_default());
        assert!(m.fault("estop"));
        assert!(!m.fault("rm -rf"));
        assert!(
            wait_for(&m, "alarm"),
            "estop should alarm, got {}",
            m.run_state()
        );
    }
}
