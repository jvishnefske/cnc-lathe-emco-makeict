//! The server in process, a fake lathe service, a fake system executor, and
//! the real agent between them. Nothing here runs Nix or reboots.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::routing::{get, post};
use axum::{Json, Router};
use emco_fleet_agent::{Activation, Agent, Config, System};
use emco_fleet_proto::keys::{Envelope, SecretKey};
use emco_fleet_proto::msg::{
    Action, Command, DeviceDetail, Manifest, Order, Outcome, PollResponse, Telemetry,
};
use emco_fleet_proto::policy::{Generation, Identity, ToplevelFacts};
use emco_fleet_proto::token::TokenHash;

const HASH: &str = "0123456789abcdfghijklmnpqrsvwxyz";
const NOW: u64 = 1_800_000_000;

fn top(v: &str) -> String {
    format!("/nix/store/{HASH}-nixos-system-kd240-{v}")
}

// ------------------------------------------------------------- the server

/// Serve a router on 127.0.0.1 from its own thread and runtime.
fn serve(router: Router) -> SocketAddr {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            tx.send(l.local_addr().unwrap()).unwrap();
            axum::serve(l, router).await.unwrap();
        });
    });
    rx.recv().unwrap()
}

/// The lathe service's `GET /api/state`, with a run state the test sets.
fn fake_lathe(run: Arc<Mutex<String>>) -> String {
    let r = Router::new().route(
        "/api/state",
        get(move || {
            let run = run.lock().unwrap().clone();
            async move {
                Json(
                    serde_json::json!({"session": {"run": run, "tool": 1}, "transport": "virtual"}),
                )
            }
        }),
    );
    format!("http://{}/api/state", serve(r))
}

struct Fleet {
    dir: tempfile::TempDir,
    url: String,
    key: SecretKey,
    run: Arc<Mutex<String>>,
    lathe_url: String,
}

fn fleet(server_checks_signatures: bool) -> Fleet {
    let dir = tempfile::tempdir().unwrap();
    let key = SecretKey::generate("emco-fleet-1").unwrap();
    let p = dir.path();
    std::fs::write(p.join("admin"), "admin-token\n").unwrap();
    std::fs::write(
        p.join("devices"),
        format!("kd240 {}\n", TokenHash::of("dev-token").to_text()),
    )
    .unwrap();
    std::fs::write(p.join("keys"), key.public().to_text()).unwrap();
    std::fs::create_dir_all(p.join("cache")).unwrap();
    let cfg = emco_fleet_server::Config {
        listen: String::new(),
        data_dir: p.join("data"),
        cache_dir: p.join("cache"),
        admin_token_file: p.join("admin"),
        device_tokens_file: p.join("devices"),
        trusted_keys_file: server_checks_signatures.then(|| p.join("keys")),
        history: 50,
        history_every_s: 0,
        max_pending: 16,
    };
    let app = emco_fleet_server::App::open(cfg).unwrap();
    let url = format!("http://{}", serve(emco_fleet_server::router(app)));
    let run = Arc::new(Mutex::new("idle".to_string()));
    let lathe_url = fake_lathe(run.clone());
    Fleet {
        dir,
        url,
        key,
        run,
        lathe_url,
    }
}

fn http() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into()
}

impl Fleet {
    fn set_run(&self, s: &str) {
        *self.run.lock().unwrap() = s.to_string();
    }

    fn agent(&self, sys: FakeSystem) -> Agent<FakeSystem> {
        self.agent_with(sys, "dev-token", &self.url, &self.lathe_url)
    }

    fn agent_with(
        &self,
        sys: FakeSystem,
        token: &str,
        server: &str,
        lathe: &str,
    ) -> Agent<FakeSystem> {
        Agent::new(
            Config {
                server: server.to_string(),
                identity: Identity {
                    device: "kd240".into(),
                    host: "kd240".into(),
                    system: "aarch64-linux".into(),
                },
                token: token.into(),
                trusted: vec![self.key.public()],
                state_dir: self.dir.path().join("agent"),
                local_state_url: lathe.to_string(),
            },
            sys,
        )
        .unwrap()
    }

    fn admin(&self, method: &str, path: &str, env: Option<&Envelope>) -> (u16, String) {
        let url = format!("{}{path}", self.url);
        let h = http();
        let auth = "Bearer admin-token";
        let mut r = match (method, env) {
            ("PUT", Some(e)) => h.put(&url).header("Authorization", auth).send_json(e),
            ("POST", Some(e)) => h.post(&url).header("Authorization", auth).send_json(e),
            ("GET", None) => h.get(&url).header("Authorization", auth).call(),
            _ => unreachable!(),
        }
        .unwrap();
        (r.status().as_u16(), r.body_mut().read_to_string().unwrap())
    }

