//! The `emco-kd240` service as a [`MachineLink`](crate::link::MachineLink):
//! status and events from SSE `GET /api/events`, commands as `POST`s
//! (kd240/README.md §API).
//!
//! Everything that turns the service's JSON into [`Status`] and requests into
//! HTTP calls is here and pure, so it is tested natively; the browser side
//! ([`web`], wasm32 only) only moves bytes.
//!
//! What the service's machine (`sender/core`'s `Session` against its
//! `VirtualMachine`) does not report, and how this adapter fills the gap:
//!
//! * **E-stop and machine-on.** No such flags. E-stop is an `ESTOP` alarm,
//!   latched here until the session is reset (it passes through
//!   `disconnected` after an abort, which is also what clears the virtual
//!   machine's alarm), because the session forgets the alarm when Home is
//!   pressed while the machine still refuses to move. The machine is "on"
//!   when not in E-stop and not `disconnected`.
//! * **Homed.** The session keeps its `homed` flag through an E-stop and a
//!   failed home, as LinuxCNC does. The adapter instead tracks the last
//!   reference run from the `core` state events: entering `homing` clears
//!   it, `homing` -> `idle` (the only way the session completes a home) sets
//!   it. A `lagged` event (missed events) falls back to the session's flag.
//! * **Diameter mode (G7/G8)** is not in the session's `modal`. It is
//!   followed from the blocks the session sends (`sent` events): G7/G8 words
//!   change it, a program run starts from the session's default (G7). Until
//!   a block has said so, it is an assumption, and [`Status`] says that.
//! * **Threading**: the session's `G33` motion mode, or a G33/G33.1/G76
//!   block in flight.
//! * **Feed override, tool table, continuous jog**: not offered; see
//!   [`CAPABILITIES`].

use crate::link::*;
use emco_sender_core::profile::MachineProfile;
use serde_json::{json, Value};

/// What the kd240 service's virtual machine can do.
pub const CAPABILITIES: Capabilities = Capabilities {
    feed_override: false,
    tool_table: false,
    continuous_jog: false,
    training_faults: true,
    abort_resets: true,
};

/// Turns the service's SSE payloads into [`Status`] and events.
#[derive(Clone, Debug)]
pub struct Decoder {
    /// From the reference-run events; `None` until known.
    referenced: Option<bool>,
    estop: bool,
    diameter_mode: bool,
    diameter_known: bool,
    /// A program block has been sent since the machine was last idle.
    in_program: bool,
    threading_block: bool,
    last: Option<Value>,
}

impl Default for Decoder {
    fn default() -> Self {
        Decoder {
            referenced: None,
            estop: false,
            // The session's (and the lathe's) default.
            diameter_mode: true,
            diameter_known: false,
            in_program: false,
            threading_block: false,
            last: None,
        }
    }
}

fn f(v: &Value) -> f64 {
    v.as_f64().unwrap_or(0.0)
}

fn line_of(v: &Value) -> Option<usize> {
    v.as_i64().filter(|l| *l >= 0).map(|l| l as usize)
}

/// G words with a plain number in one block, comments skipped.
fn g_words(text: &str) -> Vec<f64> {
    let mut out = Vec::new();
    let upper = text.to_uppercase();
    let code = upper.split(';').next().unwrap_or("");
    let mut chars = code.chars().peekable();
    let mut depth = 0;
    while let Some(c) = chars.next() {
        match c {
            '(' => depth += 1,
            ')' => depth = (depth - 1).max(0),
            'G' if depth == 0 => {
                while chars.peek().is_some_and(|c| c.is_whitespace()) {
                    chars.next();
                }
                let mut num = String::new();
                while let Some(&d) = chars.peek() {
                    if d.is_ascii_digit() || d == '.' {
                        num.push(d);
                        chars.next();
                    } else {
                        break;
                    }
                }
                if let Ok(v) = num.parse() {
                    out.push(v);
                }
            }
            _ => {}
        }
    }
    out
}

