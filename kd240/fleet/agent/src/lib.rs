//! The KD240 fleet agent.
//!
//! Each run (a systemd timer, `--once`) it:
//!
//! 1. settles the boot ([`Agent::settle_boot`]): a trial boot of a new slot
//!    commits itself once the lathe service answers, and the outcome of the
//!    last trial (committed, or rolled back by U-Boot) is reported;
//! 2. gathers telemetry and POSTs it to the server's poll endpoint, with the
//!    last sequence number it accepted;
//! 3. verifies every signed order in the answer against the owner's public
//!    keys (middleware's verifier, `emco_fleet_proto::keys`), then its device
//!    id, sequence number, expiry and bundle
//!    ([`emco_fleet_proto::policy::accept_order`]);
//! 4. carries them out in sequence order through [`System`]: staging writes
//!    the bundle's files into the inactive A/B slot and reads them back;
//!    a reboot is behind the lathe-service interlock
//!    ([`emco_fleet_proto::policy::interlock`]), asked once when the order
//!    is taken up and again immediately before the act;
//! 5. reports the results.
//!
//! A deferred order is reported and left for the next poll, and nothing
//! after it runs first. The only things it reads from the lathe service are
//! `GET /api/state`'s `session.run`. It sends the service nothing.
#![forbid(unsafe_code)]

pub mod system;
pub mod telemetry;

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::time::Duration;

use emco_fleet_proto::bootstate::{self, BootState, Slot};
use emco_fleet_proto::bundle::{Bundle, BundleFile};
use emco_fleet_proto::keys::{hex, Envelope, PublicKey};
use emco_fleet_proto::msg::{
    Action, ActionResult, Command, Manifest, Order, Outcome, PollRequest, PollResponse, Slots,
    Telemetry,
};
use emco_fleet_proto::policy::{
    accept_order, interlock, is_trial_boot, on_boot, run_state_from_api, stage_target, trial,
    trial_outcome, Gate, Identity, RunState, Step, TrialOutcome,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub use system::{Dest, RealSystem, SlotWriter, System};

const KEEP_RESULTS: usize = 10;
const CHUNK: usize = 1 << 16;

pub struct Config {
    /// `https://fleet.example.com` (no trailing path).
    pub server: String,
    pub identity: Identity,
    pub token: String,
    pub trusted: Vec<PublicKey>,
    /// Where `state.json` (the last sequence number and results) lives.
    pub state_dir: PathBuf,
    /// The lathe service's `GET /api/state`.
    pub local_state_url: String,
}

/// What one slot holds, as far as this agent wrote and checked it.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SlotContent {
    /// [`Bundle::digest`].
    pub digest: String,
    /// [`Bundle::version`].
    pub version: String,
}

/// A reboot into a slot whose outcome is not known yet.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct PendingTrial {
    /// The reboot order's sequence number.
    pub seq: u64,
    pub slot: Slot,
    pub version: String,
}

/// What the board remembers between runs.
#[derive(Serialize, Deserialize, Default, Debug, Clone, PartialEq)]
pub struct AgentState {
    /// The highest sequence number accepted (or consumed). Orders at or
    /// below it are replays.
    pub last_seq: u64,
    /// The newest manifest's bundle.
    pub available: Option<Bundle>,
    #[serde(default)]
    pub slot_a: Option<SlotContent>,
    #[serde(default)]
    pub slot_b: Option<SlotContent>,
    #[serde(default)]
    pub trial: Option<PendingTrial>,
    pub results: VecDeque<ActionResult>,
}

impl AgentState {
    pub fn slot(&self, s: Slot) -> &Option<SlotContent> {
        match s {
            Slot::A => &self.slot_a,
            Slot::B => &self.slot_b,
        }
    }

    fn slot_mut(&mut self, s: Slot) -> &mut Option<SlotContent> {
        match s {
            Slot::A => &mut self.slot_a,
            Slot::B => &mut self.slot_b,
        }
    }

    /// The slot holding `bundle`, if one does.
    pub fn holding(&self, bundle: &Bundle) -> Option<Slot> {
        let d = bundle.digest();
        [Slot::A, Slot::B]
            .into_iter()
            .find(|s| self.slot(*s).as_ref().is_some_and(|c| c.digest == d))
    }
}

/// How an order went.
enum Exec {
    Done(String),
    Failed(String),
    Deferred(String),
    /// Everything is saved and reported; reboot now.
    Reboot,
}

