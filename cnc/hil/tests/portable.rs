//! "One procedural model, many physical models": the process payload,
//! byte for byte the same build, references and runs a program on two
//! different machines, each its own physical payload build (the PC Turn 50,
//! and a bigger, finer, faster one with other belts and an eight-station
//! turret) on its own node with a simulator of that machine. A bus here
//! carries every `app/` publication between the two nodes, as Zenoh does
//! between two mw-posix-node instances.
//!
//! The runs must agree: the same machine-coordinate segments sent, the
//! program done on both, and both slides really where the program left
//! them.
//!
//! The second machine differs in its Z travel and X minimum, its step
//! resolution (2000 counts/mm), rapids, maximum feed, spindle and belts,
//! threading limit and turret, but shares the PC Turn 50's reference point:
//! the screen's preflight ties a program to it (a program must open with a
//! full X retract and park at the reference, doc 50 §8), so a program that
//! passes on one machine passes on another only if they agree there.

use std::cell::Cell;
use std::rc::Rc;

use cnc_model::wire::{self, CmState};
use cnc_model::PhysicalConfig;
use cnc_process::process::{decode_state, own_key, program_chunks, Cmd, RunState};
use middleware::transport::RequestMeta;
use middleware::{Key, SenderId};
use mw_posix_node::controller::Controller;
use mw_posix_node::device::{Clock, Out};
use mw_posix_node::driver::sim::{AxisSim, SimConfig, SimDriver};
use mw_posix_node::driver::NoDriver;
use mw_posix_node::flash::mem::{Disk, MemSectors};
use mw_posix_node::flash::SectorFlash;
use mw_posix_node::layout::SECTORS;
use sha2::Digest;

#[derive(Debug, Clone, Default)]
struct Manual(Rc<Cell<u64>>);

impl Clock for Manual {
    fn now_us(&self) -> u64 {
        self.0.get()
    }
}

type Ctl<'d> = Controller<SectorFlash<MemSectors<'d>>, Manual>;

const UNIT: SenderId = SenderId([0x4b, 0x44, 0x32, 0x34, 0, 0, 0, 1]);
const PROC: SenderId = SenderId([0x4b, 0x44, 0x32, 0x34, 0, 0, 0, 2]);

/// The simulator of the machine `cfg` describes.
fn sim_for(cfg: &PhysicalConfig) -> SimConfig {
    let um = |mm: f64| (mm * 1000.0).round() as i32;
    let axis = |min: f64, max: f64| AxisSim {
        min_um: um(min),
        max_um: um(max),
        home_at_max: true,
        start_um: um((min + max) / 2.0),
        counts_per_mm: (1.0 / cfg.resolution_mm).round() as u32,
        rapid_um_s: (cfg.rapid_mm_min * 1000.0 / 60.0).round() as u32,
    };
    SimConfig {
        x: axis(cfg.x_min_mm, cfg.x_max_mm),
        z: axis(cfg.z_min_mm, cfg.z_max_mm),
        limits: cfg.has_end_of_travel_limits,
        spindle_max_rpm: cfg.spindle_max_rpm as i32,
        spindle_accel_rpm_s: 1500,
        turret_stations: cfg.turret_stations as u8,
        turret_index_ms: (cfg.turret_index_seconds * 1000.0) as u32,
        queue: 32,
    }
}

fn open<'d>(disk: &'d mut Disk, node: SenderId, clock: &Manual, sim: Option<SimConfig>) -> Ctl<'d> {
    let flash = SectorFlash::open(MemSectors::new(disk), SECTORS).expect("flash");
    let driver: Box<dyn mw_posix_node::driver::Driver> = match sim {
        Some(s) => Box::new(SimDriver::new(s)),
        None => Box::new(NoDriver),
    };
    let facts = mw_posix_node::facts_with_driver(node, driver.as_ref());
    let (mut c, boot) = Controller::open_with_driver(
        flash,
        facts,
        mw_posix_node::limits(1024 * 1024),
        clock.clone(),
        driver,
    );
    c.boot(&boot);
    c
}

