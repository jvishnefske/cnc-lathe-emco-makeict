//! The physical model against mw-posix-node's simulated lathe, on the host:
//! the simulator's readings become [`Input`]s, the model's segments and
//! setpoints go back to it, as the payload and the node do on a board.

use cnc_model::wire::PHASES;
use cnc_physical::lathe::{
    Abort, Input, Lathe, Out, Outs, INDEX, MOVE, REFERENCE, SPIN, SPIN_STOP,
};
use middleware::wasm_abi::machine::{Digital, Position, QueueState, Setpoint};
use mw_posix_node::driver::sim::{SimConfig, SimDriver};
use mw_posix_node::driver::Driver;
use recipe_core::phase::{Command, PhaseState};

struct Rig {
    sim: SimDriver,
    lathe: Lathe,
    now_us: u64,
    reports: Vec<usize>,
    id: u32,
}

impl Rig {
    fn new(sim: SimConfig, phys: cnc_model::PhysicalConfig) -> Self {
        let mut r = Rig {
            sim: SimDriver::new(sim),
            lathe: Lathe::new(phys.limits()),
            now_us: 0,
            reports: Vec::new(),
            id: 0,
        };
        let snap = r.sim.snapshot(0);
        r.feed(snap);
        r
    }

    fn pc_turn_50() -> Self {
        Rig::new(
            SimConfig::pc_turn_50(),
            cnc_model::PhysicalConfig::pc_turn_50(),
        )
    }

    fn index(&self, name: &str) -> usize {
        self.sim
            .points()
            .iter()
            .position(|p| p.name == name)
            .unwrap()
    }

    fn input(&self, i: usize, data: &[u8]) -> Option<Input> {
        let name = self.sim.points()[i].name.as_str();
        let on = || Digital::decode(data).map(|d| d.on);
        Some(match name {
            "x_pos" => Input::XPos(Position::decode(data)?),
            "z_pos" => Input::ZPos(Position::decode(data)?),
            "x_home" => Input::XHome(on()?),
            "z_home" => Input::ZHome(on()?),
            "spindle_actual" => Input::SpindleActual(Setpoint::decode(data)?.value),
            "turret_ready" => Input::TurretReady(on()?),
            "estop" => Input::EStop(on()?),
            "motion" => Input::Queue(QueueState::decode(data)?),
            _ => return None,
        })
    }

    fn apply(&mut self, outs: Outs) {
        for o in outs.iter() {
            let (point, data) = match o {
                Out::Segment(s) => ("motion", s.encode().to_vec()),
                Out::Spindle(rpm) => ("spindle_rpm", Setpoint { value: rpm }.encode().to_vec()),
                Out::Turret(st) => (
                    "turret_station",
                    Setpoint {
                        value: i32::from(st),
                    }
                    .encode()
                    .to_vec(),
                ),
                Out::Report(i) => {
                    self.reports.push(i);
                    continue;
                }
                Out::Cm(..) => continue,
            };
            let i = self.index(point);
            self.sim.write(i, &data, self.now_us).unwrap();
        }
    }

    fn feed(&mut self, readings: Vec<(usize, Vec<u8>)>) {
        for (i, data) in readings {
            if let Some(input) = self.input(i, &data) {
                let outs = self.lathe.input(input);
                self.apply(outs);
            }
        }
    }

    fn run_ms(&mut self, ms: u64) {
        for _ in 0..ms {
            self.now_us += 1000;
            let r = self.sim.poll(self.now_us);
            self.feed(r);
        }
    }

    fn command(&mut self, phase: usize, c: Command, args: &[i32]) {
        self.id += 1;
        let mut buf = [0u8; recipe_core::wire::MAX_BODY];
        let body = recipe_core::wire::encode_command(c, self.id, args, &mut buf)
            .unwrap()
            .to_vec();
        let outs = self.lathe.command(phase, &body);
        self.apply(outs);
    }

    /// Runs until phase `i` reaches a final or quiescent state, at most `ms`.
    fn until(&mut self, i: usize, want: PhaseState, ms: u64) {
        for _ in 0..ms {
            if self.lathe.phase(i) == want {
                return;
            }
            self.run_ms(1);
        }
        panic!(
            "{}.{} is {:?}, not {want:?}",
            PHASES[i].0,
            PHASES[i].1,
            self.lathe.phase(i)
        );
    }
}