    fn publish(&self, env: &Envelope) {
        assert_eq!(
            self.admin("PUT", "/api/v1/devices/kd240/manifest", Some(env))
                .0,
            204
        );
    }

    fn send(&self, env: &Envelope) {
        assert_eq!(
            self.admin("POST", "/api/v1/devices/kd240/commands", Some(env))
                .0,
            202
        );
    }

    fn detail(&self) -> DeviceDetail {
        let (code, body) = self.admin("GET", "/api/v1/devices/kd240", None);
        assert_eq!(code, 200, "{body}");
        serde_json::from_str(&body).unwrap()
    }

    fn manifest(&self, seq: u64, toplevel: &str, stage: bool) -> Envelope {
        self.key.sign(&Order::Manifest(Manifest {
            device: "kd240".into(),
            seq,
            host: "kd240".into(),
            system: "aarch64-linux".into(),
            toplevel: toplevel.into(),
            stage,
            created: NOW,
            note: "test".into(),
        }))
    }

    fn command(&self, seq: u64, action: Action) -> Envelope {
        self.key.sign(&Order::Command(Command {
            device: "kd240".into(),
            seq,
            not_after: NOW + 3600,
            action,
        }))
    }
}

// ------------------------------------------------------ the fake executor

#[derive(Clone, Default)]
struct FakeSystem {
    log: Arc<Mutex<Vec<String>>>,
    gens: Arc<Mutex<Vec<Generation>>>,
    current: Arc<Mutex<Option<String>>>,
    /// Set the lathe's run state when the profile changes: the operator
    /// pressing cycle start in the middle of an update.
    busy_after_profile: Option<(Arc<Mutex<String>>, String)>,
}

impl FakeSystem {
    fn new() -> Self {
        let s = FakeSystem::default();
        *s.current.lock().unwrap() = Some(top("1"));
        s.gens.lock().unwrap().push(Generation {
            number: 1,
            target: top("1"),
            age_s: 60,
        });
        s
    }

    fn log(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }

    fn push(&self, s: String) {
        self.log.lock().unwrap().push(s);
    }
}

impl System for FakeSystem {
    fn now(&self) -> u64 {
        NOW
    }
    fn host_telemetry(&self) -> Telemetry {
        Telemetry {
            hostname: "kd240".into(),
            current_system: self.current.lock().unwrap().clone(),
            kernel_release: "6.18.0".into(),
            ..Default::default()
        }
    }
    fn current_system(&self) -> Option<String> {
        self.current.lock().unwrap().clone()
    }
    fn generations(&self) -> Result<Vec<Generation>, String> {
        Ok(self.gens.lock().unwrap().clone())
    }
    fn realise(&self, toplevel: &str) -> Result<(), String> {
        self.push(format!("realise {toplevel}"));
        if toplevel.contains("missing") {
            return Err("not in any cache".into());
        }
        Ok(())
    }
    fn toplevel_facts(&self, toplevel: &str) -> ToplevelFacts {
        ToplevelFacts {
            system: Some(
                if toplevel.contains("x86") {
                    "x86_64-linux"
                } else {
                    "aarch64-linux"
                }
                .into(),
            ),
            hostname: Some("kd240".into()),
            has_switch_to_configuration: true,
            has_nixos_version: true,
            has_init: true,
        }
    }
    fn set_profile(&self, toplevel: &str) -> Result<(), String> {
        self.push(format!("set-profile {toplevel}"));
        let mut g = self.gens.lock().unwrap();
        let n = g.iter().map(|g| g.number).max().unwrap_or(0) + 1;
        g.push(Generation {
            number: n,
            target: toplevel.into(),
            age_s: 0,
        });
        if let Some((run, to)) = &self.busy_after_profile {
            *run.lock().unwrap() = to.clone();
        }
        Ok(())
    }
    fn activate(&self, toplevel: &str, how: Activation) -> Result<(), String> {
        self.push(format!("activate {} {toplevel}", how.arg()));
        if how == Activation::Switch {
            *self.current.lock().unwrap() = Some(toplevel.into());
        }
        Ok(())
    }
    fn reboot(&self) -> Result<(), String> {
        self.push("reboot".into());
        Ok(())
    }
}

