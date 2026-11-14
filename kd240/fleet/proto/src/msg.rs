//! Messages. [`Order`]s are signed on the build host; everything else
//! (polls, telemetry, results) is plain JSON between the board and the
//! server, authenticated by the device's bearer token.
//!
//! There are no machine commands here, by design: no G-code, no jog, no
//! run, nothing for the RS-485 bus. The only actions are the three system
//! ones in [`Action`].

use serde::{Deserialize, Serialize};

use crate::keys::Envelope;

/// What the build host signs. One sequence-number space per device covers
/// both kinds: the board accepts an order only if its `seq` is greater than
/// the last one it accepted (persisted on the board), so a recorded order
/// cannot be replayed.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Order {
    Manifest(Manifest),
    Command(Command),
}

impl Order {
    pub fn device(&self) -> &str {
        match self {
            Order::Manifest(m) => &m.device,
            Order::Command(c) => &c.device,
        }
    }

    pub fn seq(&self) -> u64 {
        match self {
            Order::Manifest(m) => m.seq,
            Order::Command(c) => c.seq,
        }
    }

    /// A short name for results and logs.
    pub fn what(&self) -> &'static str {
        match self {
            Order::Manifest(_) => "manifest",
            Order::Command(c) => c.action.name(),
        }
    }
}

/// "This toplevel is the device's new system." The board fetches the
/// closure from its trusted binary cache and checks it; with `stage` it
/// also makes it the default for the next boot.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub device: String,
    pub seq: u64,
    /// The NixOS host name the toplevel is for (`networking.hostName`).
    pub host: String,
    /// The Nix system, e.g. `aarch64-linux`.
    pub system: String,
    /// `/nix/store/…-nixos-system-<host>-<version>`.
    pub toplevel: String,
    /// Stage it (next boot) once fetched.
    #[serde(default)]
    pub stage: bool,
    /// Unix seconds when signed.
    pub created: u64,
    #[serde(default)]
    pub note: String,
}

/// A system command for one device, valid until `not_after`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Command {
    pub device: String,
    pub seq: u64,
    /// Unix seconds. After this the board refuses it, so a switch held back
    /// by a busy machine does not fire days later.
    pub not_after: u64,
    pub action: Action,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    /// Make `toplevel` the default for the next boot. Changes nothing now,
    /// so it is allowed whatever the lathe service is doing.
    Stage { toplevel: String },
    /// Stage `toplevel` and activate it now. Interlocked.
    Switch { toplevel: String },
    /// Reboot, after staging `toplevel` if one is given. Interlocked.
    Reboot {
        #[serde(default)]
        toplevel: Option<String>,
    },
}

impl Action {
    pub fn name(&self) -> &'static str {
        match self {
            Action::Stage { .. } => "stage",
            Action::Switch { .. } => "switch",
            Action::Reboot { .. } => "reboot",
        }
    }

    pub fn toplevel(&self) -> Option<&str> {
        match self {
            Action::Stage { toplevel } | Action::Switch { toplevel } => Some(toplevel),
            Action::Reboot { toplevel } => toplevel.as_deref(),
        }
    }
}

// ----------------------------------------------------------------- the wire

/// `POST /api/v1/devices/{id}/poll`: the board's telemetry, and the last
/// sequence number it accepted (so the server can drop what is done).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct PollRequest {
    pub last_seq: u64,
    pub telemetry: Telemetry,
}

/// The poll's answer: the signed orders newer than `last_seq`.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct PollResponse {
    #[serde(default)]
    pub manifest: Option<Envelope>,
    #[serde(default)]
    pub commands: Vec<Envelope>,
}

/// What the board reports, kept small (well under 2 KiB).
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Telemetry {
    pub agent_version: String,
    pub hostname: String,
    /// `/run/current-system`.
    pub current_system: Option<String>,
    /// `/run/booted-system`.
    pub booted_system: Option<String>,
    /// What the system profile points at (the next boot's default).
    pub profile_system: Option<String>,
    /// The newest manifest's toplevel the board has fetched and checked.
    pub available_system: Option<String>,
    /// `uname -r`.
    pub kernel_release: String,
    /// `uname -v`.
    pub kernel_version: String,
    pub uptime_s: u64,
    pub load: [f32; 3],
    pub mem_total_kib: u64,
    pub mem_available_kib: u64,
    /// Up to eight sensors, milli-degrees Celsius.
    #[serde(default)]
    pub temps: Vec<Temp>,
    /// `systemctl --failed` count; None if it could not be read.
    pub failed_units: Option<u32>,
    /// The lathe service's `session.run`, or why it could not be read.
    pub run_state: String,
    /// The newest results, newest last (at most ten).
    #[serde(default)]
    pub results: Vec<ActionResult>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Temp {
    pub name: String,
    pub milli_c: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ActionResult {
    pub seq: u64,
    /// `manifest`, `stage`, `switch`, `reboot`, or `?` if unreadable.
    pub what: String,
    pub outcome: Outcome,
    pub detail: String,
    /// Unix seconds.
    pub at: u64,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Done,
    /// Not done yet, by the interlock; retried at the next poll.
    Deferred,
    /// Refused: bad signature, wrong device, replay, expired, bad toplevel.
    Rejected,
    /// Tried and failed (fetch, activation, …).
    Failed,
}

/// One telemetry sample as the server keeps it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Sample {
    /// Unix seconds, the server's clock.
    pub at: u64,
    pub last_seq: u64,
    pub telemetry: Telemetry,
}

/// `GET /api/v1/devices`, one entry per device.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct DeviceSummary {
    pub device: String,
    pub latest: Option<Sample>,
    /// The published manifest, as the server holds it.
    pub manifest: Option<Envelope>,
    pub pending_commands: Vec<Envelope>,
}

/// `GET /api/v1/devices/{id}`: the summary and the history, oldest first.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct DeviceDetail {
    pub summary: DeviceSummary,
    pub history: Vec<Sample>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn order_json_shape() {
        let o = Order::Command(Command {
            device: "kd240".into(),
            seq: 3,
            not_after: 10,
            action: Action::Switch {
                toplevel: "/nix/store/x".into(),
            },
        });
        let j = serde_json::to_value(&o).unwrap();
        assert_eq!(j["kind"], "command");
        assert_eq!(j["action"]["type"], "switch");
        assert_eq!(serde_json::from_value::<Order>(j).unwrap(), o);
    }

    #[test]
    fn unknown_fields_in_signed_orders_are_refused() {
        // A newer signer's restriction an older board would silently ignore
        // must make the order invalid instead.
        let j = r#"{"kind":"command","device":"d","seq":1,"not_after":2,
                    "action":{"type":"reboot"},"not_before":5}"#;
        assert!(serde_json::from_str::<Order>(j).is_err());
        let j = r#"{"kind":"command","device":"d","seq":1,"not_after":2,
                    "action":{"type":"reboot","force":true}}"#;
        assert!(serde_json::from_str::<Order>(j).is_err());
    }

    #[test]
    fn there_is_no_machine_action() {
        for kind in ["run", "jog", "gcode", "mdi", "command"] {
            let j = format!(
                r#"{{"kind":"command","device":"d","seq":1,"not_after":2,"action":{{"type":"{kind}"}}}}"#
            );
            assert!(serde_json::from_str::<Order>(&j).is_err(), "{kind}");
        }
    }
}