#[test]
fn reference_finds_both_switches_and_sets_machine_zero() {
    let mut r = Rig::pc_turn_50();
    assert!(!r.lathe.referenced(0) && !r.lathe.referenced(1));
    r.command(REFERENCE, Command::Start, &[]);
    assert_eq!(r.lathe.phase(REFERENCE), PhaseState::Running);
    // X from 20 mm to 48 at 5 mm/s, then Z from -100 to 0: about 26 s.
    r.until(REFERENCE, PhaseState::Complete, 60_000);
    assert!(r.lathe.referenced(0) && r.lathe.referenced(1));
    // Machine zero is where the switches made: the slides stopped there.
    let (x, z) = r.sim.real_um();
    assert!((47_700..=47_900).contains(&x), "{x}");
    assert!((200..=400).contains(&z), "{z}");

    // A move in machine coordinates lands where it says, on the real slide.
    r.command(MOVE, Command::Start, &[20_000, -50_000, 10_000]);
    r.until(MOVE, PhaseState::Complete, 30_000);
    let (x, z) = r.sim.real_um();
    let (rx, rz) = (x - 47_800, z - 300);
    // Real offsets of the switch (made 200 µm before the end) cancel out:
    // the machine frame is the one the reference defined.
    assert!(
        (rx - (20_000 - 48_000)).abs() <= 120 && (rz - (-50_000)).abs() <= 120,
        "{x} {z}"
    );
}

#[test]
fn moves_need_a_reference_and_stay_inside_the_travel() {
    let mut r = Rig::pc_turn_50();
    r.command(MOVE, Command::Start, &[10_000, -10_000, 1000]);
    assert_eq!(r.lathe.phase(MOVE), PhaseState::Aborted);
    assert_eq!(r.lathe.report(MOVE).2[0], Abort::NotReferenced as i32);
    // Outside the travel: refused before it starts (range check).
    r.command(MOVE, Command::Reset, &[]);
    r.run_ms(1);
    assert_eq!(r.lathe.phase(MOVE), PhaseState::Idle);
    r.command(MOVE, Command::Start, &[10_000, 5_000, 1000]);
    assert_eq!(r.lathe.phase(MOVE), PhaseState::Idle, "z 5 mm is past +0.5");
}

#[test]
fn the_spindle_and_the_turret_run_their_phases() {
    let mut r = Rig::pc_turn_50();
    r.command(SPIN, Command::Start, &[1200]);
    r.until(SPIN, PhaseState::Complete, 3_000);
    assert!((r.sim.spindle_rpm() - 1200).abs() <= 24);
    r.command(SPIN_STOP, Command::Start, &[]);
    r.until(SPIN_STOP, PhaseState::Complete, 3_000);
    assert_eq!(r.sim.spindle_rpm(), 0);

    r.command(INDEX, Command::Start, &[4]);
    r.until(INDEX, PhaseState::Complete, 5_000);
    assert_eq!(r.sim.station(), 4);
    // Already there: done at once.
    r.command(INDEX, Command::Reset, &[]);
    r.run_ms(1);
    r.command(INDEX, Command::Start, &[4]);
    assert_eq!(r.lathe.phase(INDEX), PhaseState::Complete);
    r.command(INDEX, Command::Reset, &[]);
    r.run_ms(1);
    r.command(INDEX, Command::Start, &[7]);
    assert_eq!(r.lathe.phase(INDEX), PhaseState::Idle, "6 stations");
}

#[test]
fn the_estop_aborts_everything_and_loses_the_reference() {
    let mut r = Rig::pc_turn_50();
    r.command(REFERENCE, Command::Start, &[]);
    r.until(REFERENCE, PhaseState::Complete, 60_000);
    r.command(SPIN, Command::Start, &[800]);
    r.command(MOVE, Command::Start, &[10_000, -150_000, 2_000]);
    r.run_ms(500);
    r.sim.set_estop(false);
    r.run_ms(50);
    assert_eq!(r.lathe.phase(MOVE), PhaseState::Aborted);
    assert_eq!(r.lathe.report(MOVE).2[0], Abort::EStop as i32);
    assert_eq!(r.lathe.phase(SPIN), PhaseState::Aborted);
    assert!(
        !r.lathe.referenced(0) && !r.lathe.referenced(1),
        "re-reference after every e-stop"
    );
    // Closed again: a reference works (the model acknowledged the stop).
    r.sim.set_estop(true);
    r.run_ms(50);
    r.command(REFERENCE, Command::Reset, &[]);
    r.run_ms(1);
    r.command(REFERENCE, Command::Start, &[]);
    r.until(REFERENCE, PhaseState::Complete, 90_000);
}

#[test]
fn a_repeated_command_is_applied_once() {
    let mut r = Rig::pc_turn_50();
    r.id += 1;
    let mut buf = [0u8; recipe_core::wire::MAX_BODY];
    let body = recipe_core::wire::encode_command(Command::Start, r.id, &[300], &mut buf)
        .unwrap()
        .to_vec();
    let outs = r.lathe.command(SPIN, &body);
    r.apply(outs);
    r.until(SPIN, PhaseState::Complete, 3_000);
    // The engine sent it again (a lost report): nothing restarts.
    let outs = r.lathe.command(SPIN, &body);
    assert!(
        outs.iter().any(|o| o == Out::Report(SPIN)),
        "the report is sent again"
    );
    assert_eq!(r.lathe.phase(SPIN), PhaseState::Complete);
}

