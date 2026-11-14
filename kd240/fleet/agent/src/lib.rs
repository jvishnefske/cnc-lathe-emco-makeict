//! The KD240 fleet agent.
//!
//! Each run (a systemd timer, `--once`) it:
//!
//! 1. gathers telemetry and POSTs it to the server's poll endpoint, with the
//!    last sequence number it accepted;
//! 2. verifies every signed order in the answer against the owner's public
//!    keys, then its device id, sequence number, expiry and toplevel
//!    ([`emco_fleet_proto::policy::accept_order`]);
//! 3. carries them out in sequence order through [`System`], with the
//!    lathe-service interlock in front of switch and reboot
//!    ([`emco_fleet_proto::policy::interlock`]), asked once when the order is
//!    taken up and again immediately before the act;
//! 4. reports the results.
//!
//! A deferred order is reported and left for the next poll, and nothing
//! after it runs first. The only things it reads from the lathe service are
//! `GET /api/state`'s `session.run`. It sends the service nothing.
#![forbid(unsafe_code)]

pub mod system;
pub mod telemetry;

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::Duration;

use emco_fleet_proto::keys::{Envelope, PublicKey};
use emco_fleet_proto::msg::{
    Action, ActionResult, Command, Manifest, Order, Outcome, PollRequest, PollResponse, Telemetry,
};
use emco_fleet_proto::policy::{
    accept_order, check_toplevel, interlock, plan_profile, run_state_from_api, Gate, Identity,
    RunState, Step, KEEP_MAX_AGE_S, MENU_GENERATIONS,
};
use serde::{Deserialize, Serialize};

pub use system::{Activation, RealSystem, System};

const KEEP_RESULTS: usize = 10;

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

/// What the board remembers between runs.
#[derive(Serialize, Deserialize, Default, Debug, Clone, PartialEq)]
pub struct AgentState {
    /// The highest sequence number accepted (or consumed). Orders at or
    /// below it are replays.
    pub last_seq: u64,
    /// The newest manifest's toplevel, fetched and checked.
    pub available: Option<String>,
    pub results: VecDeque<ActionResult>,
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
            .timeout_global(Some(Duration::from_secs(30)))
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

    pub fn telemetry(&self) -> Telemetry {
        let mut t = self.sys.host_telemetry();
        t.agent_version = env!("CARGO_PKG_VERSION").to_string();
        t.available_system = self.state.available.clone();
        t.run_state = match self.local_run_state() {
            Ok(s) => s.as_str().to_string(),
            Err(e) => telemetry::clip(&format!("unknown: {e}"), 120),
        };
        t.results = self.state.results.iter().cloned().collect();
        t
    }

    fn poll_url(&self) -> String {
        format!(
            "{}/api/v1/devices/{}/poll",
            self.cfg.server.trim_end_matches('/'),
            self.cfg.identity.device
        )
    }

    fn post_poll(&self, last_seq: u64) -> Result<PollResponse, String> {
        let req = PollRequest {
            last_seq,
            telemetry: self.telemetry(),
        };
        let mut resp = self
            .http
            .post(&self.poll_url())
            .header("Authorization", &format!("Bearer {}", self.cfg.token))
            .send_json(&req)
            .map_err(|e| format!("poll {}: {e}", self.poll_url()))?;
        resp.body_mut()
            .read_json()
            .map_err(|e| format!("poll answer: {e}"))
    }

    /// Send telemetry and results now; ignore what comes back.
    pub fn report(&self) -> Result<(), String> {
        self.post_poll(self.state.last_seq).map(drop)
    }

