//! Thin `wasm-bindgen` layer over the native core. Compiled only for the
//! `wasm32` target so native `cargo test` never needs wasm-bindgen.
//!
//! JSON-ish values cross the boundary via `serde_wasm_bindgen` with the
//! JSON-compatible serializer, so JS sees plain objects with the camelCase
//! field names promised in `sender/INTERFACE.md`.

use wasm_bindgen::prelude::*;

use crate::geom::StockSpec;
use crate::profile::MachineProfile;
use crate::session::Command;

fn to_js<T: serde::Serialize>(v: &T) -> JsValue {
    let ser = serde_wasm_bindgen::Serializer::json_compatible();
    v.serialize(&ser).unwrap_or(JsValue::NULL)
}

fn profile_from_js(profile: JsValue) -> MachineProfile {
    serde_wasm_bindgen::from_value(profile).unwrap_or_else(|_| MachineProfile::pc_turn_50_default())
}

#[wasm_bindgen]
pub struct Program {
    inner: crate::gcode::Program,
}

#[wasm_bindgen]
impl Program {
    /// Parse G-code text. Never throws — syntax problems come back as diagnostics.
    pub fn parse(text: &str) -> Program {
        Program { inner: crate::gcode::Program::parse(text) }
    }

    pub fn line_count(&self) -> u32 {
        self.inner.line_count()
    }

    /// -> Diagnostic[]
    pub fn diagnostics(&self) -> JsValue {
        to_js(&self.inner.diagnostics)
    }

    /// -> Backplot
    pub fn backplot(&self, stock: JsValue /* StockSpec */) -> JsValue {
        let stock: Option<StockSpec> = serde_wasm_bindgen::from_value(stock).ok();
        to_js(&crate::geom::backplot(&self.inner, stock))
    }

    /// Source text of one 0-based line, for the program pane.
    pub fn line(&self, index: u32) -> String {
        self.inner.line(index)
    }
}

#[wasm_bindgen]
pub struct VirtualMachine {
    inner: crate::machine::VirtualMachine,
}

#[wasm_bindgen]
impl VirtualMachine {
    #[wasm_bindgen(constructor)]
    pub fn new(profile: JsValue /* MachineProfile */) -> VirtualMachine {
        VirtualMachine { inner: crate::machine::VirtualMachine::new(profile_from_js(profile)) }
    }

    /// Advance the simulation to `now_ms` and consume bytes the host sent.
    /// Returns bytes the machine emits back.
    pub fn step(&mut self, now_ms: f64, host_tx: &[u8]) -> Vec<u8> {
        self.inner.step(now_ms, host_tx)
    }

    /// -> Dro
    pub fn dro(&self) -> JsValue {
        to_js(&self.inner.dro())
    }

    /// Inject a fault: "estop" | "limit_x" | "limit_z" | "turret_jam"
    pub fn inject_fault(&mut self, kind: &str) {
        self.inner.inject_fault(kind);
    }
}

#[wasm_bindgen]
pub struct Session {
    inner: crate::session::Session,
}

#[wasm_bindgen]
impl Session {
    #[wasm_bindgen(constructor)]
    pub fn new(profile: JsValue /* MachineProfile */) -> Session {
        Session { inner: crate::session::Session::new(profile_from_js(profile)) }
    }

    pub fn load_program(&mut self, program: &Program) {
        self.inner.load_program(&program.inner);
    }

    /// The pump. `rx` is whatever arrived from the transport since the last
    /// call (may be empty). -> Step { tx, events }
    pub fn tick(&mut self, now_ms: f64, rx: &[u8]) -> JsValue {
        to_js(&self.inner.tick(now_ms, rx))
    }

    /// Queue an operator command; effects appear on the next tick.
    /// Unknown/malformed commands are ignored.
    pub fn command(&mut self, cmd: JsValue /* Command */) {
        if let Ok(c) = serde_wasm_bindgen::from_value::<Command>(cmd) {
            self.inner.command(c);
        }
    }

    /// -> SessionState
    pub fn state(&self) -> JsValue {
        to_js(&self.inner.state())
    }

    /// Extra (beyond the contract): report the hardware CTS line state when
    /// the transport exposes it. Defaults to asserted when never called.
    pub fn set_cts(&mut self, asserted: bool) {
        self.inner.set_cts(asserted);
    }
}