pub struct Agent<S: System> {
    cfg: Config,
    sys: S,
    http: ureq::Agent,
    pub state: AgentState,
}

/// Plain HTTP only to this machine; the token must not cross a network in clear.
pub fn check_server_url(url: &str, allow_http: bool) -> Result<(), String> {
    if url.starts_with("https://") {
        return Ok(());
    }
    let loopback = ["http://127.0.0.1", "http://localhost", "http://[::1]"]
        .iter()
        .any(|p| {
            url.strip_prefix(p)
                .is_some_and(|r| r.is_empty() || r.starts_with(':') || r.starts_with('/'))
        });
    if url.starts_with("http://") && (loopback || allow_http) {
        return Ok(());
    }
    Err(format!(
        "server URL {url:?} must be https:// (or http:// on loopback)"
    ))
}

/// Copy at most `f.size` bytes from `from` into `to`, then check the count
/// and the SHA-256 against `f`.
fn copy_checked(f: &BundleFile, from: &mut dyn Read, to: &mut dyn Write) -> Result<(), String> {
    let mut hash = Sha256::new();
    let mut left = f.size;
    let mut buf = vec![0u8; CHUNK];
    let mut from = from.take(f.size.saturating_add(1));
    loop {
        let n = from
            .read(&mut buf)
            .map_err(|e| format!("{}: read: {e}", f.name))?;
        if n == 0 {
            break;
        }
        if n as u64 > left {
            return Err(format!("{}: longer than {} bytes", f.name, f.size));
        }
        left -= n as u64;
        hash.update(&buf[..n]);
        to.write_all(&buf[..n])
            .map_err(|e| format!("{}: write: {e}", f.name))?;
    }
    if left != 0 {
        return Err(format!(
            "{}: {} bytes, want {}",
            f.name,
            f.size - left,
            f.size
        ));
    }
    let got = hex(&hash.finalize());
    if got != f.sha256 {
        return Err(format!("{}: SHA-256 {got}, want {}", f.name, f.sha256));
    }
    Ok(())
}