fn last_result(a: &Agent<FakeSystem>) -> (u64, String, Outcome, String) {
    let r = a.state.results.back().expect("a result");
    (r.seq, r.what.clone(), r.outcome, r.detail.clone())
}

/// A compromised server: answers every poll with the given orders.
fn evil_server(resp: PollResponse) -> String {
    let r = Router::new().route(
        "/api/v1/devices/{id}/poll",
        post(move || {
            let resp = resp.clone();
            async move { Json(resp) }
        }),
    );
    format!("http://{}", serve(r))
}

// ---------------------------------------------------------------- tests

#[test]
fn publish_poll_fetch_stage_and_report() {
    let f = fleet(true);
    f.publish(&f.manifest(10, &top("2"), true));
    let sys = FakeSystem::new();
    let mut a = f.agent(sys.clone());
    a.poll_once().unwrap();
    assert_eq!(
        sys.log(),
        [
            format!("realise {}", top("2")),
            format!("set-profile {}", top("2")),
            format!("activate boot {}", top("2"))
        ]
    );
    assert_eq!(a.state.last_seq, 10);
    assert_eq!(a.state.available.as_deref(), Some(top("2").as_str()));
    // The report after the work reached the server.
    let d = f.detail();
    let latest = d.summary.latest.unwrap();
    assert_eq!(latest.last_seq, 10);
    assert_eq!(latest.telemetry.run_state, "idle");
    assert_eq!(latest.telemetry.available_system, Some(top("2")));
    let r = latest.telemetry.results.last().unwrap();
    assert_eq!(
        (r.seq, r.what.as_str(), r.outcome),
        (10, "manifest", Outcome::Done)
    );
    // Nothing more is handed out, and a second poll does nothing.
    a.poll_once().unwrap();
    assert_eq!(sys.log().len(), 3);
}

#[test]
fn bad_token_gets_401() {
    let f = fleet(true);
    let sys = FakeSystem::new();
    let a = f.agent_with(sys.clone(), "wrong-token", &f.url, &f.lathe_url);
    let mut a = a;
    let e = a.poll_once().unwrap_err();
    assert!(e.contains("401"), "{e}");
    assert!(sys.log().is_empty());
    // A device token cannot publish.
    let env = f.manifest(1, &top("2"), false);
    let r = http()
        .put(&format!("{}/api/v1/devices/kd240/manifest", f.url))
        .header("Authorization", "Bearer dev-token")
        .send_json(&env)
        .unwrap();
    assert_eq!(r.status().as_u16(), 401);
}

#[test]
fn tampered_manifest_relayed_by_a_server_is_rejected() {
    // The server is not checking signatures here, as a compromised one would not.
    let f = fleet(false);
    let mut env = f.manifest(10, &top("2"), false);
    env.payload = env.payload.replace(&top("2"), &top("evil"));
    f.publish(&env);
    let sys = FakeSystem::new();
    let mut a = f.agent(sys.clone());
    a.poll_once().unwrap();
    assert!(sys.log().is_empty(), "{:?}", sys.log());
    assert_eq!(a.state.last_seq, 0);
    let (seq, what, outcome, detail) = last_result(&a);
    assert_eq!(
        (seq, what.as_str(), outcome),
        (10, "manifest", Outcome::Rejected)
    );
    assert!(detail.contains("bad signature"), "{detail}");
}

#[test]
fn order_signed_by_another_key_is_rejected() {
    let f = fleet(false);
    let mallory = SecretKey::generate("emco-fleet-1").unwrap();
    f.send(&mallory.sign(&Order::Command(Command {
        device: "kd240".into(),
        seq: 5,
        not_after: NOW + 10,
        action: Action::Reboot { toplevel: None },
    })));
    let sys = FakeSystem::new();
    let mut a = f.agent(sys.clone());
    a.poll_once().unwrap();
    assert!(sys.log().is_empty());
    assert_eq!(last_result(&a).2, Outcome::Rejected);
}