fn install(c: &mut Ctl<'_>, path: &str, binding: Option<Vec<String>>) -> Vec<u8> {
    let image = std::fs::read(path).expect("the built payload");
    for step in mwctl::plan::upload_plan(&image, None, None).expect("plan") {
        c.request(step.key, &step.body, &RequestMeta::NONE)
            .expect("upload");
    }
    if let Some(specs) = binding {
        let sha: [u8; 32] = sha2::Sha256::digest(&image).into();
        let record = mwctl::bindspec::record(sha, 1, &specs).expect("binding");
        c.request(
            Key::from_static("sys/payload/bind"),
            &record,
            &RequestMeta::NONE,
        )
        .expect("bind");
    }
    c.request(
        Key::from_static("sys/payload/start"),
        &[],
        &RequestMeta::NONE,
    )
    .unwrap_or_else(|e| {
        let logs: Vec<String> = c
            .drain()
            .iter()
            .filter(|o| o.key == "sys/log")
            .map(|o| String::from_utf8_lossy(&o.payload).into_owned())
            .collect();
        panic!("start {path}: {e:?} {logs:?}")
    });
    image
}

/// Two nodes and the bus between them.
struct Rig<'d> {
    unit: Ctl<'d>,
    proc_: Ctl<'d>,
    clock: Manual,
    /// Every app/ publication, in order.
    seen: Vec<Out>,
}

impl Rig<'_> {
    fn carry(&mut self, outs: Vec<Out>, to_unit: bool) {
        for o in outs {
            if o.key == "sys/log" && std::env::var_os("HIL_DEBUG").is_some() {
                eprintln!(
                    "{} log: {}",
                    if to_unit { "proc" } else { "unit" },
                    String::from_utf8_lossy(&o.payload)
                );
            }
            if !o.key.starts_with("app/") {
                continue;
            }
            let k = Key::new(&o.key).unwrap();
            if to_unit {
                self.unit.publication(k, &o.payload);
            } else {
                self.proc_.publication(k, &o.payload);
            }
            self.seen.push(o);
        }
    }

    fn step_ms(&mut self, ms: u64) {
        for _ in 0..ms {
            self.clock.0.set(self.clock.0.get() + 1000);
            self.unit.poll();
            self.proc_.poll();
            for _ in 0..3 {
                let from_unit = self.unit.drain();
                let from_proc = self.proc_.drain();
                if from_unit.is_empty() && from_proc.is_empty() {
                    break;
                }
                self.carry(from_unit, false);
                self.carry(from_proc, true);
            }
        }
    }

    /// An operator publication to the process node.
    fn operator(&mut self, what: &str, body: &[u8]) {
        let key = own_key(PROC.0, what);
        self.proc_.publication(Key::new(&key).unwrap(), body);
        self.step_ms(1);
    }

    fn state(&self) -> Option<(u8, String)> {
        let key = own_key(PROC.0, "state");
        self.seen
            .iter()
            .rev()
            .find(|o| o.key == key)
            .and_then(|o| decode_state(&o.payload))
            .map(|s| (s.0, s.4))
    }

    fn until(&mut self, want: RunState, ms: u64) {
        for _ in 0..ms / 10 {
            match self.state() {
                Some((s, _)) if s == want as u8 => return,
                Some((s, m)) if s == RunState::Failed as u8 => panic!("failed: {m}"),
                _ => self.step_ms(10),
            }
        }
        panic!("not {want:?} after {ms} ms: {:?}", self.state());
    }

    fn cm(&self, i: usize) -> Option<CmState> {
        let key = wire::cm_key(UNIT.0).unwrap();
        self.seen
            .iter()
            .rev()
            .filter(|o| o.key == key.as_str())
            .filter_map(|o| wire::decode_cm(&o.payload))
            .find(|(j, _)| *j == i)
            .map(|(_, s)| s)
    }

    /// The segments the process sent, in order (machine µm).
    fn segments(&self) -> Vec<cnc_model::Segment> {
        let key = wire::segments_key(UNIT.0).unwrap();
        self.seen
            .iter()
            .filter(|o| o.key == key.as_str())
            .flat_map(|o| {
                wire::decode_segments(&o.payload)
                    .unwrap()
                    .segments()
                    .collect::<Vec<_>>()
            })
            .collect()
    }
}

const PROGRAM: &str = "( Stock : 30 mm alu )\n( Tools : T1 turning )\n\
G7 G18 G21 G90 G94\nG53 G0 X96\nT1\nG97 S800 M3\nG0 X20 Z2\nG1 Z-20 F150\nG1 X24\nG2 X28 Z-22 R2\nG0 Z2\nM5\nG53 G0 X96 Z0\nM30\n";

