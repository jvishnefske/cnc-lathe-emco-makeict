//! `emco-rules`: the decisions behind the emco_lathe screen, with no UI, no
//! I/O and no clock in them.
//!
//! This is a port of
//! `linuxcnc/sim/emco-pcturn50/qtvcp/screens/emco_lathe/emco_rules.py`, and
//! that file stays the reference: every function here answers what its
//! Python namesake answers, on the same input, down to the line numbers and
//! the message text. Its 25 unit tests are ported one for one in
//! `tests/emco_rules.rs`. Each rule cites the document that asks for it, so a
//! disagreement between screen and docs can be settled by reading one or the
//! other.
//!
//! Where the port departs from the Python, on purpose or by necessity, the
//! item says so under "Port note". In summary:
//!
//! * A belt is a [`Belt`], which can only hold 1, 2 or 3. Python raises
//!   `KeyError` when a function is called with another number; here such a
//!   belt cannot be made ([`Belt::new`] returns `None`).
//! * [`touchoff_tool_x`] returns `Err` where Python raises `ValueError`.
//! * [`ProvenStore`] keeps the proven hashes in memory and converts to and
//!   from the JSON text Python writes. Reading and writing the file (or, in a
//!   browser, `localStorage`) is the caller's job, because this crate does no
//!   I/O.
//! * The preflight's line scanner treats only ASCII `0`-`9` as digits. The
//!   Python regex's `\d` also takes other Unicode decimal digits (and
//!   `float()` converts them). G-code is ASCII, so this changes nothing in
//!   practice.
//!
//! [`screen`] holds the decisions the QtVCP *handler* makes around these
//! rules (the touch-off wizard's steps, the first-run feed, when a run
//! proves a program, what to do about a spindle violation). They come from
//! `emco_lathe_handler.py`, not from `emco_rules.py`, and have tests of their
//! own.
#![forbid(unsafe_code)]

pub mod py;
pub mod screen;

use std::collections::BTreeMap;

// -----------------------------------------------------------------------------
// Numbers the INI cannot express
// -----------------------------------------------------------------------------

/// Belt ranges, rpm: three V-belt positions, moved by hand with the power
/// off. docs/10-machine-reference.md §4, docs/40-operator-guide.md §6.
/// \[MANUAL\]
pub const BELT_RANGES: [(u8, (u32, u32)); 3] =
    [(1, (130, 1300)), (2, (200, 2000)), (3, (300, 3000))];

/// Threading must stay at or below 400 rpm whatever the belt (doc 40 §6).
/// \[MANUAL\]
pub const THREADING_MAX_RPM: u32 = 400;

/// A fitted belt position: 1, 2 or 3.
///
/// Port note: Python passes a plain `int` and raises `KeyError` on a number
/// that is not a belt; here such a belt cannot be constructed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Belt(u8);

impl Belt {
    /// `None` unless `n` is 1, 2 or 3.
    pub fn new(n: u8) -> Option<Belt> {
        BELT_RANGES.iter().any(|(b, _)| *b == n).then_some(Belt(n))
    }

    pub fn number(self) -> u8 {
        self.0
    }

    /// Every belt, in order.
    pub fn all() -> [Belt; 3] {
        [Belt(1), Belt(2), Belt(3)]
    }
}

/// Position 2 "covers most work on this machine" (doc 40 §6).
pub const DEFAULT_BELT: Belt = Belt(2);

/// What the rules need to know about the machine, read from the INI by the
/// QtVCP handler (and from the machine profile by the web screen).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Machine {
    /// `[JOINT_0]HOME`, radius: the fully retracted X.
    pub x_retract: f64,
    /// `[JOINT_1]HOME`: where programs should finish.
    pub z_park: f64,
    /// `[EMCIO]TOOL_CHANGE_POSITION` is set.
    pub tool_change_retracts: bool,
}

// -----------------------------------------------------------------------------
// Reference gate — doc 40 §5
// -----------------------------------------------------------------------------

/// Has the machine been referenced since it last lost its position?
///
/// The real lathe has no absolute position memory, so the docs require a
/// re-reference after every power-on, every E-stop and every stall. LinuxCNC
/// keeps its `homed` flags across an E-stop, so trusting them would teach
/// exactly the habit the docs warn against. The gate therefore remembers
/// E-stops itself: homed flags only count once they have gone from
/// not-all-homed to all-homed *after* the most recent E-stop.
#[derive(Clone, Debug)]
pub struct ReferenceGate {
    estop_seen: bool,
    was_all_homed: bool,
}

