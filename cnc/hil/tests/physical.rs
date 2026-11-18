//! The physical payload as built, in mw-posix-node's controller with the
//! simulated PC Turn 50: installed, bound to the simulator's points by its
//! I/O map, started, and commanded through the unit's keys exactly as a
//! process model publishes them.

use std::cell::Cell;
use std::rc::Rc;

use cnc_model::wire::{self, CmState};
use cnc_model::{IoMap, PhysicalConfig};
use middleware::loader::control::Page;
use middleware::loader::manifest::Manifest;
use middleware::transport::RequestMeta;
use middleware::{Key, SenderId};
use mw_posix_node::controller::Controller;
use mw_posix_node::device::{Clock, Out};
use mw_posix_node::driver::sim::{SimConfig, SimDriver};
use mw_posix_node::flash::mem::{Disk, MemSectors};
use mw_posix_node::flash::SectorFlash;
use mw_posix_node::layout::SECTORS;
use recipe_core::phase::{Command, PhaseState};
use recipe_core::wire::{decode_report, encode_command};
use sha2::Digest;

#[derive(Debug, Clone, Default)]
struct Manual(Rc<Cell<u64>>);

impl Clock for Manual {
    fn now_us(&self) -> u64 {
        self.0.get()
    }
}

const NODE: SenderId = SenderId([0x4b, 0x44, 0x32, 0x34, 0, 0, 0, 1]);

type Ctl<'d> = Controller<SectorFlash<MemSectors<'d>>, Manual>;

struct Node<'d> {
    c: Ctl<'d>,
    clock: Manual,
    id: u32,
    outs: Vec<Out>,
}

impl<'d> Node<'d> {
    fn open(disk: &'d mut Disk, sim: SimConfig) -> Self {
        let clock = Manual::default();
        let flash = SectorFlash::open(MemSectors::new(disk), SECTORS).expect("flash");
        let driver = SimDriver::new(sim);
        let facts = mw_posix_node::facts_with_driver(NODE, &driver);
        let (mut c, boot) = Controller::open_with_driver(
            flash,
            facts,
            mw_posix_node::limits(1024 * 1024),
            clock.clone(),
            Box::new(driver),
        );
        c.boot(&boot);
        Node {
            c,
            clock,
            id: 0,
            outs: Vec::new(),
        }
    }

    fn req(&mut self, key: &str, body: &[u8]) -> Vec<u8> {
        self.c
            .request(Key::new(key).unwrap(), body, &RequestMeta::NONE)
            .unwrap_or_else(|e| {
                let logs: Vec<String> = self
                    .c
                    .drain()
                    .iter()
                    .filter(|o| o.key == "sys/log")
                    .map(|o| String::from_utf8_lossy(&o.payload).into_owned())
                    .collect();
                panic!("{key}: {e:?} {logs:?}")
            })
    }

    /// Installs, binds by `io` and starts the physical payload.
    fn start_physical(&mut self, io: &IoMap) {
        let image = std::fs::read(cnc_hil::PHYSICAL_WASM).expect("the built payload");
        for step in mwctl::plan::upload_plan(&image, None, None).expect("plan") {
            self.c
                .request(step.key, &step.body, &RequestMeta::NONE)
                .expect("upload");
        }
        let sha: [u8; 32] = sha2::Sha256::digest(&image).into();
        let record = mwctl::bindspec::record(sha, 1, &io.binding_specs()).expect("binding");
        self.req("sys/payload/bind", &record);
        self.req("sys/payload/start", &[]);
        assert!(self.c.running());
    }

    fn run_ms(&mut self, ms: u64) {
        for _ in 0..ms {
            self.clock.0.set(self.clock.0.get() + 1000);
            self.c.poll();
            self.outs.extend(self.c.drain());
        }
    }

    fn command(&mut self, em: &str, phase: &str, c: Command, args: &[i32]) {
        self.id += 1;
        let mut buf = [0u8; recipe_core::wire::MAX_BODY];
        let body = encode_command(c, self.id, args, &mut buf).unwrap().to_vec();
        let key = wire::phase_key(NODE.0, em, phase).unwrap();
        self.c.publication(Key::new(key.as_str()).unwrap(), &body);
        self.outs.extend(self.c.drain());
    }

    /// The newest report published for `em.phase`.
    fn state(&self, em: &str, phase: &str) -> Option<(PhaseState, u32)> {
        let key = wire::phase_state_key(NODE.0, em, phase).unwrap();
        self.outs
            .iter()
            .rev()
            .find(|o| o.key == key.as_str())
            .and_then(|o| decode_report(&o.payload).ok())
            .map(|r| (r.state, r.last_id))
    }

