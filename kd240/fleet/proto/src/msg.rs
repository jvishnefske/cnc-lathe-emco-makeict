//! Messages. [`Order`]s are signed on the build host; everything else
//! (polls, telemetry, results) is plain JSON between the board and the
//! server, authenticated by the device's bearer token.
//!
//! There are no machine commands here, by design: no G-code, no jog, no
//! run, nothing for the RS-485 bus. The only actions are the two system
//! ones in [`Action`].

use serde::{Deserialize, Serialize};

use crate::bundle::Bundle;
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

/// "This bundle is the device's new release." With `stage`, the board
/// writes it into its inactive slot once it has it; it boots it only on a
/// `reboot` that names it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub device: String,
    pub seq: u64,
    pub bundle: Bundle,
    /// Write it into the inactive slot now.
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
    /// Unix seconds. After this the board refuses it, so a reboot held back
    /// by a busy machine does not fire days later.
    pub not_after: u64,
    pub action: Action,
}

/// There is no `switch`: an A/B system can't change its root file system
/// while it runs. A new release takes a reboot.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    /// Write `bundle` into the inactive slot and check it. Changes nothing
    /// the running system uses, so it is allowed whatever the lathe service
    /// is doing.
    Stage { bundle: Bundle },
    /// Reboot. With a bundle: stage it unless it is already staged, check
    /// it again, mark its slot to be tried once, then reboot into it; the
    /// new system commits itself once healthy, or U-Boot falls back.
    /// Interlocked.
    Reboot {
        #[serde(default)]
        bundle: Option<Bundle>,
    },
}

impl Action {
    pub fn name(&self) -> &'static str {
        match self {
            Action::Stage { .. } => "stage",
            Action::Reboot { .. } => "reboot",
        }
    }

    pub fn bundle(&self) -> Option<&Bundle> {
        match self {
            Action::Stage { bundle } => Some(bundle),
            Action::Reboot { bundle } => bundle.as_ref(),
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
    /// `/run/current-system`: the running release's version.
    pub current_system: Option<String>,
    /// The A/B slots, if the board has them.
    #[serde(default)]
    pub slots: Option<Slots>,
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

/// The A/B slots as the board sees them.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct Slots {
    /// The slot this boot came from (`a`, `b`), from the kernel command line.
    pub booted: Option<String>,
    /// The committed slot, from the boot state.
    pub active: Option<String>,
    /// A slot marked to be tried at the next boot.
    pub try_slot: Option<String>,
    /// The release in each slot, as far as the agent knows.
    pub a: Option<String>,
    pub b: Option<String>,
    /// The newest manifest's release: written and checked (`staged`), or
    /// only announced.
    pub available: Option<String>,
    pub staged: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Temp {
    pub name: String,
    pub milli_c: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ActionResult {
    pub seq: u64,
    /// `manifest`, `stage`, `reboot`, `boot`, or `?` if unreadable.
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
    /// Refused: bad signature, wrong device, replay, expired, bad bundle.
    Rejected,
    /// Tried and failed (download, hash mismatch, a new slot that did not
    /// come up and was rolled back, …).
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
            action: Action::Stage {
                bundle: crate::bundle::tests::bundle("v1"),
            },
        });
        let j = serde_json::to_value(&o).unwrap();
        assert_eq!(j["kind"], "command");
        assert_eq!(j["action"]["type"], "stage");
        assert_eq!(j["action"]["bundle"]["files"][0]["name"], "rootfs.squashfs");
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
        for kind in ["run", "jog", "gcode", "mdi", "command", "switch"] {
            let j = format!(
                r#"{{"kind":"command","device":"d","seq":1,"not_after":2,"action":{{"type":"{kind}"}}}}"#
            );
            assert!(serde_json::from_str::<Order>(&j).is_err(), "{kind}");
        }
    }
}