impl Default for ReferenceGate {
    fn default() -> Self {
        Self::new()
    }
}

impl ReferenceGate {
    pub fn new() -> ReferenceGate {
        ReferenceGate {
            // power-on counts as having lost position
            estop_seen: true,
            was_all_homed: false,
        }
    }

    /// True from an E-stop (or power-on) until the next reference run.
    pub fn position_lost(&self) -> bool {
        self.estop_seen
    }

    /// Feeds the gate what the machine reports; `None` when satisfied, else
    /// what the operator must do next.
    pub fn update(
        &mut self,
        estop: bool,
        machine_on: bool,
        homed: &[bool],
    ) -> Option<&'static str> {
        // Python: `bool(homed) and all(homed)`: no joints is not homed.
        let all_homed = !homed.is_empty() && homed.iter().all(|h| *h);
        if estop {
            self.estop_seen = true;
        } else if all_homed && !self.was_all_homed {
            // A fresh transition into all-homed: a real reference run.
            self.estop_seen = false;
        }
        self.was_all_homed = all_homed;
        self.reason(estop, machine_on, all_homed)
    }

    /// `None` when satisfied, else what the operator must do next.
    pub fn reason(&self, estop: bool, machine_on: bool, all_homed: bool) -> Option<&'static str> {
        if estop {
            return Some("E-STOP — reset, then reference");
        }
        if !machine_on {
            return Some("Machine off — switch on, then reference");
        }
        if self.estop_seen && all_homed {
            return Some("Position lost at E-stop — Home All again");
        }
        if !all_homed {
            return Some("Not referenced — Home All first");
        }
        None
    }
}

// -----------------------------------------------------------------------------
// Belt and spindle — doc 40 §6
// -----------------------------------------------------------------------------

/// `(lo, hi)` rpm for a belt.
pub fn belt_range(belt: Belt) -> (u32, u32) {
    BELT_RANGES
        .iter()
        .find(|(b, _)| *b == belt.0)
        .map(|(_, r)| *r)
        .expect("a Belt is always in BELT_RANGES")
}

/// `rpm` limited to what `belt` can turn.
pub fn clamp_rpm(belt: Belt, rpm: f64) -> f64 {
    let (lo, hi) = belt_range(belt);
    // Python: max(lo, min(hi, rpm)). Spelled out rather than f64::clamp so a
    // NaN behaves as Python's min/max would (they keep the first argument
    // when a comparison is false): min(hi, nan) is hi, then max(lo, hi) is hi.
    let inner = if rpm < f64::from(hi) {
        rpm
    } else {
        f64::from(hi)
    };
    if inner > f64::from(lo) {
        inner
    } else {
        f64::from(lo)
    }
}

/// `None` if `rpm` is acceptable on `belt`, else a reason to stop.
///
/// `rpm == 0` is a stopped spindle and always acceptable. Negative is M4.
pub fn spindle_violation(belt: Belt, rpm: f64, threading: bool) -> Option<String> {
    let rpm = rpm.abs();
    if rpm == 0.0 {
        return None;
    }
    if threading && rpm > f64::from(THREADING_MAX_RPM) {
        return Some(format!(
            "Threading at {} rpm — the limit is {} rpm (doc 40 §6)",
            py::fixed(rpm, 0),
            THREADING_MAX_RPM
        ));
    }
    let (lo, hi) = belt_range(belt);
    if !(f64::from(lo) <= rpm && rpm <= f64::from(hi)) {
        return Some(format!(
            "{} rpm is outside belt {} ({}–{} rpm) — change the belt or the S word",
            py::fixed(rpm, 0),
            belt.0,
            lo,
            hi
        ));
    }
    None
}

// -----------------------------------------------------------------------------
// Preflight — the proving ladder, doc 40 §10 and house style, doc 50 §8
// -----------------------------------------------------------------------------

/// How serious a [`Finding`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Severity {
    /// Blocks cycle start.
    Error,
    /// Shown.
    Warning,
}

impl Severity {
    /// `"error"` or `"warning"`, Python's `ERROR` and `WARNING`.
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
        }
    }
}

/// One thing the preflight found.
#[derive(Clone, Debug, PartialEq)]
pub struct Finding {
    /// 1-based, as the editor shows it.
    pub line: usize,
    pub severity: Severity,
    pub message: String,
}

