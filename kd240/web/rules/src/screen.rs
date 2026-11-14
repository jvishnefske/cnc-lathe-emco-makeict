//! The decisions the emco_lathe *handler* makes around the rules.
//!
//! `emco_lathe_handler.py` is mostly Qt wiring, but a few of its choices are
//! rules in all but name: the touch-off wizard's steps and the MDI it sends,
//! when cycle start is refused, the first-run feed, when a finished run
//! proves a program, what to do about a spindle violation, and when to ask
//! for the belt. They are here, without Qt or a clock (time comes in as
//! milliseconds), so a web screen can make the same decisions and they can
//! be tested natively. Wording is the handler's.

use crate::{belt_range, py, Belt, Machine, BELT_RANGES};

/// Single-block first runs start slow (doc 40 §10.3: "feed override low").
pub const FIRST_RUN_FEED_OVERRIDE: u32 = 25;

/// The jog increments the handler offers, mm; 0 is continuous.
pub const JOG_INCREMENTS: [(&str, f64); 4] = [
    ("Continuous", 0.0),
    ("1 mm", 1.0),
    ("0.1 mm", 0.1),
    ("0.01 mm", 0.01),
];

/// How long after the interpreter goes idle the handler waits before
/// deciding whether the run proved the program: an error can arrive in the
/// same instant (sim/acceptance-test.py hit exactly this).
pub const FINISH_DELAY_MS: f64 = 700.0;

/// The spindle box's starting speed: doc 40 §9.1's "moderate speed" for
/// small aluminium.
pub const DEFAULT_RPM: f64 = 800.0;

// =============================================================================
// The touch-off wizard — the steps are doc 40 §9, nearly word for word
// =============================================================================

/// What a wizard step's button does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WizardAction {
    WorkZ,
    Retract,
    VerifyZ,
    ToolX,
    ToolZ,
    VerifyTool,
}

/// One step: title, instructions, and the button (label and action), if any.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WizardStep {
    pub title: &'static str,
    pub text: &'static str,
    pub button: Option<(&'static str, WizardAction)>,
}

const fn step(
    title: &'static str,
    text: &'static str,
    button: Option<(&'static str, WizardAction)>,
) -> WizardStep {
    WizardStep {
        title,
        text,
        button,
    }
}

/// §9.1. Keep the wording in step with docs/40-operator-guide.md; if one
/// changes, so does the other.
pub const WORK_Z_STEPS: [WizardStep; 5] = [
    step(
        "Set the work offset (Z) — 1/5",
        "Reference the machine and chuck your stock.\n\n\
         Select a turning tool (T1) and start the spindle at a moderate speed \
         for the material, e.g. M3 S800 for small aluminium.",
        None,
    ),
    step(
        "Face the end — 2/5",
        "Jog the tool carefully to just touch the rotating face, then take a \
         light facing cut so the face is clean and square.",
        None,
    ),
    step(
        "Do not move Z — 3/5",
        "The tool tip is now exactly at your part's Z0.\n\n\
         Press the button to make this G54 Z0.",
        Some(("Set G54 Z0 here", WizardAction::WorkZ)),
    ),
    step(
        "Retract and stop — 4/5",
        "Retract in X (G53 G0 X48 in radius terms — the button does it), \
         then stop the spindle.",
        Some(("Retract X and stop spindle", WizardAction::Retract)),
    ),
    step(
        "Verify — 5/5",
        "Commanding G0 Z5 should put the tool 5 mm to the right of the fresh \
         face. If it doesn't, stop and redo — do not adjust as you go.",
        Some(("Move to Z5", WizardAction::VerifyZ)),
    ),
];

/// §9.2.
pub const TOOL_STEPS: [WizardStep; 5] = [
    step(
        "Tool offsets — 1/5",
        "With the work offset set, select the tool to measure (T_ M6 in MDI, \
         or it is the tool already in position).",
        None,
    ),
    step(
        "X by cutting — 2/5",
        "Take a light skim cut along the diameter. Retract in Z ONLY — do not \
         move X. Stop the spindle and measure the diameter with a micrometer \
         or calliper. Enter it below.",
        Some(("Set tool X from measured diameter", WizardAction::ToolX)),
    ),
    step(
        "Z by touching — 3/5",
        "Touch the tool tip to the freshly faced end (paper drag or a light \
         scratch).",
        Some(("Set tool Z0 here", WizardAction::ToolZ)),
    ),
    step(
        "Compensation — 4/5",
        "If you will use cutter compensation (G41/G42), check the nose radius \
         and orientation in the Tools tab. Otherwise leave it off (G40).",
        None,
    ),
    step(
        "Verify — 5/5",
        "With a 20 mm bar in the chuck, G0 X20 Z5 must put the tool visibly \
         at 20 mm diameter, 5 mm off the face. It costs seconds and catches \
         decade errors.",
        Some(("Move to X20 (dia) Z5", WizardAction::VerifyTool)),
    ),
];

/// Which procedure the wizard is walking through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Procedure {
    /// §9.1
    WorkZ,
    /// §9.2
    ToolOffsets,
}

