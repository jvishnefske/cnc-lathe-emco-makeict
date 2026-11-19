//! `emco-kd240` — see `kd240/README.md`.
#![forbid(unsafe_code)]

use emco_kd240::api::{self, App};
use emco_kd240::capture::Capture;
use emco_kd240::machine::Machine;
use emco_kd240::mesh::zenoh::ZenohBus;
use emco_kd240::mesh::{MeshConfig, MeshTransport};
use emco_kd240::store::Store;
use emco_sender_core::profile::MachineProfile;
use std::path::PathBuf;
use std::sync::Arc;

const USAGE: &str = "\
usage: emco-kd240 [--listen ADDR] [--data DIR] (--token-file FILE | --no-auth)
                  [--rs485 TTY] [--profile FILE]
                  [--mesh ROUTER --mesh-process ID --mesh-unit ID --mesh-belt N]

  --listen ADDR      address to serve on (default 0.0.0.0:8080)
  --data DIR         programs and captures (default /var/lib/emco-kd240)
  --token-file FILE  bearer token required for every change (first line of FILE)
  --no-auth          serve changes without a token (a closed bench network only)
  --rs485 TTY        the KD240's RS-485 tty, opened READ-ONLY for capture
  --profile FILE     a MachineProfile JSON (default: the PC Turn 50's)
  --mesh ROUTER      drive the lathe's payloads on middleware instead of the
                     virtual machine, through the Zenoh router ROUTER
                     (tcp/127.0.0.1:7447); see src/mesh.rs
  --mesh-process ID  the process payload's node id, 16 hex digits
                     (or EMCO_MESH_PROCESS)
  --mesh-unit ID     the physical unit's node id (or EMCO_MESH_UNIT)
  --mesh-belt N      the belt position fitted, 1 to 3 (or EMCO_MESH_BELT)

Without --mesh the machine is sender/core's virtual machine. Either way the
RS-485 protocol is unknown (docs/30-serial-protocol.md) and nothing here
transmits on the bus.";

struct Args {
    listen: String,
    data: PathBuf,
    token_file: Option<PathBuf>,
    no_auth: bool,
    rs485: Option<PathBuf>,
    profile: Option<PathBuf>,
    mesh: Option<(String, MeshConfig)>,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        listen: "0.0.0.0:8080".into(),
        data: "/var/lib/emco-kd240".into(),
        token_file: None,
        no_auth: false,
        rs485: None,
        profile: None,
        mesh: None,
    };
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
    let (mut router, mut process, mut unit, mut belt) = (
        None,
        env("EMCO_MESH_PROCESS"),
        env("EMCO_MESH_UNIT"),
        env("EMCO_MESH_BELT"),
    );
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut val = || it.next().ok_or(format!("{arg} needs a value"));
        match arg.as_str() {
            "--listen" => a.listen = val()?,
            "--data" => a.data = val()?.into(),
            "--token-file" => a.token_file = Some(val()?.into()),
            "--no-auth" => a.no_auth = true,
            "--rs485" => a.rs485 = Some(val()?.into()),
            "--profile" => a.profile = Some(val()?.into()),
            "--mesh" => router = Some(val()?),
            "--mesh-process" => process = Some(val()?),
            "--mesh-unit" => unit = Some(val()?),
            "--mesh-belt" => belt = Some(val()?),
            "-h" | "--help" => return Err(String::new()),
            _ => return Err(format!("unknown argument {arg}")),
        }
    }
    if a.token_file.is_some() == a.no_auth {
        return Err("give exactly one of --token-file or --no-auth".into());
    }
    if let Some(router) = router {
        let node = |what: &str, v: Option<String>| {
            let v = v.ok_or(format!("--mesh needs --mesh-{what}"))?;
            cnc_model::proc::parse_node(&v)
                .ok_or(format!("--mesh-{what} {v:?} is not 16 hex digits"))
        };
        let belt = belt.ok_or("--mesh needs --mesh-belt (1, 2 or 3)")?;
        let belt = match belt.trim().parse::<u8>() {
            Ok(b @ 1..=3) => b,
            _ => return Err(format!("--mesh-belt {belt:?} is not 1, 2 or 3")),
        };
        let cfg = MeshConfig {
            process: node("process", process)?,
            unit: node("unit", unit)?,
            belt,
        };
        a.mesh = Some((router, cfg));
    }
    Ok(a)
}

fn read_token(path: &PathBuf) -> Result<String, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let token = text.lines().next().unwrap_or("").trim().to_owned();
    if token.len() < 16 {
        return Err(format!(
            "{}: the token must be at least 16 characters",
            path.display()
        ));
    }
    Ok(token)
}

/// Ctrl-C or SIGTERM (systemd's stop).
async fn shutdown() {
    use tokio::signal::unix::{signal, SignalKind};
    match signal(SignalKind::terminate()) {
        Ok(mut term) => {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = term.recv() => {}
            }
        }
        Err(_) => {
            let _ = tokio::signal::ctrl_c().await;
        }
    }
}

#[tokio::main]
async fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            if !e.is_empty() {
                eprintln!("error: {e}\n");
            }
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    };
    if let Err(e) = run(args).await {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

async fn run(args: Args) -> Result<(), String> {
    let token = match &args.token_file {
        Some(p) => Some(read_token(p)?),
        None => None,
    };
    let profile = match &args.profile {
        Some(p) => {
            let text = std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))?;
            MachineProfile::from_json(&text).map_err(|e| format!("{}: {e}", p.display()))?
        }
        None => MachineProfile::pc_turn_50_default(),
    };
    let store =
        Store::open(args.data.join("programs")).map_err(|e| format!("program store: {e}"))?;
    let capture = Capture::new(args.rs485.clone(), args.data.join("captures"))
        .map_err(|e| format!("captures: {e}"))?;
    let machine = match &args.mesh {
        None => Machine::spawn(profile.clone()),
        Some((router, cfg)) => {
            let bus = ZenohBus::connect(router, cfg.prefixes().to_vec())?;
            let cfg = *cfg;
            Machine::spawn_with(move || Box::new(MeshTransport::new(bus, cfg)))
        }
    };
    let app = Arc::new(App::new(machine, store, capture, profile, token));
    let listener = tokio::net::TcpListener::bind(&args.listen)
        .await
        .map_err(|e| format!("listen on {}: {e}", args.listen))?;
    eprintln!(
        "emco-kd240 on http://{} — {}; RS-485 capture {}; auth {}",
        args.listen,
        match &args.mesh {
            None => "virtual machine".to_string(),
            Some((router, c)) => format!(
                "the payloads on middleware via {router} (process {}, unit {}, belt {})",
                cnc_model::proc::node_hex(c.process),
                cnc_model::proc::node_hex(c.unit),
                c.belt
            ),
        },
        args.rs485
            .as_ref()
            .map_or("off".into(), |p| format!("read-only on {}", p.display())),
        if args.no_auth { "OFF" } else { "bearer token" },
    );
    let app_for_shutdown = app.clone();
    axum::serve(listener, api::router(app))
        .with_graceful_shutdown(shutdown())
        .await
        .map_err(|e| e.to_string())?;
    // Close an open capture file cleanly.
    let _ = tokio::task::spawn_blocking(move || app_for_shutdown.capture.stop()).await;
    Ok(())
}
