//! The screen's state and what it does, without drawing.
//!
//! This is `emco_lathe_handler.py`'s job: feed the rules what the machine
//! reports, act on what they return. Immediate mode: [`Screen`] is one
//! struct, updated from the link's status/event/error channels in
//! [`Screen::apply`] and drawn each frame by [`crate::view`]. Operator
//! actions are methods that send [`Request`]s.
//!
//! Time comes in as `now_ms`; nothing here reads a clock or touches the
//! browser. Persistent preferences (belt, proven programs, token) go
//! through [`Prefs`].

use crate::link::*;
use crate::toolpath::ToolpathView;
use emco_rules::screen::{
    self as handler, CycleStart, PowerEdge, RunLadder, SpindleAction, SpindleWatch, Wizard,
    WizardAction,
};
use emco_rules::{Belt, Finding, Machine, ProvenStore, ReferenceGate, Severity, DEFAULT_BELT};
use emco_sender_core::geom::Backplot;
use emco_sender_core::profile::MachineProfile;
use std::collections::VecDeque;

/// Where the screen keeps what should survive a reload. In a browser:
/// `localStorage`, best effort.
pub trait Prefs {
    fn get(&self, key: &str) -> Option<String>;
    fn set(&mut self, key: &str, value: &str);
    fn remove(&mut self, key: &str);
}

/// [`Prefs`] in memory, for tests and for browsers that refuse storage.
#[derive(Default, Debug)]
pub struct MemoryPrefs(pub std::collections::BTreeMap<String, String>);

impl Prefs for MemoryPrefs {
    fn get(&self, key: &str) -> Option<String> {
        self.0.get(key).cloned()
    }
    fn set(&mut self, key: &str, value: &str) {
        self.0.insert(key.into(), value.into());
    }
    fn remove(&mut self, key: &str) {
        self.0.remove(key);
    }
}

pub const PREF_BELT: &str = "emco-lathe.belt";
pub const PREF_PROVEN: &str = "emco-lathe.proven";
/// Shared with the RxJS UI at `/`, which is the same origin.
pub const PREF_TOKEN: &str = "emco-kd240-token";

/// Log lines kept (the handler's `setMaximumBlockCount(200)`).
pub const LOG_LINES: usize = 200;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Program,
    Preflight,
    Mdi,
    Touchoff,
    Programs,
}

/// The program the machine has loaded, as the screen knows it.
#[derive(Clone, Debug)]
pub struct LoadedProgram {
    pub name: String,
    /// The machine accepted it (its own parser found no errors).
    pub loaded: bool,
    /// The machine parser's diagnostics (0-based lines).
    pub machine_diagnostics: Vec<Diagnostic>,
    /// The text, once read back from the machine. Preflight and the proven
    /// hash are of exactly this.
    pub text: Option<String>,
    pub lines: Vec<String>,
    /// The emco_lathe preflight's findings (1-based lines).
    pub findings: Vec<Finding>,
    /// `sender/core`'s backplot of the text, in work coordinates.
    pub backplot: Option<Backplot>,
}

impl LoadedProgram {
    fn new(name: String, loaded: bool, machine_diagnostics: Vec<Diagnostic>) -> LoadedProgram {
        LoadedProgram {
            name,
            loaded,
            machine_diagnostics,
            text: None,
            lines: Vec::new(),
            findings: Vec::new(),
            backplot: None,
        }
    }

    pub fn preflight_errors(&self) -> usize {
        self.findings
            .iter()
            .filter(|f| f.severity == Severity::Error)
            .count()
    }
}

/// The machine the rules reason about, from the link's profile. The
/// session's reference point is X = `xMaxMm` (radius), Z = 0, and its tool
/// changes do not move the carriage.
pub fn machine_from_profile(p: &MachineProfile) -> Machine {
    Machine {
        x_retract: p.x_max_mm,
        z_park: 0.0,
        tool_change_retracts: false,
    }
}

pub struct Screen {
    pub caps: Capabilities,
    pub link_state: LinkState,
    pub status: Option<Status>,
    pub profile: MachineProfile,
    pub machine: Machine,
    gate: ReferenceGate,
    pub gate_reason: Option<&'static str>,
    power: PowerEdge,
    /// The belt question is open.
    pub ask_belt: bool,
    pub belt: Belt,
    /// The spindle box's speed, rpm, kept inside the belt's range.
    pub rpm: f64,
    spindle: SpindleWatch,
    spindle_stop_pending: bool,
    pub ladder: RunLadder,
    proven: ProvenStore,
    pub programs: Vec<ProgramInfo>,
    pub program: Option<LoadedProgram>,
    /// Program text being edited for upload.
    pub editor_name: String,
    pub editor_text: String,
    pending_edit: Option<String>,
    /// The operator's single-block choice (forced on for unproven programs).
    pub single_block: bool,
    pub dry_run: bool,
    /// Index into [`handler::JOG_INCREMENTS`].
    pub jog_increment: usize,
    pub jog_rate_mm_s: f64,
    pub mdi: String,
    pub mdi_history: Vec<String>,
    pub wizard: Wizard,
    pub measured_diameter: f64,
    pub token_input: String,
    pub token_set: bool,
    pub log: VecDeque<String>,
    pub show_sent: bool,
    pub tab: Tab,
    /// Scroll the program pane to this 0-based line on the next frame.
    pub goto_line: Option<usize>,
    /// The executing line the program pane last scrolled to.
    pub followed_line: Option<usize>,
    pub toolpath: ToolpathView,
    feed_note_logged: bool,
    prev_busy: bool,
    prev_estop: bool,
    now_ms: f64,
}

