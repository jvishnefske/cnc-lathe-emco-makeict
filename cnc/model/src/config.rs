//! [`PhysicalConfig`]: one machine's hardware, every number the physical
//! model needs and nothing that is site policy.
//!
//! It is the physical half of the sender's `MachineProfile` (which embeds
//! it), plus the joint → control-module point map ([`IoMap`]) in the style
//! of the LinuxCNC retrofit's HAL files
//! (`linuxcnc/retrofit/emco-pcturn50-7i96s/*.hal`). The site's choices
//! (the G-code dialect, the default G54, feed override, the link) stay with
//! the process model and the sender. The JSON is camelCase, as the
//! sender's profile always was; [`PhysicalConfig::pc_turn_50`] is written
//! to `fixtures/pc_turn_50.json` by the tests, and the browser sender's
//! fallback profile is checked against that file, so no copy of these
//! numbers can drift.

use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

use crate::Limits;

/// One spindle belt position. Changed by physically moving a belt, so it
/// is operator-declared, never machine-reported.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BeltStep {
    pub min_rpm: f64,
    pub max_rpm: f64,
}

/// Which control-module point each of the physical model's channels binds
/// to on a node (the binding's `driver:<point>` targets). The defaults are
/// the names of `mw-posix-node`'s simulator.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct IoMap {
    pub x_pos: String,
    pub z_pos: String,
    pub x_home: String,
    pub z_home: String,
    pub spindle_rpm: String,
    pub spindle_actual: String,
    pub turret_station: String,
    pub turret_ready: String,
    pub estop: String,
    pub motion: String,
}

impl Default for IoMap {
    fn default() -> Self {
        IoMap {
            x_pos: "x_pos".into(),
            z_pos: "z_pos".into(),
            x_home: "x_home".into(),
            z_home: "z_home".into(),
            spindle_rpm: "spindle_rpm".into(),
            spindle_actual: "spindle_actual".into(),
            turret_station: "turret_station".into(),
            turret_ready: "turret_ready".into(),
            estop: "estop".into(),
            motion: "motion".into(),
        }
    }
}

impl IoMap {
    /// `(channel, point)` for every channel the physical model declares.
    pub fn pairs(&self) -> [(&'static str, &str); 11] {
        [
            ("x_pos", &self.x_pos),
            ("z_pos", &self.z_pos),
            ("x_home", &self.x_home),
            ("z_home", &self.z_home),
            ("spindle_rpm", &self.spindle_rpm),
            ("spindle_actual", &self.spindle_actual),
            ("turret_station", &self.turret_station),
            ("turret_ready", &self.turret_ready),
            ("estop", &self.estop),
            ("motion", &self.motion),
            ("queue", &self.motion),
        ]
    }

    /// The binding specs (`channel=driver:point`, as `mwctl bind` takes them).
    pub fn binding_specs(&self) -> Vec<String> {
        self.pairs()
            .iter()
            .map(|(c, p)| alloc::format!("{c}=driver:{p}"))
            .collect()
    }
}

/// How an axis finds its reference.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Reference {
    /// Machine coordinates at the X reference switch, radius mm.
    pub x_mm: f64,
    /// Machine coordinates at the Z reference switch, mm.
    pub z_mm: f64,
    /// The approach to the switch, mm/min.
    pub search_mm_min: f64,
    /// X first (it clears the work before Z moves).
    pub x_first: bool,
}

/// One machine's hardware (module docs).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PhysicalConfig {
    pub name: String,
    /// X travel, **radius** mm.
    pub x_min_mm: f64,
    pub x_max_mm: f64,
    pub z_min_mm: f64,
    pub z_max_mm: f64,
    pub rapid_mm_min: f64,
    pub max_feed_mm_min: f64,
    /// Step resolution, mm.
    pub resolution_mm: f64,
    pub accel_mm_sec2: f64,
    pub spindle_min_rpm: f64,
    pub spindle_max_rpm: f64,
    /// Belt positions; the active one is operator-declared, not sensed.
    pub spindle_belt_steps: Vec<BeltStep>,
    /// Threading (G33/G78) is only synchronised up to this speed.
    pub threading_max_rpm: f64,
    pub turret_stations: u32,
    pub turret_index_seconds: f64,
    /// False on the PC Turn 50: travel ends at mechanical stops and the
    /// stepper stalls audibly, silently losing position.
    pub has_end_of_travel_limits: bool,
    /// True on the PC Turn 50: no absolute position memory, so the
    /// reference point must be re-approached after every power-off.
    pub requires_reference_after_power_on: bool,
    /// Where the reference switches are and how they are approached.
    #[serde(default = "Reference::pc_turn_50")]
    pub reference: Reference,
    /// The control-module points the channels bind to.
    #[serde(default)]
    pub io: IoMap,
}

