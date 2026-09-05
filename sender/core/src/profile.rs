//! Machine profile: every machine-specific number lives here, not in code.
//!
//! The default values are the **real PC Turn 50 numbers from EMCO's own
//! manuals and schematics** (see `docs/_machine-facts.md`). They have been
//! read from documentation but **not re-measured at the machine**, so treat
//! them as documented-but-unconfirmed. Items nobody has documented at all
//! keep an explicit UNVERIFIED tag.

use serde::{Deserialize, Serialize};

/// Default G54 work offset on Z, machine coordinates: "a piece of stock whose
/// face was touched off 50 mm in from the reference point". Matches the
/// LinuxCNC simulator's shipped parameter file
/// (`linuxcnc/sim/emco-pcturn50/emco-pcturn50.var`, parameter 5223), so the
/// sender, the LinuxCNC sim and the example programs in `gcode/` all agree.
/// On the real machine the operator touches this off for every new stock.
pub const DEFAULT_G54_Z_MM: f64 = -50.0;

/// Which wire dialect `Session` speaks. See `docs/30-serial-protocol.md`.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum DialectKind {
    /// The real machine link. Its protocol is **unknown** (RS-485 bus driven
    /// by a memory-mapped ISA card on the original DOS PC — no known byte
    /// stream). This dialect is a stub until discovery produces facts.
    #[serde(rename = "emco")]
    Emco,
    /// Development/training fiction spoken by the `VirtualMachine`.
    #[serde(rename = "plain-gcode")]
    PlainGcode,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LinkKind {
    /// The machine's DB9 (`X300`) is RS-485: pin 2 GND, pin 3 DATA−,
    /// pin 8 DATA+. **Not RS-232** — do not connect an RS-232 adapter.
    Rs485,
}

/// The physical PC link. Replaces the earlier `serial` struct: the old
/// "9600 8N1 RS-232 with hardware flow control" story was wrong for this
/// machine (those settings belong to the DOS PC's file-transfer COM port).
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct LinkConfig {
    pub kind: LinkKind,
    /// UNVERIFIED: the RS-485 wire framing (and hence any baud rate) is not
    /// yet characterised; the original ISA card was memory-mapped, not a UART.
    pub baud: u32,
}

/// One spindle belt position. Changed by physically moving a belt, so this is
/// operator-declared, never machine-reported.
#[derive(Serialize, Deserialize, Clone, Copy, Debug)]
#[serde(rename_all = "camelCase")]
pub struct BeltStep {
    pub min_rpm: f64,
    pub max_rpm: f64,
}

/// See `sender/INTERFACE.md` for the JSON shape (camelCase field names).
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct MachineProfile {
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
    /// False on this machine: travel ends at mechanical stops and the stepper
    /// stalls audibly, silently losing position. When false the simulator
    /// models that stall; when true it raises a clean limit alarm instead.
    pub has_end_of_travel_limits: bool,
    /// True on this machine: no absolute position memory, so the reference
    /// point must be re-approached after every power-off.
    pub requires_reference_after_power_on: bool,
    pub link: LinkConfig,
    pub dialect: DialectKind,
}

impl MachineProfile {
    /// Parse a profile from JSON.
    pub fn from_json(json: &str) -> Result<MachineProfile, String> {
        serde_json::from_str(json).map_err(|e| e.to_string())
    }

    /// The EMCO PC Turn 50, from EMCO's manuals and schematics
    /// (`docs/_machine-facts.md`). Documented but not re-measured at the
    /// machine, except where marked UNVERIFIED (nobody has documented those).
    pub fn pc_turn_50_default() -> MachineProfile {
        MachineProfile {
            name: "EMCO PC Turn 50".to_string(),
            // From EMCO manual: X travel 48 mm of radius, not re-measured.
            // The -0.5 mm margin below centreline matches the LinuxCNC sim
            // config (AXIS_X MIN_LIMIT) and is what lets a facing pass cut
            // slightly past centre so no pip is left.
            x_min_mm: -0.5,
            x_max_mm: 48.0,
            // From EMCO manual: Z travel 228 mm usable (275 mm mechanical),
            // not re-measured. Machine zero is at the reference point and all
            // working travel runs negative from it (toward the chuck); the
            // +0.5 mm margin matches the LinuxCNC sim config (AXIS_Z
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
                BeltStep { min_rpm: 130.0, max_rpm: 1300.0 },
                BeltStep { min_rpm: 200.0, max_rpm: 2000.0 },
                BeltStep { min_rpm: 300.0, max_rpm: 3000.0 },
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
            link: LinkConfig {
                // From EMCO schematics: RS-485 on connector X300. The baud is
                // UNVERIFIED (wire framing not yet characterised); 0 = unknown.
                kind: LinkKind::Rs485,
                baud: 0,
            },
            dialect: DialectKind::Emco,
        }
    }
}