impl Screen {
    pub fn new(prefs: &dyn Prefs, caps: Capabilities) -> Screen {
        let belt = prefs
            .get(PREF_BELT)
            .and_then(|b| b.parse().ok())
            .and_then(Belt::new)
            .unwrap_or(DEFAULT_BELT);
        let proven = prefs
            .get(PREF_PROVEN)
            .map(|j| ProvenStore::from_json(&j))
            .unwrap_or_default();
        let token_input = prefs.get(PREF_TOKEN).unwrap_or_default();
        let profile = MachineProfile::pc_turn_50_default();
        let machine = machine_from_profile(&profile);
        Screen {
            caps,
            link_state: LinkState::Connecting,
            status: None,
            profile,
            machine,
            gate: ReferenceGate::new(),
            gate_reason: Some("Not referenced"),
            power: PowerEdge::default(),
            ask_belt: false,
            belt,
            rpm: emco_rules::clamp_rpm(belt, handler::DEFAULT_RPM),
            spindle: SpindleWatch::default(),
            spindle_stop_pending: false,
            ladder: RunLadder::default(),
            proven,
            programs: Vec::new(),
            program: None,
            editor_name: String::new(),
            editor_text: String::new(),
            pending_edit: None,
            single_block: false,
            dry_run: false,
            // Continuous jog needs the link to support it.
            jog_increment: if caps.continuous_jog { 0 } else { 1 },
            jog_rate_mm_s: 1.0,
            mdi: String::new(),
            mdi_history: Vec::new(),
            wizard: Wizard::default(),
            measured_diameter: 0.0,
            token_set: !token_input.is_empty(),
            token_input,
            log: VecDeque::new(),
            show_sent: false,
            tab: Tab::Program,
            goto_line: None,
            followed_line: None,
            toolpath: ToolpathView::default(),
            feed_note_logged: false,
            prev_busy: false,
            prev_estop: false,
            now_ms: 0.0,
        }
    }

    /// What to ask for once the link is up.
    pub fn startup(&mut self, link: &mut dyn MachineLink) {
        if self.token_set {
            link.set_token(Some(self.token_input.clone()));
        }
        link.send(Request::Profile);
        link.send(Request::ListPrograms);
        link.send(Request::LoadedProgram);
    }

    pub fn log(&mut self, text: impl Into<String>) {
        self.log.push_back(text.into());
        while self.log.len() > LOG_LINES {
            self.log.pop_front();
        }
    }

    // -------------------------------------------------------------------------
    // Derived state
    // -------------------------------------------------------------------------

    /// The reference gate is satisfied: motion controls are enabled.
    pub fn referenced(&self) -> bool {
        self.gate_reason.is_none()
    }

    /// The machine is on (and the link is up): Home is enabled.
    pub fn machine_on(&self) -> bool {
        self.link_state == LinkState::Connected
            && self.status.as_ref().is_some_and(|s| s.machine_on)
    }

    pub fn has_errors(&self) -> bool {
        self.program
            .as_ref()
            .is_some_and(|p| p.preflight_errors() > 0)
    }

    pub fn is_proven(&self) -> bool {
        self.program
            .as_ref()
            .and_then(|p| p.text.as_deref())
            .is_some_and(|t| !t.is_empty() && self.proven.is_proven(t))
    }

    pub fn proven_label(&self) -> String {
        let name = self
            .program
            .as_ref()
            .filter(|p| p.text.is_some())
            .map(|p| p.name.as_str());
        handler::proven_label(name, self.is_proven())
    }

    /// Unproven programs run single block, and the box cannot be unticked.
    pub fn single_block_forced(&self) -> bool {
        self.program.as_ref().is_some_and(|p| p.text.is_some()) && !self.is_proven()
    }

    pub fn single_block_effective(&self) -> bool {
        self.single_block || self.single_block_forced()
    }

