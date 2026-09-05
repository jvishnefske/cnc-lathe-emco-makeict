//! End-to-end: a small program driven through `Session` + `VirtualMachine`
//! in a lockstep loop, exactly the way the UI wires them together in
//! "Simulated machine" mode. No hardware, no clock — time is a counter.

use emco_sender_core::gcode::Program;
use emco_sender_core::machine::VirtualMachine;
use emco_sender_core::profile::{DialectKind, MachineProfile};
use emco_sender_core::session::{Command, Event, RunState, Session};

fn profile() -> MachineProfile {
    let mut p = MachineProfile::pc_turn_50_default();
    p.dialect = DialectKind::PlainGcode;
    p.turret_index_seconds = 0.2;
    // The default profile puts Z max at the work face (0); give the tests some
    // positive clearance travel so approach moves like Z2 are legal.
    p.z_max_mm = 50.0;
    p
}

/// Pump session and machine against each other until the session goes idle.
/// Returns (events, final_time_ms).
fn run_loop(
    session: &mut Session,
    vm: &mut VirtualMachine,
    start_ms: f64,
    max_ms: f64,
    tick_ms: f64,
) -> (Vec<Event>, f64) {
    let mut events = Vec::new();
    let mut t = start_ms;
    let mut rx: Vec<u8> = Vec::new();
    loop {
        t += tick_ms;
        let step = session.tick(t, &rx);
        events.extend(step.events.iter().cloned());
        rx = vm.step(t, &step.tx);
        let st = session.state();
        if st.run == RunState::Idle && rx.is_empty() && step.tx.is_empty() && !events.is_empty() {
            // Give the loop a couple of settle ticks before declaring done.
            let step2 = session.tick(t + tick_ms, &[]);
            if step2.tx.is_empty() && session.state().run == RunState::Idle {
                events.extend(step2.events);
                break;
            }
        }
        if t > start_ms + max_ms {
            panic!("run did not finish within {max_ms} ms (state {:?})", st.run);
        }
    }
    (events, t)
}

const DEMO_PROGRAM: &str = "\
( face and turn demo, diameter programming )
N10 G21 G90 G95
N20 T0101
N30 S1200 M03
N40 G00 X22 Z2
N50 G01 Z-30 F0.15
N60 G01 X30 F0.1
N70 G00 X40 Z2
N80 M05
N90 M30
";

#[test]
fn program_runs_to_completion_on_virtual_machine() {
    let program = Program::parse(DEMO_PROGRAM);
    assert!(
        program.diagnostics.is_empty(),
        "demo program must be clean: {:?}",
        program.diagnostics
    );

    let mut session = Session::new(profile());
    let mut vm = VirtualMachine::new(profile());

    session.tick(0.0, &[]); // connect
    session.load_program(&program);
    session.command(Command::Run);

    let (events, _t) = run_loop(&mut session, &mut vm, 0.0, 600_000.0, 10.0);

    // Every block that was sent must have been acknowledged, in order.
    let sent: Vec<i32> = events
        .iter()
        .filter_map(|e| match e {
            Event::Sent { line, .. } => Some(*line),
            _ => None,
        })
        .collect();
    let acked: Vec<i32> = events
        .iter()
        .filter_map(|e| match e {
            Event::Acked { line, .. } => Some(*line),
            _ => None,
        })
        .collect();
    // 9 word-bearing blocks (the comment line produces none).
    assert_eq!(sent, vec![1, 2, 3, 4, 5, 6, 7, 8, 9], "sent lines");
    assert_eq!(acked, sent, "every block acknowledged, in order");

    // No alarms anywhere.
    assert!(
        !events.iter().any(|e| matches!(e, Event::Alarm { .. })),
        "clean run must not alarm"
    );

    // Final machine truth: X40 diameter -> radius 20, Z2.
    let (x, z) = vm.pos();
    assert!((x - 20.0).abs() < 1e-6, "vm x = {x}");
    assert!((z - 2.0).abs() < 1e-6, "vm z = {z}");
    assert!(!vm.spindle_on(), "M05 must stop the spindle");
    assert_eq!(vm.tool(), 1, "T0101 must have indexed the turret");

    // Session's predicted DRO agrees with the machine's truth.
    let st = session.state();
    assert_eq!(st.run, RunState::Idle);
    assert_eq!(st.current_line, -1);
    assert!((st.dro.x_radius_mm - x).abs() < 1e-6);
    assert!((st.dro.x_diameter_mm - 40.0).abs() < 1e-6);
    assert!((st.dro.z_mm - z).abs() < 1e-6);
    assert_eq!(st.tool, 1);
    assert!(!st.spindle_on);
}