impl Finding {
    fn new(line: usize, severity: Severity, message: String) -> Finding {
        Finding {
            line,
            severity,
            message,
        }
    }
}

const TOL: f64 = 0.001;

/// Python's `_code_part`: the line before any `;`, parenthesised comments
/// replaced by a space, upper-cased and stripped.
fn code_part(raw: &str) -> String {
    let line = raw.split(';').next().unwrap_or("");
    // _COMMENT = r"\([^)]*\)", substituted with " ".
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(open) = rest.find('(') {
        match rest[open..].find(')') {
            Some(close) => {
                out.push_str(&rest[..open]);
                out.push(' ');
                rest = &rest[open + close + 1..];
            }
            // No ')' after this '(' means none after any later '(' either.
            None => break,
        }
    }
    out.push_str(rest);
    py::strip(&out.to_uppercase()).to_string()
}

/// Python's `_WORD.findall`: a letter and a plain number,
/// `([A-Z])\s*([-+]?(?:\d+\.?\d*|\.\d+))(?![\d.\[#])`.
///
/// Words with `[expressions]` or `#parameters` are skipped: this is a line
/// scanner, not an interpreter, and guessing at a subroutine argument would
/// be worse than saying nothing.
fn words(code: &str) -> Vec<(char, f64)> {
    let chars: Vec<char> = code.chars().collect();
    let digit = |i: usize| chars.get(i).is_some_and(|c| c.is_ascii_digit());
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let letter = chars[i];
        if !letter.is_ascii_uppercase() {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while chars.get(j).is_some_and(|c| py::is_space(*c)) {
            j += 1;
        }
        let num_start = j;
        if matches!(chars.get(j), Some('-' | '+')) {
            j += 1;
        }
        let mut k = j;
        if digit(k) {
            while digit(k) {
                k += 1;
            }
            if chars.get(k) == Some(&'.') {
                k += 1;
                while digit(k) {
                    k += 1;
                }
            }
        } else if chars.get(k) == Some(&'.') && digit(k + 1) {
            k += 1;
            while digit(k) {
                k += 1;
            }
        } else {
            i += 1;
            continue;
        }
        // Every shorter match the regex could backtrack to ends just before
        // a digit or a '.', which the lookahead refuses, so only the longest
        // number needs checking.
        let refused = chars
            .get(k)
            .is_some_and(|c| c.is_ascii_digit() || matches!(c, '.' | '[' | '#'));
        if refused {
            i += 1;
            continue;
        }
        let text: String = chars[num_start..k].iter().collect();
        let value: f64 = text.parse().expect("the scanner only accepts float syntax");
        out.push((letter, value));
        i = k;
    }
    out
}

/// Python's `_OWORD.match`: `^O\s*<[^>]*>|^O\d+` (the code is upper-case).
fn is_oword(code: &str) -> bool {
    let Some(rest) = code.strip_prefix('O') else {
        return false;
    };
    let angled = rest
        .trim_start_matches(py::is_space)
        .strip_prefix('<')
        .is_some_and(|r| r.contains('>'));
    angled || rest.starts_with(|c: char| c.is_ascii_digit())
}

/// Python's `last_motion` tuple: (line, is_g53, x, z, full X retract value).
type LastMotion = (usize, bool, Option<f64>, Option<f64>, f64);

