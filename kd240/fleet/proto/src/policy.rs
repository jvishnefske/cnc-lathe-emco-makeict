//! The board's update policy, as pure functions with no I/O and no clock
//! (except [`read_toplevel_facts`], which only reads a few small files).
//!
//! The rules:
//!
//! 1. An order is accepted only if it verified against a trusted key (done
//!    before this module), names this device, has a sequence number above
//!    the last one accepted, has not expired, and names a store path that is
//!    a NixOS system for this host and Nix system ([`accept_order`]).
//! 2. Fetching and staging are always allowed: they change nothing until the
//!    next boot. Switching and rebooting are allowed only while the lathe
//!    service reports `run` = `idle` or `disconnected`; anything else,
//!    including "could not ask", defers them ([`interlock`]). The agent asks
//!    again immediately before it acts.
//! 3. Before a new generation is made, the running system is kept as a
//!    recent generation, so it stays in the U-Boot menu and survives the
//!    weekly garbage collection ([`plan_profile`]).
//!
//! What the agent never does lives in its system executor: it runs only
//! `nix-store --realise`, `nix-env -p /nix/var/nix/profiles/system --set`,
//! `<toplevel>/bin/switch-to-configuration boot|switch` and
//! `systemctl reboot`.

use std::path::Path;

use crate::msg::{Action, Order};

// ------------------------------------------------------------ the interlock

/// The lathe service's `session.run` (`sender/core` `RunState`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunState {
    Disconnected,
    Idle,
    Running,
    Hold,
    Alarm,
    Homing,
    Toolchange,
    /// A value this agent does not know. Treated as busy.
    Other(String),
}

impl RunState {
    pub fn parse(s: &str) -> RunState {
        match s {
            "disconnected" => RunState::Disconnected,
            "idle" => RunState::Idle,
            "running" => RunState::Running,
            "hold" => RunState::Hold,
            "alarm" => RunState::Alarm,
            "homing" => RunState::Homing,
            "toolchange" => RunState::Toolchange,
            other => RunState::Other(other.to_string()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            RunState::Disconnected => "disconnected",
            RunState::Idle => "idle",
            RunState::Running => "running",
            RunState::Hold => "hold",
            RunState::Alarm => "alarm",
            RunState::Homing => "homing",
            RunState::Toolchange => "toolchange",
            RunState::Other(s) => s,
        }
    }
}

/// `session.run` out of the service's `GET /api/state` body.
pub fn run_state_from_api(body: &serde_json::Value) -> Result<RunState, String> {
    body.get("session")
        .and_then(|s| s.get("run"))
        .and_then(|r| r.as_str())
        .map(RunState::parse)
        .ok_or_else(|| "no session.run in /api/state".to_string())
}

/// What the agent is about to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Substitute the closure from the trusted cache.
    Fetch,
    /// New system profile generation + boot entry; nothing runs.
    Stage,
    /// Activate now.
    Switch,
    /// Reboot now.
    Reboot,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Gate {
    Proceed,
    /// Not now, and why. Never forced.
    Defer(String),
}

/// May `step` go ahead, given what the lathe service said (or why it could
/// not be asked)?
pub fn interlock(step: Step, local: &Result<RunState, String>) -> Gate {
    match step {
        Step::Fetch | Step::Stage => Gate::Proceed,
        Step::Switch | Step::Reboot => match local {
            Ok(RunState::Idle | RunState::Disconnected) => Gate::Proceed,
            Ok(other) => Gate::Defer(format!("lathe service is {}", other.as_str())),
            Err(e) => Gate::Defer(format!("lathe service state unknown ({e})")),
        },
    }
}

// --------------------------------------------------------- order acceptance

/// Who the board is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    pub device: String,
    /// `networking.hostName`.
    pub host: String,
    /// e.g. `aarch64-linux`.
    pub system: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub reason: String,
    /// Count its sequence number as used, so an honest server stops sending
    /// it. True only for orders that are validly signed, for this device
    /// and newer than the last, but unusable (expired, wrong host, …). A
    /// replay or another device's order leaves the counter alone.
    pub consume: bool,
}

