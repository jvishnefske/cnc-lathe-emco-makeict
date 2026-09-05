//! Lexer, parser and modal interpreter for the Fanuc-0T-style dialect subset
//! used by the EMCO PC Turn 50 programs in this repo.
//!
//! Design notes:
//! - Parsing never panics and never stops at the first bad line; every problem
//!   becomes a [`Diagnostic`].
//! - **X is radius everywhere inside the core.** Programs written in diameter
//!   mode (the lathe default) are converted during interpretation.
//! - The same [`ModalState`] machinery is used by the backplot builder
//!   ([`crate::geom`]), by [`crate::session::Session`] (to predict DRO and
//!   modal display) and by [`crate::machine::VirtualMachine`] (to execute
//!   bytes), so all three agree about what a block means.
//!
//! Known limitation (documented, deliberate): numbers are read literally.
//! Historical Fanuc-0T "no decimal point means least input increment"
//! (`X1000` = 1.000 mm) is **not** applied; write decimal points.

use serde::Serialize;

// ---------------------------------------------------------------------------
// Diagnostics
// ---------------------------------------------------------------------------

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
}

/// Matches the `Diagnostic` shape in `sender/INTERFACE.md` (0-based line/column).
#[derive(Serialize, Clone, Debug)]
pub struct Diagnostic {
    pub line: u32,
    pub column: u32,
    pub severity: Severity,
    pub message: String,
}

impl Diagnostic {
    pub fn error(line: u32, column: u32, message: impl Into<String>) -> Self {
        Diagnostic { line, column, severity: Severity::Error, message: message.into() }
    }
    pub fn warning(line: u32, column: u32, message: impl Into<String>) -> Self {
        Diagnostic { line, column, severity: Severity::Warning, message: message.into() }
    }
}

// ---------------------------------------------------------------------------
// Lexing
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Word {
    pub letter: char,
    pub value: f64,
    pub column: u32,
}

/// One source line after lexing. Comment text is stripped; `clean` is the
/// word text that would be sent to the machine.
#[derive(Clone, Debug)]
pub struct Block {
    /// 0-based source line index.
    pub line: u32,
    /// Words in source order, letters upper-cased.
    pub words: Vec<Word>,
    /// True when the line starts with the block-skip character `/`.
    pub block_skip: bool,
    /// Source text with comments removed and whitespace collapsed.
    pub clean: String,
}

impl Block {
    pub fn word(&self, letter: char) -> Option<f64> {
        self.words.iter().find(|w| w.letter == letter).map(|w| w.value)
    }
    /// All values for a letter, in order (G and M can repeat in a block).
    pub fn all(&self, letter: char) -> impl Iterator<Item = f64> + '_ {
        self.words.iter().filter(move |w| w.letter == letter).map(|w| w.value)
    }
    pub fn has_any_of(&self, letters: &[char]) -> bool {
        self.words.iter().any(|w| letters.contains(&w.letter))
    }
}