impl Reference {
    /// The PC Turn 50's: X at full retract (48 mm radius), Z at machine
    /// zero, approached at 300 mm/min, X first.
    pub fn pc_turn_50() -> Self {
        Reference {
            x_mm: 48.0,
            z_mm: 0.0,
            search_mm_min: 300.0,
            x_first: true,
        }
    }
}

impl PhysicalConfig {
    /// The EMCO PC Turn 50, from EMCO's manuals and schematics
    /// (`docs/_machine-facts.md`). Documented but not re-measured at the
    /// machine, except where marked UNVERIFIED (nobody has documented
    /// those).
    pub fn pc_turn_50() -> Self {
        PhysicalConfig {
            name: "EMCO PC Turn 50".to_string(),
            // From EMCO manual: X travel 48 mm of radius, not re-measured.
            // The -0.5 mm margin below centreline matches the LinuxCNC sim
            // config (AXIS_X MIN_LIMIT) and is what lets a facing pass cut
            // slightly past centre so no pip is left.
            x_min_mm: -0.5,
            x_max_mm: 48.0,
            // From EMCO manual: Z travel 228 mm usable (275 mm mechanical),
            // not re-measured. Machine zero is at the reference point and
            // all working travel runs negative from it (toward the chuck);
            // the +0.5 mm margin matches the LinuxCNC sim config (AXIS_Z
            // MAX_LIMIT).
            z_min_mm: -228.0,
            z_max_mm: 0.5,
            // From EMCO manual: rapid traverse 750 mm/min.
            rapid_mm_min: 750.0,
            // From EMCO manual: feed range 0-750 mm/min (1500 absolute max).
            max_feed_mm_min: 750.0,
            // From EMCO manual: step resolution 0.001 mm.
            resolution_mm: 0.001,
            // UNVERIFIED: axis acceleration is not documented anywhere;
            // this is a guess pending measurement at the lathe.
            accel_mm_sec2: 200.0,
            // From EMCO manual: 130-3000 rpm across three belt steps.
            spindle_min_rpm: 130.0,
            spindle_max_rpm: 3000.0,
            spindle_belt_steps: vec![
                BeltStep {
                    min_rpm: 130.0,
                    max_rpm: 1300.0,
                },
                BeltStep {
                    min_rpm: 200.0,
                    max_rpm: 2000.0,
                },
                BeltStep {
                    min_rpm: 300.0,
                    max_rpm: 3000.0,
                },
            ],
            // From EMCO manual: thread cutting synchronised only up to 400 rpm.
            threading_max_rpm: 400.0,
            // From EMCO manual: 6 stations (3 external + 3 internal). NOT 8.
            turret_stations: 6,
            // UNVERIFIED: turret index time is not documented; guess.
            turret_index_seconds: 2.0,
            // From EMCO manual: no end-of-travel limit switches — travel ends
            // at mechanical stops and the stepper stalls.
            has_end_of_travel_limits: false,
            // From EMCO manual: no absolute position memory; reference after
            // every power-on.
            requires_reference_after_power_on: true,
            reference: Reference::pc_turn_50(),
            io: IoMap::default(),
        }
    }

    /// Belt `n` (1-based): its rpm range.
    pub fn belt(&self, n: u8) -> Option<BeltStep> {
        self.spindle_belt_steps
            .get(usize::from(n).checked_sub(1)?)
            .copied()
    }

