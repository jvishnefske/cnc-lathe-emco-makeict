//! The server in process, a fake lathe service, a fake SD card with two
//! slots and the boot-state sectors (and U-Boot's half of the A/B rule),
//! and the real agent between them. Nothing here touches a disk or reboots.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::routing::{get, post};
use axum::{Json, Router};
use emco_fleet_agent::{Agent, Config, Dest, SlotWriter, System};
use emco_fleet_proto::bootstate::{self, BootState, Slot, SECTOR};
use emco_fleet_proto::bundle::{Bundle, BundleFile, FILES, FORMAT};
use emco_fleet_proto::keys::{
    hex, sign_order, Envelope, P256SecretKey, PublicKey, SecretKey, Signer,
};
use emco_fleet_proto::msg::{
    Action, Command, DeviceDetail, Manifest, Order, Outcome, PollResponse, Telemetry,
};
use emco_fleet_proto::policy::Identity;
use emco_fleet_proto::token::TokenHash;
use sha2::{Digest, Sha256};

const NOW: u64 = 1_800_000_000;
/// The fake card's root file system partitions.
const ROOTFS_CAPACITY: u64 = 4096;

/// A bundle and its files' bytes.
struct Built {
    bundle: Bundle,
    files: Vec<(String, Vec<u8>)>,
}

fn build(v: &str) -> Built {
    let mut files = Vec::new();
    let entries = FILES
        .iter()
        .map(|name| {
            let data = match *name {
                "rootfs.squashfs" => format!("squashfs {v} ").repeat(40).into_bytes(),
                "cmdline" => format!("bootargs=init=/nix/store/x-{v}/init\n").into_bytes(),
                _ => format!("{name} of {v}").into_bytes(),
            };
            let sha = hex(&Sha256::digest(&data));
            files.push((sha.clone(), data.clone()));
            BundleFile {
                name: name.to_string(),
                size: data.len() as u64,
                sha256: sha,
            }
        })
        .collect();
    Built {
        bundle: Bundle {
            format: FORMAT.into(),
            class: "kd240".into(),
            version: format!("/nix/store/0123456789abcdfghijklmnpqrsvwxyz-kd240-{v}"),
            created: NOW,
            files: entries,
        },
        files,
    }
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
    key: Box<dyn Signer>,
    public: PublicKey,
    run: Arc<Mutex<String>>,
    lathe_url: String,
}

fn fleet(server_checks_signatures: bool) -> Fleet {
    let key = SecretKey::generate("emco-fleet-1").unwrap();
    let public = key.public();
    fleet_with(Box::new(key), public, server_checks_signatures)
}