/// The wizard's place: a procedure and a step in it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Wizard {
    pub procedure: Procedure,
    pub step: usize,
}

impl Default for Wizard {
    /// The handler opens the tab on §9.1, step 1.
    fn default() -> Self {
        Wizard::start(Procedure::WorkZ)
    }
}

impl Wizard {
    pub fn start(procedure: Procedure) -> Wizard {
        Wizard { procedure, step: 0 }
    }

    pub fn steps(&self) -> &'static [WizardStep] {
        match self.procedure {
            Procedure::WorkZ => &WORK_Z_STEPS,
            Procedure::ToolOffsets => &TOOL_STEPS,
        }
    }

    pub fn current(&self) -> &'static WizardStep {
        &self.steps()[self.step]
    }

    /// `_wizard_show`: any index, clamped into the procedure.
    pub fn show(&mut self, index: isize) {
        let last = self.steps().len() as isize - 1;
        self.step = index.clamp(0, last) as usize;
    }

    pub fn back(&mut self) {
        self.show(self.step as isize - 1);
    }

    pub fn next(&mut self) {
        self.show(self.step as isize + 1);
    }

    pub fn can_go_back(&self) -> bool {
        self.step > 0
    }

    pub fn can_go_next(&self) -> bool {
        self.step < self.steps().len() - 1
    }

    /// The step shows the measured-diameter entry.
    pub fn wants_diameter(&self) -> bool {
        matches!(self.current().button, Some((_, WizardAction::ToolX)))
    }
}

/// `_wizard_do`: the MDI lines a wizard button sends, or the message the
/// handler logs instead of sending anything.
///
/// `tool` is the tool in the spindle (0: none); `diameter_mode` is G7.
pub fn wizard_commands(
    action: WizardAction,
    tool: u32,
    diameter_mode: bool,
    measured_diameter: f64,
    machine: &Machine,
) -> Result<Vec<String>, String> {
    Ok(match action {
        WizardAction::WorkZ => crate::touchoff_work_z(),
        WizardAction::Retract => vec![
            "M5".into(),
            format!(
                "G53 G0 X{}",
                py::g(machine.x_retract * if diameter_mode { 2.0 } else { 1.0 })
            ),
        ],
        WizardAction::VerifyZ => crate::verify_work_z(),
        WizardAction::ToolX | WizardAction::ToolZ => {
            if tool == 0 {
                return Err("No tool in position — select one first (T_ M6).".into());
            }
            let mut cmds = if action == WizardAction::ToolX {
                crate::touchoff_tool_x(tool, measured_diameter, diameter_mode)
                    .map_err(|e| format!("Touch-off: {e}"))?
            } else {
                crate::touchoff_tool_z(tool)
            };
            // A changed table entry only takes effect once re-applied.
            cmds.push("G43".into());
            cmds
        }
        WizardAction::VerifyTool => crate::verify_tool(diameter_mode, crate::VERIFY_BAR_DIAMETER),
    })
}