    /// The limits the physical payload enforces, in its integer units.
    pub fn limits(&self) -> Limits {
        let um = |mm: f64| libm_round(mm * 1000.0) as i32;
        Limits {
            x_min_um: um(self.x_min_mm),
            x_max_um: um(self.x_max_mm),
            z_min_um: um(self.z_min_mm),
            z_max_um: um(self.z_max_mm),
            rapid_um_s: libm_round(self.rapid_mm_min * 1000.0 / 60.0) as u32,
            max_feed_um_s: libm_round(self.max_feed_mm_min * 1000.0 / 60.0) as u32,
            spindle_max_rpm: libm_round(self.spindle_max_rpm) as i32,
            turret_stations: self.turret_stations.min(u32::from(u8::MAX)) as u8,
            ref_x_um: um(self.reference.x_mm),
            ref_z_um: um(self.reference.z_mm),
            search_um_s: libm_round(self.reference.search_mm_min * 1000.0 / 60.0) as u32,
            ref_x_first: self.reference.x_first,
            has_limits: self.has_end_of_travel_limits,
            counts_per_mm: libm_round(1.0 / self.resolution_mm) as u32,
            threading_max_rpm: libm_round(self.threading_max_rpm) as i32,
            belt_rpm: {
                let mut b = [(0, 0); crate::limits::MAX_BELTS];
                for (slot, s) in b.iter_mut().zip(&self.spindle_belt_steps) {
                    *slot = (libm_round(s.min_rpm) as i32, libm_round(s.max_rpm) as i32);
                }
                b
            },
            belts: self.spindle_belt_steps.len().min(crate::limits::MAX_BELTS) as u8,
        }
    }

    /// Checks the numbers make a machine: ordered travel, positive speeds,
    /// belts inside the spindle's range, a reference inside the travel.
    pub fn check(&self) -> Result<(), String> {
        let bad = |what: &str| Err(alloc::format!("{}: {what}", self.name));
        if !(self.x_min_mm < self.x_max_mm && self.z_min_mm < self.z_max_mm) {
            return bad("travel min must be below max");
        }
        if !(self.rapid_mm_min > 0.0 && self.max_feed_mm_min > 0.0 && self.resolution_mm > 0.0) {
            return bad("speeds and the resolution must be positive");
        }
        if !(0.0 < self.spindle_min_rpm && self.spindle_min_rpm <= self.spindle_max_rpm) {
            return bad("spindle range");
        }
        if self
            .spindle_belt_steps
            .iter()
            .any(|b| b.min_rpm > b.max_rpm || b.max_rpm > self.spindle_max_rpm)
        {
            return bad("a belt outside the spindle's range");
        }
        if self.spindle_belt_steps.len() > crate::limits::MAX_BELTS {
            return bad("at most four belt positions");
        }
        if self.turret_stations == 0 || self.turret_stations > 32 {
            return bad("1 to 32 turret stations");
        }
        let r = self.reference;
        let inside = |v: f64, lo: f64, hi: f64| lo <= v && v <= hi;
        if !(inside(r.x_mm, self.x_min_mm, self.x_max_mm)
            && inside(r.z_mm, self.z_min_mm, self.z_max_mm))
        {
            return bad("the reference point is outside the travel");
        }
        if r.search_mm_min <= 0.0 || r.search_mm_min > self.rapid_mm_min {
            return bad("the reference search speed must be positive and at most the rapid");
        }
        Ok(())
    }
}