fn fleet_with(key: Box<dyn Signer>, public: PublicKey, server_checks_signatures: bool) -> Fleet {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    std::fs::write(p.join("admin"), "admin-token\n").unwrap();
    std::fs::write(
        p.join("devices"),
        format!("kd240 {}\n", TokenHash::of("dev-token").to_text()),
    )
    .unwrap();
    std::fs::write(p.join("keys"), public.to_text()).unwrap();
    let cfg = emco_fleet_server::Config {
        listen: String::new(),
        data_dir: p.join("data"),
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
        public,
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

    fn sign(&self, o: &Order) -> Envelope {
        sign_order(self.key.as_ref(), o).unwrap()
    }

    /// Upload a bundle's files, as `emco-fleet publish` does.
    fn upload(&self, b: &Built) {
        for (sha, data) in &b.files {
            let r = http()
                .put(&format!("{}/api/v1/files/{sha}", self.url))
                .header("Authorization", "Bearer admin-token")
                .send(&data[..])
                .unwrap();
            assert!(r.status().is_success(), "{}", r.status());
        }
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
                    class: "kd240".into(),
                },
                token: token.into(),
                trusted: vec![self.public.clone()],
                state_dir: self.dir.path().join("agent"),
                local_state_url: lathe.to_string(),
                process_state: None,
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

    fn manifest(&self, seq: u64, bundle: &Bundle, stage: bool) -> Envelope {
        self.sign(&Order::Manifest(Manifest {
            device: "kd240".into(),
            seq,
            bundle: bundle.clone(),
            stage,
            created: NOW,
            note: "test".into(),
        }))
    }

    fn command(&self, seq: u64, action: Action) -> Envelope {
        self.sign(&Order::Command(Command {
            device: "kd240".into(),
            seq,
            not_after: NOW + 3600,
            action,
        }))
    }
}

// ------------------------------------------------------------ the fake card

#[derive(Default)]
struct Card {
    files: BTreeMap<Dest, Vec<u8>>,
    bootstate: [Vec<u8>; 2],
    booted: Option<Slot>,
    log: Vec<String>,
    /// Flip a byte of this file as it is written (a bad card).
    corrupt: Option<&'static str>,
    /// After a boot-state write, set the lathe's run state (the operator
    /// started something between the two interlock checks).
    busy_after_bootstate: Option<(Arc<Mutex<String>>, String)>,
}

#[derive(Clone)]
struct FakeSystem(Arc<Mutex<Card>>);

impl FakeSystem {
    /// A fresh card: slot A committed and booted, nothing to try.
    fn new() -> Self {
        let card = Card {
            bootstate: [
                bootstate::encode(&BootState::initial()).to_vec(),
                vec![0; SECTOR],
            ],
            booted: Some(Slot::A),
            ..Default::default()
        };
        FakeSystem(Arc::new(Mutex::new(card)))
    }

    fn card(&self) -> std::sync::MutexGuard<'_, Card> {
        self.0.lock().unwrap()
    }

    fn log(&self) -> Vec<String> {
        self.card().log.clone()
    }

    fn state(&self) -> BootState {
        let c = self.card();
        bootstate::newest([&c.bootstate[0], &c.bootstate[1]])
            .unwrap()
            .1
    }

    /// U-Boot's boot.scr at power-on: a slot with tries left is booted
    /// once (the count goes down first), otherwise the committed one.
    fn power_on(&self) {
        let s = self.state();
        let mut c = self.card();
        let booted = match s.pending_try() {
            Some(t) => {
                let (i, bytes) = bootstate::plan_write(
                    [&c.bootstate[0], &c.bootstate[1]],
                    BootState {
                        tries: s.tries - 1,
                        ..s
                    },
                );
                c.bootstate[i] = bytes.to_vec();
                t
            }
            None => s.active,
        };
        c.booted = Some(booted);
        c.log.clear();
    }

    fn slot_file(&self, slot: Slot, name: &str) -> Option<Vec<u8>> {
        self.card()
            .files
            .get(&Dest::new(slot, name).unwrap())
            .cloned()
    }
}

struct FakeWriter {
    sys: FakeSystem,
    dest: Dest,
    buf: Vec<u8>,
}