#[test]
fn the_estop_tells_the_driver_to_stop_the_spindle() {
    let mut r = Rig::pc_turn_50();
    r.command(SPIN, Command::Start, &[800]);
    r.until(SPIN, PhaseState::Complete, 3_000);
    let outs = r.lathe.input(Input::EStop(false));
    assert!(
        outs.iter().any(|o| o == Out::Spindle(0)),
        "an explicit 0 to the driver"
    );
}

#[test]
fn one_motion_phase_at_a_time() {
    let mut r = Rig::pc_turn_50();
    r.command(REFERENCE, Command::Start, &[]);
    r.run_ms(100);
    assert_eq!(r.lathe.phase(REFERENCE), PhaseState::Running);
    // A move or an index while referencing: refused, the reference goes on.
    r.command(MOVE, Command::Start, &[10_000, -10_000, 1000]);
    assert_eq!(r.lathe.phase(MOVE), PhaseState::Aborted);
    assert_eq!(r.lathe.report(MOVE).2[0], Abort::MotionBusy as i32);
    r.command(INDEX, Command::Start, &[2]);
    assert_eq!(r.lathe.phase(INDEX), PhaseState::Aborted);
    assert_eq!(r.lathe.phase(REFERENCE), PhaseState::Running);
    r.until(REFERENCE, PhaseState::Complete, 60_000);
}

#[test]
fn synchronised_segments_obey_the_feed_and_threading_limits() {
    use cnc_model::wire::encode_segments;
    use cnc_physical::lathe::RUN;
    use middleware::wasm_abi::machine::Segment;
    let mut r = Rig::pc_turn_50();
    r.command(REFERENCE, Command::Start, &[]);
    r.until(REFERENCE, PhaseState::Complete, 60_000);
    let thread = |seq, feed| Segment {
        seq,
        x_um: 10_000,
        z_um: -60_000,
        feed,
        flags: Segment::SYNC,
    };
    // Above the 400 rpm threading limit: refused, the run aborts.
    r.command(SPIN, Command::Start, &[1000]);
    r.until(SPIN, PhaseState::Complete, 3_000);
    r.command(RUN, Command::Start, &[]);
    let outs = r
        .lathe
        .segments(&encode_segments(&[thread(1, 1500)], true).unwrap());
    r.apply(outs);
    assert_eq!(r.lathe.phase(RUN), PhaseState::Aborted);
    assert_eq!(r.lathe.report(RUN).2[0], Abort::BadSegments as i32);
    // At 300 rpm: 1.5 mm/rev is 7.5 mm/s, under 12.5; 3 mm/rev is 15, over.
    r.command(RUN, Command::Reset, &[]);
    r.command(SPIN, Command::Reset, &[]);
    r.run_ms(1);
    r.command(SPIN, Command::Start, &[300]);
    r.until(SPIN, PhaseState::Complete, 3_000);
    r.command(RUN, Command::Start, &[]);
    let outs = r
        .lathe
        .segments(&encode_segments(&[thread(2, 3000)], true).unwrap());
    r.apply(outs);
    assert_eq!(
        r.lathe.phase(RUN),
        PhaseState::Aborted,
        "15 mm/s is over the 12.5 mm/s maximum"
    );
    r.command(RUN, Command::Reset, &[]);
    r.run_ms(1);
    r.command(RUN, Command::Start, &[]);
    let outs = r
        .lathe
        .segments(&encode_segments(&[thread(3, 1500)], true).unwrap());
    r.apply(outs);
    r.until(RUN, PhaseState::Complete, 20_000);
}

#[test]
fn the_spindle_does_not_change_under_a_running_program() {
    use cnc_physical::lathe::RUN;
    let mut r = Rig::pc_turn_50();
    r.command(REFERENCE, Command::Start, &[]);
    r.until(REFERENCE, PhaseState::Complete, 60_000);
    r.command(SPIN, Command::Start, &[300]);
    r.until(SPIN, PhaseState::Complete, 3_000);
    r.command(SPIN, Command::Reset, &[]);
    r.run_ms(1);
    r.command(RUN, Command::Start, &[]);
    assert_eq!(r.lathe.phase(RUN), PhaseState::Running);
    r.command(SPIN, Command::Start, &[1200]);
    assert_eq!(r.lathe.phase(SPIN), PhaseState::Aborted);
    assert_eq!(r.lathe.report(SPIN).2[0], Abort::MotionBusy as i32);
    r.command(SPIN_STOP, Command::Start, &[]);
    assert_eq!(r.lathe.phase(SPIN_STOP), PhaseState::Aborted);
    r.run_ms(500);
    assert_eq!(r.sim.spindle_rpm(), 300);
}