/// What the handler logs before sending a wizard's MDI.
pub fn wizard_log(cmds: &[String]) -> String {
    format!("Touch-off: {}", cmds.join(" / "))
}

// =============================================================================
// Texts
// =============================================================================

/// The MDI tab's reminder.
pub fn mdi_hint(machine: &Machine) -> String {
    format!(
        "Type a line and press Enter. Remember G7: in diameter mode the full \
         X retract is G53 G0 X{}, not X{}.",
        py::g(machine.x_retract * 2.0),
        py::g(machine.x_retract)
    )
}

/// Logged when Home All is pressed.
pub fn home_log(machine: &Machine) -> String {
    format!(
        "Referencing: X drives out to X{} (radius), Z to Z{}. Keep the path clear.",
        py::g(machine.x_retract),
        py::g(machine.z_park)
    )
}

/// Logged when the operator says which belt is fitted.
pub fn belt_log(belt: Belt) -> String {
    let (lo, hi) = belt_range(belt);
    format!("Belt {} fitted: {}–{} rpm.", belt.number(), lo, hi)
}

/// The belt question's choices, as the handler words them.
pub fn belt_choices() -> Vec<(Belt, String)> {
    BELT_RANGES
        .iter()
        .filter_map(|(b, (lo, hi))| {
            Belt::new(*b).map(|belt| (belt, format!("Belt {b}  ({lo}–{hi} rpm)")))
        })
        .collect()
}

/// The banner over the machine strip.
pub fn banner(gate_reason: Option<&str>) -> &str {
    gate_reason.unwrap_or("REFERENCED — ready")
}

/// The run box's proven label.
pub fn proven_label(name: Option<&str>, proven: bool) -> String {
    match name {
        None => "No program loaded.".into(),
        Some(name) if proven => format!("{name} — PROVEN in this simulator."),
        Some(name) => format!(
            "{name} — NOT PROVEN. The first run is single block at {FIRST_RUN_FEED_OVERRIDE}% feed; \
             a clean finish marks it proven. Any edit un-proves it (doc 40 §10)."
        ),
    }
}

/// The preflight list when it found nothing.
pub const PREFLIGHT_CLEAN: &str =
    "Preflight: nothing found. Still read the header and check your setup.";

/// The preflight override's label.
pub const OVERRIDE_LABEL: &str = "Run despite errors (training only — it is logged)";

// =============================================================================
// Machine-on: ask for the belt
// =============================================================================

/// Asks for the belt at every machine-on (doc 40 §6): `update` is true on
/// the transition from off to on.
#[derive(Clone, Copy, Debug, Default)]
pub struct PowerEdge {
    was_on: bool,
}

impl PowerEdge {
    pub fn update(&mut self, on: bool) -> bool {
        let rising = on && !self.was_on;
        self.was_on = on;
        rising
    }
}

// =============================================================================
// The spindle watch
// =============================================================================

/// What to do about a spindle violation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpindleAction {
    /// A program is running: feed hold.
    Pause,
    /// Nothing is running: stop the spindle.
    StopSpindle,
    /// A program is already paused: only log.
    None,
}

/// `_watch_spindle`: log and act once per new violation.
#[derive(Clone, Debug, Default)]
pub struct SpindleWatch {
    last_violation: Option<String>,
}

impl SpindleWatch {
    /// `rpm` is the actual speed (speed times override). Returns the log
    /// line (`"SPINDLE: ..."`) and the action, the first time a given
    /// violation is seen.
    pub fn update(
        &mut self,
        belt: Belt,
        rpm: f64,
        threading: bool,
        auto_running: bool,
        paused: bool,
    ) -> Option<(String, SpindleAction)> {
        let violation = crate::spindle_violation(belt, rpm, threading);
        let out = match &violation {
            Some(v) if Some(v) != self.last_violation.as_ref() => {
                let action = if auto_running && !paused {
                    SpindleAction::Pause
                } else if !auto_running {
                    SpindleAction::StopSpindle
                } else {
                    SpindleAction::None
                };
                Some((format!("SPINDLE: {v}"), action))
            }
            _ => None,
        };
        self.last_violation = violation;
        out
    }
}