/// Scans a program for the mistakes the docs single out.
///
/// Tracks just enough modal state to be right about the one trap that
/// matters most: under G7 (diameter mode) the fully retracted position is
/// X = 2 * x_retract, and `G53 G0 X48` only retracts half way
/// (gcode/tutorial-part.ngc explains this at length).
pub fn preflight(text: &str, belt: Belt, machine: &Machine) -> Vec<Finding> {
    use Severity::{Error, Warning};
    let mut findings = Vec::new();
    let lines = py::splitlines(text);

    // House style: a header that says what stock and tools the program wants.
    let header = lines
        .iter()
        .take(40)
        .filter(|l| py::strip(l).starts_with('('))
        .copied()
        .collect::<Vec<_>>()
        .join(" ")
        .to_uppercase();
    for (need, title) in [("STOCK", "Stock"), ("TOOL", "Tool")] {
        if !header.contains(need) {
            findings.push(Finding::new(
                1,
                Warning,
                format!(
                    "No '{title}' in the header comment — say what the program expects \
                     so the operator can check the setup (doc 40 §10.2)"
                ),
            ));
        }
    }

    let mut diameter = false;
    let mut metric_seen = false;
    let mut css = false;
    let mut spindle_on = false;
    let mut s_active = 0.0_f64;
    let mut seen_motion = false;
    let mut retracted = false; // X is known to be fully retracted
                               // (line, is_g53, x, z, full X retract value) of the last motion line.
    let mut last_motion: Option<LastMotion> = None;
    let mut in_sub = false;
    let (lo, hi) = belt_range(belt);

    for (index, raw) in lines.iter().enumerate() {
        let n = index + 1;
        let code = code_part(raw);
        if code.is_empty() {
            continue;
        }
        if is_oword(&code) {
            // Subroutine bodies run wherever they are called from; their
            // moves are not "the first move of the program".
            if code.contains("ENDSUB") {
                in_sub = false;
            } else if py::split_whitespace(&code).any(|w| w == "SUB") {
                in_sub = true;
            }
            continue;
        }

        let words = words(&code);
        let gs: Vec<f64> = words
            .iter()
            .filter(|(l, _)| *l == 'G')
            .map(|w| w.1)
            .collect();
        let ms: Vec<f64> = words
            .iter()
            .filter(|(l, _)| *l == 'M')
            .map(|w| w.1)
            .collect();
        // A dict: the last word of a letter wins.
        let mut vals: BTreeMap<char, f64> = BTreeMap::new();
        for (l, v) in &words {
            if *l != 'G' && *l != 'M' {
                vals.insert(*l, *v);
            }
        }
        let has_g = |v: f64| gs.contains(&v);
        let has_m = |v: f64| ms.contains(&v);

        if has_g(21.0) {
            metric_seen = true;
        }
        if has_g(20.0) {
            findings.push(Finding::new(
                n,
                Warning,
                "G20 inches — house style is millimetres (G21)".into(),
            ));
        }
        if has_g(7.0) {
            diameter = true;
        }
        if has_g(8.0) {
            diameter = false;
        }
        if has_g(96.0) {
            css = true;
        }
        if has_g(97.0) {
            css = false;
        }

        if let Some(&s) = vals.get(&'S') {
            s_active = s;
            if !css && s_active > 0.0 && !(f64::from(lo) <= s_active && s_active <= f64::from(hi)) {
                findings.push(Finding::new(
                    n,
                    Error,
                    format!(
                        "S{} is outside belt {} ({}–{} rpm)",
                        py::g(s_active),
                        belt.0,
                        lo,
                        hi
                    ),
                ));
            }
        }

        if has_m(3.0) || has_m(4.0) {
            spindle_on = true;
        }
        if has_m(5.0) || has_m(2.0) || has_m(30.0) {
            spindle_on = false;
        }

        // Port note: like the Python, this compares the last S word even
        // under G96, where S is a surface speed rather than rpm.
        if (has_g(33.0) || has_g(33.1) || has_g(76.0)) && s_active > f64::from(THREADING_MAX_RPM) {
            findings.push(Finding::new(
                n,
                Error,
                format!(
                    "Threading with S{} — the limit is {} rpm",
                    py::g(s_active),
                    THREADING_MAX_RPM
                ),
            ));
        }

        if has_m(6.0) {
            if spindle_on {
                findings.push(Finding::new(
                    n,
                    Error,
                    "Tool change with the spindle running — M5 first (doc 40 §8)".into(),
                ));
            }
            if !retracted && !machine.tool_change_retracts {
                findings.push(Finding::new(
                    n,
                    Error,
                    "Tool change without a full X retract — the turret sweeps a wide circle (doc 40 §8)"
                        .into(),
                ));
            }
        }

        let x = vals.get(&'X').copied();
        let z = vals.get(&'Z').copied();
        let has_motion_axis = x.is_some() || z.is_some();
        let not_a_move = [10.0, 28.0, 30.0, 92.0, 43.0, 43.1]
            .iter()
            .any(|g| has_g(*g));
        let moves = has_motion_axis && !not_a_move;
        if moves && !in_sub {
            let g53 = has_g(53.0);
            let x_retract_value = machine.x_retract * if diameter { 2.0 } else { 1.0 };
            if !seen_motion {
                seen_motion = true;
                let full = g53 && x.is_some_and(|x| x >= x_retract_value - TOL);
                if !full {
                    let hint = format!("G53 G0 X{}", py::g(x_retract_value));
                    let half =
                        diameter && g53 && x.is_some_and(|x| (x - machine.x_retract).abs() < TOL);
                    let msg = if half {
                        format!(
                            "First move retracts only half way: under G7 an X value is a \
                             diameter, so the full retract is {hint}"
                        )
                    } else {
                        format!("First move is not a full X retract — open with {hint} (doc 50 §8)")
                    };
                    findings.push(Finding::new(n, Error, msg));
                }
            }
            if let Some(x) = x {
                retracted = g53 && x >= x_retract_value - TOL;
            }
            last_motion = Some((n, g53, x, z, x_retract_value));
        }
    }

    if !metric_seen {
        findings.push(Finding::new(
            1,
            Warning,
            "No G21 — state millimetres explicitly (doc 50 §8)".into(),
        ));
    }

    if let Some((n, g53, x, z, x_full)) = last_motion {
        let parked = g53
            && x.is_some_and(|x| x >= x_full - TOL)
            && z.is_some_and(|z| (z - machine.z_park).abs() < TOL);
        if !parked {
            findings.push(Finding::new(
                n,
                Warning,
                format!(
                    "Program does not finish at G53 G0 X{} Z{} — leave the machine clear \
                     for the next person (doc 50 §8)",
                    py::g(x_full),
                    py::g(machine.z_park)
                ),
            ));
        }
    }

    // Python's sort is stable, and so is this one.
    findings.sort_by_key(|f| (f.line, f.severity != Error));
    findings
}

