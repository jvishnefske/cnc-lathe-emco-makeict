//! The mesh transport against the process model itself: `cnc-process`'s
//! pure state machine (the code the process payload runs), carried over a
//! fake bus. What the UI sends must be what the process model takes, and
//! what it publishes must be what the UI shows.
#![forbid(unsafe_code)]

use cnc_model::proc::own_key;
use cnc_model::wire;
use cnc_process::process::Process;
use emco_kd240::machine::Transport;
use emco_kd240::mesh::{Bus, MeshConfig, MeshTransport, G54_MM};
use emco_sender_core::gcode::Program;
use emco_sender_core::session::Command;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

const PROC: [u8; 8] = [0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0x80];
const UNIT: [u8; 8] = [0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x01, 0x02];

/// Everything published, in order; the transport reads what it subscribes
/// to (its two prefixes), the process model reads what it subscribes to.
#[derive(Default)]
struct Wire {
    log: Vec<(String, Vec<u8>)>,
    to_ui: VecDeque<(String, Vec<u8>)>,
    to_proc: VecDeque<(String, Vec<u8>)>,
}

#[derive(Clone)]
struct UiBus(Rc<RefCell<Wire>>);

impl Bus for UiBus {
    fn publish(&mut self, key: &str, body: &[u8]) -> Result<(), String> {
        let mut w = self.0.borrow_mut();
        w.log.push((key.into(), body.into()));
        w.to_proc.push_back((key.into(), body.into()));
        Ok(())
    }
    fn try_recv(&mut self) -> Option<(String, Vec<u8>)> {
        self.0.borrow_mut().to_ui.pop_front()
    }
}

struct Rig {
    wire: Rc<RefCell<Wire>>,
    ui: MeshTransport<UiBus>,
    process: Process,
    prefixes: [String; 2],
    now: f64,
}

impl Rig {
    fn new() -> Rig {
        let cfg = MeshConfig {
            process: PROC,
            unit: UNIT,
            belt: 2,
        };
        let wire = Rc::new(RefCell::new(Wire::default()));
        let prefixes = cfg.prefixes();
        let ui = MeshTransport::new(UiBus(wire.clone()), cfg);
        Rig {
            wire,
            ui,
            process: Process::new(PROC, G54_MM),
            prefixes,
            now: 0.0,
        }
    }

    /// A publication from the process model or the unit, to whoever
    /// subscribes.
    fn node_publishes(&mut self, key: String, body: Vec<u8>) {
        let mut w = self.wire.borrow_mut();
        w.log.push((key.clone(), body.clone()));
        if self
            .prefixes
            .iter()
            .any(|p| key.starts_with(&format!("{p}/")))
        {
            w.to_ui.push_back((key.clone(), body.clone()));
        }
        // The process model hears its unit (once assigned).
        w.to_proc.push_back((key, body));
    }

    /// The unit's heartbeat: its info.
    fn unit_beat(&mut self) {
        let info = cnc_model::PhysicalConfig::pc_turn_50().limits().encode();
        self.node_publishes(wire::info_key(UNIT).unwrap().as_str().into(), info.to_vec());
    }

    /// Carries publications until nothing moves, then polls the UI.
    fn step(&mut self, ms: f64) -> serde_json::Value {
        self.now += ms;
        self.process.heartbeat();
        loop {
            let next = self.wire.borrow_mut().to_proc.pop_front();
            if let Some((k, b)) = &next {
                let (k, b) = (k.clone(), b.clone());
                self.process.publication(&k, &b);
            }
            let out = self.process.take();
            let idle = next.is_none() && out.is_empty();
            for (k, b) in out {
                self.node_publishes(k, b);
            }
            if idle {
                break;
            }
        }
        self.ui.poll(self.now).state
    }

    fn published(&self, key: &str) -> usize {
        self.wire
            .borrow()
            .log
            .iter()
            .filter(|(k, _)| k == key)
            .count()
    }
}

#[test]
fn the_ui_assigns_the_unit_and_references_through_the_process_model() {
    let mut r = Rig::new();
    r.unit_beat();
    assert_eq!(r.ui.poll(0.0).state["session"]["run"], "disconnected");
    let s = r.step(10.0);
    // The process model is up with no unit; the UI assigns it its unit.
    assert_eq!(s["mesh"]["processState"]["unit"], serde_json::Value::Null);
    r.step(10.0);
    assert_eq!(r.process.unit(), Some(UNIT));
    r.unit_beat();
    let s = r.step(10.0);
    assert_eq!(s["mesh"]["processState"]["unit"], "0a0b0c0d0e0f0102", "{s}");
    assert_eq!(s["session"]["run"], "idle");
    assert!(r.process.machine().is_some(), "it heard the unit's info");

    r.ui.command(Command::Home);
    let s = r.step(10.0);
    assert_eq!(s["session"]["run"], "homing");
    // The process model commanded the unit's reference phase.
    let reference = wire::phase_key(UNIT, "motion", "reference").unwrap();
    assert!(r.published(reference.as_str()) > 0);
}

#[test]
fn a_loaded_program_reaches_the_process_model_and_its_gate_answers() {
    let mut r = Rig::new();
    r.unit_beat();
    r.step(10.0);
    r.step(10.0);
    r.unit_beat();
    r.step(10.0);
    r.ui.load(&Program::parse("G21 G90\nG00 X10 Z1\nG01 Z-5 F100\nM30\n"));
    r.ui.command(Command::Run);
    let s = r.step(10.0);
    // Not referenced: the process model's gate refuses, and the UI shows
    // its reason as an alarm.
    assert_eq!(s["session"]["run"], "alarm", "{s}");
    let msg = s["session"]["alarms"][0]["message"].as_str().unwrap();
    assert!(msg.contains("not referenced"), "{msg}");
    assert!(r.published(&own_key(PROC, "program")) >= 1);
    // Home clears the failure and references.
    r.ui.command(Command::Home);
    let s = r.step(10.0);
    assert_eq!(s["session"]["run"], "homing", "{s}");
}