#[test]
fn replayed_manifest_is_rejected() {
    let f = fleet(true);
    let old = f.manifest(10, &top("2"), true);
    f.publish(&old);
    let sys = FakeSystem::new();
    let mut a = f.agent(sys.clone());
    a.poll_once().unwrap();
    assert_eq!(a.state.last_seq, 10);
    let done = sys.log().len();

    // The honest server refuses to republish it...
    assert_eq!(
        f.admin("PUT", "/api/v1/devices/kd240/manifest", Some(&old))
            .0,
        409
    );
    // ...and a compromised one that replays it gets nowhere.
    let evil = evil_server(PollResponse {
        manifest: Some(old),
        commands: vec![],
    });
    drop(a);
    let mut a = f.agent_with(sys.clone(), "dev-token", &evil, &f.lathe_url);
    a.poll_once().unwrap();
    assert_eq!(
        sys.log().len(),
        done,
        "replay ran something: {:?}",
        sys.log()
    );
    assert_eq!(a.state.last_seq, 10);
    let (seq, _, outcome, detail) = last_result(&a);
    assert_eq!((seq, outcome), (10, Outcome::Rejected));
    assert!(detail.contains("replay"), "{detail}");
}

#[test]
fn order_for_another_device_is_rejected() {
    let f = fleet(false);
    let other = f.key.sign(&Order::Command(Command {
        device: "kd240-b".into(),
        seq: 5,
        not_after: NOW + 10,
        action: Action::Reboot { toplevel: None },
    }));
    let evil = evil_server(PollResponse {
        manifest: None,
        commands: vec![other],
    });
    let sys = FakeSystem::new();
    let mut a = f.agent_with(sys.clone(), "dev-token", &evil, &f.lathe_url);
    a.poll_once().unwrap();
    assert!(sys.log().is_empty());
    assert_eq!(a.state.last_seq, 0);
    assert!(last_result(&a).3.contains("kd240-b"));
}

#[test]
fn switch_is_deferred_while_the_lathe_is_busy() {
    let f = fleet(true);
    f.set_run("running");
    f.send(&f.command(20, Action::Switch { toplevel: top("2") }));
    let sys = FakeSystem::new();
    let mut a = f.agent(sys.clone());
    a.poll_once().unwrap();
    // Nothing done, nothing used up, reported, still queued.
    assert!(sys.log().is_empty(), "{:?}", sys.log());
    assert_eq!(a.state.last_seq, 0);
    let (seq, what, outcome, detail) = last_result(&a);
    assert_eq!(
        (seq, what.as_str(), outcome),
        (20, "switch", Outcome::Deferred)
    );
    assert!(detail.contains("running"), "{detail}");
    let d = f.detail();
    assert_eq!(d.summary.pending_commands.len(), 1);
    assert_eq!(
        d.summary
            .latest
            .unwrap()
            .telemetry
            .results
            .last()
            .unwrap()
            .outcome,
        Outcome::Deferred
    );

    // Every busy state defers.
    for busy in ["hold", "alarm", "homing", "toolchange"] {
        f.set_run(busy);
        a.poll_once().unwrap();
        assert!(sys.log().is_empty(), "{busy}");
        assert!(last_result(&a).3.contains(busy));
    }

    // Idle: it goes ahead, and the queue drains.
    f.set_run("idle");
    a.poll_once().unwrap();
    assert_eq!(
        sys.log(),
        [
            format!("realise {}", top("2")),
            format!("set-profile {}", top("2")),
            format!("activate switch {}", top("2"))
        ]
    );
    assert_eq!(a.state.last_seq, 20);
    assert_eq!(last_result(&a).2, Outcome::Done);
    assert!(f.detail().summary.pending_commands.is_empty());
}

#[test]
fn switch_rechecks_immediately_before_activating() {
    let f = fleet(true);
    f.send(&f.command(20, Action::Switch { toplevel: top("2") }));
    let mut sys = FakeSystem::new();
    sys.busy_after_profile = Some((f.run.clone(), "running".into()));
    let mut a = f.agent(sys.clone());
    a.poll_once().unwrap();
    // Idle when taken up, busy by the time it would activate: staged only.
    assert_eq!(
        sys.log(),
        [
            format!("realise {}", top("2")),
            format!("set-profile {}", top("2")),
            format!("activate boot {}", top("2"))
        ]
    );
    assert_eq!(a.state.last_seq, 0);
    let (_, _, outcome, detail) = last_result(&a);
    assert_eq!(outcome, Outcome::Deferred);
    assert!(
        detail.contains("staged") && detail.contains("running"),
        "{detail}"
    );
}

