//! `linuxcnc/sim/emco-pcturn50/tests/test_emco_rules.py`, ported test for
//! test: the same 25 cases, with the same names and the same assertions, run
//! against the Rust port. If one of these and its Python twin disagree, the
//! Python is the reference.

use emco_rules::*;
use std::path::PathBuf;

/// Matches emco-pcturn50.ini: [JOINT_0]HOME 48, [JOINT_1]HOME 0, TOOL_CHANGE_POSITION set.
const SIM: Machine = Machine {
    x_retract: 48.0,
    z_park: 0.0,
    tool_change_retracts: true,
};
const BARE: Machine = Machine {
    x_retract: 48.0,
    z_park: 0.0,
    tool_change_retracts: false,
};

const HEADER: &str = "( Stock : 12 mm alu )\n( Tools : T1 turning )\n";
const GOOD_TAIL: &str = "M5\nG53 G0 X96 Z0\nM30\n";

fn belt(n: u8) -> Belt {
    Belt::new(n).expect("a belt")
}

fn errors(findings: &[Finding]) -> Vec<&Finding> {
    findings
        .iter()
        .filter(|f| f.severity == Severity::Error)
        .collect()
}

fn repo_gcode(name: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../gcode")
        .join(name);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

// -----------------------------------------------------------------------------
// ReferenceGateTest
// -----------------------------------------------------------------------------

#[test]
fn reference_gate_test_power_on_is_gated() {
    let mut g = ReferenceGate::new();
    assert!(g.update(true, false, &[false, false]).is_some());
    assert!(g.update(false, true, &[false, false]).is_some());
}

#[test]
fn reference_gate_test_homing_satisfies() {
    let mut g = ReferenceGate::new();
    g.update(false, true, &[false, false]);
    g.update(false, true, &[true, false]);
    assert_eq!(g.update(false, true, &[true, true]), None);
    assert!(!g.position_lost());
}

#[test]
fn reference_gate_test_estop_regates_even_though_linuxcnc_keeps_homed() {
    let mut g = ReferenceGate::new();
    g.update(false, true, &[false, false]);
    g.update(false, true, &[true, true]);
    g.update(true, false, &[true, true]); // E-stop; homed flags survive
    let reason = g.update(false, true, &[true, true]); // reset + on
    assert!(reason.unwrap().contains("Position lost"));
    assert!(g.position_lost());
}

#[test]
fn reference_gate_test_rehome_after_estop_satisfies() {
    let mut g = ReferenceGate::new();
    g.update(false, true, &[false, false]);
    g.update(false, true, &[true, true]);
    g.update(true, false, &[true, true]);
    g.update(false, true, &[true, true]);
    g.update(false, true, &[false, false]); // unhomed / homing in progress
    assert_eq!(g.update(false, true, &[true, true]), None);
}

#[test]
fn reference_gate_test_homing_while_in_estop_does_not_count() {
    let mut g = ReferenceGate::new();
    g.update(true, false, &[false, false]);
    g.update(true, false, &[true, true]);
    assert!(g.update(false, true, &[true, true]).is_some());
}

#[test]
fn reference_gate_test_machine_off_is_gated() {
    let mut g = ReferenceGate::new();
    g.update(false, true, &[false, false]);
    g.update(false, true, &[true, true]);
    assert!(g
        .update(false, false, &[true, true])
        .unwrap()
        .contains("off"));
}

// -----------------------------------------------------------------------------
// BeltTest
// -----------------------------------------------------------------------------

#[test]
fn belt_test_ranges_match_doc() {
    assert_eq!(belt_range(belt(1)), (130, 1300));
    assert_eq!(belt_range(belt(2)), (200, 2000));
    assert_eq!(belt_range(belt(3)), (300, 3000));
}

#[test]
fn belt_test_clamp() {
    assert_eq!(clamp_rpm(belt(1), 3000.0), 1300.0);
    assert_eq!(clamp_rpm(belt(3), 100.0), 300.0);
    assert_eq!(clamp_rpm(belt(2), 800.0), 800.0);
}

#[test]
fn belt_test_violations() {
    assert_eq!(spindle_violation(belt(2), 0.0, false), None);
    assert_eq!(spindle_violation(belt(2), 1200.0, false), None);
    assert_eq!(spindle_violation(belt(2), -1200.0, false), None); // M4
    assert!(spindle_violation(belt(1), 1400.0, false)
        .unwrap()
        .contains("belt 1"));
    assert!(spindle_violation(belt(2), 500.0, true)
        .unwrap()
        .contains("400"));
    assert_eq!(spindle_violation(belt(2), 350.0, true), None);
}

// -----------------------------------------------------------------------------
// PreflightTest
// -----------------------------------------------------------------------------

#[test]
fn preflight_test_example_programs_are_clean() {
    for name in ["tutorial-part.ngc", "stepped-shaft.ngc"] {
        let text = repo_gcode(name);
        assert_eq!(preflight(&text, belt(2), &SIM), vec![], "program={name}");
    }
}

#[test]
fn preflight_test_stepped_shaft_on_belt_1_flags_speed() {
    let text = repo_gcode("stepped-shaft.ngc");
    let f = preflight(&text, belt(1), &SIM);
    let errs = errors(&f);
    assert!(errs.iter().any(|e| e.message.contains("S1400")), "{errs:?}");
}

#[test]
fn preflight_test_half_retract_under_g7() {
    let prog = format!("{HEADER}G21 G7\nG53 G0 X48\nT1 M6\n{GOOD_TAIL}");
    let f = preflight(&prog, belt(2), &SIM);
    let errs = errors(&f);
    assert_eq!(errs.len(), 1, "{errs:?}");
    assert!(errs[0].message.contains("half way"));
    assert_eq!(errs[0].line, 4);
}

#[test]
fn preflight_test_x48_is_full_retract_in_radius_mode() {
    let prog = format!("{HEADER}G21 G8\nG53 G0 X48\nT1 M6\nM5\nG53 G0 X48 Z0\nM30\n");
    assert_eq!(preflight(&prog, belt(2), &SIM), vec![]);
}

#[test]
fn preflight_test_first_move_not_a_retract() {
    let prog = format!("{HEADER}G21 G7\nG0 X14 Z1\n{GOOD_TAIL}");
    let f = preflight(&prog, belt(2), &SIM);
    let errs = errors(&f);
    assert!(errs[0].message.contains("G53 G0 X96"));
}

#[test]
fn preflight_test_tool_change_with_spindle_on() {
    let prog = format!("{HEADER}G21 G7\nG53 G0 X96\nS800 M3\nT3 M6\n{GOOD_TAIL}");
    let f = preflight(&prog, belt(2), &SIM);
    assert!(errors(&f)
        .iter()
        .any(|e| e.message.contains("spindle running")));
}

#[test]
fn preflight_test_tool_change_without_retract_when_ini_does_not_retract() {
    let prog = format!("{HEADER}G21 G7\nG53 G0 X96\nG0 X14 Z1\nT3 M6\n{GOOD_TAIL}");
    assert_eq!(
        errors(&preflight(&prog, belt(2), &SIM)),
        Vec::<&Finding>::new()
    );
    let f = preflight(&prog, belt(2), &BARE);
    assert!(errors(&f).iter().any(|e| e.message.contains("retract")));
}

#[test]
fn preflight_test_threading_speed() {
    let prog =
        format!("{HEADER}G21 G7\nG53 G0 X96\nS500 M3\nG76 P1 Z-10 I-1 J0.1 K0.6\n{GOOD_TAIL}");
    let f = preflight(&prog, belt(2), &SIM);
    assert!(errors(&f).iter().any(|e| e.message.contains("Threading")));
}

#[test]
fn preflight_test_css_s_is_not_rpm() {
    let prog = format!("{HEADER}G21 G7\nG53 G0 X96\nG96 S5000 D2000 M3\nG97\n{GOOD_TAIL}");
    assert_eq!(
        errors(&preflight(&prog, belt(2), &SIM)),
        Vec::<&Finding>::new()
    );
}

#[test]
fn preflight_test_house_style_warnings() {
    let prog = "G53 G0 X48\nG0 X10 Z1\nM30\n";
    let f = preflight(prog, belt(2), &SIM);
    assert_eq!(errors(&f), Vec::<&Finding>::new());
    let msgs = f
        .iter()
        .map(|x| x.message.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(msgs.contains("Stock"));
    assert!(msgs.contains("G21"));
    assert!(msgs.contains("does not finish"));
}

#[test]
fn preflight_test_subroutine_bodies_are_not_first_move() {
    let prog = format!(
        "{HEADER}o<r> sub\n G0 X[#1] Z0.5\no<r> endsub\nG21 G7\nG53 G0 X96\no<r> call [10]\n{GOOD_TAIL}"
    );
    assert_eq!(preflight(&prog, belt(2), &SIM), vec![]);
}

// -----------------------------------------------------------------------------
// TouchoffTest
// -----------------------------------------------------------------------------

#[test]
fn touchoff_test_work_z() {
    assert_eq!(touchoff_work_z(), ["G10 L20 P1 Z0"]);
}

#[test]
fn touchoff_test_tool_x_is_radius_and_restores_g7() {
    assert_eq!(
        touchoff_tool_x(1, 11.98, true).unwrap(),
        ["G8", "G10 L10 P1 X5.9900", "G7"]
    );
    assert_eq!(
        touchoff_tool_x(3, 12.0, false).unwrap(),
        ["G8", "G10 L10 P3 X6.0000"]
    );
    assert!(touchoff_tool_x(1, 0.0, true).is_err());
}

#[test]
fn touchoff_test_tool_z_and_verify() {
    assert_eq!(touchoff_tool_z(2), ["G10 L10 P2 Z0"]);
    assert_eq!(verify_work_z(), ["G0 Z5"]);
    assert_eq!(verify_tool(true, VERIFY_BAR_DIAMETER), ["G0 X20 Z5"]);
    assert_eq!(verify_tool(false, VERIFY_BAR_DIAMETER), ["G0 X10 Z5"]);
}

// -----------------------------------------------------------------------------
// ProvenStoreTest
// -----------------------------------------------------------------------------
//
// Python's store reads and writes its file itself; this one hands the caller
// the JSON text to keep (here: a file, as the Python does). The tests keep
// the Python's shape: save under a fresh path, open a second store on it.

fn temp_path(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("emco-rules-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

fn open(path: &std::path::Path) -> ProvenStore {
    ProvenStore::from_json(&std::fs::read_to_string(path).unwrap_or_default())
}

fn save(store: &ProvenStore, path: &std::path::Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, store.to_json()).unwrap();
}

#[test]
fn proven_store_test_any_edit_unproves() {
    let d = temp_path("edit");
    let path = d.join("sub").join("proven.json");
    let mut s = open(&path);
    assert!(!s.is_proven("G0 X1\n"));
    s.mark_proven("G0 X1\n", "a.ngc");
    save(&s, &path);
    assert!(open(&path).is_proven("G0 X1\n"));
    assert!(!open(&path).is_proven("G0 X1 \n"));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn proven_store_test_corrupt_file_is_empty() {
    let d = temp_path("corrupt");
    std::fs::create_dir_all(&d).unwrap();
    let path = d.join("proven.json");
    std::fs::write(&path, "{not json").unwrap();
    assert!(!open(&path).is_proven("x"));
    let _ = std::fs::remove_dir_all(&d);
}