    /// Cycle start is enabled.
    pub fn start_enabled(&self) -> bool {
        let loaded = self
            .program
            .as_ref()
            .is_some_and(|p| p.loaded && p.text.is_some());
        loaded
            && self
                .ladder
                .start_enabled(self.gate_reason, self.has_errors())
    }

    fn program_name(&self) -> String {
        self.program
            .as_ref()
            .map(|p| p.name.clone())
            .unwrap_or_else(|| "?".into())
    }

    // -------------------------------------------------------------------------
    // The channels
    // -------------------------------------------------------------------------

    /// Everything from the link, then the timers.
    pub fn update(&mut self, now_ms: f64, link: &mut dyn MachineLink, prefs: &mut dyn Prefs) {
        self.now_ms = now_ms;
        for inc in link.poll() {
            self.apply(inc, link);
        }
        if let Some(proves) = self.ladder.poll_finish(now_ms) {
            self.finish_run(proves, prefs);
        }
    }

    /// One thing from the link.
    pub fn apply(&mut self, inc: Incoming, link: &mut dyn MachineLink) {
        match inc {
            Incoming::Link(state) => {
                if state != self.link_state {
                    match &state {
                        LinkState::Connected => self.log("Connected to the machine."),
                        LinkState::Lost(why) => {
                            let why = why.clone();
                            self.log(format!("Link: {why}"));
                            if let Some(m) = self.ladder.spoil("the link to the machine dropped") {
                                self.log(m);
                            }
                        }
                        LinkState::Connecting => {}
                    }
                    let first =
                        self.link_state == LinkState::Connecting && state == LinkState::Connected;
                    self.link_state = state;
                    if first {
                        self.startup(link);
                    }
                    // Re-run the gate: a lost link means the machine is off
                    // as far as this screen can act on it.
                    if let Some(st) = self.status.clone() {
                        self.status_in(st, link);
                    }
                }
            }
            Incoming::Status(st) => self.status_in(*st, link),
            Incoming::Event(e) => self.event_in(e),
            Incoming::Error(e) => {
                self.log(e.text.clone());
                if matches!(e.kind, ErrorKind::Machine | ErrorKind::Operator) {
                    if let Some(m) = self.ladder.spoil("an error was reported") {
                        self.log(m);
                    }
                }
            }
            Incoming::Reply(r) => self.reply_in(r, link),
        }
    }

    /// `_periodic`: the reference gate, the belt question, the spindle watch.
    fn status_in(&mut self, st: Status, link: &mut dyn MachineLink) {
        let on = self.link_state == LinkState::Connected && st.machine_on;
        self.gate_reason = self.gate.update(st.estop, on, &st.homed);
        if self.power.update(on) {
            self.ask_belt = true;
        }
        if st.estop && !self.prev_estop {
            if let Some(m) = self.ladder.spoil("E-stop") {
                self.log(m);
            }
        }
        self.prev_estop = st.estop;
        if st.program_running() {
            self.ladder.observe_program_running();
        }
        let busy = st.run.busy();
        if self.prev_busy && !busy {
            self.ladder.interp_idle(self.now_ms);
        }
        self.prev_busy = busy;

        if let Some((msg, action)) = self.spindle.update(
            self.belt,
            st.spindle_rpm,
            st.threading,
            st.auto_running(),
            st.paused(),
        ) {
            self.log(msg);
            match action {
                SpindleAction::Pause => link.send(Request::Hold),
                SpindleAction::StopSpindle => self.spindle_stop_pending = true,
                SpindleAction::None => {}
            }
        }
        if emco_rules::spindle_violation(self.belt, st.spindle_rpm, st.threading).is_none() {
            self.spindle_stop_pending = false;
        }
        // The machine takes MDI only when idle, so the stop waits for that.
        if self.spindle_stop_pending && st.run == RunState::Idle && st.spindle_on {
            self.spindle_stop_pending = false;
            link.send(Request::Mdi {
                lines: vec!["M5".into()],
                tag: MdiTag::Spindle,
            });
        }
        self.status = Some(st);
    }

    fn event_in(&mut self, e: MachineEvent) {
        match e {
            MachineEvent::Sent { line, text } => {
                if self.show_sent {
                    match line {
                        Some(l) => self.log(format!("→ {}: {text}", l + 1)),
                        None => self.log(format!("→ {text}")),
                    }
                }
            }
            MachineEvent::Acked { .. } => {}
            MachineEvent::State { from, to } => {
                self.log(format!("Machine: {} → {}", from.name(), to.name()));
            }
            MachineEvent::Lagged(n) => {
                self.log(format!(
                    "Missed {n} machine events: the reference and G7/G8 are re-read from the machine's own flags."
                ));
                if let Some(m) = self.ladder.spoil("the screen missed machine events") {
                    self.log(m);
                }
            }
        }
    }

