//! Everything the agent does to the system goes through [`System`], so the
//! tests run against a fake and never run Nix or reboot anything.
//!
//! [`RealSystem`] runs exactly four programs, and the agent's tests check
//! this file for any other (`lib.rs`, `executor_runs_only_the_allowed_programs`):
//!
//! * `nix-store --realise <toplevel>`: substitute the closure from the
//!   binary caches the board trusts (Nix checks their signatures)
//! * `nix-env -p /nix/var/nix/profiles/system --set <toplevel>`: a new
//!   system profile generation (older ones stay)
//! * `systemd-run … <toplevel>/bin/switch-to-configuration boot|switch`:
//!   write the boot entry on the root partition's /boot, and with `switch`
//!   activate; run as its own transient unit, as nixos-rebuild does, so a
//!   restart of this agent's unit cannot cut it short
//! * `systemctl reboot`
//!
//! It never touches the boot firmware, its saved environment, or the
//! card's first (FAT) partition.

use std::path::Path;
use std::process::Command;

use emco_fleet_proto::msg::{Telemetry, Temp};
use emco_fleet_proto::policy::{read_toplevel_facts, Generation, ToplevelFacts};

use crate::telemetry::*;

pub const SYSTEM_PROFILE: &str = "/nix/var/nix/profiles/system";
const PROFILES_DIR: &str = "/nix/var/nix/profiles";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Activation {
    /// Boot entry only; takes effect at the next boot.
    Boot,
    /// Boot entry and activate now.
    Switch,
}

impl Activation {
    pub fn arg(self) -> &'static str {
        match self {
            Activation::Boot => "boot",
            Activation::Switch => "switch",
        }
    }
}

pub trait System {
    /// Unix seconds.
    fn now(&self) -> u64;
    /// Host facts for telemetry. The agent adds its own fields.
    fn host_telemetry(&self) -> Telemetry;
    /// `/run/current-system`'s store path.
    fn current_system(&self) -> Option<String>;
    /// The system profile's generations.
    fn generations(&self) -> Result<Vec<Generation>, String>;
    /// Fetch the closure into the store.
    fn realise(&self, toplevel: &str) -> Result<(), String>;
    fn toplevel_facts(&self, toplevel: &str) -> ToplevelFacts;
    /// A new system profile generation pointing at `toplevel`.
    fn set_profile(&self, toplevel: &str) -> Result<(), String>;
    /// `<toplevel>/bin/switch-to-configuration boot|switch`.
    fn activate(&self, toplevel: &str, how: Activation) -> Result<(), String>;
    fn reboot(&self) -> Result<(), String>;
}

/// The board.
pub struct RealSystem;

fn read(path: impl AsRef<Path>) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

fn store_target(link: &str) -> Option<String> {
    std::fs::canonicalize(link)
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
}