/// References, then runs [`PROGRAM`] with belt 2, on the machine `cfg`
/// whose physical payload is `physical`. Returns what the run sent and
/// where the slides ended (machine µm).
fn run_on(cfg: &PhysicalConfig, physical: &str) -> (Vec<cnc_model::Segment>, (i32, i32), Vec<u8>) {
    let (mut du, mut dp) = (Disk::default(), Disk::default());
    let clock = Manual::default();
    let mut unit = open(&mut du, UNIT, &clock, Some(sim_for(cfg)));
    let mut proc_ = open(&mut dp, PROC, &clock, None);
    install(&mut unit, physical, Some(cfg.io.binding_specs()));
    let process_image = install(&mut proc_, cnc_hil::PROCESS_WASM, None);
    let mut r = Rig {
        unit,
        proc_,
        clock,
        seen: Vec::new(),
    };
    r.step_ms(20);

    r.operator("cmd", &Cmd::Assign(UNIT.0).encode());
    r.step_ms(1_100); // the unit's next heartbeat: its info and states
    for chunk in program_chunks(1, PROGRAM) {
        r.operator("program", &chunk);
    }
    // Not referenced yet: the gate refuses.
    r.operator(
        "cmd",
        &Cmd::Run {
            program: 1,
            belt: 2,
        }
        .encode(),
    );
    let (s, m) = r.state().unwrap();
    assert_eq!(s, RunState::Failed as u8);
    assert!(m.contains("not referenced"), "{m}");
    r.operator("cmd", &Cmd::Clear.encode());

    r.operator("cmd", &Cmd::Reference.encode());
    r.until(RunState::Done, 120_000);
    r.operator(
        "cmd",
        &Cmd::Run {
            program: 1,
            belt: 2,
        }
        .encode(),
    );
    r.until(RunState::Done, 120_000);
    r.step_ms(200);
    let (
        Some(CmState::Axis {
            position_um: x,
            referenced: true,
        }),
        Some(CmState::Axis {
            position_um: z,
            referenced: true,
        }),
    ) = (r.cm(0), r.cm(1))
    else {
        panic!("{:?} {:?}", r.cm(0), r.cm(1))
    };
    assert_eq!(
        r.cm(3),
        Some(CmState::Turret {
            station: 1,
            ready: true
        })
    );
    assert!(matches!(
        r.cm(2),
        Some(CmState::Spindle { commanded: 0, .. })
    ));
    if std::env::var_os("HIL_DEBUG").is_some() {
        let mut keys: Vec<&str> = r.seen.iter().map(|o| o.key.as_str()).collect();
        keys.sort();
        keys.dedup();
        eprintln!("{keys:#?}");
    }
    (r.segments(), (x, z), process_image)
}

#[test]
fn one_process_build_runs_the_same_program_on_two_machines() {
    let a = PhysicalConfig::pc_turn_50();
    let b: PhysicalConfig =
        serde_json::from_str(include_str!("../../model/fixtures/alt_lathe.json"))
            .expect("alt_lathe.json");
    assert_ne!(a.limits(), b.limits(), "two different machines");
    let physical_a = std::fs::read(cnc_hil::PHYSICAL_WASM).unwrap();
    let physical_b = std::fs::read(cnc_hil::PHYSICAL_ALT_WASM).unwrap();
    assert_ne!(physical_a, physical_b, "two physical builds");

    let (segs_a, end_a, proc_a) = run_on(&a, cnc_hil::PHYSICAL_WASM);
    let (segs_b, end_b, proc_b) = run_on(&b, cnc_hil::PHYSICAL_ALT_WASM);
    assert_eq!(proc_a, proc_b, "the same process image, byte for byte");

    // The same work in machine coordinates: 11 segments (rapid, two feeds,
    // the arc's chords, the retract and the park).
    assert!(segs_a.len() > 6, "{segs_a:?}");
    assert_eq!(
        segs_a, segs_b,
        "the process model sent the same moves to both"
    );
    // Both ended at the park, G53 X96 (radius 48) Z0, within a step.
    for (x, z) in [end_a, end_b] {
        assert!((x - 48_000).abs() <= 2 && z.abs() <= 2, "({x}, {z})");
    }
}