    fn reply_in(&mut self, r: Reply, link: &mut dyn MachineLink) {
        match r {
            Reply::Profile(p) => {
                self.machine = machine_from_profile(&p.0);
                self.profile = p.0;
                self.run_preflight();
            }
            Reply::Programs(list) => self.programs = list,
            Reply::LoadedProgram(Some(name)) => {
                self.program = Some(LoadedProgram::new(name.clone(), true, Vec::new()));
                link.send(Request::ReadProgram(name));
            }
            Reply::LoadedProgram(None) => {}
            Reply::Loaded {
                name,
                loaded,
                diagnostics,
            } => {
                self.program = Some(LoadedProgram::new(name.clone(), loaded, diagnostics));
                self.goto_line = Some(0);
                link.send(Request::ReadProgram(name));
            }
            Reply::ProgramText { name, text } => {
                if self.pending_edit.as_deref() == Some(name.as_str()) {
                    self.pending_edit = None;
                    self.editor_name = name.clone();
                    self.editor_text = text.clone();
                }
                if self
                    .program
                    .as_ref()
                    .is_some_and(|p| p.name == name && p.text.is_none())
                {
                    self.file_loaded(text);
                }
            }
            Reply::Written(name) => {
                self.log(format!("Saved {name} on the machine."));
                link.send(Request::ListPrograms);
            }
            Reply::Deleted(name) => {
                self.log(format!("Deleted {name}."));
                link.send(Request::ListPrograms);
            }
            Reply::MdiAccepted(MdiTag::Wizard) => self.wizard.next(),
            Reply::MdiAccepted(_) => {}
            Reply::Accepted(what) => {
                if what.starts_with("fault") {
                    self.log(format!("Training {what} injected."));
                }
            }
        }
    }

    /// `_file_loaded`: the program's text is here.
    fn file_loaded(&mut self, text: String) {
        let Some(p) = self.program.as_mut() else {
            return;
        };
        let parsed = emco_sender_core::gcode::Program::parse(&text);
        p.backplot = Some(emco_sender_core::geom::backplot(&parsed, None));
        p.lines = text.lines().map(str::to_string).collect();
        p.text = Some(text);
        let name = p.name.clone();
        self.ladder.program_loaded();
        self.toolpath.request_fit();
        self.log(format!("Loaded {name}"));
        self.run_preflight();
    }

    /// `_run_preflight`.
    pub fn run_preflight(&mut self) {
        let (belt, machine) = (self.belt, self.machine);
        let Some(p) = self.program.as_mut() else {
            return;
        };
        let Some(text) = p.text.as_deref() else {
            return;
        };
        p.findings = emco_rules::preflight(text, belt, &machine);
        if p.preflight_errors() > 0 {
            self.tab = Tab::Preflight;
        }
    }

    /// `_finish_run`.
    fn finish_run(&mut self, proves: bool, prefs: &mut dyn Prefs) {
        let Some(p) = self.program.as_ref() else {
            return;
        };
        let Some(text) = p.text.clone() else { return };
        if proves && !text.is_empty() && !self.proven.is_proven(&text) {
            let name = p.name.clone();
            self.proven.mark_proven(&text, &name);
            prefs.set(PREF_PROVEN, &self.proven.to_json());
            self.log(handler::PROVEN_LOG);
        }
    }

    // -------------------------------------------------------------------------
    // Operator actions
    // -------------------------------------------------------------------------

    /// `_set_belt`.
    pub fn set_belt(&mut self, belt: Belt, announce: bool, prefs: &mut dyn Prefs) {
        self.belt = belt;
        prefs.set(PREF_BELT, &belt.number().to_string());
        self.rpm = emco_rules::clamp_rpm(belt, self.rpm);
        if announce {
            self.log(handler::belt_log(belt));
        }
        self.run_preflight();
    }

    /// `_home_all`.
    pub fn home(&mut self, link: &mut dyn MachineLink) {
        if !self.machine_on() {
            return;
        }
        self.log(handler::home_log(&self.machine));
        link.send(Request::Home);
    }

    /// `_spindle`: M3 (1) or M4 (-1) at the box's speed, clamped to the belt.
    pub fn spindle(&mut self, direction: i8, link: &mut dyn MachineLink) {
        if !self.referenced() {
            return;
        }
        let rpm = emco_rules::clamp_rpm(self.belt, self.rpm);
        let m = if direction < 0 { "M4" } else { "M3" };
        link.send(Request::Mdi {
            lines: vec![format!("{m} S{}", emco_rules::py::g(rpm))],
            tag: MdiTag::Spindle,
        });
    }

    pub fn spindle_stop(&mut self, link: &mut dyn MachineLink) {
        link.send(Request::Mdi {
            lines: vec!["M5".into()],
            tag: MdiTag::Spindle,
        });
    }