// =============================================================================
// The proving ladder's run tracking
// =============================================================================

/// What cycle start should do.
#[derive(Clone, Debug, PartialEq)]
pub enum CycleStart {
    /// Refused; log this.
    Refused(String),
    /// Go. `first_run_feed` is the feed override to set first (an unproven
    /// program starting from idle); `starting` is true when the interpreter
    /// was idle, so this begins a run (rather than stepping or resuming one).
    Go {
        first_run_feed: Option<u32>,
        starting: bool,
    },
}

/// `_cycle_start`, `_run_spoiled`, `_interp_idle` and `_finish_run`: whether
/// the run under way can prove the program.
#[derive(Clone, Debug, Default)]
pub struct RunLadder {
    running_program: bool,
    run_clean: bool,
    preflight_override: bool,
    /// Port note: the handler has no such flag. A web link cannot promise
    /// that a run command started a program (the service ignores it when
    /// nothing is loaded), so a run proves a program only if the program was
    /// seen running in between. Without this, cycle start with no program
    /// followed by any MDI would mark the program proven.
    seen_running: bool,
    finish_at_ms: Option<f64>,
}

impl RunLadder {
    pub fn preflight_override(&self) -> bool {
        self.preflight_override
    }

    pub fn running_program(&self) -> bool {
        self.running_program
    }

    /// The handler unticks the override whenever a file is loaded.
    pub fn program_loaded(&mut self) {
        self.preflight_override = false;
    }

    /// `_override_toggled`: the log line, when switching it on.
    pub fn set_override(&mut self, checked: bool, name: &str) -> Option<String> {
        self.preflight_override = checked;
        checked.then(|| {
            format!("Preflight override ON for {name} — running a program with known errors.")
        })
    }

    /// Cycle start is enabled: the gate is satisfied and errors are not
    /// blocking.
    pub fn start_enabled(&self, gate_reason: Option<&str>, has_errors: bool) -> bool {
        // The handler's `blocked = self._has_errors() and not self.preflight_override`.
        let blocked = has_errors && !self.preflight_override;
        gate_reason.is_none() && !blocked
    }

    /// `_cycle_start`'s decision. `interp_idle`: nothing is running.
    pub fn cycle_start(
        &mut self,
        gate_reason: Option<&str>,
        has_errors: bool,
        interp_idle: bool,
        proven: bool,
    ) -> CycleStart {
        if let Some(reason) = gate_reason {
            return CycleStart::Refused(format!("Cycle start refused: {reason}"));
        }
        if has_errors && !self.preflight_override {
            return CycleStart::Refused("Cycle start refused: the preflight found errors.".into());
        }
        let mut first_run_feed = None;
        if interp_idle {
            if !proven {
                first_run_feed = Some(FIRST_RUN_FEED_OVERRIDE);
            }
            self.running_program = true;
            self.run_clean = !self.preflight_override;
            self.seen_running = false;
            self.finish_at_ms = None;
        }
        CycleStart::Go {
            first_run_feed,
            starting: interp_idle,
        }
    }

    /// The program was seen running (see the port note on the struct).
    pub fn observe_program_running(&mut self) {
        if self.running_program {
            self.seen_running = true;
        }
    }

    /// `_run_spoiled`: an abort, an E-stop or an error. The log line, if
    /// this spoils a run that was going to prove the program.
    pub fn spoil(&mut self, why: &str) -> Option<String> {
        let msg = (self.running_program && self.run_clean)
            .then(|| format!("This run will not prove the program ({why})."));
        self.run_clean = false;
        msg
    }