/// `f64::round` without std.
fn libm_round(v: f64) -> f64 {
    let t = v as i64 as f64;
    let frac = v - t;
    if frac >= 0.5 {
        t + 1.0
    } else if frac <= -0.5 {
        t - 1.0
    } else {
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pc_turn_50_matches_the_manual() {
        let p = PhysicalConfig::pc_turn_50();
        assert_eq!(
            (p.x_min_mm, p.x_max_mm),
            (-0.5, 48.0),
            "LinuxCNC sim envelope"
        );
        assert_eq!(
            (p.z_min_mm, p.z_max_mm),
            (-228.0, 0.5),
            "LinuxCNC sim envelope"
        );
        assert_eq!(p.turret_stations, 6, "6 stations, not 8");
        assert!(
            !p.has_end_of_travel_limits,
            "no limit switches on this machine"
        );
        assert_eq!(
            p.belt(2),
            Some(BeltStep {
                min_rpm: 200.0,
                max_rpm: 2000.0
            })
        );
        assert_eq!(p.belt(0), None);
        assert_eq!(p.belt(4), None);
        assert_eq!(p.check(), Ok(()));
        let l = p.limits();
        assert_eq!(
            (l.x_min_um, l.z_max_um, l.rapid_um_s, l.ref_x_um),
            (-500, 500, 12_500, 48_000)
        );
        assert!(l.contains(0, -100_000) && !l.contains(0, 600));
        assert_eq!(
            (l.belts, l.belt(2), l.belt(4), l.threading_max_rpm),
            (3, Some((200, 2000)), None, 400)
        );
        let back = Limits::decode_info(&l.encode()).unwrap();
        assert_eq!(
            (
                back.z_min_um,
                back.max_feed_um_s,
                back.spindle_max_rpm,
                back.belt(3),
                back.turret_stations,
                back.counts_per_mm
            ),
            (
                l.z_min_um,
                l.max_feed_um_s,
                3000,
                Some((300, 3000)),
                6,
                1000
            )
        );
        assert_eq!(Limits::decode_info(&l.encode()[1..]), None);
    }

    /// The single source: this file is what the browser sender's fallback
    /// profile is checked against (sender/web/tests/profile.test.ts).
    #[test]
    fn the_fixture_is_the_pc_turn_50() {
        let json = serde_json::to_string_pretty(&PhysicalConfig::pc_turn_50()).unwrap() + "\n";
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/pc_turn_50.json");
        if std::env::var_os("CNC_WRITE_FIXTURES").is_some() {
            std::fs::write(path, &json).unwrap();
        }
        let on_disk = std::fs::read_to_string(path).expect("run with CNC_WRITE_FIXTURES=1 once");
        assert_eq!(
            on_disk, json,
            "fixtures/pc_turn_50.json is stale: CNC_WRITE_FIXTURES=1 cargo test"
        );
        let back: PhysicalConfig = serde_json::from_str(&on_disk).unwrap();
        assert_eq!(back, PhysicalConfig::pc_turn_50());
    }

    #[test]
    fn older_profiles_without_reference_or_io_still_parse() {
        let json = r#"{"name":"t","xMinMm":0,"xMaxMm":48,"zMinMm":-228,"zMaxMm":0,
            "rapidMmMin":750,"maxFeedMmMin":750,"resolutionMm":0.001,"accelMmSec2":150,
            "spindleMinRpm":130,"spindleMaxRpm":3000,"spindleBeltSteps":[],
            "threadingMaxRpm":400,"turretStations":6,"turretIndexSeconds":1.5,
            "hasEndOfTravelLimits":false,"requiresReferenceAfterPowerOn":true}"#;
        let p: PhysicalConfig = serde_json::from_str(json).unwrap();
        assert_eq!(p.reference, Reference::pc_turn_50());
        assert_eq!(p.io, IoMap::default());
    }

    #[test]
    fn bad_machines_are_refused() {
        let base = PhysicalConfig::pc_turn_50();
        let cases: [fn(&mut PhysicalConfig); 5] = [
            |p| p.x_max_mm = p.x_min_mm,
            |p| p.spindle_belt_steps[0].max_rpm = 9000.0,
            |p| p.turret_stations = 0,
            |p| p.reference.z_mm = 10.0,
            |p| p.reference.search_mm_min = 10_000.0,
        ];
        for f in cases {
            let mut p = base.clone();
            f(&mut p);
            assert!(p.check().is_err(), "{p:?}");
        }
    }

    #[test]
    fn the_io_map_makes_the_binding() {
        let specs = IoMap::default().binding_specs();
        assert_eq!(specs.len(), 11);
        assert_eq!(specs[0], "x_pos=driver:x_pos");
        assert_eq!(
            specs[10], "queue=driver:motion",
            "the queue's state comes from the motion point"
        );
    }
}