impl Default for MachineProfile {
    fn default() -> Self {
        MachineProfile::pc_turn_50_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_profile_matches_the_manual() {
        let p = MachineProfile::default();
        assert_eq!((p.x_min_mm, p.x_max_mm), (-0.5, 48.0), "LinuxCNC sim envelope");
        assert_eq!((p.z_min_mm, p.z_max_mm), (-228.0, 0.5), "LinuxCNC sim envelope");
        assert_eq!(DEFAULT_G54_Z_MM, -50.0, "LinuxCNC var parameter 5223");
        assert_eq!(p.rapid_mm_min, 750.0);
        assert_eq!(p.max_feed_mm_min, 750.0);
        assert_eq!(p.resolution_mm, 0.001);
        assert_eq!((p.spindle_min_rpm, p.spindle_max_rpm), (130.0, 3000.0));
        assert_eq!(p.spindle_belt_steps.len(), 3);
        assert_eq!(p.spindle_belt_steps[2].max_rpm, 3000.0);
        assert_eq!(p.threading_max_rpm, 400.0);
        assert_eq!(p.turret_stations, 6, "6 stations, not 8");
        assert!(!p.has_end_of_travel_limits, "no limit switches on this machine");
        assert!(p.requires_reference_after_power_on);
        assert_eq!(p.link.kind, LinkKind::Rs485);
        assert_eq!(p.dialect, DialectKind::Emco);
        assert!(p.accel_mm_sec2 > 0.0);
    }

    #[test]
    fn round_trips_through_camel_case_json() {
        let p = MachineProfile::default();
        let json = serde_json::to_string(&p).unwrap();
        assert!(json.contains("\"xMinMm\""), "camelCase field names required by contract");
        assert!(json.contains("\"resolutionMm\":0.001"));
        assert!(json.contains("\"spindleBeltSteps\""));
        assert!(json.contains("\"minRpm\":130.0") || json.contains("\"minRpm\":130"));
        assert!(json.contains("\"threadingMaxRpm\""));
        assert!(json.contains("\"hasEndOfTravelLimits\":false"));
        assert!(json.contains("\"requiresReferenceAfterPowerOn\":true"));
        assert!(json.contains("\"link\":{\"kind\":\"rs485\""));
        assert!(json.contains("\"dialect\":\"emco\""));
        assert!(!json.contains("serial"), "old serial struct must be gone");
        let back = MachineProfile::from_json(&json).unwrap();
        assert_eq!(back.turret_stations, p.turret_stations);
        assert_eq!(back.spindle_belt_steps.len(), 3);
    }

    #[test]
    fn parses_ui_style_profile() {
        let json = r#"{
            "name": "test", "xMinMm": 0, "xMaxMm": 48, "zMinMm": -228, "zMaxMm": 0,
            "rapidMmMin": 750, "maxFeedMmMin": 750, "resolutionMm": 0.001,
            "accelMmSec2": 150,
            "spindleMinRpm": 130, "spindleMaxRpm": 3000,
            "spindleBeltSteps": [{"minRpm":130,"maxRpm":1300},{"minRpm":200,"maxRpm":2000},
                                 {"minRpm":300,"maxRpm":3000}],
            "threadingMaxRpm": 400,
            "turretStations": 6, "turretIndexSeconds": 1.5,
            "hasEndOfTravelLimits": false,
            "requiresReferenceAfterPowerOn": true,
            "link": {"kind": "rs485", "baud": 0},
            "dialect": "plain-gcode"
        }"#;
        let p = MachineProfile::from_json(json).unwrap();
        assert_eq!(p.dialect, DialectKind::PlainGcode);
        assert_eq!(p.turret_stations, 6);
        assert_eq!(p.threading_max_rpm, 400.0);
    }
}
