//! The machine pump: `sender/core`'s `Session` against its `VirtualMachine`.
//!
//! One thread owns both and pumps them every [`TICK`], as the browser
//! sender's rAF loop does: the bytes the session emits go to the virtual
//! machine, the machine's answer is fed to the session on the next tick.
//! Commands arrive over a channel; the state is published on a `watch`
//! (latest value wins) and the session's events on a `broadcast` (a slow
//! web client lags and is told so, it never stalls the pump).
//!
//! **There is only the virtual machine.** The RS-485 protocol is unknown
//! (`docs/30-serial-protocol.md`), so this service has no transport that
//! writes the machine bus; the dialect seam in `sender/core` is where a
//! recovered `emco-485` dialect would land, and a real transport would be
//! added here, behind a decision recorded in the docs, not before.

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

/// The handle the API holds.
#[derive(Clone)]
pub struct Machine {
    cmd: mpsc::Sender<Cmd>,
    pub state: watch::Receiver<Value>,
    pub events: broadcast::Sender<Value>,
}

impl Machine {
    /// Starts the pump thread.
    pub fn spawn(profile: MachineProfile) -> Machine {
        let (cmd, rx) = mpsc::channel();
        let (state_tx, state) = watch::channel(Value::Null);
        let (events, _) = broadcast::channel(1024);
        let ev = events.clone();
        std::thread::Builder::new()
            .name("machine-pump".into())
            .spawn(move || pump(profile, rx, state_tx, ev))
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
    profile: MachineProfile,
    rx: mpsc::Receiver<Cmd>,
    state: watch::Sender<Value>,
    events: broadcast::Sender<Value>,
) {
    let origin = Instant::now();
    let mut vm = VirtualMachine::new(profile.clone());
    let mut session = Session::new(profile);
    let mut from_machine: Vec<u8> = Vec::new();
    loop {
        loop {
            match rx.try_recv() {
                Ok(Cmd::Session(c)) => session.command(c),
                Ok(Cmd::Load(p)) => session.load_program(&p),
                Ok(Cmd::Fault(f)) => vm.inject_fault(f),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => return,
            }
        }
        let now = origin.elapsed().as_secs_f64() * 1000.0;
        let out = session.tick(now, &from_machine);
        from_machine = vm.step(now, &out.tx);
        for e in out.events {
            if let Ok(v) = serde_json::to_value(&e) {
                let _ = events.send(v);
            }
        }
        let snapshot = json!({
            "session": session.state(),
            // The simulator's own truth: where the carriage really is, which
            // differs from the session's predicted DRO after a stall.
            "sim": vm.dro(),
            "transport": "virtual",
        });
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