impl Write for FakeWriter {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.buf.extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl SlotWriter for FakeWriter {
    fn finish(self: Box<Self>) -> Result<(), String> {
        let mut c = self.sys.card();
        let mut data = self.buf;
        if c.corrupt == Some(self.dest.file) && !data.is_empty() {
            data[0] ^= 1;
        }
        c.log
            .push(format!("write {}/{}", self.dest.slot, self.dest.file));
        c.files.insert(self.dest, data);
        Ok(())
    }
}

impl System for FakeSystem {
    fn now(&self) -> u64 {
        NOW
    }
    fn host_telemetry(&self) -> Telemetry {
        Telemetry {
            hostname: "kd240".into(),
            ..Default::default()
        }
    }
    fn current_system(&self) -> Option<String> {
        None
    }
    fn booted_slot(&self) -> Option<Slot> {
        self.card().booted
    }
    fn read_bootstate(&self) -> Result<[Vec<u8>; 2], String> {
        Ok(self.card().bootstate.clone())
    }
    fn write_bootstate(&self, copy: usize, sector: &[u8; SECTOR]) -> Result<(), String> {
        let mut c = self.card();
        c.bootstate[copy] = sector.to_vec();
        let s = bootstate::decode(sector).unwrap();
        let t = s.try_slot.map(|t| t.as_str()).unwrap_or("-");
        c.log.push(format!("bootstate active={} try={t}", s.active));
        if let Some((run, to)) = c.busy_after_bootstate.take() {
            *run.lock().unwrap() = to;
        }
        Ok(())
    }
    fn capacity(&self, dest: &Dest) -> Result<u64, String> {
        Ok(if dest.is_rootfs() {
            ROOTFS_CAPACITY
        } else {
            u64::MAX
        })
    }
    fn open_write(&self, dest: &Dest) -> Result<Box<dyn SlotWriter>, String> {
        assert_ne!(
            Some(dest.slot),
            self.booted_slot(),
            "wrote the running slot"
        );
        Ok(Box::new(FakeWriter {
            sys: self.clone(),
            dest: dest.clone(),
            buf: Vec::new(),
        }))
    }
    fn open_read(&self, dest: &Dest, len: u64) -> Result<Box<dyn Read>, String> {
        let data = self.card().files.get(dest).cloned().unwrap_or_default();
        Ok(Box::new(std::io::Cursor::new(data).take(len)))
    }
    fn invalidate(&self, slot: Slot) -> Result<(), String> {
        let mut c = self.card();
        c.files.retain(|d, _| d.slot != slot);
        c.log.push(format!("invalidate {slot}"));
        Ok(())
    }
    fn reboot(&self) -> Result<(), String> {
        self.card().log.push("reboot".into());
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

fn writes(slot: Slot) -> Vec<String> {
    FILES.iter().map(|f| format!("write {slot}/{f}")).collect()
}

// ---------------------------------------------------------------- tests

#[test]
fn publish_stage_reboot_and_commit() {
    let f = fleet(true);
    let v2 = build("2");
    f.upload(&v2);
    f.publish(&f.manifest(10, &v2.bundle, true));
    let sys = FakeSystem::new();
    let mut a = f.agent(sys.clone());
    a.poll_once().unwrap();

    // Written into B, the slot this boot did not come from, and checked.
    assert_eq!(sys.log(), writes(Slot::B));
    for (file, (_, data)) in FILES.iter().zip(&v2.files) {
        assert_eq!(sys.slot_file(Slot::B, file).as_ref(), Some(data), "{file}");
    }
    assert_eq!(a.state.last_seq, 10);
    assert_eq!(
        a.state.slot_b.as_ref().map(|c| c.version.as_str()),
        Some(v2.bundle.version.as_str())
    );
    let t = f.detail().summary.latest.unwrap().telemetry;
    let slots = t.slots.unwrap();
    assert_eq!(slots.booted.as_deref(), Some("a"));
    assert_eq!(slots.active.as_deref(), Some("a"));
    assert!(slots.staged);
    assert_eq!(slots.b.as_deref(), Some(v2.bundle.version.as_str()));
    let r = t.results.last().unwrap();
    assert_eq!(
        (r.seq, r.what.as_str(), r.outcome),
        (10, "manifest", Outcome::Done)
    );

    // Reboot into it: already there, so only read back, then marked for
    // one try, then the reboot.
    f.send(&f.command(
        11,
        Action::Reboot {
            bundle: Some(v2.bundle.clone()),
        },
    ));
    a.poll_once().unwrap();
    assert_eq!(
        sys.log()[FILES.len()..],
        ["bootstate active=a try=b", "reboot"]
    );
    assert_eq!(a.state.last_seq, 11);
    drop(a);

    // U-Boot boots B once; the new system is healthy and commits itself.
    sys.power_on();
    assert_eq!(sys.booted_slot(), Some(Slot::B));
    let mut a = f.agent(sys.clone());
    a.poll_once().unwrap();
    assert_eq!(sys.log(), ["bootstate active=b try=-"]);
    assert_eq!(sys.state().active, Slot::B);
    let (seq, what, outcome, detail) = last_result(&a);
    assert_eq!((seq, what.as_str(), outcome), (11, "boot", Outcome::Done));
    assert!(detail.contains("committed"), "{detail}");
    assert!(a.state.trial.is_none());
    let slots = f.detail().summary.latest.unwrap().telemetry.slots.unwrap();
    assert_eq!(
        (slots.booted.as_deref(), slots.active.as_deref()),
        (Some("b"), Some("b"))
    );
    assert!(f.detail().summary.pending_commands.is_empty());

    // And the next release goes to A.
    let v3 = build("3");
    f.upload(&v3);
    f.send(&f.command(
        12,
        Action::Stage {
            bundle: v3.bundle.clone(),
        },
    ));
    a.poll_once().unwrap();
    assert_eq!(sys.log()[1..], writes(Slot::A));
}

#[test]
fn a_trial_that_never_commits_is_rolled_back() {
    let f = fleet(true);
    let v2 = build("2");
    f.upload(&v2);
    f.send(&f.command(
        11,
        Action::Reboot {
            bundle: Some(v2.bundle.clone()),
        },
    ));
    let sys = FakeSystem::new();
    let mut a = f.agent(sys.clone());
    a.poll_once().unwrap();
    assert_eq!(sys.log().last().unwrap(), "reboot");
    drop(a);

    // B boots once, hangs before the agent commits it, and is power-cycled:
    // the try is used up, so U-Boot boots A again.
    sys.power_on();
    assert_eq!(sys.booted_slot(), Some(Slot::B));
    sys.power_on();
    assert_eq!(sys.booted_slot(), Some(Slot::A));

    let mut a = f.agent(sys.clone());
    a.poll_once().unwrap();
    // A, healthy, clears the spent try, and says what happened.
    assert_eq!(sys.log(), ["bootstate active=a try=-"]);
    let (seq, what, outcome, detail) = last_result(&a);
    assert_eq!((seq, what.as_str(), outcome), (11, "boot", Outcome::Failed));
    assert!(detail.contains("did not come up"), "{detail}");
    assert!(a.state.trial.is_none());
}

#[test]
fn a_trial_boot_commits_only_once_the_lathe_service_answers() {
    let f = fleet(true);
    let v2 = build("2");
    f.upload(&v2);
    f.send(&f.command(
        11,
        Action::Reboot {
            bundle: Some(v2.bundle.clone()),
        },
    ));
    let sys = FakeSystem::new();
    f.agent(sys.clone()).poll_once().unwrap();
    sys.power_on();

    // The new system's lathe service does not answer: not committed.
    let mut a = f.agent_with(
        sys.clone(),
        "dev-token",
        &f.url,
        "http://127.0.0.1:9/api/state",
    );
    a.poll_once().unwrap();
    assert!(sys.log().is_empty(), "{:?}", sys.log());
    assert_eq!(sys.state().active, Slot::A);
    assert_eq!(last_result(&a).2, Outcome::Deferred);

    // While on trial, nothing may be written over the way back.
    let v3 = build("3");
    f.upload(&v3);
    f.send(&f.command(
        12,
        Action::Stage {
            bundle: v3.bundle.clone(),
        },
    ));
    a.poll_once().unwrap();
    let r = a.state.results.iter().find(|r| r.seq == 12).unwrap();
    assert_eq!(r.outcome, Outcome::Failed);
    assert!(r.detail.contains("not committed"), "{}", r.detail);
    assert!(sys.log().is_empty(), "{:?}", sys.log());
    drop(a);

    // Once it answers, the trial commits.
    let mut a = f.agent(sys.clone());
    a.poll_once().unwrap();
    assert_eq!(sys.state().active, Slot::B);
    assert!(a
        .state
        .results
        .iter()
        .any(|r| r.seq == 11 && r.what == "boot" && r.outcome == Outcome::Done));
}

#[test]
fn a_bad_write_is_caught_on_read_back_and_never_tried() {
    let f = fleet(true);
    let v2 = build("2");
    f.upload(&v2);
    f.send(&f.command(
        11,
        Action::Reboot {
            bundle: Some(v2.bundle.clone()),
        },
    ));
    let sys = FakeSystem::new();
    sys.card().corrupt = Some("dtb");
    let mut a = f.agent(sys.clone());
    a.poll_once().unwrap();
    let log = sys.log();
    assert_eq!(log.last().unwrap(), "invalidate b");
    assert!(!log
        .iter()
        .any(|l| l.starts_with("bootstate") || l == "reboot"));
    let (seq, _, outcome, detail) = last_result(&a);
    assert_eq!((seq, outcome), (11, Outcome::Failed));
    assert!(
        detail.contains("dtb") && detail.contains("read back"),
        "{detail}"
    );
    assert!(a.state.slot_b.is_none());
    assert_eq!(sys.state().try_slot, None);
}

#[test]
fn a_missing_or_oversized_file_fails_the_stage() {
    let f = fleet(true);
    let v2 = build("2");
    // Not uploaded.
    f.send(&f.command(
        11,
        Action::Stage {
            bundle: v2.bundle.clone(),
        },
    ));
    let mut big = build("big");
    big.bundle.files[0].size = ROOTFS_CAPACITY + 1;
    f.send(&f.command(
        12,
        Action::Stage {
            bundle: big.bundle.clone(),
        },
    ));
    let sys = FakeSystem::new();
    let mut a = f.agent(sys.clone());
    a.poll_once().unwrap();
    let r: Vec<_> = a
        .state
        .results
        .iter()
        .map(|r| (r.seq, r.outcome, r.detail.clone()))
        .collect();
    assert_eq!(r[0].1, Outcome::Failed);
    assert!(r[0].2.contains("404"), "{}", r[0].2);
    assert_eq!(r[1].1, Outcome::Failed);
    assert!(r[1].2.contains("holds"), "{}", r[1].2);
    assert_eq!(a.state.last_seq, 12);
}

#[test]
fn bad_token_gets_401() {
    let f = fleet(true);
    let sys = FakeSystem::new();
    let mut a = f.agent_with(sys.clone(), "wrong-token", &f.url, &f.lathe_url);
    let e = a.poll_once().unwrap_err();
    assert!(e.contains("401"), "{e}");
    assert!(sys.log().is_empty());
    // A device token cannot publish.
    let env = f.manifest(1, &build("2").bundle, false);
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
    let v2 = build("2");
    let mut env = f.manifest(10, &v2.bundle, true);
    env.payload = env.payload.replace("kd240-2", "kd240-6");
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
        action: Action::Reboot { bundle: None },
    })));
    let sys = FakeSystem::new();
    let mut a = f.agent(sys.clone());
    a.poll_once().unwrap();
    assert!(sys.log().is_empty());
    assert_eq!(last_result(&a).2, Outcome::Rejected);
}