// -----------------------------------------------------------------------------
// Touch-off — doc 40 §9
// -----------------------------------------------------------------------------

/// §9.1: with the tool tip on the fresh face, that face becomes G54 Z0.
pub fn touchoff_work_z() -> Vec<String> {
    vec!["G10 L20 P1 Z0".into()]
}

/// §9.2 step 2: the skim-cut diameter becomes the tool's X offset.
///
/// Issued in radius mode with an explicit radius, rather than trusting how a
/// given LinuxCNC version applies G7 to G10, and the operator's diameter mode
/// is restored afterwards.
///
/// Port note: `Err` where Python raises `ValueError`.
pub fn touchoff_tool_x(
    tool: u32,
    measured_diameter: f64,
    diameter_mode: bool,
) -> Result<Vec<String>, String> {
    if measured_diameter <= 0.0 {
        return Err("measured diameter must be positive".into());
    }
    let mut cmds = vec![
        "G8".to_string(),
        format!(
            "G10 L10 P{} X{}",
            tool,
            py::fixed(measured_diameter / 2.0, 4)
        ),
    ];
    if diameter_mode {
        cmds.push("G7".into());
    }
    Ok(cmds)
}

/// §9.2 step 3: the tool tip touching the faced end is this tool's Z0.
pub fn touchoff_tool_z(tool: u32) -> Vec<String> {
    vec![format!("G10 L10 P{tool} Z0")]
}

/// §9.1 step 6: the tool should stop 5 mm clear of the fresh face.
pub fn verify_work_z() -> Vec<String> {
    vec!["G0 Z5".into()]
}

/// The bar diameter [`verify_tool`] uses when not told otherwise.
pub const VERIFY_BAR_DIAMETER: f64 = 20.0;

/// §9.2 step 5: a known bar diameter, 5 mm off the face.
pub fn verify_tool(diameter_mode: bool, bar_diameter: f64) -> Vec<String> {
    let x = if diameter_mode {
        bar_diameter
    } else {
        bar_diameter / 2.0
    };
    vec![format!("G0 X{} Z5", py::g(x))]
}

// -----------------------------------------------------------------------------
// Proven programs — doc 40 §10.1 "even for the second run after one small edit"
// -----------------------------------------------------------------------------

/// The SHA-256 of the program text's UTF-8, in lower-case hex.
pub fn program_hash(text: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Which program texts have completed a supervised first run.
///
/// Keyed on the text's hash, so any edit at all makes a program unproven.
///
/// Port note: Python's `ProvenStore` reads and writes
/// `~/.config/emco-lathe/proven.json` itself. This one holds the map and
/// converts it to and from that file's JSON ([`ProvenStore::from_json`],
/// [`ProvenStore::to_json`], the same text Python's `json.dump(...,
/// indent=1, sort_keys=True)` writes for ASCII names), and the caller
/// stores the text. The web screen keeps it in the browser's
/// `localStorage`, so "per user" there means per browser profile.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProvenStore {
    proven: BTreeMap<String, String>,
}