fn refuse(reason: String, consume: bool) -> Refusal {
    Refusal { reason, consume }
}

/// The checks on a verified order before anything is done with it.
pub fn accept_order(order: &Order, me: &Identity, last_seq: u64, now: u64) -> Result<(), Refusal> {
    if order.device() != me.device {
        return Err(refuse(
            format!("for device {:?}, this is {:?}", order.device(), me.device),
            false,
        ));
    }
    if order.seq() <= last_seq {
        return Err(refuse(
            format!("replay: seq {} is not above {last_seq}", order.seq()),
            false,
        ));
    }
    let toplevel = match order {
        Order::Manifest(m) => {
            if m.host != me.host {
                return Err(refuse(
                    format!("manifest is for host {:?}, this is {:?}", m.host, me.host),
                    true,
                ));
            }
            if m.system != me.system {
                return Err(refuse(
                    format!("manifest is for {}, this is {}", m.system, me.system),
                    true,
                ));
            }
            Some(m.toplevel.as_str())
        }
        Order::Command(c) => {
            if now > c.not_after {
                return Err(refuse(
                    format!("expired at {} (now {now})", c.not_after),
                    true,
                ));
            }
            c.action.toplevel()
        }
    };
    if let Some(t) = toplevel {
        system_store_path(t, &me.host).map_err(|e| refuse(e, true))?;
    }
    Ok(())
}

/// Whether a command needs the interlock (switch, reboot) or not (stage).
pub fn needs_interlock(action: &Action) -> bool {
    !matches!(action, Action::Stage { .. })
}

// ------------------------------------------------------------ the toplevel

const NIX_BASE32: &[u8] = b"0123456789abcdfghijklmnpqrsvwxyz";

/// `/nix/store/<32 base32>-nixos-system-<host>-<version>`: an output path
/// (never a `.drv`, which realising would *build*), with nothing after it.
pub fn system_store_path(p: &str, host: &str) -> Result<(), String> {
    let bad = |why: &str| {
        Err(format!(
            "{p:?} is not a NixOS system store path for {host:?}: {why}"
        ))
    };
    let Some(rest) = p.strip_prefix("/nix/store/") else {
        return bad("not in /nix/store");
    };
    if rest.len() < 34 || !rest.is_char_boundary(32) {
        return bad("too short");
    }
    let (hash, name) = rest.split_at(32);
    if !hash.bytes().all(|b| NIX_BASE32.contains(&b)) {
        return bad("bad hash");
    }
    let Some(name) = name.strip_prefix('-') else {
        return bad("bad hash");
    };
    if name.len() > 211
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"+-._?=".contains(&b))
    {
        return bad("bad name");
    }
    if name.ends_with(".drv") {
        return bad("a derivation, not an output");
    }
    if !name.starts_with(&format!("nixos-system-{host}-")) {
        return bad("not nixos-system-<host>-…");
    }
    Ok(())
}

/// What a realised toplevel looks like on disk.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ToplevelFacts {
    /// `<toplevel>/system`, e.g. `aarch64-linux`.
    pub system: Option<String>,
    /// `<toplevel>/etc/hostname`.
    pub hostname: Option<String>,
    pub has_switch_to_configuration: bool,
    pub has_nixos_version: bool,
    pub has_init: bool,
}

/// Read the facts of a toplevel. Missing files read as None or false.
pub fn read_toplevel_facts(path: &Path) -> ToplevelFacts {
    let read = |rel: &str| {
        std::fs::read_to_string(path.join(rel))
            .ok()
            .map(|s| s.trim().to_string())
    };
    ToplevelFacts {
        system: read("system"),
        hostname: read("etc/hostname"),
        has_switch_to_configuration: path.join("bin/switch-to-configuration").is_file(),
        has_nixos_version: path.join("nixos-version").is_file(),
        has_init: path.join("init").is_file(),
    }
}

