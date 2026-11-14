//! `emco-kd240` — see `kd240/README.md`.
#![forbid(unsafe_code)]

use emco_kd240::api::{self, App};
use emco_kd240::capture::Capture;
use emco_kd240::machine::Machine;
use emco_kd240::store::Store;
use emco_sender_core::profile::MachineProfile;
use std::path::PathBuf;
use std::sync::Arc;

const USAGE: &str = "\
usage: emco-kd240 [--listen ADDR] [--data DIR] (--token-file FILE | --no-auth)
                  [--rs485 TTY] [--profile FILE]

  --listen ADDR      address to serve on (default 0.0.0.0:8080)
  --data DIR         programs and captures (default /var/lib/emco-kd240)
  --token-file FILE  bearer token required for every change (first line of FILE)
  --no-auth          serve changes without a token (a closed bench network only)
  --rs485 TTY        the KD240's RS-485 tty, opened READ-ONLY for capture
  --profile FILE     a MachineProfile JSON (default: the PC Turn 50's)

The machine is sender/core's virtual machine: the RS-485 protocol is
unknown (docs/30-serial-protocol.md) and nothing here transmits on the bus.";

struct Args {
    listen: String,
    data: PathBuf,
    token_file: Option<PathBuf>,
    no_auth: bool,
    rs485: Option<PathBuf>,
    profile: Option<PathBuf>,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        listen: "0.0.0.0:8080".into(),
        data: "/var/lib/emco-kd240".into(),
        token_file: None,
        no_auth: false,
        rs485: None,
        profile: None,
    };
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
            "-h" | "--help" => return Err(String::new()),
            _ => return Err(format!("unknown argument {arg}")),
        }
    }
    if a.token_file.is_some() == a.no_auth {
        return Err("give exactly one of --token-file or --no-auth".into());
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
    let machine = Machine::spawn(profile.clone());
    let app = Arc::new(App::new(machine, store, capture, profile, token));
    let listener = tokio::net::TcpListener::bind(&args.listen)
        .await
        .map_err(|e| format!("listen on {}: {e}", args.listen))?;
    eprintln!(
        "emco-kd240 on http://{} — virtual machine only; RS-485 capture {}; auth {}",
        args.listen,
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