    /// One poll: report, take the orders, carry them out, report again if
    /// anything happened.
    pub fn poll_once(&mut self) -> Result<(), String> {
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

    /// Substitute the closure, then check it is a NixOS system for this host.
    fn fetch(&self, toplevel: &str) -> Result<(), String> {
        self.sys.realise(toplevel)?;
        check_toplevel(
            toplevel,
            &self.sys.toplevel_facts(toplevel),
            &self.cfg.identity,
        )
    }

    /// Make `toplevel` the newest system generation, keeping the running
    /// system in the boot menu (`plan_profile`).
    fn set_profile(&self, toplevel: &str) -> Result<String, String> {
        let gens = self.sys.generations()?;
        let current = self.sys.current_system();
        let plan = plan_profile(
            current.as_deref(),
            &gens,
            toplevel,
            MENU_GENERATIONS,
            KEEP_MAX_AGE_S,
        );
        let mut note = String::new();
        if let Some(keep) = &plan.preserve {
            self.sys.set_profile(keep)?;
            note.push_str("kept the running system as a new generation; ");
        }
        if plan.set_target {
            self.sys.set_profile(toplevel)?;
        }
        Ok(note)
    }

    fn stage(&self, toplevel: &str) -> Result<String, String> {
        let note = self.set_profile(toplevel)?;
        self.sys.activate(toplevel, Activation::Boot)?;
        Ok(format!("{note}staged {toplevel} for the next boot"))
    }

    fn execute(&mut self, order: &Order) -> Result<Exec, String> {
        Ok(match order {
            Order::Manifest(m) => self.manifest(m),
            Order::Command(c) => match &c.action {
                Action::Stage { toplevel } => {
                    match self.fetch(toplevel).and_then(|()| self.stage(toplevel)) {
                        Ok(d) => Exec::Done(d),
                        Err(e) => Exec::Failed(e),
                    }
                }
                Action::Switch { toplevel } => self.switch(c, toplevel)?,
                Action::Reboot { toplevel } => self.reboot(c, toplevel.as_deref())?,
            },
        })
    }

    fn manifest(&mut self, m: &Manifest) -> Exec {
        if let Err(e) = self.fetch(&m.toplevel) {
            return Exec::Failed(e);
        }
        self.state.available = Some(m.toplevel.clone());
        if !m.stage {
            return Exec::Done(format!("fetched {}", m.toplevel));
        }
        match self.stage(&m.toplevel) {
            Ok(d) => Exec::Done(format!("fetched; {d}")),
            Err(e) => Exec::Failed(e),
        }
    }

    fn switch(&mut self, c: &Command, toplevel: &str) -> Result<Exec, String> {
        if let Gate::Defer(why) = self.gate(Step::Switch) {
            return Ok(Exec::Deferred(format!("switch deferred: {why}")));
        }
        if let Err(e) = self.fetch(toplevel) {
            return Ok(Exec::Failed(e));
        }
        let note = match self.set_profile(toplevel) {
            Ok(n) => n,
            Err(e) => return Ok(Exec::Failed(e)),
        };
        // Again, immediately before activating.
        if let Gate::Defer(why) = self.gate(Step::Switch) {
            // The profile already points at it; make the boot entry agree,
            // which is the same as a stage.
            return Ok(match self.sys.activate(toplevel, Activation::Boot) {
                Ok(()) => Exec::Deferred(format!(
                    "{note}staged for the next boot; switch deferred: {why}"
                )),
                Err(e) => Exec::Failed(format!("switch deferred ({why}), and staging failed: {e}")),
            });
        }
        // At most once: if activation restarts this agent, the order is
        // already used up and the next run sees the new current-system.
        self.state.last_seq = c.seq;
        self.record(
            c.seq,
            "switch",
            Outcome::Done,
            format!("{note}activating {toplevel}"),
        );
        self.save()?;
        Ok(match self.sys.activate(toplevel, Activation::Switch) {
            Ok(()) => Exec::Done(format!("{note}switched to {toplevel}")),
            Err(e) => Exec::Failed(e),
        })
    }

    fn reboot(&mut self, c: &Command, toplevel: Option<&str>) -> Result<Exec, String> {
        let mut staged = String::new();
        if let Some(t) = toplevel {
            match self.fetch(t).and_then(|()| self.stage(t)) {
                Ok(d) => staged = format!("{d}; "),
                Err(e) => return Ok(Exec::Failed(e)),
            }
        }
        if let Gate::Defer(why) = self.gate(Step::Reboot) {
            return Ok(Exec::Deferred(format!("{staged}reboot deferred: {why}")));
        }
        // Use the order up on disk before going down, so it cannot loop, and
        // say so. The report still gives the server the previous seq, so it
        // keeps the order queued in case the check below defers it; the
        // first poll after the reboot carries the new one.
        let previous = self.state.last_seq;
        self.state.last_seq = c.seq;
        self.record(c.seq, "reboot", Outcome::Done, format!("{staged}rebooting"));
        self.save()?;
        let reported = self.post_poll(previous).map(drop);
        // Again, immediately before acting.
        if let Gate::Defer(why) = self.gate(Step::Reboot) {
            self.state.last_seq = previous;
            return Ok(Exec::Deferred(format!("{staged}reboot deferred: {why}")));
        }
        if let Err(e) = reported {
            eprintln!("emco-fleet-agent: could not report before rebooting: {e}");
        }
        Ok(Exec::Reboot)
    }
}

#[cfg(test)]
mod tests {
    /// The executor runs nothing but the four programs in its docs, and
    /// names none of the boot-firmware tools.
    #[test]
    fn executor_runs_only_the_allowed_programs() {
        let src = include_str!("system.rs");
        let allowed = ["nix-store", "nix-env", "systemd-run", "systemctl"];
        for (i, _) in src.match_indices("Command::new(") {
            let rest = &src[i + "Command::new(".len()..];
            let arg = rest.split(')').next().unwrap().trim();
            let prog = arg.trim_matches('"');
            assert!(
                arg.starts_with('"') && allowed.contains(&prog),
                "spawns {arg}"
            );
        }
        assert_eq!(src.matches("Command::new(").count(), 5);
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
        ];
        for f in &forbidden {
            assert!(!src.contains(f.as_str()), "system.rs mentions {f}");
        }
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