    /// `_jog`: one increment; X- toward the centreline, Z- toward the chuck.
    pub fn jog(&mut self, axis: Axis, direction: f64, link: &mut dyn MachineLink) {
        if !self.machine_on() || !self.referenced() {
            return;
        }
        let distance = handler::JOG_INCREMENTS[self.jog_increment].1;
        if distance == 0.0 {
            self.log("Continuous jog is not available on this machine link: choose an increment.");
            return;
        }
        link.send(Request::Jog {
            axis,
            delta_mm: direction * distance,
            feed_mm_min: self.jog_rate_mm_s * 60.0,
        });
    }

    /// The MDI line's Enter.
    pub fn mdi_submit(&mut self, link: &mut dyn MachineLink) {
        let line = self.mdi.trim().to_string();
        if line.is_empty() || !self.referenced() {
            return;
        }
        self.mdi_history.push(line.clone());
        self.mdi.clear();
        link.send(Request::Mdi {
            lines: vec![line],
            tag: MdiTag::Operator,
        });
    }

    /// `_cycle_start`.
    pub fn cycle_start(&mut self, link: &mut dyn MachineLink) {
        let loaded = self
            .program
            .as_ref()
            .is_some_and(|p| p.loaded && p.text.is_some());
        if !loaded {
            self.log("Cycle start refused: no program is loaded on the machine.");
            return;
        }
        let (idle, paused, running) = match &self.status {
            Some(s) => (s.interp_idle(), s.paused(), s.run == RunState::Running),
            None => (true, false, false),
        };
        let proven = self.is_proven();
        let has_errors = self.has_errors();
        match self
            .ladder
            .cycle_start(self.gate_reason, has_errors, idle, proven)
        {
            CycleStart::Refused(m) => self.log(m),
            CycleStart::Go {
                first_run_feed,
                starting,
            } => {
                // Not in the handler (emco_lathe has no dry run): a dry run's
                // blocks never reach the machine, so it proves nothing.
                let dry = self.dry_run || self.status.as_ref().is_some_and(|s| s.dry_run);
                if starting && dry {
                    if let Some(m) = self.ladder.spoil("dry run: nothing reaches the machine") {
                        self.log(m);
                    }
                }
                if let Some(feed) = first_run_feed {
                    if self.caps.feed_override {
                        link.send(Request::FeedOverride(feed));
                    } else if !self.feed_note_logged {
                        self.feed_note_logged = true;
                        self.log(format!(
                            "This machine link has no feed override, so the first run cannot go at {feed}%: \
                             it is single block at the programmed feed."
                        ));
                    }
                }
                let single = self.single_block_effective();
                link.send(Request::SingleBlock(single));
                if single {
                    // Idle: start, holding after the first block. Held: the
                    // next block. Running: the block in flight finishes and
                    // the machine holds by itself.
                    if !running {
                        link.send(Request::Run);
                    }
                } else if paused {
                    link.send(Request::Resume);
                } else {
                    link.send(Request::Run);
                }
            }
        }
    }

    /// Pause / resume.
    pub fn pause_resume(&mut self, link: &mut dyn MachineLink) {
        if self.status.as_ref().is_some_and(Status::paused) {
            link.send(Request::Resume);
        } else {
            link.send(Request::Hold);
        }
    }

    /// `_abort`. Advisory: not a safety function.
    pub fn abort(&mut self, link: &mut dyn MachineLink) {
        if let Some(m) = self.ladder.spoil("stopped by operator") {
            self.log(m);
        }
        link.send(Request::Abort);
    }

    pub fn set_dry_run(&mut self, on: bool, link: &mut dyn MachineLink) {
        self.dry_run = on;
        link.send(Request::DryRun(on));
    }

    /// `_override_toggled`.
    pub fn set_override(&mut self, checked: bool) {
        let name = self.program_name();
        if let Some(m) = self.ladder.set_override(checked, &name) {
            self.log(m);
        }
    }

    pub fn fault(&mut self, kind: &str, link: &mut dyn MachineLink) {
        link.send(Request::Fault(kind.into()));
    }

    /// `_wizard_do`.
    pub fn wizard_do(&mut self, link: &mut dyn MachineLink) {
        let Some((_, action)) = self.wizard.current().button else {
            return;
        };
        if !self.referenced() {
            return;
        }
        if matches!(action, WizardAction::ToolX | WizardAction::ToolZ) && !self.caps.tool_table {
            self.log("Touch-off: this machine link has no tool table, so tool offsets cannot be set here.");
            return;
        }
        let (tool, diameter_mode) = self
            .status
            .as_ref()
            .map(|s| (s.tool, s.diameter_mode))
            .unwrap_or((0, true));
        match handler::wizard_commands(
            action,
            tool,
            diameter_mode,
            self.measured_diameter,
            &self.machine,
        ) {
            Err(m) => self.log(m),
            Ok(cmds) => {
                self.log(handler::wizard_log(&cmds));
                link.send(Request::Mdi {
                    lines: cmds,
                    tag: MdiTag::Wizard,
                });
            }
        }
    }