impl ProvenStore {
    /// An empty store.
    pub fn new() -> ProvenStore {
        ProvenStore::default()
    }

    /// The store saved as `json`. Anything unreadable is an empty store,
    /// as Python treats a corrupt file. Port note: Python would also take
    /// a JSON list or other non-object (and fail later, on the first
    /// `mark_proven`); here that is an empty store too. Entries whose name
    /// is not a string are skipped.
    pub fn from_json(json: &str) -> ProvenStore {
        let proven = match serde_json::from_str::<serde_json::Value>(json) {
            Ok(serde_json::Value::Object(m)) => m
                .into_iter()
                .filter_map(|(k, v)| v.as_str().map(|v| (k, v.to_string())))
                .collect(),
            _ => BTreeMap::new(),
        };
        ProvenStore { proven }
    }

    /// The text to save: a JSON object `{hash: name}`, keys sorted, one
    /// space of indent.
    pub fn to_json(&self) -> String {
        use serde::Serialize;
        let mut out = Vec::new();
        let fmt = serde_json::ser::PrettyFormatter::with_indent(b" ");
        let mut ser = serde_json::Serializer::with_formatter(&mut out, fmt);
        self.proven
            .serialize(&mut ser)
            .expect("a string map always serialises");
        String::from_utf8(out).expect("serde_json writes UTF-8")
    }

    pub fn is_proven(&self, text: &str) -> bool {
        self.proven.contains_key(&program_hash(text))
    }

    /// Records `text` as proven under `name`. The caller saves
    /// [`ProvenStore::to_json`] afterwards.
    pub fn mark_proven(&mut self, text: &str, name: &str) {
        self.proven.insert(program_hash(text), name.to_string());
    }

    pub fn len(&self) -> usize {
        self.proven.len()
    }

    pub fn is_empty(&self) -> bool {
        self.proven.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_word_scanner_matches_the_python_regex() {
        // re.findall(r"([A-Z])\s*([-+]?(?:\d+\.?\d*|\.\d+))(?![\d.\[#])", s)
        assert_eq!(words("G0 X[#1] Z0.5"), vec![('G', 0.0), ('Z', 0.5)]);
        assert_eq!(
            words("X 5 Z-.5 F+2."),
            vec![('X', 5.0), ('Z', -0.5), ('F', 2.0)]
        );
        assert_eq!(words("X1#2 Z1..2 G33.1"), vec![('G', 33.1)]);
        assert_eq!(words("O<R> CALL [10]"), vec![]);
        assert_eq!(
            words("G10L20P1Z0"),
            vec![('G', 10.0), ('L', 20.0), ('P', 1.0), ('Z', 0.0)]
        );
    }

    #[test]
    fn code_part_matches_python() {
        assert_eq!(code_part("g0 x1 (move (a) b) z2 ; rest"), "G0 X1   B) Z2");
        assert_eq!(code_part("(only)"), "");
        assert_eq!(code_part("g0 (open"), "G0 (OPEN");
        assert_eq!(code_part("(a;b) g1"), "(A");
    }

    #[test]
    fn owords_match_python() {
        assert!(is_oword("O<R> SUB"));
        assert!(is_oword("O <R> ENDSUB"));
        assert!(is_oword("O100 CALL"));
        assert!(!is_oword("O <R"));
        assert!(!is_oword("O 100"));
        assert!(!is_oword("G0 X1"));
    }

    #[test]
    fn proven_json_is_what_python_writes() {
        let mut s = ProvenStore::new();
        s.mark_proven("G0 X1\n", "a.ngc");
        // json.dump({...}, f, indent=1, sort_keys=True)
        let want = format!("{{\n \"{}\": \"a.ngc\"\n}}", program_hash("G0 X1\n"));
        assert_eq!(s.to_json(), want);
        assert_eq!(ProvenStore::from_json(&want), s);
        assert_eq!(ProvenStore::from_json("[1, 2]"), ProvenStore::new());
        assert_eq!(
            program_hash("x"),
            "2d711642b726b04401627ca9fbac32f5c8530fb1903cc4db02258717921a4881"
        );
    }

    #[test]
    fn clamp_handles_nan_like_python() {
        assert_eq!(clamp_rpm(DEFAULT_BELT, f64::NAN), 2000.0);
    }
}