    fn until(&mut self, em: &str, phase: &str, want: PhaseState, ms: u64) {
        for _ in 0..ms / 10 {
            if self.state(em, phase).map(|s| s.0) == Some(want) {
                return;
            }
            self.run_ms(10);
        }
        panic!("{em}.{phase}: {:?}, not {want:?}", self.state(em, phase));
    }

    /// The newest state of control module `cm`.
    fn cm(&self, cm: usize) -> Option<CmState> {
        let key = wire::cm_key(NODE.0).unwrap();
        self.outs
            .iter()
            .rev()
            .filter(|o| o.key == key.as_str())
            .filter_map(|o| wire::decode_cm(&o.payload))
            .find(|(i, _)| *i == cm)
            .map(|(_, s)| s)
    }
}

#[test]
fn the_node_lists_the_points_the_io_map_names() {
    let mut disk = Disk::default();
    let mut n = Node::open(&mut disk, SimConfig::pc_turn_50());
    let m = Manifest::decode(&n.req("sys/manifest", &[])).unwrap();
    assert!(m.control.is_some());
    let page = n.req("sys/manifest/cm", &[]);
    let names: Vec<String> = Page::decode(&page)
        .unwrap()
        .points()
        .map(|p| p.name.to_string())
        .collect();
    for (_, point) in IoMap::default().pairs() {
        assert!(names.iter().any(|n| n == point), "{point}");
    }
}

#[test]
fn the_built_payload_references_moves_and_spins_on_the_simulator() {
    let mut disk = Disk::default();
    let mut n = Node::open(&mut disk, SimConfig::pc_turn_50());
    n.start_physical(&PhysicalConfig::pc_turn_50().io);
    n.run_ms(50);
    // Every phase reports Idle at the start, and nothing leaves on drv/.
    assert_eq!(n.state("motion", "reference"), Some((PhaseState::Idle, 0)));
    assert!(n.outs.iter().all(|o| !o.key.starts_with("drv/")));
    assert!(matches!(
        n.cm(0),
        Some(CmState::Axis {
            referenced: false,
            ..
        })
    ));
    // The machine announces itself, so a process model needs no build for it.
    let info_key = wire::info_key(NODE.0).unwrap();
    let info = n
        .outs
        .iter()
        .rev()
        .find(|o| o.key == info_key.as_str())
        .expect("info");
    let lim = cnc_model::Limits::decode_info(&info.payload).unwrap();
    assert_eq!(
        (lim.z_min_um, lim.turret_stations, lim.belt(1)),
        (-228_000, 6, Some((130, 1300)))
    );

    n.command("motion", "reference", Command::Start, &[]);
    n.until("motion", "reference", PhaseState::Complete, 60_000);
    let Some(CmState::Axis {
        referenced: true,
        position_um,
    }) = n.cm(1)
    else {
        panic!("{:?}", n.cm(1))
    };
    assert!(position_um.abs() <= 100, "Z at machine zero: {position_um}");

    n.command("motion", "move", Command::Start, &[20_000, -50_000, 10_000]);
    n.until("motion", "move", PhaseState::Complete, 30_000);
    n.run_ms(50);
    let Some(CmState::Axis { position_um: x, .. }) = n.cm(0) else {
        panic!()
    };
    let Some(CmState::Axis { position_um: z, .. }) = n.cm(1) else {
        panic!()
    };
    assert!(
        (x - 20_000).abs() <= 50 && (z + 50_000).abs() <= 50,
        "{x} {z}"
    );

    n.command("spindle", "run", Command::Start, &[900]);
    n.until("spindle", "run", PhaseState::Complete, 3_000);
    n.command("turret", "index", Command::Start, &[3]);
    n.until("turret", "index", PhaseState::Complete, 5_000);
    assert_eq!(
        n.cm(3),
        Some(CmState::Turret {
            station: 3,
            ready: true
        })
    );
}

#[test]
fn a_command_outside_the_machine_changes_nothing() {
    let mut disk = Disk::default();
    let mut n = Node::open(&mut disk, SimConfig::pc_turn_50());
    n.start_physical(&IoMap::default());
    n.run_ms(20);
    n.command("spindle", "run", Command::Start, &[5_000]);
    n.run_ms(20);
    assert_eq!(
        n.state("spindle", "run").map(|s| s.0),
        Some(PhaseState::Idle),
        "3000 rpm machine"
    );
    n.command("turret", "index", Command::Start, &[7]);
    n.run_ms(20);
    assert_eq!(
        n.state("turret", "index").map(|s| s.0),
        Some(PhaseState::Idle),
        "6 stations"
    );
}