impl Decoder {
    /// A `state` event (or `GET /api/state`): `{session, sim, transport}`.
    pub fn state(&mut self, v: &Value) -> Status {
        self.last = Some(v.clone());
        self.derive(true)
    }

    /// `fresh`: `last` just arrived. A re-derivation (after an event) must
    /// not re-latch an E-stop from a snapshot older than the reset.
    fn derive(&mut self, fresh: bool) -> Status {
        let v = self.last.clone().unwrap_or(Value::Null);
        let s = &v["session"];
        let run = RunState::from_name(s["run"].as_str().unwrap_or("disconnected"));
        let alarms: Vec<Alarm> = s["alarms"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|a| Alarm {
                        code: a["code"].as_str().unwrap_or("").to_string(),
                        message: a["message"].as_str().unwrap_or("").to_string(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        if fresh && alarms.iter().any(|a| a.code == "ESTOP") {
            self.estop = true;
        }
        if run == RunState::Disconnected {
            // The session passes through here on abort, which also resets
            // the virtual machine (its alarms clear).
            self.estop = false;
        }
        if !run.busy() {
            self.in_program = false;
            self.threading_block = false;
        }
        let dro = &s["dro"];
        let session_homed = dro["homed"].as_bool().unwrap_or(false);
        if !session_homed {
            self.referenced = Some(false);
        }
        let referenced = self.referenced.unwrap_or(session_homed);
        let modal: Vec<String> = s["modal"]
            .as_array()
            .map(|m| {
                m.iter()
                    .filter_map(|c| c.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let sim = (!v["sim"].is_null()).then(|| (f(&v["sim"]["xRadiusMm"]), f(&v["sim"]["zMm"])));
        Status {
            machine_on: run != RunState::Disconnected && !self.estop,
            estop: self.estop,
            homed: vec![referenced && run != RunState::Homing],
            pos: Position {
                x: f(&dro["xRadiusMm"]),
                z: f(&dro["zMm"]),
                x_work: f(&dro["xWorkMm"]),
                z_work: f(&dro["zWorkMm"]),
                x_dtg: f(&dro["xDtgMm"]),
                z_dtg: f(&dro["zDtgMm"]),
            },
            sim,
            current_line: line_of(&s["currentLine"]),
            tool: s["tool"].as_u64().unwrap_or(0) as u32,
            spindle_rpm: f(&s["spindleRpm"]),
            spindle_on: s["spindleOn"].as_bool().unwrap_or(false),
            feed_mm_min: f(&s["feedMmMin"]),
            threading: modal.iter().any(|m| m == "G33") || self.threading_block,
            modal,
            diameter_mode: self.diameter_mode,
            diameter_mode_known: self.diameter_known,
            alarms,
            single_block: s["singleBlock"].as_bool().unwrap_or(false),
            dry_run: s["dryRun"].as_bool().unwrap_or(false),
            transport: v["transport"].as_str().unwrap_or("?").to_string(),
            run,
        }
    }

    /// A `core` event (a session `Event`). Returns what the screen should
    /// see: the event or error, and a re-derived status when the event
    /// changed what the adapter infers.
    pub fn core(&mut self, v: &Value) -> Vec<Incoming> {
        let mut out = Vec::new();
        let mut restatus = false;
        match v["kind"].as_str().unwrap_or("") {
            "sent" => {
                let line = line_of(&v["line"]);
                let text = v["text"].as_str().unwrap_or("").to_string();
                if line.is_some() && !self.in_program {
                    // A program run starts from a fresh modal state.
                    self.in_program = true;
                    self.diameter_mode = true;
                }
                let gs = g_words(&text);
                if gs.contains(&7.0) {
                    self.diameter_mode = true;
                    self.diameter_known = true;
                }
                if gs.contains(&8.0) {
                    self.diameter_mode = false;
                    self.diameter_known = true;
                }
                let threading = gs.iter().any(|g| [33.0, 33.1, 76.0].contains(g));
                restatus =
                    threading != self.threading_block || gs.contains(&7.0) || gs.contains(&8.0);
                self.threading_block = threading;
                out.push(Incoming::Event(MachineEvent::Sent { line, text }));
            }
            "acked" => out.push(Incoming::Event(MachineEvent::Acked {
                line: line_of(&v["line"]),
            })),
            "alarm" => {
                let code = v["code"].as_str().unwrap_or("").to_string();
                if code == "ESTOP" {
                    self.estop = true;
                    restatus = true;
                }
                let message = v["message"].as_str().unwrap_or("");
                out.push(Incoming::Error(ErrorMsg {
                    // "parse": an MDI line the session could not read.
                    kind: if code == "parse" {
                        ErrorKind::Operator
                    } else {
                        ErrorKind::Machine
                    },
                    text: format!("ALARM {code}: {message}"),
                }));
            }
            "state" => {
                let from = RunState::from_name(v["from"].as_str().unwrap_or(""));
                let to = RunState::from_name(v["to"].as_str().unwrap_or(""));
                if to == RunState::Homing {
                    self.referenced = Some(false);
                    restatus = true;
                }
                if from == RunState::Homing && to == RunState::Idle {
                    self.referenced = Some(true);
                    restatus = true;
                }
                if to == RunState::Disconnected {
                    self.estop = false;
                    restatus = true;
                }
                out.push(Incoming::Event(MachineEvent::State { from, to }));
            }
            // "raw" bytes and anything new: not the screen's business.
            _ => {}
        }
        if restatus && self.last.is_some() {
            out.push(Incoming::Status(Box::new(self.derive(false))));
        }
        out
    }

    /// A `lagged` event: `n` events were dropped.
    pub fn lagged(&mut self, n: u64) -> Vec<Incoming> {
        self.referenced = None;
        self.diameter_known = false;
        vec![Incoming::Event(MachineEvent::Lagged(n))]
    }

    pub fn diameter_mode(&self) -> bool {
        self.diameter_mode
    }
}

// -----------------------------------------------------------------------------
// Requests as HTTP calls
// -----------------------------------------------------------------------------

/// What a call's response means.
#[derive(Clone, Debug, PartialEq)]
pub enum CallKind {
    Command(String),
    Mdi(MdiTag),
    Fault(String),
    Profile,
    ListPrograms,
    ReadProgram(String),
    WriteProgram(String),
    DeleteProgram(String),
    LoadProgram(String),
    LoadedProgram,
}

/// One HTTP request.
#[derive(Clone, Debug, PartialEq)]
pub struct HttpCall {
    pub method: &'static str,
    pub path: String,
    pub body: Option<String>,
    pub content_type: Option<&'static str>,
    pub kind: CallKind,
}

impl HttpCall {
    fn get(path: String, kind: CallKind) -> HttpCall {
        HttpCall {
            method: "GET",
            path,
            body: None,
            content_type: None,
            kind,
        }
    }

    fn command(cmd: Value, label: &str) -> HttpCall {
        HttpCall {
            method: "POST",
            path: "/api/command".into(),
            body: Some(cmd.to_string()),
            content_type: Some("application/json"),
            kind: CallKind::Command(label.into()),
        }
    }

    /// True for calls that change something, which carry the token.
    pub fn changes(&self) -> bool {
        self.method != "GET"
    }
}

fn axis_name(a: Axis) -> &'static str {
    match a {
        Axis::X => "x",
        Axis::Z => "z",
    }
}

/// A `G10 L20 P1` line: the touch-off. `Some(Err)` for G10 forms the
/// service's machine would ignore (it applies `G10 L2` only, and warns and
/// skips the rest, which an operator would not see).
fn g10(line: &str, diameter_mode: bool) -> Option<Result<Vec<(Axis, f64)>, String>> {
    let upper = line.to_uppercase();
    let code = upper.split(['(', ';']).next().unwrap_or("");
    // Letters and plain numbers.
    let mut words: Vec<(char, f64)> = Vec::new();
    let mut chars = code.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_ascii_alphabetic() {
            let mut num = String::new();
            while chars.peek().is_some_and(|c| c.is_whitespace()) {
                chars.next();
            }
            while let Some(&d) = chars.peek() {
                if d.is_ascii_digit() || matches!(d, '.' | '-' | '+') {
                    num.push(d);
                    chars.next();
                } else {
                    break;
                }
            }
            if let Ok(v) = num.parse() {
                words.push((c, v));
            }
        }
    }
    let word = |l: char| words.iter().find(|(c, _)| *c == l).map(|w| w.1);
    if !words.contains(&('G', 10.0)) {
        return None;
    }
    match word('L') {
        Some(2.0) => None, // the machine does this one itself
        Some(20.0) => {
            if word('P').is_some_and(|p| p != 1.0) {
                return Some(Err("only G54 (P1) is kept by this machine".into()));
            }
            let mut out = Vec::new();
            if let Some(x) = word('X') {
                out.push((Axis::X, if diameter_mode { x / 2.0 } else { x }));
            }
            if let Some(z) = word('Z') {
                out.push((Axis::Z, z));
            }
            Some(Ok(out))
        }
        Some(l) if [1.0, 10.0, 11.0].contains(&l) => Some(Err(
            "this machine has no tool table: G10 L1/L10/L11 would be ignored".into(),
        )),
        _ => Some(Err(format!(
            "{line}: this machine applies G10 L2 and L20 only"
        ))),
    }
}

/// The HTTP calls for a request, or why the request cannot be made on
/// this link. `diameter_mode` is the decoder's current belief, for G10 L20
/// X words.
pub fn calls_for(req: &Request, diameter_mode: bool) -> Result<Vec<HttpCall>, String> {
    let one = |c| Ok(vec![c]);
    match req {
        Request::Run => one(HttpCall::command(json!({"kind": "run"}), "run")),
        Request::Hold => one(HttpCall::command(json!({"kind": "hold"}), "hold")),
        Request::Resume => one(HttpCall::command(json!({"kind": "resume"}), "resume")),
        Request::Abort => one(HttpCall::command(json!({"kind": "abort"}), "abort")),
        Request::Home => one(HttpCall::command(json!({"kind": "home"}), "home")),
        Request::SingleBlock(on) => one(HttpCall::command(
            json!({"kind": "singleBlock", "on": on}),
            "single block",
        )),
        Request::DryRun(on) => one(HttpCall::command(
            json!({"kind": "dryRun", "on": on}),
            "dry run",
        )),
        Request::Jog {
            axis,
            delta_mm,
            feed_mm_min,
        } => one(HttpCall::command(
            json!({"kind": "jog", "axis": axis_name(*axis), "deltaMm": delta_mm, "feedMmMin": feed_mm_min}),
            "jog",
        )),
        Request::Mdi { lines, tag } => {
            let mut offsets = Vec::new();
            let mut plain = Vec::new();
            for line in lines {
                match g10(line, diameter_mode) {
                    Some(Ok(o)) => offsets.extend(o),
                    Some(Err(e)) => return Err(e),
                    None => plain.push(line.clone()),
                }
            }
            if !offsets.is_empty() && !plain.is_empty() {
                // The session takes MDI only when idle, and an MDI makes it
                // busy, so a touch-off queued behind one would be dropped.
                return Err("send G10 L20 on its own line, not with other MDI".into());
            }
            if offsets.is_empty() {
                let mut c =
                    HttpCall::command(json!({"kind": "mdi", "text": plain.join("\n")}), "mdi");
                c.kind = CallKind::Mdi(*tag);
                return one(c);
            }
            Ok(offsets
                .into_iter()
                .map(|(axis, value)| {
                    let mut c = HttpCall::command(
                        json!({"kind": "setWorkOffset", "axis": axis_name(axis), "valueMm": value}),
                        "touch-off",
                    );
                    c.kind = CallKind::Mdi(*tag);
                    c
                })
                .collect())
        }
        Request::FeedOverride(_) => Err("this machine has no feed override".into()),
        Request::Fault(kind) => one(HttpCall {
            method: "POST",
            path: "/api/sim/fault".into(),
            body: Some(json!({ "kind": kind }).to_string()),
            content_type: Some("application/json"),
            kind: CallKind::Fault(kind.clone()),
        }),
        Request::Profile => one(HttpCall::get("/api/profile".into(), CallKind::Profile)),
        Request::ListPrograms => one(HttpCall::get(
            "/api/programs".into(),
            CallKind::ListPrograms,
        )),
        Request::ReadProgram(name) => one(HttpCall::get(
            format!("/api/programs/{name}"),
            CallKind::ReadProgram(name.clone()),
        )),
        Request::WriteProgram { name, text } => one(HttpCall {
            method: "PUT",
            path: format!("/api/programs/{name}"),
            body: Some(text.clone()),
            content_type: Some("text/plain; charset=utf-8"),
            kind: CallKind::WriteProgram(name.clone()),
        }),
        Request::DeleteProgram(name) => one(HttpCall {
            method: "DELETE",
            path: format!("/api/programs/{name}"),
            body: None,
            content_type: None,
            kind: CallKind::DeleteProgram(name.clone()),
        }),
        Request::LoadProgram(name) => one(HttpCall {
            method: "POST",
            path: format!("/api/programs/{name}/load"),
            body: None,
            content_type: None,
            kind: CallKind::LoadProgram(name.clone()),
        }),
        Request::LoadedProgram => one(HttpCall::get(
            "/api/program".into(),
            CallKind::LoadedProgram,
        )),
    }
}

/// True when `name` is a program name the service accepts (store.rs).
pub fn valid_program_name(name: &str) -> bool {
    let b = name.as_bytes();
    (1..=64).contains(&b.len())
        && b[0].is_ascii_alphanumeric()
        && b.iter()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
        && !name.contains("..")
}

fn diagnostics(doc: &Value) -> Vec<Diagnostic> {
    doc["diagnostics"]
        .as_array()
        .map(|d| {
            d.iter()
                .map(|d| Diagnostic {
                    line: d["line"].as_u64().unwrap_or(0) as usize,
                    column: d["column"].as_u64().unwrap_or(0) as usize,
                    error: d["severity"] == "error",
                    message: d["message"].as_str().unwrap_or("").to_string(),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// What a call's response means for the screen.
pub fn interpret(kind: &CallKind, status: u16, body: &str) -> Vec<Incoming> {
    let ok = (200..300).contains(&status);
    let json: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let what = match kind {
        CallKind::Command(l) => l.clone(),
        CallKind::Mdi(_) => "MDI".into(),
        CallKind::Fault(k) => format!("fault {k}"),
        CallKind::Profile => "profile".into(),
        CallKind::ListPrograms => "program list".into(),
        CallKind::ReadProgram(n) => format!("read {n}"),
        CallKind::WriteProgram(n) => format!("save {n}"),
        CallKind::DeleteProgram(n) => format!("delete {n}"),
        CallKind::LoadProgram(n) => format!("load {n}"),
        CallKind::LoadedProgram => "loaded program".into(),
    };
    // A load the parser refused still describes the program.
    if let (CallKind::LoadProgram(name), 422) = (kind, status) {
        return vec![
            Incoming::Reply(Reply::Loaded {
                name: name.clone(),
                loaded: false,
                diagnostics: diagnostics(&json),
            }),
            Incoming::Error(ErrorMsg {
                kind: ErrorKind::Operator,
                text: format!("{name}: the machine's parser found errors; it was not loaded"),
            }),
        ];
    }
    if !ok {
        let reason = json["error"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| {
                if status == 0 {
                    "no response".into()
                } else {
                    format!("HTTP {status}")
                }
            });
        return vec![Incoming::Error(ErrorMsg {
            kind: if status == 0 || status >= 500 {
                ErrorKind::Link
            } else {
                ErrorKind::Operator
            },
            text: format!("{what} refused ({status}): {reason}"),
        })];
    }
    let reply = match kind {
        CallKind::Command(l) => Reply::Accepted(l.clone()),
        CallKind::Mdi(tag) => Reply::MdiAccepted(*tag),
        CallKind::Fault(k) => Reply::Accepted(format!("fault {k}")),
        CallKind::Profile => match serde_json::from_value::<MachineProfile>(json) {
            Ok(p) => Reply::Profile(Box::new(Profile(p))),
            Err(e) => {
                return vec![Incoming::Error(ErrorMsg {
                    kind: ErrorKind::Link,
                    text: format!("profile unreadable: {e}"),
                })]
            }
        },
        CallKind::ListPrograms => Reply::Programs(
            json.as_array()
                .map(|a| {
                    a.iter()
                        .map(|p| ProgramInfo {
                            name: p["name"].as_str().unwrap_or("").to_string(),
                            bytes: p["bytes"].as_u64().unwrap_or(0),
                        })
                        .collect()
                })
                .unwrap_or_default(),
        ),
        CallKind::ReadProgram(name) => Reply::ProgramText {
            name: name.clone(),
            text: body.to_string(),
        },
        CallKind::WriteProgram(name) => Reply::Written(name.clone()),
        CallKind::DeleteProgram(name) => Reply::Deleted(name.clone()),
        CallKind::LoadProgram(name) => Reply::Loaded {
            name: name.clone(),
            loaded: true,
            diagnostics: diagnostics(&json),
        },
        CallKind::LoadedProgram => Reply::LoadedProgram(json["name"].as_str().map(str::to_string)),
    };
    vec![Incoming::Reply(reply)]
}

#[cfg(target_arch = "wasm32")]
pub mod web;

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(run: &str, homed: bool, alarms: Value) -> Value {
        json!({
            "session": {
                "run": run, "currentLine": -1, "tool": 1, "spindleRpm": 0.0, "spindleOn": false,
                "feedMmMin": 0.0, "modal": ["G21", "G90", "G95", "G00", "G54"], "alarms": alarms,
                "singleBlock": false, "dryRun": false,
                "dro": {"xRadiusMm": 48.0, "xDiameterMm": 96.0, "zMm": 0.0, "xWorkMm": 48.0,
                        "zWorkMm": 50.0, "xDtgMm": 0.0, "zDtgMm": 0.0, "homed": homed}
            },
            "sim": {"xRadiusMm": 48.0, "xDiameterMm": 96.0, "zMm": 0.0, "xWorkMm": 48.0,
                    "zWorkMm": 50.0, "xDtgMm": 0.0, "zDtgMm": 0.0, "homed": homed},
            "transport": "virtual"
        })
    }

    fn state_event(from: &str, to: &str) -> Value {
        json!({"kind": "state", "from": from, "to": to, "atMs": 1.0})
    }

    fn last_status(v: Vec<Incoming>) -> Status {
        v.into_iter()
            .rev()
            .find_map(|i| match i {
                Incoming::Status(s) => Some(*s),
                _ => None,
            })
            .expect("a status")
    }

    #[test]
    fn a_snapshot_decodes() {
        let mut d = Decoder::default();
        let s = d.state(&snapshot("idle", true, json!([])));
        assert_eq!(s.run, RunState::Idle);
        assert!(s.machine_on && !s.estop && s.all_homed());
        assert_eq!(s.pos.work_offset(), (0.0, -50.0));
        assert_eq!(s.sim, Some((48.0, 0.0)));
        assert_eq!(s.current_line, None);
        assert!(s.diameter_mode && !s.diameter_mode_known);
        assert_eq!(s.transport, "virtual");
    }

    #[test]
    fn estop_latches_until_the_session_is_reset() {
        let mut d = Decoder::default();
        d.state(&snapshot("idle", true, json!([])));
        let s = last_status(
            d.core(&json!({"kind": "alarm", "code": "ESTOP", "message": "emergency stop"})),
        );
        assert!(s.estop && !s.machine_on);
        // Home in alarm clears the session's alarms, not the machine's E-stop.
        let s = d.state(&snapshot(
            "alarm",
            true,
            json!([{"code": "NAK", "message": "machine in alarm state"}]),
        ));
        assert!(s.estop);
        // The reset's state event, re-derived over the stale E-stop snapshot.
        d.state(&snapshot(
            "alarm",
            true,
            json!([{"code": "ESTOP", "message": "emergency stop"}]),
        ));
        let s = last_status(d.core(&state_event("alarm", "disconnected")));
        assert!(!s.estop, "a stale snapshot must not re-latch the E-stop");
        let s = d.state(&snapshot("disconnected", true, json!([])));
        assert!(!s.estop && !s.machine_on);
        let s = d.state(&snapshot("idle", true, json!([])));
        assert!(s.machine_on);
    }

    #[test]
    fn homed_follows_reference_runs_not_the_sessions_flag() {
        let mut d = Decoder::default();
        assert!(d.state(&snapshot("idle", true, json!([]))).all_homed());
        // Homing starts: not homed, whatever the session's flag says.
        let s = last_status(d.core(&state_event("alarm", "homing")));
        assert!(!s.all_homed());
        // It fails: still not homed, though the session kept its flag.
        d.core(&state_event("homing", "alarm"));
        assert!(!d.state(&snapshot("alarm", true, json!([]))).all_homed());
        // A completed home.
        d.core(&state_event("alarm", "homing"));
        let s = last_status(d.core(&state_event("homing", "idle")));
        assert!(s.all_homed());
        // Missed events: back to the session's flag.
        d.lagged(3);
        assert!(d.state(&snapshot("idle", true, json!([]))).all_homed());
    }

    #[test]
    fn diameter_mode_and_threading_follow_sent_blocks() {
        let mut d = Decoder::default();
        d.state(&snapshot("idle", true, json!([])));
        let s = last_status(
            d.core(&json!({"kind": "sent", "line": -1, "text": "G8 (radius)", "atMs": 0})),
        );
        assert!(!s.diameter_mode && s.diameter_mode_known);
        // A program run starts in the session's default, G7.
        d.state(&snapshot("running", true, json!([])));
        d.core(&json!({"kind": "sent", "line": 0, "text": "G21 G90", "atMs": 0}));
        assert!(d.diameter_mode());
        let s = last_status(
            d.core(&json!({"kind": "sent", "line": 5, "text": "G76 P1 Z-10", "atMs": 0})),
        );
        assert!(s.threading);
        assert!(!d.state(&snapshot("idle", true, json!([]))).threading);
    }

    #[test]
    fn requests_become_the_documented_calls() {
        let c = calls_for(&Request::Run, true).unwrap();
        assert_eq!(c[0].path, "/api/command");
        assert_eq!(c[0].body.as_deref(), Some(r#"{"kind":"run"}"#));
        assert!(c[0].changes());
        let c = calls_for(
            &Request::Jog {
                axis: Axis::X,
                delta_mm: -0.1,
                feed_mm_min: 60.0,
            },
            true,
        )
        .unwrap();
        let body: Value = serde_json::from_str(c[0].body.as_deref().unwrap()).unwrap();
        assert_eq!(
            body,
            json!({"kind": "jog", "axis": "x", "deltaMm": -0.1, "feedMmMin": 60.0})
        );
        let c = calls_for(&Request::ReadProgram("a.nc".into()), true).unwrap();
        assert_eq!(
            (c[0].method, c[0].path.as_str(), c[0].changes()),
            ("GET", "/api/programs/a.nc", false)
        );
        assert!(calls_for(&Request::FeedOverride(25), true).is_err());
    }

    #[test]
    fn mdi_touch_off_becomes_set_work_offset() {
        let mdi = |l: &[&str], dia| {
            calls_for(
                &Request::Mdi {
                    lines: l.iter().map(|s| s.to_string()).collect(),
                    tag: MdiTag::Wizard,
                },
                dia,
            )
        };
        let c = mdi(&["G10 L20 P1 Z0"], true).unwrap();
        let body: Value = serde_json::from_str(c[0].body.as_deref().unwrap()).unwrap();
        assert_eq!(
            body,
            json!({"kind": "setWorkOffset", "axis": "z", "valueMm": 0.0})
        );
        assert_eq!(c[0].kind, CallKind::Mdi(MdiTag::Wizard));
        let c = mdi(&["g10 l20 p1 x20"], true).unwrap();
        assert!(
            c[0].body.as_deref().unwrap().contains(r#""valueMm":10.0"#),
            "diameter halves"
        );
        let c = mdi(&["M5", "G53 G0 X96"], true).unwrap();
        assert_eq!(c.len(), 1);
        assert!(c[0]
            .body
            .as_deref()
            .unwrap()
            .contains(r#""text":"M5\nG53 G0 X96""#));
        assert!(mdi(&["G10 L2 P1 Z-50"], true).unwrap()[0]
            .body
            .as_deref()
            .unwrap()
            .contains("mdi"));
        assert!(mdi(&["G8", "G10 L10 P1 X5.99", "G7", "G43"], true)
            .unwrap_err()
            .contains("tool table"));
        assert!(mdi(&["G10 L20 P2 Z0"], true).is_err());
        assert!(mdi(&["M5", "G10 L20 P1 Z0"], true).is_err());
    }

    #[test]
    fn responses_become_replies_and_errors() {
        let r = interpret(&CallKind::Command("run".into()), 202, "");
        assert_eq!(r, vec![Incoming::Reply(Reply::Accepted("run".into()))]);
        let r = interpret(
            &CallKind::Command("run".into()),
            401,
            r#"{"error":"missing or wrong bearer token"}"#,
        );
        assert!(
            matches!(&r[0], Incoming::Error(e) if e.kind == ErrorKind::Operator && e.text.contains("bearer"))
        );
        let doc = r#"{"name":"a","lines":[],"errors":1,"backplot":{},
                      "diagnostics":[{"line":2,"column":0,"severity":"error","message":"bad"}]}"#;
        let r = interpret(&CallKind::LoadProgram("a".into()), 422, doc);
        assert!(
            matches!(&r[0], Incoming::Reply(Reply::Loaded { loaded: false, diagnostics, .. })
            if diagnostics[0].line == 2 && diagnostics[0].error)
        );
        let r = interpret(
            &CallKind::ListPrograms,
            200,
            r#"[{"name":"a","bytes":3,"modified":1}]"#,
        );
        assert_eq!(
            r,
            vec![Incoming::Reply(Reply::Programs(vec![ProgramInfo {
                name: "a".into(),
                bytes: 3
            }]))]
        );
        let p = serde_json::to_string(&MachineProfile::pc_turn_50_default()).unwrap();
        assert!(
            matches!(&interpret(&CallKind::Profile, 200, &p)[0], Incoming::Reply(Reply::Profile(p)) if p.0.x_max_mm == 48.0)
        );
        assert_eq!(
            interpret(&CallKind::LoadedProgram, 200, "null"),
            vec![Incoming::Reply(Reply::LoadedProgram(None))]
        );
        assert!(
            matches!(&interpret(&CallKind::Profile, 0, "")[0], Incoming::Error(e) if e.kind == ErrorKind::Link)
        );
    }

    #[test]
    fn program_names_follow_the_store() {
        assert!(valid_program_name("tutorial-part.ngc"));
        assert!(!valid_program_name("../x"));
        assert!(!valid_program_name(".hidden"));
        assert!(!valid_program_name(""));
    }
}