/// Is the toplevel at `path` a NixOS system for this host?
pub fn check_toplevel(path: &str, facts: &ToplevelFacts, me: &Identity) -> Result<(), String> {
    system_store_path(path, &me.host)?;
    if !(facts.has_switch_to_configuration && facts.has_nixos_version && facts.has_init) {
        return Err(format!(
            "{path} is not a NixOS system (no bin/switch-to-configuration, nixos-version or init)"
        ));
    }
    match &facts.system {
        Some(s) if *s == me.system => {}
        other => return Err(format!("{path} is for {other:?}, this is {}", me.system)),
    }
    match &facts.hostname {
        Some(h) if *h == me.host => {}
        other => {
            return Err(format!(
                "{path} has host name {other:?}, this is {:?}",
                me.host
            ))
        }
    }
    Ok(())
}

// --------------------------------------------- keeping the previous system

/// One `/nix/var/nix/profiles/system-<number>-link`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Generation {
    pub number: u64,
    /// The store path it points at.
    pub target: String,
    /// Seconds since it was made.
    pub age_s: u64,
}

/// The board's boot menu keeps this many generations (configuration.nix:
/// `boot.loader.generic-extlinux-compatible.configurationLimit`).
pub const MENU_GENERATIONS: usize = 5;
/// The weekly GC deletes generations older than 30 days; re-recording the
/// running system once its generation is a week old keeps it well clear.
pub const KEEP_MAX_AGE_S: u64 = 7 * 24 * 3600;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfilePlan {
    /// First record this (the running system) as a new generation, so that
    /// rolling back to it from the U-Boot menu stays possible.
    pub preserve: Option<String>,
    /// Then make the target the newest generation (false: it already is).
    pub set_target: bool,
}