    /// `_interp_idle`: the interpreter went idle at `now_ms`.
    pub fn interp_idle(&mut self, now_ms: f64) {
        if !self.running_program {
            return;
        }
        self.running_program = false;
        self.finish_at_ms = Some(now_ms + FINISH_DELAY_MS);
    }

    /// `_finish_run`, once its delay has passed: true when the run that just
    /// ended proves the program (the caller checks it is not proven already,
    /// marks it, and logs [`PROVEN_LOG`]). Returns false until then, and
    /// after.
    pub fn poll_finish(&mut self, now_ms: f64) -> Option<bool> {
        match self.finish_at_ms {
            Some(at) if now_ms >= at => {
                self.finish_at_ms = None;
                Some(self.run_clean && self.seen_running)
            }
            _ => None,
        }
    }
}

/// Logged when a run proves a program.
pub const PROVEN_LOG: &str = "Program proven. Next run may go at full feed, without single block.";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DEFAULT_BELT;

    const SIM: Machine = Machine {
        x_retract: 48.0,
        z_park: 0.0,
        tool_change_retracts: true,
    };

    #[test]
    fn wizard_navigation_clamps_like_wizard_show() {
        let mut w = Wizard::default();
        assert_eq!(w.current().title, "Set the work offset (Z) — 1/5");
        assert!(!w.can_go_back());
        w.back();
        assert_eq!(w.step, 0);
        for _ in 0..10 {
            w.next();
        }
        assert_eq!(w.step, 4);
        assert!(!w.can_go_next());
        w = Wizard::start(Procedure::ToolOffsets);
        w.next();
        assert!(w.wants_diameter());
    }

    #[test]
    fn wizard_commands_match_wizard_do() {
        let c = |a, tool, dia, d| wizard_commands(a, tool, dia, d, &SIM);
        assert_eq!(
            c(WizardAction::WorkZ, 0, true, 0.0).unwrap(),
            ["G10 L20 P1 Z0"]
        );
        assert_eq!(
            c(WizardAction::Retract, 0, true, 0.0).unwrap(),
            ["M5", "G53 G0 X96"]
        );
        assert_eq!(
            c(WizardAction::Retract, 0, false, 0.0).unwrap(),
            ["M5", "G53 G0 X48"]
        );
        assert_eq!(c(WizardAction::VerifyZ, 0, true, 0.0).unwrap(), ["G0 Z5"]);
        assert_eq!(
            c(WizardAction::ToolX, 1, true, 11.98).unwrap(),
            ["G8", "G10 L10 P1 X5.9900", "G7", "G43"]
        );
        assert_eq!(
            c(WizardAction::ToolZ, 2, true, 0.0).unwrap(),
            ["G10 L10 P2 Z0", "G43"]
        );
        assert!(c(WizardAction::ToolZ, 0, true, 0.0)
            .unwrap_err()
            .contains("No tool"));
        assert_eq!(
            c(WizardAction::ToolX, 1, true, 0.0).unwrap_err(),
            "Touch-off: measured diameter must be positive"
        );
        assert_eq!(
            c(WizardAction::VerifyTool, 0, false, 0.0).unwrap(),
            ["G0 X10 Z5"]
        );
        assert_eq!(
            wizard_log(&["M5".into(), "G0 Z5".into()]),
            "Touch-off: M5 / G0 Z5"
        );
    }

    #[test]
    fn texts() {
        assert_eq!(
            mdi_hint(&SIM),
            "Type a line and press Enter. Remember G7: in diameter mode the full X retract is G53 G0 X96, not X48."
        );
        assert_eq!(
            home_log(&SIM),
            "Referencing: X drives out to X48 (radius), Z to Z0. Keep the path clear."
        );
        assert_eq!(belt_log(DEFAULT_BELT), "Belt 2 fitted: 200–2000 rpm.");
        assert_eq!(belt_choices()[0].1, "Belt 1  (130–1300 rpm)");
        assert_eq!(proven_label(None, false), "No program loaded.");
        assert!(proven_label(Some("a"), false)
            .contains("NOT PROVEN. The first run is single block at 25% feed"));
    }

    #[test]
    fn belt_is_asked_at_every_machine_on() {
        let mut p = PowerEdge::default();
        assert!(p.update(true));
        assert!(!p.update(true));
        assert!(!p.update(false));
        assert!(p.update(true));
    }

    #[test]
    fn spindle_watch_acts_once_per_violation() {
        let mut w = SpindleWatch::default();
        let b1 = Belt::new(1).unwrap();
        assert_eq!(w.update(b1, 800.0, false, false, false), None);
        let (msg, act) = w.update(b1, 1400.0, false, true, false).unwrap();
        assert!(msg.starts_with("SPINDLE: 1400 rpm is outside belt 1"));
        assert_eq!(act, SpindleAction::Pause);
        assert_eq!(
            w.update(b1, 1400.0, false, true, true),
            None,
            "same violation: once"
        );
        assert_eq!(w.update(b1, 0.0, false, false, false), None);
        assert_eq!(
            w.update(DEFAULT_BELT, 500.0, true, false, false).unwrap().1,
            SpindleAction::StopSpindle
        );
        assert_eq!(
            w.update(DEFAULT_BELT, 600.0, true, true, true).unwrap().1,
            SpindleAction::None
        );
    }

    #[test]
    fn a_clean_run_proves_after_the_delay() {
        let mut l = RunLadder::default();
        assert_eq!(
            l.cycle_start(None, false, true, false),
            CycleStart::Go {
                first_run_feed: Some(25),
                starting: true
            }
        );
        l.observe_program_running();
        l.interp_idle(1000.0);
        assert_eq!(l.poll_finish(1600.0), None);
        assert_eq!(l.poll_finish(1700.0), Some(true));
        assert_eq!(l.poll_finish(1800.0), None);
    }

    #[test]
    fn spoiled_overridden_or_unseen_runs_do_not_prove() {
        let mut l = RunLadder::default();
        l.cycle_start(None, false, true, true);
        l.observe_program_running();
        assert_eq!(
            l.spoil("stopped by operator").as_deref(),
            Some("This run will not prove the program (stopped by operator).")
        );
        assert_eq!(l.spoil("again"), None, "logged once");
        l.interp_idle(0.0);
        assert_eq!(l.poll_finish(700.0), Some(false));

        let mut l = RunLadder::default();
        assert!(!l.start_enabled(None, true));
        assert!(
            matches!(l.cycle_start(None, true, true, false), CycleStart::Refused(m) if m.contains("preflight"))
        );
        assert!(l
            .set_override(true, "a.nc")
            .unwrap()
            .contains("Preflight override ON for a.nc"));
        assert!(l.start_enabled(None, true));
        assert!(!l.start_enabled(Some("x"), false));
        l.cycle_start(None, true, true, false);
        l.observe_program_running();
        l.interp_idle(0.0);
        assert_eq!(
            l.poll_finish(700.0),
            Some(false),
            "an override run never proves"
        );
        l.program_loaded();
        assert!(!l.preflight_override());

        let mut l = RunLadder::default();
        l.cycle_start(None, false, true, false);
        l.interp_idle(0.0);
        assert_eq!(l.poll_finish(700.0), Some(false), "never seen running");
    }

    #[test]
    fn cycle_start_while_running_steps_without_restarting() {
        let mut l = RunLadder::default();
        assert!(matches!(
            l.cycle_start(Some("Not referenced — Home All first"), false, true, false),
            CycleStart::Refused(m) if m == "Cycle start refused: Not referenced — Home All first"
        ));
        l.cycle_start(None, false, true, false);
        l.observe_program_running();
        assert_eq!(
            l.cycle_start(None, false, false, false),
            CycleStart::Go {
                first_run_feed: None,
                starting: false
            }
        );
        assert!(l.running_program());
    }
}