#[test]
fn feed_hold_pauses_and_resumes_mid_program() {
    let program = Program::parse(DEMO_PROGRAM);
    let mut session = Session::new(profile());
    let mut vm = VirtualMachine::new(profile());
    session.tick(0.0, &[]);
    session.load_program(&program);
    session.command(Command::Run);

    // Run a little, then hold.
    let mut t = 0.0;
    let mut rx: Vec<u8> = Vec::new();
    for _ in 0..200 {
        t += 10.0;
        let step = session.tick(t, &rx);
        rx = vm.step(t, &step.tx);
    }
    session.command(Command::Hold);
    let step = session.tick(t + 10.0, &rx);
    rx = vm.step(t + 10.0, &step.tx);
    assert_eq!(session.state().run, RunState::Hold);

    // While holding, the machine may finish its in-flight block but the
    // session must not feed it new ones.
    let mut new_tx = 0;
    for i in 0..200 {
        let tt = t + 20.0 + i as f64 * 10.0;
        let step = session.tick(tt, &rx);
        new_tx += step.tx.len();
        rx = vm.step(tt, &step.tx);
    }
    assert_eq!(new_tx, 0, "no bytes may flow to the machine during hold");
    t += 20.0 + 2000.0;

    // Resume and finish.
    session.command(Command::Resume);
    let (events, _) = run_loop(&mut session, &mut vm, t, 600_000.0, 10.0);
    assert!(events.iter().any(|e| matches!(e, Event::Acked { line: 9, .. })));
    let (x, _) = vm.pos();
    assert!((x - 20.0).abs() < 1e-6);
}

#[test]
fn estop_fault_mid_program_alarms_the_session() {
    let program = Program::parse(DEMO_PROGRAM);
    let mut session = Session::new(profile());
    let mut vm = VirtualMachine::new(profile());
    session.tick(0.0, &[]);
    session.load_program(&program);
    session.command(Command::Run);

    let mut t = 0.0;
    let mut rx: Vec<u8> = Vec::new();
    for _ in 0..100 {
        t += 10.0;
        let step = session.tick(t, &rx);
        rx = vm.step(t, &step.tx);
    }
    vm.inject_fault("estop");
    // The machine rejects the next thing the session sends; drive until alarm.
    for _ in 0..2000 {
        t += 10.0;
        let step = session.tick(t, &rx);
        rx = vm.step(t, &step.tx);
        if session.state().run == RunState::Alarm {
            break;
        }
    }
    assert_eq!(session.state().run, RunState::Alarm, "estop must reach the session");
    assert!(!session.state().alarms.is_empty());
}

#[test]
fn soft_limit_program_alarms_and_stops_the_run() {
    // X80 diameter = radius 40 (fine), then X200 diameter = radius 100 > 50 max.
    let program = Program::parse("G00 X80 Z0\nG00 X200\nM30");
    let mut session = Session::new(profile());
    let mut vm = VirtualMachine::new(profile());
    session.tick(0.0, &[]);
    session.load_program(&program);
    session.command(Command::Run);

    let mut t = 0.0;
    let mut rx: Vec<u8> = Vec::new();
    for _ in 0..5000 {
        t += 10.0;
        let step = session.tick(t, &rx);
        rx = vm.step(t, &step.tx);
        if session.state().run == RunState::Alarm {
            break;
        }
    }
    let st = session.state();
    assert_eq!(st.run, RunState::Alarm);
    assert!(
        st.alarms.iter().any(|a| a.message.contains("soft limit") || a.code.contains("SOFT")),
        "alarm should mention the soft limit: {:?}",
        st.alarms
    );
    // The machine never moved past its travel.
    let (x, _) = vm.pos();
    assert!(x <= 50.0 + 1e-6);
}

#[test]
fn jog_and_touch_off_round_trip_through_the_machine() {
    let mut session = Session::new(profile());
    let mut vm = VirtualMachine::new(profile());
    session.tick(0.0, &[]);
    session.command(Command::Jog {
        axis: emco_sender_core::session::Axis::Z,
        delta_mm: -5.0,
        feed_mm_min: 400.0,
    });
    let mut t = 0.0;
    let mut rx: Vec<u8> = Vec::new();
    for _ in 0..1000 {
        t += 10.0;
        let step = session.tick(t, &rx);
        rx = vm.step(t, &step.tx);
        if !vm.busy() && rx.is_empty() && t > 100.0 {
            break;
        }
    }
    let (_, z) = vm.pos();
    assert!((z + 5.0).abs() < 1e-6, "vm z = {z}");
    let step = session.tick(t + 10.0, &rx);
    drop(step);
    assert!((session.state().dro.z_mm + 5.0).abs() < 1e-6, "session tracks the jog");
}