impl<S: System> Agent<S> {
    pub fn new(cfg: Config, sys: S) -> Result<Self, String> {
        if cfg.trusted.is_empty() {
            return Err("no trusted public key: every order would be refused".into());
        }
        std::fs::create_dir_all(&cfg.state_dir)
            .map_err(|e| format!("{}: {e}", cfg.state_dir.display()))?;
        let path = cfg.state_dir.join("state.json");
        let state = match std::fs::read(&path) {
            Ok(b) => serde_json::from_slice(&b).map_err(|e| format!("{}: {e}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => AgentState::default(),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        let http = ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(30)))
            .timeout_recv_response(Some(Duration::from_secs(30)))
            .timeout_recv_body(Some(Duration::from_secs(3600)))
            .user_agent(concat!("emco-fleet-agent/", env!("CARGO_PKG_VERSION")))
            .build()
            .into();
        Ok(Agent {
            cfg,
            sys,
            http,
            state,
        })
    }

    pub fn system(&self) -> &S {
        &self.sys
    }

    fn save(&self) -> Result<(), String> {
        let path = self.cfg.state_dir.join("state.json");
        let tmp = self.cfg.state_dir.join("state.json.tmp");
        let bytes = serde_json::to_vec_pretty(&self.state).map_err(|e| e.to_string())?;
        std::fs::write(&tmp, bytes).map_err(|e| format!("{}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, &path).map_err(|e| format!("{}: {e}", path.display()))
    }

    fn record(&mut self, seq: u64, what: &str, outcome: Outcome, detail: String) {
        let at = self.sys.now();
        let detail = telemetry::clip(&detail, 300);
        // The same answer to the same order again (a deferral, a replayed
        // order) refreshes the entry instead of filling the list.
        if let Some(last) = self.state.results.back_mut() {
            if last.seq == seq
                && last.what == what
                && last.outcome == outcome
                && last.detail == detail
            {
                last.at = at;
                return;
            }
        }
        self.state.results.push_back(ActionResult {
            seq,
            what: what.to_string(),
            outcome,
            detail,
            at,
        });
        while self.state.results.len() > KEEP_RESULTS {
            self.state.results.pop_front();
        }
    }

    /// The lathe service's run state, or why it is not known.
    pub fn local_run_state(&self) -> Result<RunState, String> {
        let mut resp = self
            .http
            .get(&self.cfg.local_state_url)
            .config()
            .timeout_global(Some(Duration::from_secs(3)))
            .build()
            .call()
            .map_err(|e| e.to_string())?;
        let body: serde_json::Value = resp.body_mut().read_json().map_err(|e| e.to_string())?;
        run_state_from_api(&body)
    }

    // ------------------------------------------------------- the boot state

    /// The newest valid boot-state copy; `Ok(None)` when neither is valid,
    /// `Err` when the card could not be read.
    pub fn boot_state(&self) -> Result<Option<BootState>, String> {
        let [a, b] = self.sys.read_bootstate()?;
        Ok(bootstate::newest([&a, &b]).map(|(_, s)| s))
    }

    /// Replace the boot state with `next` (its `seq` is ignored): written
    /// to the older copy, so a torn write leaves the current one.
    fn write_boot_state(&self, next: BootState) -> Result<(), String> {
        let [a, b] = self.sys.read_bootstate()?;
        let (copy, bytes) = bootstate::plan_write([&a, &b], next);
        self.sys.write_bootstate(copy, &bytes)
    }

    /// Commit a healthy trial boot, tidy the state after a fallback, and
    /// report how the last trial went. Writes nothing if the card cannot be
    /// read or this boot's slot is unknown.
    pub fn settle_boot(&mut self) -> Result<(), String> {
        let Some(booted) = self.sys.booted_slot() else {
            return Ok(());
        };
        let state = match self.boot_state() {
            Ok(s) => s,
            Err(e) => {
                eprintln!("emco-fleet-agent: boot state: {e}");
                return Ok(());
            }
        };
        let before = self.state.clone();
        let trying = is_trial_boot(state.as_ref(), booted);
        let healthy = self.local_run_state();
        let mut now = state;
        if trying && healthy.is_err() {
            // Not proven: leave it uncommitted. The next reboot falls back.
        } else if let Some(next) = on_boot(state.as_ref(), booted) {
            self.write_boot_state(next)?;
            now = self.boot_state()?;
        }
        if let Some(t) = self.state.trial.clone() {
            match trial_outcome(t.slot, Some(booted), now.as_ref()) {
                TrialOutcome::Committed => {
                    self.state.trial = None;
                    self.record(
                        t.seq,
                        "boot",
                        Outcome::Done,
                        format!("booted slot {} ({}) and committed it", t.slot, t.version),
                    );
                }
                TrialOutcome::RolledBack => {
                    self.state.trial = None;
                    self.record(
                        t.seq,
                        "boot",
                        Outcome::Failed,
                        format!(
                            "slot {} ({}) did not come up; back on slot {booted}",
                            t.slot, t.version
                        ),
                    );
                }
                TrialOutcome::Pending => {
                    if let (true, Err(why)) = (trying, &healthy) {
                        self.record(
                            t.seq,
                            "boot",
                            Outcome::Deferred,
                            format!("trial boot of slot {} not committed: {why}", t.slot),
                        );
                    }
                }
            }
        }
        if self.state != before {
            self.save()?;
        }
        Ok(())
    }

    // ------------------------------------------------------------ telemetry

    fn slots(&self) -> Option<Slots> {
        let booted = self.sys.booted_slot();
        let state = self.boot_state().ok().flatten();
        if booted.is_none() && state.is_none() {
            return None;
        }
        let name = |s: Slot| self.state.slot(s).as_ref().map(|c| c.version.clone());
        Some(Slots {
            booted: booted.map(|s| s.to_string()),
            active: state.map(|s| s.active.to_string()),
            try_slot: state
                .as_ref()
                .and_then(BootState::pending_try)
                .map(|s| s.to_string()),
            a: name(Slot::A),
            b: name(Slot::B),
            available: self.state.available.as_ref().map(|b| b.version.clone()),
            staged: self
                .state
                .available
                .as_ref()
                .is_some_and(|b| self.state.holding(b).is_some()),
        })
    }

    pub fn telemetry(&self) -> Telemetry {
        let mut t = self.sys.host_telemetry();
        t.agent_version = env!("CARGO_PKG_VERSION").to_string();
        t.slots = self.slots();
        t.run_state = match self.local_run_state() {
            Ok(s) => s.as_str().to_string(),
            Err(e) => telemetry::clip(&format!("unknown: {e}"), 120),
        };
        t.results = self.state.results.iter().cloned().collect();
        t
    }

    // ------------------------------------------------------------- the wire

    fn api(&self, path: &str) -> String {
        format!("{}/api/v1/{path}", self.cfg.server.trim_end_matches('/'))
    }

    fn post_poll(&self, last_seq: u64) -> Result<PollResponse, String> {
        let url = self.api(&format!("devices/{}/poll", self.cfg.identity.device));
        let req = PollRequest {
            last_seq,
            telemetry: self.telemetry(),
        };
        let mut resp = self
            .http
            .post(&url)
            .header("Authorization", &format!("Bearer {}", self.cfg.token))
            .send_json(&req)
            .map_err(|e| format!("poll {url}: {e}"))?;
        resp.body_mut()
            .read_json()
            .map_err(|e| format!("poll answer: {e}"))
    }

    /// Send telemetry and results now; ignore what comes back.
    pub fn report(&self) -> Result<(), String> {
        self.post_poll(self.state.last_seq).map(drop)
    }

    /// One poll: settle the boot, report, take the orders, carry them out,
    /// report again if anything happened.
    pub fn poll_once(&mut self) -> Result<(), String> {
        self.settle_boot()?;
        let resp = self.post_poll(self.state.last_seq)?;
        let envs: Vec<Envelope> = resp.manifest.into_iter().chain(resp.commands).collect();
        if envs.is_empty() {
            return Ok(());
        }
        let before = (self.state.last_seq, self.state.results.clone());
        let reboot = self.process(envs)?;
        if reboot {
            // State saved and reported, interlock checked just now.
            return self.sys.reboot();
        }
        if (self.state.last_seq, &self.state.results) != (before.0, &before.1) {
            self.report()?;
        }
        Ok(())
    }

    /// Verify and carry out orders. Ok(true): reboot now.
    pub fn process(&mut self, envs: Vec<Envelope>) -> Result<bool, String> {
        let mut orders = Vec::new();
        for env in envs {
            match env.verify(&self.cfg.trusted) {
                Ok(o) => orders.push(o),
                Err(e) => {
                    // Nothing in it can be believed, the seq included; report
                    // what it claims, for the owner to see.
                    let claimed = env.unverified_order().ok();
                    let seq = claimed.as_ref().map(Order::seq).unwrap_or(0);
                    let what = claimed.as_ref().map(Order::what).unwrap_or("?");
                    self.record(seq, what, Outcome::Rejected, format!("unverified: {e}"));
                }
            }
        }
        orders.sort_by_key(Order::seq);
        for order in orders {
            let (seq, what) = (order.seq(), order.what());
            if let Err(r) = accept_order(
                &order,
                &self.cfg.identity,
                self.state.last_seq,
                self.sys.now(),
            ) {
                self.record(seq, what, Outcome::Rejected, r.reason);
                if r.consume {
                    self.state.last_seq = seq;
                }
                self.save()?;
                continue;
            }
            match self.execute(&order)? {
                Exec::Done(d) => {
                    self.state.last_seq = seq;
                    self.record(seq, what, Outcome::Done, d);
                    self.save()?;
                }
                Exec::Failed(d) => {
                    // Not retried by itself: the owner signs a new order.
                    self.state.last_seq = seq;
                    self.record(seq, what, Outcome::Failed, d);
                    self.save()?;
                }
                Exec::Deferred(d) => {
                    self.record(seq, what, Outcome::Deferred, d);
                    self.save()?;
                    // Nothing after it overtakes it.
                    break;
                }
                Exec::Reboot => return Ok(true),
            }
        }
        Ok(false)
    }

    fn gate(&self, step: Step) -> Gate {
        interlock(step, &self.local_run_state())
    }

    // ------------------------------------------------------------ the slots

    fn download(&self, f: &BundleFile, to: &mut dyn Write) -> Result<(), String> {
        let url = self.api(&format!("files/{}", f.sha256));
        let resp = self
            .http
            .get(&url)
            .header("Authorization", &format!("Bearer {}", self.cfg.token))
            .call()
            .map_err(|e| format!("{}: {url}: {e}", f.name))?;
        let mut body = resp.into_body().into_reader();
        copy_checked(f, &mut body, to)
    }

    /// Read every file of `bundle` back from `slot`, from the card.
    fn check_slot(&self, slot: Slot, bundle: &Bundle) -> Result<(), String> {
        for f in &bundle.files {
            let dest = Dest::new(slot, &f.name).ok_or_else(|| format!("{}?", f.name))?;
            let mut from = self.sys.open_read(&dest, f.size)?;
            copy_checked(f, &mut from, &mut std::io::sink())
                .map_err(|e| format!("slot {slot} read back: {e}"))?;
        }
        Ok(())
    }

    fn write_slot(&self, slot: Slot, bundle: &Bundle) -> Result<(), String> {
        for f in &bundle.files {
            let dest = Dest::new(slot, &f.name).ok_or_else(|| format!("{}?", f.name))?;
            if dest.is_rootfs() {
                let cap = self.sys.capacity(&dest)?;
                if f.size > cap {
                    return Err(format!(
                        "{} is {} bytes; slot {slot} holds {cap}",
                        f.name, f.size
                    ));
                }
            }
            let mut w = self.sys.open_write(&dest)?;
            self.download(f, &mut w)?;
            w.finish()?;
        }
        self.check_slot(slot, bundle)
    }

    /// Write `bundle` into the inactive slot and check it, unless it is
    /// there already (then check it again). Returns the slot.
    fn stage(&mut self, bundle: &Bundle) -> Result<(Slot, String), String> {
        let state = self.boot_state()?;
        let plan = stage_target(state.as_ref(), self.sys.booted_slot())?;
        let slot = plan.slot;
        let digest = bundle.digest();
        if self
            .state
            .slot(slot)
            .as_ref()
            .is_some_and(|c| c.digest == digest)
        {
            self.check_slot(slot, bundle)?;
            return Ok((
                slot,
                format!("{} already in slot {slot}; checked", bundle.version),
            ));
        }
        if let Some(clear) = plan.clear_try {
            self.write_boot_state(clear)?;
        }
        // From here until it is checked, the slot holds nothing usable.
        *self.state.slot_mut(slot) = None;
        self.save()?;
        if let Err(e) = self.write_slot(slot, bundle) {
            let undo = self
                .sys
                .invalidate(slot)
                .err()
                .map(|u| format!("; and invalidating it failed: {u}"))
                .unwrap_or_default();
            return Err(format!("{e}{undo}"));
        }
        *self.state.slot_mut(slot) = Some(SlotContent {
            digest,
            version: bundle.version.clone(),
        });
        self.save()?;
        Ok((
            slot,
            format!(
                "wrote {} ({} bytes) to slot {slot} and read it back",
                bundle.version,
                bundle.total_size()
            ),
        ))
    }

    fn execute(&mut self, order: &Order) -> Result<Exec, String> {
        Ok(match order {
            Order::Manifest(m) => self.manifest(m),
            Order::Command(c) => match &c.action {
                Action::Stage { bundle } => match self.stage(bundle) {
                    Ok((_, d)) => Exec::Done(d),
                    Err(e) => Exec::Failed(e),
                },
                Action::Reboot { bundle } => self.reboot(c, bundle.as_ref())?,
            },
        })
    }

    fn manifest(&mut self, m: &Manifest) -> Exec {
        self.state.available = Some(m.bundle.clone());
        if !m.stage {
            return Exec::Done(format!("{} is available", m.bundle.version));
        }
        match self.stage(&m.bundle) {
            Ok((_, d)) => Exec::Done(d),
            Err(e) => Exec::Failed(e),
        }
    }

    fn reboot(&mut self, c: &Command, bundle: Option<&Bundle>) -> Result<Exec, String> {
        let mut note = String::new();
        let mut target = None;
        if let Some(b) = bundle {
            match self.stage(b) {
                Ok((slot, d)) => {
                    note = format!("{d}; ");
                    target = Some((slot, b.version.clone()));
                }
                Err(e) => return Ok(Exec::Failed(e)),
            }
        }
        if let Gate::Defer(why) = self.gate(Step::Reboot) {
            return Ok(Exec::Deferred(format!("{note}reboot deferred: {why}")));
        }
        let previous = self.state.last_seq;
        let mut committed = None;
        if let Some((slot, version)) = &target {
            let Some(state) = self.boot_state()? else {
                return Ok(Exec::Failed("the boot state is unreadable".into()));
            };
            if let Err(e) = self.write_boot_state(trial(&state, *slot)) {
                return Ok(Exec::Failed(e));
            }
            committed = Some(state);
            self.state.trial = Some(PendingTrial {
                seq: c.seq,
                slot: *slot,
                version: version.clone(),
            });
            note.push_str(&format!("slot {slot} to be tried once; "));
        }
        // Use the order up on disk before going down, so it cannot loop, and
        // say so. The report still gives the server the previous seq, so it
        // keeps the order queued in case the check below defers it; the
        // first poll after the reboot carries the new one.
        self.state.last_seq = c.seq;
        self.record(c.seq, "reboot", Outcome::Done, format!("{note}rebooting"));
        self.save()?;
        let reported = self.post_poll(previous).map(drop);
        // Again, immediately before acting.
        if let Gate::Defer(why) = self.gate(Step::Reboot) {
            self.state.last_seq = previous;
            self.state.trial = None;
            if let Some(state) = committed {
                // Back to what it was: nothing to try.
                self.write_boot_state(BootState {
                    try_slot: None,
                    tries: 0,
                    ..state
                })?;
            }
            return Ok(Exec::Deferred(format!("{note}reboot deferred: {why}")));
        }
        if let Err(e) = reported {
            eprintln!("emco-fleet-agent: could not report before rebooting: {e}");
        }
        Ok(Exec::Reboot)
    }
}

#[cfg(test)]
mod tests {
    /// The executor runs nothing but `systemctl`, and names none of the
    /// boot-firmware tools.
    #[test]
    fn executor_touches_only_the_slots() {
        // The code, not the comments that say what it leaves alone.
        let src: String = include_str!("system.rs")
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .map(|l| format!("{l}\n"))
            .collect();
        let allowed = ["systemctl"];
        for (i, _) in src.match_indices("Command::new(") {
            let rest = &src[i + "Command::new(".len()..];
            let arg = rest.split(')').next().unwrap().trim();
            let prog = arg.trim_matches('"');
            assert!(
                arg.starts_with('"') && allowed.contains(&prog),
                "spawns {arg}"
            );
        }
        assert_eq!(src.matches("Command::new(").count(), 2);
        let forbidden = [
            ["fw_", "setenv"].concat(),
            ["fw_", "printenv"].concat(),
            ["save", "env"].concat(),
            ["/dev/", "mtd"].concat(),
            ["flash", "cp"].concat(),
            ["flash_", "erase"].concat(),
            ["/boot/", "firmware"].concat(),
            ["\"s", "f\""].concat(),
            ["\"d", "d\""].concat(),
            ["mk", "fs"].concat(),
            ["nix-collect", "-garbage"].concat(),
            ["--delete", "-generations"].concat(),
            ["boot", ".scr"].concat(),
            ["sfdisk"].concat(),
            ["-0", "1\""].concat(),
            ["-0", "4\""].concat(),
        ];
        for f in &forbidden {
            assert!(!src.contains(f.as_str()), "system.rs mentions {f}");
        }
    }

    #[test]
    fn copies_are_checked() {
        use super::copy_checked;
        use emco_fleet_proto::bundle::BundleFile;
        use sha2::{Digest, Sha256};
        let data = b"0123456789";
        let f = BundleFile {
            name: "Image".into(),
            size: 10,
            sha256: emco_fleet_proto::keys::hex(&Sha256::digest(data)),
        };
        let mut out = Vec::new();
        copy_checked(&f, &mut &data[..], &mut out).unwrap();
        assert_eq!(out, data);
        let e = copy_checked(&f, &mut &b"01234567890"[..], &mut Vec::new()).unwrap_err();
        assert!(e.contains("longer"), "{e}");
        let e = copy_checked(&f, &mut &b"012345678"[..], &mut Vec::new()).unwrap_err();
        assert!(e.contains("9 bytes"), "{e}");
        let e = copy_checked(&f, &mut &b"0123456780"[..], &mut Vec::new()).unwrap_err();
        assert!(e.contains("SHA-256"), "{e}");
    }

    #[test]
    fn server_urls() {
        use super::check_server_url as c;
        assert!(c("https://fleet.example.com", false).is_ok());
        assert!(c("http://127.0.0.1:9000", false).is_ok());
        assert!(c("http://localhost", false).is_ok());
        assert!(c("http://localhost.evil.com", false).is_err());
        assert!(c("http://fleet.example.com", false).is_err());
        assert!(c("http://fleet.example.com", true).is_ok());
        assert!(c("ftp://x", true).is_err());
    }
}