#[test]
fn a_p256_key_signs_orders_end_to_end() {
    // The card's kind of key, verified on the board by middleware's code.
    let key = P256SecretKey::generate("emco-fleet-card").unwrap();
    let public = key.public();
    let f = fleet_with(Box::new(key), public, true);
    let v2 = build("2");
    f.upload(&v2);
    f.publish(&f.manifest(10, &v2.bundle, true));
    let sys = FakeSystem::new();
    let mut a = f.agent(sys.clone());
    a.poll_once().unwrap();
    assert_eq!(last_result(&a).2, Outcome::Done);
    assert_eq!(sys.log(), writes(Slot::B));
}

#[test]
fn replayed_manifest_is_rejected() {
    let f = fleet(true);
    let v2 = build("2");
    f.upload(&v2);
    let old = f.manifest(10, &v2.bundle, true);
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
    let other = f.sign(&Order::Command(Command {
        device: "kd240-b".into(),
        seq: 5,
        not_after: NOW + 10,
        action: Action::Reboot { bundle: None },
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
fn stage_goes_ahead_while_the_lathe_is_running() {
    let f = fleet(true);
    f.set_run("running");
    let v2 = build("2");
    f.upload(&v2);
    f.send(&f.command(
        20,
        Action::Stage {
            bundle: v2.bundle.clone(),
        },
    ));
    let sys = FakeSystem::new();
    let mut a = f.agent(sys.clone());
    a.poll_once().unwrap();
    assert_eq!(sys.log(), writes(Slot::B));
    assert_eq!(last_result(&a).2, Outcome::Done);
}

#[test]
fn reboot_waits_for_idle_and_does_not_loop() {
    let f = fleet(true);
    f.set_run("hold");
    let v3 = build("3");
    f.upload(&v3);
    f.send(&f.command(
        30,
        Action::Reboot {
            bundle: Some(v3.bundle.clone()),
        },
    ));
    let sys = FakeSystem::new();
    let mut a = f.agent(sys.clone());
    a.poll_once().unwrap();
    // Staged (allowed), reboot held back, nothing marked to try.
    assert_eq!(sys.log(), writes(Slot::B));
    assert_eq!(a.state.last_seq, 0);
    let (_, _, outcome, detail) = last_result(&a);
    assert_eq!(outcome, Outcome::Deferred);
    assert!(detail.contains("hold"), "{detail}");

    // Every busy state defers.
    for busy in ["running", "alarm", "homing", "toolchange"] {
        f.set_run(busy);
        a.poll_once().unwrap();
        assert!(!sys.log().iter().any(|l| l == "reboot"), "{busy}");
        assert!(last_result(&a).3.contains(busy));
    }
    // Nothing was written again while it waited.
    assert_eq!(sys.log().len(), FILES.len());

    f.set_run("idle");
    a.poll_once().unwrap();
    assert_eq!(
        sys.log()[FILES.len()..],
        ["bootstate active=a try=b", "reboot"]
    );
    assert_eq!(a.state.last_seq, 30);
    drop(a);

    // "After the reboot": the server still had the order queued, but the
    // board has used it up.
    sys.power_on();
    let mut a = f.agent(sys.clone());
    assert_eq!(a.state.last_seq, 30);
    a.poll_once().unwrap();
    assert!(!sys.log().iter().any(|l| l == "reboot"), "{:?}", sys.log());
    assert!(f.detail().summary.pending_commands.is_empty());
}

#[test]
fn reboot_rechecks_immediately_before_acting() {
    let f = fleet(true);
    let v2 = build("2");
    f.upload(&v2);
    f.send(&f.command(
        11,
        Action::Reboot {
            bundle: Some(v2.bundle.clone()),
        },
    ));
    let sys = FakeSystem::new();
    sys.card().busy_after_bootstate = Some((f.run.clone(), "running".into()));
    let mut a = f.agent(sys.clone());
    a.poll_once().unwrap();
    // Idle when taken up, busy by the time it would reboot: the try is
    // taken back and the order stays.
    assert_eq!(
        sys.log()[FILES.len()..],
        ["bootstate active=a try=b", "bootstate active=a try=-"]
    );
    assert_eq!(sys.state().pending_try(), None);
    assert_eq!(a.state.last_seq, 0);
    assert!(a.state.trial.is_none());
    let (_, _, outcome, detail) = last_result(&a);
    assert_eq!(outcome, Outcome::Deferred);
    assert!(detail.contains("running"), "{detail}");
}

#[test]
fn the_process_payload_and_the_service_must_both_say_idle() {
    let f = fleet(true);
    f.set_run("idle");
    f.send(&f.command(21, Action::Reboot { bundle: None }));
    let sys = FakeSystem::new();
    let mut a = f.agent(sys.clone());
    // The process payload's state publication: running (byte 2).
    let state = Arc::new(Mutex::new(2u8));
    let s = state.clone();
    a.set_process_state(Some(Box::new(move || {
        let mut b = vec![0u8; 11];
        b[0] = *s.lock().unwrap();
        Ok(b)
    })));
    a.poll_once().unwrap();
    assert!(sys.log().is_empty(), "{:?}", sys.log());
    let (_, _, outcome, detail) = last_result(&a);
    assert_eq!(outcome, Outcome::Deferred);
    assert!(detail.contains("running"), "{detail}");
    // Done: the reboot goes ahead.
    *state.lock().unwrap() = 4;
    a.poll_once().unwrap();
    assert_eq!(sys.log(), ["reboot"]);
    drop(a);

    // A payload that cannot be heard defers, even with the service idle.
    f.send(&f.command(22, Action::Reboot { bundle: None }));
    sys.power_on();
    f.set_run("idle");
    let mut a = f.agent(sys.clone());
    a.set_process_state(Some(Box::new(|| Err("no router".into()))));
    a.poll_once().unwrap();
    assert!(!sys.log().iter().any(|l| l == "reboot"), "{:?}", sys.log());
    let (_, _, outcome, detail) = last_result(&a);
    assert_eq!(outcome, Outcome::Deferred);
    assert!(detail.contains("no router"), "{detail}");
    drop(a);
    // And a payload saying idle does not outvote a service that cannot
    // be asked.
    let mut a = f.agent_with(
        sys.clone(),
        "dev-token",
        &f.url,
        "http://127.0.0.1:9/api/state",
    );
    a.set_process_state(Some(Box::new(|| Ok(vec![0u8; 11]))));
    a.poll_once().unwrap();
    assert!(!sys.log().iter().any(|l| l == "reboot"), "{:?}", sys.log());
    assert_eq!(last_result(&a).2, Outcome::Deferred);
}

#[test]
fn disconnected_lathe_allows_a_reboot_but_an_unreachable_one_does_not() {
    let f = fleet(true);
    f.send(&f.command(20, Action::Reboot { bundle: None }));
    let sys = FakeSystem::new();
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
    // A plain reboot: no slot change.
    assert_eq!(sys.log(), ["reboot"]);
}

#[test]
fn expired_and_bad_bundles_are_used_up() {
    let f = fleet(true);
    f.send(&f.sign(&Order::Command(Command {
        device: "kd240".into(),
        seq: 40,
        not_after: NOW - 1,
        action: Action::Reboot { bundle: None },
    })));
    let mut wrong_class = build("2").bundle;
    wrong_class.class = "kr260".into();
    f.send(&f.command(
        41,
        Action::Stage {
            bundle: wrong_class,
        },
    ));
    let mut short = build("2").bundle;
    short.files.pop();
    f.send(&f.command(42, Action::Stage { bundle: short }));
    let sys = FakeSystem::new();
    let mut a = f.agent(sys.clone());
    a.poll_once().unwrap();
    assert!(sys.log().is_empty(), "{:?}", sys.log());
    let outcomes: Vec<_> = a.state.results.iter().map(|r| (r.seq, r.outcome)).collect();
    assert_eq!(
        outcomes,
        [
            (40, Outcome::Rejected),
            (41, Outcome::Rejected),
            (42, Outcome::Rejected)
        ]
    );
    assert_eq!(a.state.last_seq, 42);
}