/// Lex a single source line. Never fails; problems go into `diags`.
pub fn lex_line(text: &str, line: u32, diags: &mut Vec<Diagnostic>) -> Option<Block> {
    let bytes: Vec<char> = text.chars().collect();
    let mut i = 0usize;
    let mut words = Vec::new();
    let mut block_skip = false;
    let mut clean = String::new();

    // Leading whitespace, then optional block-skip `/`.
    while i < bytes.len() && bytes[i].is_whitespace() {
        i += 1;
    }
    if i < bytes.len() && bytes[i] == '/' {
        block_skip = true;
        i += 1;
    }

    while i < bytes.len() {
        let c = bytes[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c == ';' {
            break; // comment to end of line
        }
        if c == '(' {
            // Parenthesis comment (no nesting in this dialect).
            let start = i;
            i += 1;
            while i < bytes.len() && bytes[i] != ')' {
                i += 1;
            }
            if i >= bytes.len() {
                diags.push(Diagnostic::warning(line, start as u32, "unclosed '(' comment"));
                break;
            }
            i += 1; // consume ')'
            continue;
        }
        if c == '%' {
            // Program start/end marker: ignore the rest of the line.
            i = bytes.len();
            continue;
        }
        if c.is_ascii_alphabetic() {
            let letter = c.to_ascii_uppercase();
            let col = i as u32;
            i += 1;
            // Optional sign, digits, optional decimal part.
            let num_start = i;
            if i < bytes.len() && (bytes[i] == '+' || bytes[i] == '-') {
                i += 1;
            }
            let mut saw_digit = false;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
                saw_digit = true;
            }
            if i < bytes.len() && bytes[i] == '.' {
                i += 1;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                    saw_digit = true;
                }
            }
            if !saw_digit {
                diags.push(Diagnostic::error(
                    line,
                    col,
                    format!("word '{letter}' has no number"),
                ));
                continue;
            }
            let num_text: String = bytes[num_start..i].iter().collect();
            match num_text.parse::<f64>() {
                Ok(v) if v.is_finite() => {
                    if !clean.is_empty() {
                        clean.push(' ');
                    }
                    clean.push(letter);
                    clean.push_str(&num_text);
                    words.push(Word { letter, value: v, column: col });
                }
                _ => {
                    diags.push(Diagnostic::error(
                        line,
                        col,
                        format!("bad number '{num_text}' after '{letter}'"),
                    ));
                }
            }
            continue;
        }
        diags.push(Diagnostic::error(line, i as u32, format!("unexpected character '{c}'")));
        i += 1;
    }

    if words.is_empty() && !block_skip {
        return None; // blank or comment-only line: no block
    }
    Some(Block { line, words, block_skip, clean })
}