fn run(cmd: &mut Command) -> Result<String, String> {
    let shown = format!("{cmd:?}");
    let out = cmd.output().map_err(|e| format!("{shown}: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        let tail = err.len().saturating_sub(400);
        let tail = (tail..err.len())
            .find(|&i| err.is_char_boundary(i))
            .unwrap_or(err.len());
        Err(format!("{shown}: {}: {}", out.status, err[tail..].trim()))
    }
}

fn temps() -> Vec<Temp> {
    let mut out = Vec::new();
    // hwmon: temp*_input in milli-degrees.
    if let Ok(dirs) = std::fs::read_dir("/sys/class/hwmon") {
        for d in dirs.flatten() {
            let dir = d.path();
            let name = read(dir.join("name"))
                .unwrap_or_default()
                .trim()
                .to_string();
            let Ok(files) = std::fs::read_dir(&dir) else {
                continue;
            };
            for f in files.flatten() {
                let fname = f.file_name().to_string_lossy().into_owned();
                let Some(chan) = fname
                    .strip_suffix("_input")
                    .filter(|c| c.starts_with("temp"))
                else {
                    continue;
                };
                if let Some(v) = read(f.path()).as_deref().and_then(hwmon_milli_c) {
                    let label = read(dir.join(format!("{chan}_label")))
                        .map(|l| l.trim().to_string())
                        .unwrap_or_else(|| chan.to_string());
                    out.push(Temp {
                        name: clip(&format!("{name}/{label}"), 40),
                        milli_c: v,
                    });
                }
            }
        }
    }
    // IIO (the Zynq UltraScale+ AMS): (raw + offset) * scale.
    if let Ok(dirs) = std::fs::read_dir("/sys/bus/iio/devices") {
        for d in dirs.flatten() {
            let dir = d.path();
            let name = read(dir.join("name"))
                .unwrap_or_default()
                .trim()
                .to_string();
            let Ok(files) = std::fs::read_dir(&dir) else {
                continue;
            };
            for f in files.flatten() {
                let fname = f.file_name().to_string_lossy().into_owned();
                let Some(chan) = fname
                    .strip_suffix("_raw")
                    .filter(|c| c.starts_with("in_temp"))
                else {
                    continue;
                };
                let offset = read(dir.join(format!("{chan}_offset")));
                let scale = read(dir.join(format!("{chan}_scale")));
                if let Some(v) = read(f.path())
                    .and_then(|r| iio_milli_c(&r, offset.as_deref(), scale.as_deref()))
                {
                    let label = read(dir.join(format!("{chan}_label")))
                        .map(|l| l.trim().to_string())
                        .unwrap_or_else(|| chan.to_string());
                    out.push(Temp {
                        name: clip(&format!("{name}/{label}"), 40),
                        milli_c: v,
                    });
                }
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out.truncate(8);
    out
}

impl System for RealSystem {
    fn now(&self) -> u64 {
        emco_fleet_proto::unix_now()
    }

    fn host_telemetry(&self) -> Telemetry {
        let (mem_total_kib, mem_available_kib) = read("/proc/meminfo")
            .map(|m| parse_meminfo(&m))
            .unwrap_or((0, 0));
        let failed = run(Command::new("systemctl").args([
            "list-units",
            "--state=failed",
            "--no-legend",
            "--plain",
        ]))
        .ok()
        .map(|s| count_lines(&s));
        Telemetry {
            hostname: clip(
                read("/proc/sys/kernel/hostname").unwrap_or_default().trim(),
                64,
            ),
            current_system: store_target("/run/current-system"),
            booted_system: store_target("/run/booted-system"),
            profile_system: store_target(SYSTEM_PROFILE),
            kernel_release: clip(
                read("/proc/sys/kernel/osrelease")
                    .unwrap_or_default()
                    .trim(),
                64,
            ),
            kernel_version: clip(
                read("/proc/sys/kernel/version").unwrap_or_default().trim(),
                96,
            ),
            uptime_s: read("/proc/uptime")
                .and_then(|s| parse_uptime(&s))
                .unwrap_or(0),
            load: read("/proc/loadavg")
                .and_then(|s| parse_loadavg(&s))
                .unwrap_or([0.0; 3]),
            mem_total_kib,
            mem_available_kib,
            temps: temps(),
            failed_units: failed,
            ..Default::default()
        }
    }

    fn current_system(&self) -> Option<String> {
        store_target("/run/current-system")
    }

    fn generations(&self) -> Result<Vec<Generation>, String> {
        let now = std::time::SystemTime::now();
        let mut out = Vec::new();
        for e in std::fs::read_dir(PROFILES_DIR)
            .map_err(|e| format!("{PROFILES_DIR}: {e}"))?
            .flatten()
        {
            let name = e.file_name().to_string_lossy().into_owned();
            let Some(n) = name
                .strip_prefix("system-")
                .and_then(|r| r.strip_suffix("-link"))
                .and_then(|n| n.parse().ok())
            else {
                continue;
            };
            let Ok(target) = std::fs::read_link(e.path()) else {
                continue;
            };
            let age_s = e
                .path()
                .symlink_metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| now.duration_since(t).ok())
                .map(|d| d.as_secs())
                .unwrap_or(u64::MAX);
            out.push(Generation {
                number: n,
                target: target.to_string_lossy().into_owned(),
                age_s,
            });
        }
        Ok(out)
    }

    fn realise(&self, toplevel: &str) -> Result<(), String> {
        run(Command::new("nix-store").args(["--realise", toplevel])).map(drop)
    }

    fn toplevel_facts(&self, toplevel: &str) -> ToplevelFacts {
        read_toplevel_facts(Path::new(toplevel))
    }

    fn set_profile(&self, toplevel: &str) -> Result<(), String> {
        run(Command::new("nix-env").args(["-p", SYSTEM_PROFILE, "--set", toplevel])).map(drop)
    }

    fn activate(&self, toplevel: &str, how: Activation) -> Result<(), String> {
        let stc = format!("{toplevel}/bin/switch-to-configuration");
        run(Command::new("systemd-run").args([
            "--unit=emco-fleet-activate",
            "--collect",
            "--no-ask-password",
            "--wait",
            "--pipe",
            "--quiet",
            "--service-type=exec",
            "--",
            &stc,
            how.arg(),
        ]))
        .map(drop)
    }

    fn reboot(&self) -> Result<(), String> {
        run(Command::new("systemctl").arg("reboot")).map(drop)
    }
}