/// How to make `target` the profile's newest generation while keeping the
/// running system `current` reachable: it must be among the newest
/// `menu - 1` generations after the change, and young enough not to be
/// collected.
pub fn plan_profile(
    current: Option<&str>,
    gens: &[Generation],
    target: &str,
    menu: usize,
    max_age_s: u64,
) -> ProfilePlan {
    let mut newest_first: Vec<&Generation> = gens.iter().collect();
    newest_first.sort_by(|a, b| b.number.cmp(&a.number));
    let set_target = newest_first
        .first()
        .map(|g| g.target != target)
        .unwrap_or(true);
    let preserve = match current {
        Some(cur) if cur != target && set_target => {
            // After setting the target, the newest `menu - 1` older generations
            // stay in the menu; the running system must be one of them.
            let kept = newest_first
                .iter()
                .take(menu.saturating_sub(1))
                .any(|g| g.target == cur && g.age_s <= max_age_s);
            (!kept).then(|| cur.to_string())
        }
        _ => None,
    };
    ProfilePlan {
        preserve,
        set_target,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::msg::{Command, Manifest};

    const H: &str = "0123456789abcdfghijklmnpqrsvwxyz";

    fn me() -> Identity {
        Identity {
            device: "kd240".into(),
            host: "kd240".into(),
            system: "aarch64-linux".into(),
        }
    }

    fn top(v: &str) -> String {
        format!("/nix/store/{H}-nixos-system-kd240-{v}")
    }

    fn idle() -> Result<RunState, String> {
        Ok(RunState::Idle)
    }

    // ---- interlock

    #[test]
    fn stage_and_fetch_always_proceed() {
        for local in [
            idle(),
            Ok(RunState::Running),
            Ok(RunState::Alarm),
            Err("down".into()),
        ] {
            assert_eq!(interlock(Step::Stage, &local), Gate::Proceed);
            assert_eq!(interlock(Step::Fetch, &local), Gate::Proceed);
        }
    }

    #[test]
    fn switch_and_reboot_only_when_idle_or_disconnected() {
        for step in [Step::Switch, Step::Reboot] {
            assert_eq!(interlock(step, &Ok(RunState::Idle)), Gate::Proceed);
            assert_eq!(interlock(step, &Ok(RunState::Disconnected)), Gate::Proceed);
            for busy in ["running", "hold", "alarm", "homing", "toolchange", "estop?"] {
                let g = interlock(step, &Ok(RunState::parse(busy)));
                assert!(
                    matches!(&g, Gate::Defer(why) if why.contains(busy)),
                    "{busy}: {g:?}"
                );
            }
        }
    }

    #[test]
    fn unknown_state_defers() {
        let g = interlock(Step::Switch, &Err("connection refused".into()));
        assert!(matches!(g, Gate::Defer(why) if why.contains("connection refused")));
    }

    #[test]
    fn run_state_from_the_service() {
        let body =
            serde_json::json!({"session": {"run": "running", "tool": 1}, "transport": "virtual"});
        assert_eq!(run_state_from_api(&body), Ok(RunState::Running));
        let body = serde_json::json!({"session": {"run": "idle"}});
        assert_eq!(run_state_from_api(&body), Ok(RunState::Idle));
        assert!(run_state_from_api(&serde_json::json!({"session": {}})).is_err());
        assert!(run_state_from_api(&serde_json::json!({"session": {"run": 3}})).is_err());
    }

    #[test]
    fn stage_needs_no_interlock() {
        assert!(!needs_interlock(&Action::Stage { toplevel: top("a") }));
        assert!(needs_interlock(&Action::Switch { toplevel: top("a") }));
        assert!(needs_interlock(&Action::Reboot { toplevel: None }));
    }

    // ---- acceptance

    fn manifest(seq: u64) -> Order {
        Order::Manifest(Manifest {
            device: "kd240".into(),
            seq,
            host: "kd240".into(),
            system: "aarch64-linux".into(),
            toplevel: top("25.11"),
            stage: false,
            created: 1,
            note: String::new(),
        })
    }

    fn command(seq: u64, not_after: u64, action: Action) -> Order {
        Order::Command(Command {
            device: "kd240".into(),
            seq,
            not_after,
            action,
        })
    }

    #[test]
    fn accepts_a_good_order() {
        assert_eq!(accept_order(&manifest(5), &me(), 4, 100), Ok(()));
        assert_eq!(
            accept_order(
                &command(5, 100, Action::Reboot { toplevel: None }),
                &me(),
                4,
                100
            ),
            Ok(())
        );
    }

    #[test]
    fn replay_is_refused_without_consuming() {
        for seq in [3, 4] {
            let r = accept_order(&manifest(seq), &me(), 4, 0).unwrap_err();
            assert!(r.reason.contains("replay") && !r.consume);
        }
    }

    #[test]
    fn other_device_is_refused_without_consuming() {
        let mut o = manifest(9);
        if let Order::Manifest(m) = &mut o {
            m.device = "kd240-2".into();
        }
        let r = accept_order(&o, &me(), 0, 0).unwrap_err();
        assert!(!r.consume);
    }

    #[test]
    fn expired_wrong_host_and_bad_paths_are_consumed() {
        let r = accept_order(
            &command(5, 99, Action::Reboot { toplevel: None }),
            &me(),
            0,
            100,
        )
        .unwrap_err();
        assert!(r.reason.contains("expired") && r.consume);

        let mut o = manifest(5);
        if let Order::Manifest(m) = &mut o {
            m.system = "x86_64-linux".into();
        }
        assert!(accept_order(&o, &me(), 0, 0).unwrap_err().consume);

        let o = command(
            5,
            100,
            Action::Switch {
                toplevel: format!("/nix/store/{H}-nixos-system-kd240-1.drv"),
            },
        );
        assert!(accept_order(&o, &me(), 0, 0).unwrap_err().consume);
    }

    #[test]
    fn store_paths() {
        assert!(system_store_path(&top("25.11.20251114.abc"), "kd240").is_ok());
        for bad in [
            "/nix/store/short-nixos-system-kd240-1".to_string(),
            format!("/nix/store/{}-nixos-system-kd240-1", "e".repeat(32)), // 'e' is not nix base32
            format!("/nix/store/{H}-nixos-system-other-1"),
            format!("/nix/store/{H}-nixos-system-kd240-1/bin/sh"),
            format!("/nix/store/{H}-nixos-system-kd240-1.drv"),
            format!("/tmp/{H}-nixos-system-kd240-1"),
            format!("/nix/store/{H}-nixos-system-kd240-1 --option"),
            format!("/nix/store/{H}-hello-2.12"),
        ] {
            assert!(system_store_path(&bad, "kd240").is_err(), "{bad}");
        }
    }

    // ---- toplevel

    fn facts() -> ToplevelFacts {
        ToplevelFacts {
            system: Some("aarch64-linux".into()),
            hostname: Some("kd240".into()),
            has_switch_to_configuration: true,
            has_nixos_version: true,
            has_init: true,
        }
    }

    #[test]
    fn toplevel_checks() {
        assert_eq!(check_toplevel(&top("1"), &facts(), &me()), Ok(()));
        let mut f = facts();
        f.system = Some("x86_64-linux".into());
        assert!(check_toplevel(&top("1"), &f, &me()).is_err());
        let mut f = facts();
        f.hostname = Some("laptop".into());
        assert!(check_toplevel(&top("1"), &f, &me()).is_err());
        let mut f = facts();
        f.has_switch_to_configuration = false;
        assert!(check_toplevel(&top("1"), &f, &me()).is_err());
        assert!(check_toplevel(&top("1"), &ToplevelFacts::default(), &me()).is_err());
    }

    #[test]
    fn reads_facts_from_disk() {
        let dir = std::env::temp_dir().join(format!("emco-fleet-facts-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::create_dir_all(dir.join("etc")).unwrap();
        std::fs::write(dir.join("system"), "aarch64-linux").unwrap();
        std::fs::write(dir.join("etc/hostname"), "kd240\n").unwrap();
        std::fs::write(dir.join("bin/switch-to-configuration"), "").unwrap();
        std::fs::write(dir.join("nixos-version"), "25.11").unwrap();
        std::fs::write(dir.join("init"), "").unwrap();
        assert_eq!(read_toplevel_facts(&dir), facts());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    // ---- profile plan

    fn gen(number: u64, v: &str, age_s: u64) -> Generation {
        Generation {
            number,
            target: top(v),
            age_s,
        }
    }

    #[test]
    fn plain_upgrade_needs_no_preserve() {
        let gens = [gen(1, "a", 100), gen(2, "b", 10)];
        let p = plan_profile(Some(&top("b")), &gens, &top("c"), 5, KEEP_MAX_AGE_S);
        assert_eq!(
            p,
            ProfilePlan {
                preserve: None,
                set_target: true
            }
        );
    }

    #[test]
    fn already_staged_is_a_no_op() {
        let gens = [gen(1, "a", 100), gen(2, "c", 10)];
        let p = plan_profile(Some(&top("a")), &gens, &top("c"), 5, KEEP_MAX_AGE_S);
        assert_eq!(
            p,
            ProfilePlan {
                preserve: None,
                set_target: false
            }
        );
    }

    #[test]
    fn running_system_off_the_menu_is_preserved() {
        // Rolled back from the U-Boot menu to generation 1 while the profile
        // went on to 6: setting 7 would push 1 off a five-entry menu.
        let gens: Vec<_> = (1..=6).map(|n| gen(n, &n.to_string(), 10)).collect();
        let p = plan_profile(Some(&top("1")), &gens, &top("7"), 5, KEEP_MAX_AGE_S);
        assert_eq!(
            p,
            ProfilePlan {
                preserve: Some(top("1")),
                set_target: true
            }
        );
    }

    #[test]
    fn running_system_not_in_any_generation_is_preserved() {
        let gens = [gen(1, "a", 10)];
        let p = plan_profile(Some(&top("manual")), &gens, &top("c"), 5, KEEP_MAX_AGE_S);
        assert_eq!(p.preserve, Some(top("manual")));
    }

    #[test]
    fn old_generation_of_the_running_system_is_refreshed() {
        let gens = [gen(1, "a", 20 * 24 * 3600)];
        let p = plan_profile(Some(&top("a")), &gens, &top("c"), 5, KEEP_MAX_AGE_S);
        assert_eq!(p.preserve, Some(top("a")));
    }

    #[test]
    fn empty_profile_and_unknown_current() {
        let p = plan_profile(None, &[], &top("c"), 5, KEEP_MAX_AGE_S);
        assert_eq!(
            p,
            ProfilePlan {
                preserve: None,
                set_target: true
            }
        );
    }
}