#[test]
fn disconnected_lathe_allows_a_switch_but_an_unreachable_one_does_not() {
    let f = fleet(true);
    f.send(&f.command(20, Action::Switch { toplevel: top("2") }));
    let sys = FakeSystem::new();
    // Nothing listens on port 9 here.
    let mut a = f.agent_with(
        sys.clone(),
        "dev-token",
        &f.url,
        "http://127.0.0.1:9/api/state",
    );
    a.poll_once().unwrap();
    assert!(sys.log().is_empty());
    assert!(
        last_result(&a).3.contains("unknown"),
        "{:?}",
        last_result(&a)
    );
    drop(a);

    f.set_run("disconnected");
    let mut a = f.agent(sys.clone());
    a.poll_once().unwrap();
    assert!(sys.log().last().unwrap().starts_with("activate switch"));
}

#[test]
fn stage_goes_ahead_while_the_lathe_is_running() {
    let f = fleet(true);
    f.set_run("running");
    f.send(&f.command(20, Action::Stage { toplevel: top("2") }));
    let sys = FakeSystem::new();
    let mut a = f.agent(sys.clone());
    a.poll_once().unwrap();
    assert_eq!(
        sys.log().last().unwrap(),
        &format!("activate boot {}", top("2"))
    );
    assert_eq!(last_result(&a).2, Outcome::Done);
}

#[test]
fn reboot_waits_for_idle_and_does_not_loop() {
    let f = fleet(true);
    f.set_run("hold");
    f.send(&f.command(
        30,
        Action::Reboot {
            toplevel: Some(top("3")),
        },
    ));
    let sys = FakeSystem::new();
    let mut a = f.agent(sys.clone());
    a.poll_once().unwrap();
    // Staged (allowed), reboot held back.
    assert_eq!(
        sys.log().last().unwrap(),
        &format!("activate boot {}", top("3"))
    );
    assert!(!sys.log().contains(&"reboot".to_string()));
    assert_eq!(last_result(&a).2, Outcome::Deferred);

    f.set_run("idle");
    a.poll_once().unwrap();
    assert_eq!(sys.log().last().unwrap(), "reboot");
    assert_eq!(a.state.last_seq, 30);
    drop(a);

    // "After the reboot": a fresh agent on the same state directory. The
    // server still had the order queued, but the board has used it up.
    let sys2 = FakeSystem::new();
    let mut a = f.agent(sys2.clone());
    assert_eq!(a.state.last_seq, 30);
    a.poll_once().unwrap();
    assert!(sys2.log().is_empty(), "rebooted again: {:?}", sys2.log());
    assert!(f.detail().summary.pending_commands.is_empty());
}

#[test]
fn expired_and_bad_toplevels_are_used_up() {
    let f = fleet(true);
    let expired = f.key.sign(&Order::Command(Command {
        device: "kd240".into(),
        seq: 40,
        not_after: NOW - 1,
        action: Action::Reboot { toplevel: None },
    }));
    f.send(&expired);
    let wrong_arch = format!("/nix/store/{HASH}-nixos-system-kd240-x86");
    f.send(&f.command(
        41,
        Action::Stage {
            toplevel: wrong_arch.clone(),
        },
    ));
    f.send(&f.command(
        42,
        Action::Stage {
            toplevel: top("missing"),
        },
    ));
    let sys = FakeSystem::new();
    let mut a = f.agent(sys.clone());
    a.poll_once().unwrap();
    // 40 refused unread; 41 fetched but refused as the wrong system; 42 failed to fetch.
    assert_eq!(
        sys.log(),
        [
            format!("realise {wrong_arch}"),
            format!("realise {}", top("missing"))
        ]
    );
    let outcomes: Vec<_> = a.state.results.iter().map(|r| (r.seq, r.outcome)).collect();
    assert_eq!(
        outcomes,
        [
            (40, Outcome::Rejected),
            (41, Outcome::Failed),
            (42, Outcome::Failed)
        ]
    );
    assert_eq!(a.state.last_seq, 42);
}

#[test]
fn the_running_system_is_kept_when_it_would_drop_off_the_menu() {
    let f = fleet(true);
    f.send(&f.command(50, Action::Stage { toplevel: top("9") }));
    let sys = FakeSystem::new();
    {
        // Rolled back at the U-Boot menu to generation 1 of 6.
        let mut g = sys.gens.lock().unwrap();
        for n in 2..=6 {
            g.push(Generation {
                number: n,
                target: top(&n.to_string()),
                age_s: 60,
            });
        }
    }
    let mut a = f.agent(sys.clone());
    a.poll_once().unwrap();
    assert_eq!(
        sys.log(),
        [
            format!("realise {}", top("9")),
            format!("set-profile {}", top("1")),
            format!("set-profile {}", top("9")),
            format!("activate boot {}", top("9")),
        ]
    );
}