    pub fn load(&mut self, name: &str, link: &mut dyn MachineLink) {
        link.send(Request::LoadProgram(name.into()));
    }

    pub fn edit(&mut self, name: &str, link: &mut dyn MachineLink) {
        self.pending_edit = Some(name.into());
        link.send(Request::ReadProgram(name.into()));
    }

    pub fn delete(&mut self, name: &str, link: &mut dyn MachineLink) {
        link.send(Request::DeleteProgram(name.into()));
    }

    /// Saves the editor's text on the machine.
    pub fn save_editor(&mut self, link: &mut dyn MachineLink) {
        let name = self.editor_name.trim().to_string();
        if !crate::kd240::valid_program_name(&name) {
            self.log(format!(
                "Cannot save {name:?}: a name is 1-64 of A-Z a-z 0-9 . _ - and starts with a letter or digit."
            ));
            return;
        }
        link.send(Request::WriteProgram {
            name,
            text: self.editor_text.clone(),
        });
    }

    /// The token for changes. Kept in the browser, never logged.
    pub fn apply_token(&mut self, link: &mut dyn MachineLink, prefs: &mut dyn Prefs) {
        let t = self.token_input.trim().to_string();
        if t.is_empty() {
            link.set_token(None);
            prefs.remove(PREF_TOKEN);
            self.token_set = false;
            self.log("Token cleared.");
        } else {
            link.set_token(Some(t.clone()));
            prefs.set(PREF_TOKEN, &t);
            self.token_set = true;
            self.log("Token set (kept in this browser).");
        }
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::kd240::CAPABILITIES;

    /// A link that records what it was sent and replays what it is given.
    #[derive(Default)]
    pub struct MockLink {
        pub sent: Vec<Request>,
        pub inbox: Vec<Incoming>,
        pub token: Option<String>,
    }

    impl MachineLink for MockLink {
        fn send(&mut self, req: Request) {
            self.sent.push(req);
        }
        fn poll(&mut self) -> Vec<Incoming> {
            std::mem::take(&mut self.inbox)
        }
        fn capabilities(&self) -> Capabilities {
            CAPABILITIES
        }
        fn set_token(&mut self, token: Option<String>) {
            self.token = token;
        }
    }

    pub fn status(run: RunState, estop: bool, homed: bool) -> Status {
        Status {
            machine_on: run != RunState::Disconnected && !estop,
            run,
            estop,
            homed: vec![homed],
            pos: Position::default(),
            sim: None,
            current_line: None,
            tool: 1,
            spindle_rpm: 0.0,
            spindle_on: false,
            feed_mm_min: 0.0,
            modal: vec![],
            diameter_mode: true,
            diameter_mode_known: false,
            threading: false,
            alarms: vec![],
            single_block: false,
            dry_run: false,
            transport: "virtual".into(),
        }
    }

    const PROGRAM: &str = "( Stock : 12 mm alu )\n( Tools : T1 turning )\nG21 G7\nG53 G0 X96\nM5\nG53 G0 X96 Z0\nM30\n";

    fn connected() -> (Screen, MockLink, MemoryPrefs) {
        let prefs = MemoryPrefs::default();
        let mut s = Screen::new(&prefs, CAPABILITIES);
        let mut link = MockLink::default();
        s.apply(Incoming::Link(LinkState::Connected), &mut link);
        (s, link, prefs)
    }

    fn feed(s: &mut Screen, link: &mut MockLink, st: Status) {
        s.apply(Incoming::Status(Box::new(st)), link);
    }

    fn referenced() -> (Screen, MockLink, MemoryPrefs) {
        let (mut s, mut link, prefs) = connected();
        feed(&mut s, &mut link, status(RunState::Idle, false, false));
        feed(&mut s, &mut link, status(RunState::Idle, false, true));
        (s, link, prefs)
    }

    fn with_program(s: &mut Screen, link: &mut MockLink, text: &str) {
        s.apply(
            Incoming::Reply(Reply::Loaded {
                name: "p.nc".into(),
                loaded: true,
                diagnostics: vec![],
            }),
            link,
        );
        s.apply(
            Incoming::Reply(Reply::ProgramText {
                name: "p.nc".into(),
                text: text.into(),
            }),
            link,
        );
    }

    #[test]
    fn connecting_asks_for_profile_programs_and_the_loaded_program() {
        let (s, link, _) = connected();
        assert_eq!(
            link.sent,
            vec![
                Request::Profile,
                Request::ListPrograms,
                Request::LoadedProgram
            ]
        );
        assert!(!s.referenced());
    }

    #[test]
    fn the_belt_is_asked_at_machine_on_and_remembered() {
        let (mut s, mut link, mut prefs) = connected();
        feed(&mut s, &mut link, status(RunState::Idle, false, false));
        assert!(s.ask_belt);
        s.ask_belt = false;
        s.set_belt(Belt::new(1).unwrap(), true, &mut prefs);
        assert_eq!(prefs.get(PREF_BELT).as_deref(), Some("1"));
        assert_eq!(s.rpm, 800.0);
        assert_eq!(s.log.back().unwrap(), "Belt 1 fitted: 130–1300 rpm.");
        // E-stop and reset: on again, asked again.
        feed(&mut s, &mut link, status(RunState::Alarm, true, true));
        feed(&mut s, &mut link, status(RunState::Idle, false, true));
        assert!(s.ask_belt);
        assert_eq!(Screen::new(&prefs, CAPABILITIES).belt.number(), 1);
    }

    #[test]
    fn motion_is_gated_until_referenced_and_again_after_an_estop() {
        let (mut s, mut link, _) = connected();
        feed(&mut s, &mut link, status(RunState::Idle, false, false));
        link.sent.clear();
        s.jog(Axis::X, 1.0, &mut link);
        s.mdi = "G0 X10".into();
        s.mdi_submit(&mut link);
        assert!(link.sent.is_empty(), "gated: {:?}", link.sent);
        feed(&mut s, &mut link, status(RunState::Idle, false, true));
        assert!(s.referenced());
        s.jog(Axis::Z, -1.0, &mut link);
        assert_eq!(
            link.sent,
            vec![Request::Jog {
                axis: Axis::Z,
                delta_mm: -1.0,
                feed_mm_min: 60.0
            }]
        );
        feed(&mut s, &mut link, status(RunState::Alarm, true, true));
        feed(&mut s, &mut link, status(RunState::Idle, false, true));
        assert_eq!(
            s.gate_reason,
            Some("Position lost at E-stop — Home All again")
        );
        // A lost link counts as machine off.
        s.apply(Incoming::Link(LinkState::Lost("x".into())), &mut link);
        assert_eq!(
            s.gate_reason,
            Some("Machine off — switch on, then reference")
        );
    }

    #[test]
    fn an_unproven_program_runs_single_block_and_a_clean_finish_proves_it() {
        let (mut s, mut link, mut prefs) = referenced();
        with_program(&mut s, &mut link, PROGRAM);
        assert_eq!(s.program.as_ref().unwrap().findings, vec![]);
        assert!(s.single_block_forced() && s.start_enabled());
        link.sent.clear();
        s.cycle_start(&mut link);
        assert_eq!(link.sent, vec![Request::SingleBlock(true), Request::Run]);
        assert!(s.log.iter().any(|l| l.contains("no feed override")));
        let mut running = status(RunState::Running, false, true);
        running.current_line = Some(3);
        feed(&mut s, &mut link, running);
        feed(&mut s, &mut link, status(RunState::Idle, false, true));
        s.update(5000.0, &mut link, &mut prefs);
        assert!(s.is_proven(), "{:?}", s.log);
        assert!(!s.single_block_forced());
        assert!(prefs.get(PREF_PROVEN).unwrap().contains("p.nc"));
        // Any edit un-proves it.
        with_program(&mut s, &mut link, &format!("{PROGRAM} "));
        assert!(!s.is_proven());
    }

    #[test]
    fn an_aborted_run_does_not_prove() {
        let (mut s, mut link, mut prefs) = referenced();
        with_program(&mut s, &mut link, PROGRAM);
        s.cycle_start(&mut link);
        let mut running = status(RunState::Running, false, true);
        running.current_line = Some(3);
        feed(&mut s, &mut link, running);
        s.abort(&mut link);
        assert_eq!(link.sent.last(), Some(&Request::Abort));
        feed(&mut s, &mut link, status(RunState::Idle, false, true));
        s.update(5000.0, &mut link, &mut prefs);
        assert!(!s.is_proven());
        assert!(s
            .log
            .iter()
            .any(|l| l.contains("will not prove the program (stopped by operator)")));
    }

    #[test]
    fn a_dry_run_does_not_prove() {
        let (mut s, mut link, mut prefs) = referenced();
        with_program(&mut s, &mut link, PROGRAM);
        s.set_dry_run(true, &mut link);
        s.cycle_start(&mut link);
        assert!(s
            .log
            .iter()
            .any(|l| l.contains("dry run: nothing reaches the machine")));
        let mut running = status(RunState::Running, false, true);
        running.current_line = Some(3);
        feed(&mut s, &mut link, running);
        feed(&mut s, &mut link, status(RunState::Idle, false, true));
        s.update(5000.0, &mut link, &mut prefs);
        assert!(!s.is_proven());
    }

    #[test]
    fn preflight_errors_block_cycle_start_unless_overridden() {
        let (mut s, mut link, _) = referenced();
        with_program(&mut s, &mut link, "G21 G7\nG53 G0 X48\nM30\n");
        assert_eq!(s.tab, Tab::Preflight);
        assert!(!s.start_enabled());
        link.sent.clear();
        s.cycle_start(&mut link);
        assert!(link.sent.is_empty());
        assert_eq!(
            s.log.back().unwrap(),
            "Cycle start refused: the preflight found errors."
        );
        s.set_override(true);
        assert!(s.start_enabled());
    }

    #[test]
    fn a_belt_change_reruns_the_preflight() {
        let (mut s, mut link, mut prefs) = referenced();
        with_program(&mut s, &mut link, &format!("{PROGRAM}S1400 M3\n"));
        assert!(!s.has_errors());
        s.set_belt(Belt::new(1).unwrap(), true, &mut prefs);
        assert!(s.has_errors());
    }

    #[test]
    fn spindle_violations_hold_a_program_or_stop_the_spindle_when_idle() {
        let (mut s, mut link, _) = referenced();
        link.sent.clear();
        let mut st = status(RunState::Running, false, true);
        st.current_line = Some(4);
        st.spindle_rpm = 500.0;
        st.spindle_on = true;
        st.threading = true;
        feed(&mut s, &mut link, st);
        assert_eq!(link.sent, vec![Request::Hold]);
        link.sent.clear();
        let mut st = status(RunState::Running, false, true);
        st.spindle_rpm = 2500.0;
        st.spindle_on = true;
        feed(&mut s, &mut link, st.clone());
        assert!(link.sent.is_empty(), "MDI still running: wait for idle");
        st.run = RunState::Idle;
        feed(&mut s, &mut link, st);
        assert_eq!(
            link.sent,
            vec![Request::Mdi {
                lines: vec!["M5".into()],
                tag: MdiTag::Spindle
            }]
        );
    }

    #[test]
    fn spindle_buttons_clamp_to_the_belt() {
        let (mut s, mut link, _) = referenced();
        link.sent.clear();
        s.rpm = 2500.0;
        s.spindle(-1, &mut link);
        assert_eq!(
            link.sent,
            vec![Request::Mdi {
                lines: vec!["M4 S2000".into()],
                tag: MdiTag::Spindle
            }]
        );
    }

    #[test]
    fn the_wizard_sends_mdi_and_advances_when_it_is_accepted() {
        let (mut s, mut link, _) = referenced();
        s.wizard.show(2);
        link.sent.clear();
        s.wizard_do(&mut link);
        assert_eq!(
            link.sent,
            vec![Request::Mdi {
                lines: vec!["G10 L20 P1 Z0".into()],
                tag: MdiTag::Wizard
            }]
        );
        assert_eq!(s.wizard.step, 2);
        s.apply(
            Incoming::Reply(Reply::MdiAccepted(MdiTag::Wizard)),
            &mut link,
        );
        assert_eq!(s.wizard.step, 3);
        // Tool offsets need a tool table, which this link lacks.
        s.wizard = Wizard::start(emco_rules::screen::Procedure::ToolOffsets);
        s.wizard.show(1);
        link.sent.clear();
        s.wizard_do(&mut link);
        assert!(link.sent.is_empty());
        assert!(s.log.back().unwrap().contains("no tool table"));
    }

    #[test]
    fn the_token_is_kept_but_never_logged() {
        let (mut s, mut link, mut prefs) = connected();
        s.token_input = " sekrit ".into();
        s.apply_token(&mut link, &mut prefs);
        assert_eq!(link.token.as_deref(), Some("sekrit"));
        assert_eq!(prefs.get(PREF_TOKEN).as_deref(), Some("sekrit"));
        assert!(s.log.iter().all(|l| !l.contains("sekrit")));
        s.token_input.clear();
        s.apply_token(&mut link, &mut prefs);
        assert_eq!(link.token, None);
        assert_eq!(prefs.get(PREF_TOKEN), None);
    }

    #[test]
    fn errors_spoil_the_run_and_the_log_is_bounded() {
        let (mut s, mut link, _) = referenced();
        with_program(&mut s, &mut link, PROGRAM);
        s.cycle_start(&mut link);
        s.apply(
            Incoming::Error(ErrorMsg {
                kind: ErrorKind::Machine,
                text: "ALARM LIMITX: X axis limit switch".into(),
            }),
            &mut link,
        );
        assert!(s.log.iter().any(|l| l.contains("an error was reported")));
        for i in 0..500 {
            s.log(format!("{i}"));
        }
        assert_eq!(s.log.len(), LOG_LINES);
    }
}
