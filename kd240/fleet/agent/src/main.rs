//! `emco-fleet-agent`: see the crate docs and `kd240/fleet/README.md`.
#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;
use emco_fleet_agent::{check_server_url, Agent, Config, RealSystem};
use emco_fleet_proto::keys::PublicKey;
use emco_fleet_proto::policy::Identity;

#[derive(Parser)]
#[command(
    name = "emco-fleet-agent",
    version,
    about = "KD240 fleet agent: telemetry and owner-signed NixOS updates"
)]
struct Args {
    /// The fleet server, `https://…`.
    #[arg(long, env = "FLEET_SERVER")]
    server: String,
    /// This board's device id, as in the server's device tokens file.
    #[arg(long, env = "FLEET_DEVICE")]
    device: String,
    /// The device's bearer token (a file outside the Nix store).
    #[arg(long, env = "FLEET_TOKEN_FILE")]
    token_file: PathBuf,
    /// A trusted order-signing public key, `<name>:<base64>`; repeatable.
    #[arg(
        long = "trusted-key",
        env = "FLEET_TRUSTED_KEYS",
        value_delimiter = ' ',
        required = true
    )]
    trusted_keys: Vec<String>,
    /// The NixOS host name toplevels must be for. Default: this machine's.
    #[arg(long)]
    host: Option<String>,
    /// The Nix system toplevels must be for.
    #[arg(long, default_value_t = format!("{}-linux", std::env::consts::ARCH))]
    system: String,
    #[arg(long, default_value = "/var/lib/emco-fleet-agent")]
    state_dir: PathBuf,
    /// The lathe service's state endpoint (read only).
    #[arg(long, default_value = "http://127.0.0.1:8080/api/state")]
    local_state_url: String,
    /// Allow a plain-HTTP server that is not on loopback (testing only).
    #[arg(long)]
    allow_http: bool,
    /// Poll once and exit (for a systemd timer).
    #[arg(long)]
    once: bool,
    /// Seconds between polls without --once.
    #[arg(long, default_value_t = 60)]
    interval_s: u64,
}

fn setup(a: &Args) -> Result<Agent<RealSystem>, String> {
    check_server_url(&a.server, a.allow_http)?;
    let token = std::fs::read_to_string(&a.token_file)
        .map_err(|e| format!("{}: {e}", a.token_file.display()))?;
    let token = token.trim().to_string();
    if token.is_empty() {
        return Err(format!("{}: empty token", a.token_file.display()));
    }
    let trusted = a
        .trusted_keys
        .iter()
        .filter(|k| !k.is_empty())
        .map(|k| PublicKey::parse(k))
        .collect::<Result<Vec<_>, _>>()?;
    let host = match &a.host {
        Some(h) => h.clone(),
        None => std::fs::read_to_string("/proc/sys/kernel/hostname")
            .map_err(|e| format!("host name: {e}"))?
            .trim()
            .to_string(),
    };
    let cfg = Config {
        server: a.server.clone(),
        identity: Identity {
            device: a.device.clone(),
            host,
            system: a.system.clone(),
        },
        token,
        trusted,
        state_dir: a.state_dir.clone(),
        local_state_url: a.local_state_url.clone(),
    };
    Agent::new(cfg, RealSystem)
}

fn main() {
    let args = Args::parse();
    let mut agent = match setup(&args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("emco-fleet-agent: {e}");
            std::process::exit(2);
        }
    };
    loop {
        let r = agent.poll_once();
        if let Err(e) = &r {
            eprintln!("emco-fleet-agent: {e}");
        }
        if args.once {
            std::process::exit(if r.is_ok() { 0 } else { 1 });
        }
        std::thread::sleep(Duration::from_secs(args.interval_s.max(5)));
    }
}
