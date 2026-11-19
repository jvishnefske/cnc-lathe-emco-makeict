//! The process model on the ABI: subscriptions, and [`Process`]'s
//! publications.
//!
//! It subscribes to its own command and program keys at start, and to the
//! assigned unit's reports, control modules and info once it has one. A
//! tick every 100 ms republishes its state when it changed, and once a
//! second (`STATE_HEARTBEAT_MS`) when it did not.

use cnc_model::proc::STATE_HEARTBEAT_MS;
use mw_payload::{Payload, Slot};

use crate::process::Process;

/// The site's work zero, machine mm: the LinuxCNC simulator's G54 (doc 50).
const G54: (f64, f64) = (0.0, -50.0);
/// The tick, ms.
const TICK_MS: u32 = 100;

struct App {
    model: Process,
    /// `(slot, key)` for every subscription.
    keys: Vec<(Slot, String)>,
    unit: Option<[u8; 8]>,
    /// Ticks since the last heartbeat.
    ticks: u32,
}

impl App {
    fn subscribe(&mut self, key: String) {
        if self.keys.iter().any(|(_, k)| *k == key) {
            return;
        }
        if let Ok(slot) = mw_payload::subscribe(&key) {
            self.keys.push((slot, key));
        }
    }

    fn flush(&mut self) {
        if self.model.unit() != self.unit {
            self.unit = self.model.unit();
            for k in self.model.unit_keys() {
                self.subscribe(k);
            }
        }
        for (k, body) in self.model.take() {
            let _ = mw_payload::publish(&k, &body);
        }
    }
}

impl Payload for App {
    fn init() -> Self {
        let node = mw_payload::serve::node_id()
            .map(|n| n.0)
            .unwrap_or_default();
        let mut app = App {
            model: Process::new(node, G54),
            keys: Vec::new(),
            unit: None,
            ticks: 0,
        };
        for k in app.model.own_keys() {
            app.subscribe(k);
        }
        let _ = mw_payload::set_tick(TICK_MS);
        mw_payload::log("cnc-process: up");
        app.flush();
        app
    }

    fn on_message(&mut self, slot: Slot, data: &[u8]) {
        if let Some((_, key)) = self.keys.iter().find(|(s, _)| *s == slot) {
            let key = key.clone();
            self.model.publication(&key, data);
            self.flush();
        }
    }

    fn tick(&mut self, _now_ms: u32) {
        self.ticks += 1;
        if self.ticks * TICK_MS >= STATE_HEARTBEAT_MS {
            self.ticks = 0;
            self.model.heartbeat();
        }
        self.flush();
    }
}

mw_payload::payload!(App);