// ---------------------------------------------------------------------------
// Modal state and block semantics
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Units {
    Mm,
    Inch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Distance {
    Absolute,
    Incremental,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedMode {
    /// G94: units per minute.
    UnitsPerMin,
    /// G95: units per spindle revolution.
    UnitsPerRev,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Plane {
    Xy,
    /// G18, the lathe default.
    Zx,
    Yz,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpindleDir {
    Off,
    Cw,
    Ccw,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MotionGroup {
    Rapid,
    Linear,
    ArcCw,
    ArcCcw,
    Thread,
}

/// What kind of move a block produced, in machine radius/mm coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MoveKind {
    Rapid,
    Feed,
    Arc { cx: f64, cz: f64, ccw: bool },
    Thread,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MotionCmd {
    pub kind: MoveKind,
    pub x0: f64,
    pub z0: f64,
    pub x1: f64,
    pub z1: f64,
    /// Effective feed in mm/min (rapid rate is machine-defined; 0 here for rapids).
    pub feed_mm_min: f64,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct BlockEffects {
    pub motion: Option<MotionCmd>,
    /// `Some(station)` when the block commands a tool change.
    pub tool_change: Option<u32>,
    /// True on M02/M30.
    pub end_of_program: bool,
}

#[derive(Clone, Debug)]
pub struct ModalState {
    pub units: Units,
    pub distance: Distance,
    pub feed_mode: FeedMode,
    pub plane: Plane,
    pub motion: MotionGroup,
    /// Diameter programming for the X word. **Lathe default: true.**
    /// G07 selects diameter mode, G08 radius mode (LinuxCNC-style; Fanuc-0T
    /// sets this by parameter so programs rarely switch it).
    pub diameter_mode: bool,
    /// Active work offset, 54..=59.
    pub work_offset: u8,
    pub tool: u32,
    pub tool_offset: u32,
    pub spindle_rpm: f64,
    pub spindle: SpindleDir,
    pub coolant: bool,
    /// Modal feed, stored in mm per (min|rev) according to `feed_mode`.
    pub feed: f64,
    /// Current position, radius mm / mm.
    pub x: f64,
    pub z: f64,
    pub ended: bool,
    /// Emitted the "feed with no F" warning already (once per program).
    warned_no_feed: bool,
    warned_no_rpm: bool,
}

impl Default for ModalState {
    fn default() -> Self {
        ModalState {
            units: Units::Mm,
            distance: Distance::Absolute,
            feed_mode: FeedMode::UnitsPerRev, // lathe power-on default
            plane: Plane::Zx,
            motion: MotionGroup::Rapid,
            diameter_mode: true,
            work_offset: 54,
            tool: 0,
            tool_offset: 0,
            spindle_rpm: 0.0,
            spindle: SpindleDir::Off,
            coolant: false,
            feed: 0.0,
            x: 0.0,
            z: 0.0,
            ended: false,
            warned_no_feed: false,
            warned_no_rpm: false,
        }
    }
}

impl ModalState {
    fn unit_factor(&self) -> f64 {
        match self.units {
            Units::Mm => 1.0,
            Units::Inch => 25.4,
        }
    }

    /// Effective feed in mm/min for the current modal state.
    pub fn feed_mm_min(&self) -> f64 {
        match self.feed_mode {
            FeedMode::UnitsPerMin => self.feed,
            FeedMode::UnitsPerRev => self.feed * self.spindle_rpm,
        }
    }

    /// Modal display strings for the UI, e.g. `["G21","G90","G95","G01"]`.
    pub fn modal_display(&self) -> Vec<String> {
        let mut v = Vec::with_capacity(6);
        v.push(match self.units {
            Units::Mm => "G21",
            Units::Inch => "G20",
        }.to_string());
        v.push(match self.distance {
            Distance::Absolute => "G90",
            Distance::Incremental => "G91",
        }.to_string());
        v.push(match self.feed_mode {
            FeedMode::UnitsPerMin => "G94",
            FeedMode::UnitsPerRev => "G95",
        }.to_string());
        v.push(match self.motion {
            MotionGroup::Rapid => "G00",
            MotionGroup::Linear => "G01",
            MotionGroup::ArcCw => "G02",
            MotionGroup::ArcCcw => "G03",
            MotionGroup::Thread => "G33",
        }.to_string());
        v.push(format!("G{}", self.work_offset));
        v
    }

    /// Interpret one block, updating modal state and returning its effects.
    /// Never panics; problems are appended to `diags`.
    pub fn apply_block(&mut self, block: &Block, diags: &mut Vec<Diagnostic>) -> BlockEffects {
        let mut fx = BlockEffects::default();
        if self.ended {
            return fx;
        }
        let line = block.line;

        // --- Non-motion G codes first (units affect coordinates in the same block).
        let mut motion_g: Option<MotionGroup> = None;
        let mut g76 = false;
        for g in block.all('G') {
            let gi = (g * 10.0).round() as i64; // G84.5-safe integer form (x10)
            match gi {
                0 => motion_g = Some(MotionGroup::Rapid),
                10 => motion_g = Some(MotionGroup::Linear),
                20 => motion_g = Some(MotionGroup::ArcCw),
                30 => motion_g = Some(MotionGroup::ArcCcw),
                40 => {} // dwell: no geometric effect
                70 => self.diameter_mode = true,
                80 => self.diameter_mode = false,
                170 => self.plane = Plane::Xy,
                180 => self.plane = Plane::Zx,
                190 => self.plane = Plane::Yz,
                200 => self.units = Units::Inch,
                210 => self.units = Units::Mm,
                280 => {} // reference return: handled by machine/session, not geometry
                330 => motion_g = Some(MotionGroup::Thread),
                400 | 410 | 420 => {} // tool nose comp: accepted, not modeled (warned once below)
                540..=590 if gi % 10 == 0 => self.work_offset = (gi / 10) as u8,
                760 => g76 = true,
                900 => self.distance = Distance::Absolute,
                910 => self.distance = Distance::Incremental,
                940 => self.feed_mode = FeedMode::UnitsPerMin,
                950 => self.feed_mode = FeedMode::UnitsPerRev,
                700..=750 => {
                    diags.push(Diagnostic::warning(
                        line,
                        0,
                        format!("canned cycle G{g:.0} is not supported; block ignored for geometry"),
                    ));
                }
                _ => {
                    diags.push(Diagnostic::warning(line, 0, format!("unsupported G{g}")));
                }
            }
        }
        let k = self.unit_factor();

        // --- Simple address words.
        if let Some(f) = block.word('F') {
            if f < 0.0 {
                diags.push(Diagnostic::error(line, 0, "negative feed rate"));
            } else {
                self.feed = f * k;
            }
        }
        if let Some(s) = block.word('S') {
            if s < 0.0 {
                diags.push(Diagnostic::error(line, 0, "negative spindle speed"));
            } else {
                self.spindle_rpm = s;
            }
        }
        if let Some(t) = block.word('T') {
            // Fanuc lathe convention: Txxyy = station xx, offset yy. Plain "T3"
            // is accepted as station 3, offset 3.
            let ti = t.round() as i64;
            if ti < 0 {
                diags.push(Diagnostic::error(line, 0, "negative tool number"));
            } else if ti >= 100 {
                self.tool = (ti / 100) as u32;
                self.tool_offset = (ti % 100) as u32;
                fx.tool_change = Some(self.tool);
            } else {
                self.tool = ti as u32;
                self.tool_offset = ti as u32;
                fx.tool_change = Some(self.tool);
            }
        }

        // --- M codes.
        for m in block.all('M') {
            match m.round() as i64 {
                0 | 1 => {} // program stop / optional stop: sender-level concern
                2 | 30 => {
                    self.ended = true;
                    fx.end_of_program = true;
                    self.spindle = SpindleDir::Off;
                    self.coolant = false;
                }
                3 => self.spindle = SpindleDir::Cw,
                4 => self.spindle = SpindleDir::Ccw,
                5 => self.spindle = SpindleDir::Off,
                7 | 8 => self.coolant = true,
                9 => self.coolant = false,
                6 => {} // tool change (handled via T)
                other => {
                    diags.push(Diagnostic::warning(line, 0, format!("unsupported M{other}")));
                }
            }
        }

        // --- Motion.
        if let Some(m) = motion_g {
            self.motion = m;
        }
        let has_coords = block.has_any_of(&['X', 'Z', 'U', 'W']);
        if g76 {
            // TODO(protocol/geometry): Fanuc-0T G76 is a two-block multi-pass
            // threading cycle. We model only the programmed X/Z endpoint as a
            // single thread pass so the backplot shows *something* sensible.
            if has_coords {
                diags.push(Diagnostic::warning(
                    line,
                    0,
                    "G76 threading cycle simplified to a single thread pass",
                ));
                let (x1, z1) = self.resolve_target(block);
                fx.motion = Some(MotionCmd {
                    kind: MoveKind::Thread,
                    x0: self.x,
                    z0: self.z,
                    x1,
                    z1,
                    feed_mm_min: self.feed_mm_min(),
                });
                self.x = x1;
                self.z = z1;
            } else {
                diags.push(Diagnostic::warning(
                    line,
                    0,
                    "G76 parameter block ignored (threading cycle not modeled)",
                ));
            }
            return fx;
        }
        if !has_coords {
            return fx;
        }

        let (x1, z1) = self.resolve_target(block);
        let (x0, z0) = (self.x, self.z);

        match self.motion {
            MotionGroup::Rapid => {
                fx.motion = Some(MotionCmd { kind: MoveKind::Rapid, x0, z0, x1, z1, feed_mm_min: 0.0 });
            }
            MotionGroup::Linear | MotionGroup::Thread => {
                let feed = self.checked_feed(line, diags);
                let kind = if self.motion == MotionGroup::Thread { MoveKind::Thread } else { MoveKind::Feed };
                fx.motion = Some(MotionCmd { kind, x0, z0, x1, z1, feed_mm_min: feed });
            }
            MotionGroup::ArcCw | MotionGroup::ArcCcw => {
                let ccw = self.motion == MotionGroup::ArcCcw;
                match self.resolve_arc_center(block, x0, z0, x1, z1, ccw, line, diags) {
                    Some((cx, cz)) => {
                        let feed = self.checked_feed(line, diags);
                        fx.motion = Some(MotionCmd {
                            kind: MoveKind::Arc { cx, cz, ccw },
                            x0,
                            z0,
                            x1,
                            z1,
                            feed_mm_min: feed,
                        });
                    }
                    None => {
                        // Diagnostic already emitted; skip the move but still
                        // update position so following blocks stay coherent.
                    }
                }
            }
        }
        self.x = x1;
        self.z = z1;
        fx
    }

    fn checked_feed(&mut self, line: u32, diags: &mut Vec<Diagnostic>) -> f64 {
        if self.feed <= 0.0 && !self.warned_no_feed {
            self.warned_no_feed = true;
            diags.push(Diagnostic::warning(line, 0, "feed move with no feed rate set"));
        }
        if self.feed_mode == FeedMode::UnitsPerRev && self.spindle_rpm <= 0.0 && !self.warned_no_rpm {
            self.warned_no_rpm = true;
            diags.push(Diagnostic::warning(
                line,
                0,
                "G95 feed-per-rev with spindle speed 0; effective feed is 0",
            ));
        }
        self.feed_mm_min()
    }

    /// Resolve the X/Z (or U/W incremental) target of a block into radius/mm.
    fn resolve_target(&self, block: &Block) -> (f64, f64) {
        let k = self.unit_factor();
        let xdiv = if self.diameter_mode { 2.0 } else { 1.0 };
        let mut x1 = self.x;
        let mut z1 = self.z;
        if let Some(x) = block.word('X') {
            x1 = match self.distance {
                Distance::Absolute => x * k / xdiv,
                Distance::Incremental => self.x + x * k / xdiv,
            };
        }
        if let Some(u) = block.word('U') {
            // U is always incremental X (diameter value in diameter mode).
            x1 += u * k / xdiv;
        }
        if let Some(z) = block.word('Z') {
            z1 = match self.distance {
                Distance::Absolute => z * k,
                Distance::Incremental => self.z + z * k,
            };
        }
        if let Some(w) = block.word('W') {
            w_apply(&mut z1, w * k);
        }
        (x1, z1)
    }

    /// Compute the absolute arc centre from I/K or R words.
    /// Returns `None` (with a diagnostic) when the arc is invalid.
    #[allow(clippy::too_many_arguments)]
    fn resolve_arc_center(
        &self,
        block: &Block,
        x0: f64,
        z0: f64,
        x1: f64,
        z1: f64,
        ccw: bool,
        line: u32,
        diags: &mut Vec<Diagnostic>,
    ) -> Option<(f64, f64)> {
        let k = self.unit_factor();
        let full_circle = (x1 - x0).abs() < 1e-9 && (z1 - z0).abs() < 1e-9;

        if let Some(r) = block.word('R') {
            // R form. Full circles cannot be specified with R (and make no
            // sense on a lathe anyway).
            if full_circle {
                diags.push(Diagnostic::error(
                    line,
                    0,
                    "R-form arc with zero chord (full circle) is not allowed",
                ));
                return None;
            }
            let r_mm = r * k;
            // Work in the (z, x) plane treated as (X, Y).
            let dx = z1 - z0;
            let dy = x1 - x0;
            let d2 = dx * dx + dy * dy;
            let disc = 4.0 * r_mm * r_mm - d2;
            if disc < -1e-9 {
                diags.push(Diagnostic::error(
                    line,
                    0,
                    format!("arc radius {r} too small for chord length {:.3}", d2.sqrt()),
                ));
                return None;
            }
            // GRBL-style centre selection: positive R -> minor arc,
            // negative R -> major arc.
            let mut h_x2_div_d = -(disc.max(0.0)).sqrt() / d2.sqrt();
            if ccw {
                h_x2_div_d = -h_x2_div_d;
            }
            if r_mm < 0.0 {
                h_x2_div_d = -h_x2_div_d;
            }
            let cz = z0 + 0.5 * (dx - dy * h_x2_div_d);
            let cx = x0 + 0.5 * (dy + dx * h_x2_div_d);
            return Some((cx, cz));
        }

        let i = block.word('I');
        let kk = block.word('K');
        if i.is_none() && kk.is_none() {
            diags.push(Diagnostic::error(line, 0, "arc without I/K or R"));
            return None;
        }
        if full_circle {
            diags.push(Diagnostic::error(
                line,
                0,
                "full-circle arc is not allowed on a lathe",
            ));
            return None;
        }
        // I and K are incremental offsets from the start point to the centre.
        // I is a **radius** value even in diameter mode (Fanuc lathe convention).
        let cx = x0 + i.unwrap_or(0.0) * k;
        let cz = z0 + kk.unwrap_or(0.0) * k;
        let r_start = ((x0 - cx).powi(2) + (z0 - cz).powi(2)).sqrt();
        let r_end = ((x1 - cx).powi(2) + (z1 - cz).powi(2)).sqrt();
        if (r_start - r_end).abs() > 0.01 {
            diags.push(Diagnostic::warning(
                line,
                0,
                format!(
                    "arc radius differs between start ({r_start:.3}) and end ({r_end:.3}); \
                     check I/K values"
                ),
            ));
        }
        Some((cx, cz))
    }
}

#[inline]
fn w_apply(z1: &mut f64, dw: f64) {
    *z1 += dw;
}

// ---------------------------------------------------------------------------
// Program
// ---------------------------------------------------------------------------

/// A parsed program: source lines, lexed blocks, cached backplot geometry and
/// every diagnostic (lexical and semantic).
#[derive(Clone, Debug)]
pub struct Program {
    pub lines: Vec<String>,
    pub blocks: Vec<Block>,
    pub diagnostics: Vec<Diagnostic>,
    pub segments: Vec<crate::geom::Segment>,
    pub bounds: crate::geom::Bounds,
}

impl Program {
    /// Parse G-code text. Never panics; problems come back as diagnostics.
    pub fn parse(text: &str) -> Program {
        let mut diagnostics = Vec::new();
        let mut blocks = Vec::new();
        let lines: Vec<String> = text.lines().map(|l| l.to_string()).collect();
        for (i, l) in lines.iter().enumerate() {
            if let Some(b) = lex_line(l, i as u32, &mut diagnostics) {
                blocks.push(b);
            }
        }
        let (segments, bounds) = crate::geom::interpret(&blocks, &mut diagnostics);
        Program { lines, blocks, diagnostics, segments, bounds }
    }

    pub fn line_count(&self) -> u32 {
        self.lines.len() as u32
    }

    pub fn line(&self, index: u32) -> String {
        self.lines.get(index as usize).cloned().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lex(s: &str) -> (Option<Block>, Vec<Diagnostic>) {
        let mut d = Vec::new();
        let b = lex_line(s, 0, &mut d);
        (b, d)
    }

    #[test]
    fn lexes_word_address_line() {
        let (b, d) = lex("N10 G01 X12.5 Z-4.0 F0.1");
        assert!(d.is_empty());
        let b = b.unwrap();
        assert_eq!(b.word('N'), Some(10.0));
        assert_eq!(b.word('G'), Some(1.0));
        assert_eq!(b.word('X'), Some(12.5));
        assert_eq!(b.word('Z'), Some(-4.0));
        assert_eq!(b.word('F'), Some(0.1));
        assert_eq!(b.clean, "N10 G01 X12.5 Z-4.0 F0.1");
    }

    #[test]
    fn lexes_lowercase_and_dense_text() {
        let (b, d) = lex("g1x5.z-2.f.1");
        assert!(d.is_empty(), "{d:?}");
        let b = b.unwrap();
        assert_eq!(b.word('X'), Some(5.0));
        assert_eq!(b.word('F'), Some(0.1));
    }

    #[test]
    fn block_skip_flag() {
        let (b, d) = lex("/ N20 M08");
        assert!(d.is_empty());
        assert!(b.unwrap().block_skip);
        // '/' not at line start is an error, not block-skip.
        let (_, d) = lex("G01 / X2");
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Error);
    }

    #[test]
    fn strips_comments() {
        let (b, d) = lex("G0 (rapid to start) X10 ; trailing note");
        assert!(d.is_empty());
        let b = b.unwrap();
        assert_eq!(b.words.len(), 2);
        assert_eq!(b.clean, "G0 X10");
    }

    #[test]
    fn comment_only_line_produces_no_block() {
        let (b, d) = lex("( just a comment )");
        assert!(b.is_none());
        assert!(d.is_empty());
    }

    #[test]
    fn unclosed_comment_is_warning_not_abort() {
        let (b, d) = lex("G1 X5 (oops");
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Warning);
        assert_eq!(b.unwrap().words.len(), 2);
    }

    #[test]
    fn word_without_number_is_error_but_rest_parses() {
        let (b, d) = lex("G1 X Z5");
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Error);
        assert!(d[0].message.contains('X'));
        let b = b.unwrap();
        assert_eq!(b.word('Z'), Some(5.0));
    }

    #[test]
    fn garbage_characters_are_diagnosed_with_columns() {
        let (b, d) = lex("G1 #$ X5");
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].column, 3);
        assert_eq!(d[1].column, 4);
        assert_eq!(b.unwrap().word('X'), Some(5.0));
    }

    #[test]
    fn parse_never_stops_on_bad_line() {
        let p = Program::parse("G1 X\nqqq***\nG0 X10 Z0");
        assert!(p.diagnostics.iter().any(|d| d.severity == Severity::Error));
        // Last line still produced geometry.
        assert_eq!(p.segments.len(), 1);
        assert_eq!(p.line_count(), 3);
    }

    #[test]
    fn diameter_mode_default_halves_x() {
        let mut m = ModalState::default();
        let mut d = Vec::new();
        let b = lex_line("G0 X20 Z-5", 0, &mut d).unwrap();
        let fx = m.apply_block(&b, &mut d);
        let mv = fx.motion.unwrap();
        assert_eq!(mv.x1, 10.0, "X20 in diameter mode is radius 10");
        assert_eq!(mv.z1, -5.0);
    }

    #[test]
    fn radius_mode_via_g08() {
        let mut m = ModalState::default();
        let mut d = Vec::new();
        let b = lex_line("G8 G0 X20", 0, &mut d).unwrap();
        let fx = m.apply_block(&b, &mut d);
        assert_eq!(fx.motion.unwrap().x1, 20.0);
    }

    #[test]
    fn inch_units_convert() {
        let mut m = ModalState::default();
        let mut d = Vec::new();
        let b = lex_line("G20 G8 G0 X1.0 Z-0.5", 0, &mut d).unwrap();
        let fx = m.apply_block(&b, &mut d);
        let mv = fx.motion.unwrap();
        assert!((mv.x1 - 25.4).abs() < 1e-9);
        assert!((mv.z1 + 12.7).abs() < 1e-9);
    }

    #[test]
    fn incremental_g91_and_uw_words() {
        let mut m = ModalState { diameter_mode: false, ..ModalState::default() };
        let mut d = Vec::new();
        let b1 = lex_line("G0 X10 Z-10", 0, &mut d).unwrap();
        m.apply_block(&b1, &mut d);
        let b2 = lex_line("G91 G1 X2 Z-3 F100", 1, &mut d).unwrap();
        let fx = m.apply_block(&b2, &mut d);
        let mv = fx.motion.unwrap();
        assert_eq!((mv.x1, mv.z1), (12.0, -13.0));
        // U/W are incremental even in G90.
        let b3 = lex_line("G90 G1 U1 W-1", 2, &mut d).unwrap();
        let fx = m.apply_block(&b3, &mut d);
        let mv = fx.motion.unwrap();
        assert_eq!((mv.x1, mv.z1), (13.0, -14.0));
    }

    #[test]
    fn u_word_is_diameter_in_diameter_mode() {
        let mut m = ModalState::default();
        let mut d = Vec::new();
        let b = lex_line("G0 U4", 0, &mut d).unwrap();
        let fx = m.apply_block(&b, &mut d);
        assert_eq!(fx.motion.unwrap().x1, 2.0);
    }

    #[test]
    fn modal_motion_carries_between_blocks() {
        let mut m = ModalState::default();
        let mut d = Vec::new();
        let b1 = lex_line("G01 X10 F0.2 S1000 M3", 0, &mut d).unwrap();
        m.apply_block(&b1, &mut d);
        // No G word: still linear feed.
        let b2 = lex_line("Z-20", 1, &mut d).unwrap();
        let fx = m.apply_block(&b2, &mut d);
        assert_eq!(fx.motion.unwrap().kind, MoveKind::Feed);
    }

    #[test]
    fn g95_feed_per_rev_multiplies_rpm() {
        let mut m = ModalState::default();
        let mut d = Vec::new();
        let b1 = lex_line("G95 S1000 M3 F0.1", 0, &mut d).unwrap();
        m.apply_block(&b1, &mut d);
        assert!((m.feed_mm_min() - 100.0).abs() < 1e-9);
        let b2 = lex_line("G94 F250", 1, &mut d).unwrap();
        m.apply_block(&b2, &mut d);
        assert!((m.feed_mm_min() - 250.0).abs() < 1e-9);
    }

    #[test]
    fn g95_without_spindle_warns_once() {
        let mut m = ModalState::default();
        let mut d = Vec::new();
        let b = lex_line("G1 X5 F0.1", 0, &mut d).unwrap();
        m.apply_block(&b, &mut d);
        let b2 = lex_line("G1 Z-5", 1, &mut d).unwrap();
        m.apply_block(&b2, &mut d);
        let n = d.iter().filter(|x| x.message.contains("spindle speed 0")).count();
        assert_eq!(n, 1);
    }

    #[test]
    fn tool_word_fanuc_style() {
        let mut m = ModalState::default();
        let mut d = Vec::new();
        let b = lex_line("T0302", 0, &mut d).unwrap();
        let fx = m.apply_block(&b, &mut d);
        assert_eq!(fx.tool_change, Some(3));
        assert_eq!(m.tool, 3);
        assert_eq!(m.tool_offset, 2);
    }

    #[test]
    fn spindle_and_coolant_and_end() {
        let mut m = ModalState::default();
        let mut d = Vec::new();
        let b = lex_line("S1200 M04 M08", 0, &mut d).unwrap();
        m.apply_block(&b, &mut d);
        assert_eq!(m.spindle, SpindleDir::Ccw);
        assert!(m.coolant);
        let b = lex_line("M30", 1, &mut d).unwrap();
        let fx = m.apply_block(&b, &mut d);
        assert!(fx.end_of_program);
        assert_eq!(m.spindle, SpindleDir::Off);
        assert!(!m.coolant);
        // Blocks after M30 are inert.
        let b = lex_line("G0 X10", 2, &mut d).unwrap();
        let fx = m.apply_block(&b, &mut d);
        assert!(fx.motion.is_none());
    }

    #[test]
    fn work_offset_group_tracked() {
        let mut m = ModalState::default();
        let mut d = Vec::new();
        let b = lex_line("G55", 0, &mut d).unwrap();
        m.apply_block(&b, &mut d);
        assert_eq!(m.work_offset, 55);
        assert!(m.modal_display().contains(&"G55".to_string()));
    }

    #[test]
    fn modal_display_matches_contract_example_shape() {
        let mut m = ModalState::default();
        let mut d = Vec::new();
        let b = lex_line("G21 G90 G95 G01 X5 F0.1 S500 M3", 0, &mut d).unwrap();
        m.apply_block(&b, &mut d);
        let disp = m.modal_display();
        assert!(disp.contains(&"G21".to_string()));
        assert!(disp.contains(&"G90".to_string()));
        assert!(disp.contains(&"G95".to_string()));
        assert!(disp.contains(&"G01".to_string()));
    }

    #[test]
    fn arc_without_center_words_is_error() {
        let p = Program::parse("G8\nG0 X0 Z0\nG2 X10 Z-10 F100");
        assert!(p.diagnostics.iter().any(|d| d.message.contains("arc without I/K or R")));
        assert!(p.segments.iter().all(|s| s.kind != crate::geom::SegKind::Arc));
    }

    #[test]
    fn full_circle_rejected_both_forms() {
        let p = Program::parse("G8 G94 F100\nG0 X10 Z0\nG2 X10 Z0 I5");
        assert!(p.diagnostics.iter().any(|d| d.message.contains("full-circle")));
        let p = Program::parse("G8 G94 F100\nG0 X10 Z0\nG2 X10 Z0 R5");
        assert!(p.diagnostics.iter().any(|d| d.message.contains("full circle")));
    }

    #[test]
    fn r_too_small_for_chord_is_error() {
        let p = Program::parse("G8 G94 F100\nG0 X0 Z0\nG2 X20 Z0 R5");
        assert!(p.diagnostics.iter().any(|d| d.message.contains("too small")));
    }

    #[test]
    fn ik_radius_mismatch_warns() {
        let p = Program::parse("G8 G94 F100\nG0 X0 Z0\nG2 X10 Z-10 I10 K0.5");
        assert!(p.diagnostics.iter().any(|d| d.message.contains("radius differs")));
    }

    #[test]
    fn negative_feed_is_error() {
        let p = Program::parse("G1 X5 F-3");
        assert!(p.diagnostics.iter().any(|d| d.message.contains("negative feed")));
    }
}
