//! `emco-fleet`: the build host's tool (`kd240/fleet/README.md`).
//!
//! ```text
//! emco-fleet keygen  --name emco-fleet-1 --out ~/.config/emco-fleet/fleet.sec
//! emco-fleet token   --device kd240
//! emco-fleet publish --device kd240 --toplevel /nix/store/…-nixos-system-kd240-… [--stage]
//! emco-fleet command --device kd240 switch --toplevel /nix/store/…
//! emco-fleet command --device kd240 reboot
//! emco-fleet devices
//! ```
//!
//! The secret key is read here and used here. Only signed envelopes leave.
#![forbid(unsafe_code)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::{Args, Parser, Subcommand};
use emco_fleet_proto::keys::{Envelope, SecretKey};
use emco_fleet_proto::msg::{
    Action, Command, DeviceDetail, DeviceSummary, Manifest, Order, Outcome,
};
use emco_fleet_proto::policy::{check_toplevel, read_toplevel_facts, system_store_path, Identity};
use emco_fleet_proto::token::{generate, TokenHash};
use emco_fleet_proto::{unix_now, valid_name};

#[derive(Parser)]
#[command(
    name = "emco-fleet",
    version,
    about = "Sign and publish KD240 updates and commands; show devices"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Args)]
struct Remote {
    /// The fleet server, e.g. https://fleet.example.com
    #[arg(long, env = "FLEET_SERVER")]
    server: Option<String>,
    /// File holding the admin token.
    #[arg(long, env = "FLEET_ADMIN_TOKEN_FILE")]
    admin_token_file: Option<PathBuf>,
}

#[derive(Args)]
struct Signing {
    /// The secret signing key (from `keygen`).
    #[arg(long, env = "FLEET_KEY_FILE")]
    key: PathBuf,
    #[arg(long)]
    device: String,
    /// Sequence number. Default: the time in milliseconds, which only grows.
    #[arg(long)]
    seq: Option<u64>,
    /// Print the signed envelope instead of sending it (to carry it over by hand).
    #[arg(long)]
    print: bool,
}

#[derive(Subcommand)]
enum Cmd {
    /// Make an order-signing key pair. Prints the public key for the board.
    Keygen {
        #[arg(long, default_value = "emco-fleet-1")]
        name: String,
        #[arg(long)]
        out: PathBuf,
    },
    /// Print the public key of a secret key file.
    Pubkey {
        #[arg(long, env = "FLEET_KEY_FILE")]
        key: PathBuf,
    },
    /// Make a device token: the token for the board, a line for the server.
    Token {
        #[arg(long)]
        device: String,
    },
    /// Sign and publish a manifest for a built toplevel.
    Publish {
        #[command(flatten)]
        sign: Signing,
        #[command(flatten)]
        remote: Remote,
        /// The built system, `/nix/store/…-nixos-system-<host>-…` (must exist here).
        #[arg(long)]
        toplevel: PathBuf,
        /// Also stage it on the board (next boot) once fetched.
        #[arg(long)]
        stage: bool,
        #[arg(long, default_value = "")]
        note: String,
    },
    /// Sign and send a command: stage, switch or reboot.
    Command {
        #[command(flatten)]
        sign: Signing,
        #[command(flatten)]
        remote: Remote,
        #[arg(value_parser = ["stage", "switch", "reboot"])]
        action: String,
        /// The toplevel (stage, switch; optional for reboot: stage it first).
        #[arg(long)]
        toplevel: Option<PathBuf>,
        /// The host name the toplevel is for, when it is not in the local store.
        #[arg(long)]
        host: Option<String>,
        /// Seconds the command stays valid (a deferred switch expires).
        #[arg(long, default_value_t = 3600)]
        ttl_s: u64,
    },
    /// Drop a device's queued commands on the server.
    Cancel {
        #[command(flatten)]
        remote: Remote,
        #[arg(long)]
        device: String,
    },
    /// Show the devices, or one device with its history.
    Devices {
        #[command(flatten)]
        remote: Remote,
        #[arg(long)]
        device: Option<String>,
        /// Raw JSON.
        #[arg(long)]
        json: bool,
    },
}

type R<T> = Result<T, String>;

fn read_trim(p: &Path) -> R<String> {
    Ok(std::fs::read_to_string(p)
        .map_err(|e| format!("{}: {e}", p.display()))?
        .trim()
        .to_string())
}

struct Client {
    server: String,
    token: String,
    http: ureq::Agent,
}

impl Remote {
    fn client(&self) -> R<Client> {
        let server = self
            .server
            .clone()
            .ok_or("--server (or FLEET_SERVER) is needed")?;
        let tf = self
            .admin_token_file
            .as_ref()
            .ok_or("--admin-token-file (or FLEET_ADMIN_TOKEN_FILE) is needed")?;
        let http = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(30)))
            .http_status_as_error(false)
            .build()
            .into();
        Ok(Client {
            server: server.trim_end_matches('/').to_string(),
            token: read_trim(tf)?,
            http,
        })
    }
}

impl Client {
    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.server)
    }

    fn check(&self, what: &str, mut resp: ureq::http::Response<ureq::Body>) -> R<String> {
        let code = resp.status();
        let body = resp.body_mut().read_to_string().unwrap_or_default();
        if code.is_success() {
            Ok(body)
        } else {
            Err(format!("{what}: {code}: {body}"))
        }
    }

    fn send(&self, method: &str, path: &str, env: Option<&Envelope>) -> R<String> {
        let url = self.url(path);
        let auth = format!("Bearer {}", self.token);
        let resp = match (method, env) {
            ("PUT", Some(e)) => self
                .http
                .put(&url)
                .header("Authorization", &auth)
                .send_json(e),
            ("POST", Some(e)) => self
                .http
                .post(&url)
                .header("Authorization", &auth)
                .send_json(e),
            ("DELETE", None) => self.http.delete(&url).header("Authorization", &auth).call(),
            ("GET", None) => self.http.get(&url).header("Authorization", &auth).call(),
            _ => unreachable!(),
        }
        .map_err(|e| format!("{url}: {e}"))?;
        self.check(&format!("{method} {url}"), resp)
    }
}

fn load_key(p: &Path) -> R<SecretKey> {
    SecretKey::parse(&read_trim(p)?).map_err(|e| format!("{}: {e}", p.display()))
}

fn seq_or_now(seq: Option<u64>) -> u64 {
    seq.unwrap_or_else(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    })
}

/// The toplevel as the board will check it, here first.
fn local_toplevel(device: &str, toplevel: &Path) -> R<(String, Identity)> {
    let path = std::fs::canonicalize(toplevel)
        .map_err(|e| format!("{}: {e} (build it first)", toplevel.display()))?;
    let path_s = path.to_string_lossy().into_owned();
    let facts = read_toplevel_facts(&path);
    let host = facts
        .hostname
        .clone()
        .ok_or_else(|| format!("{path_s} has no etc/hostname: not a NixOS system?"))?;
    let system = facts
        .system
        .clone()
        .ok_or_else(|| format!("{path_s} has no `system` file: not a NixOS system?"))?;
    let me = Identity {
        device: device.to_string(),
        host,
        system,
    };
    check_toplevel(&path_s, &facts, &me)?;
    Ok((path_s, me))
}

fn deliver(sign: &Signing, remote: &Remote, env: Envelope, method: &str, what: &str) -> R<()> {
    if sign.print {
        println!(
            "{}",
            serde_json::to_string_pretty(&env).map_err(|e| e.to_string())?
        );
        return Ok(());
    }
    let c = remote.client()?;
    c.send(
        method,
        &format!("/api/v1/devices/{}/{what}", sign.device),
        Some(&env),
    )?;
    let order = env.unverified_order()?;
    eprintln!(
        "emco-fleet: sent {} seq {} to {}",
        order.what(),
        order.seq(),
        sign.device
    );
    Ok(())
}

/// Device-reported text, made safe for the admin's terminal: every control
/// character (C0, DEL, C1, so no ESC/CSI sequences) is shown escaped.
fn clean(s: &str) -> String {
    s.chars()
        .flat_map(|c| -> Box<dyn Iterator<Item = char>> {
            if c.is_control() {
                Box::new(c.escape_unicode())
            } else {
                Box::new(std::iter::once(c))
            }
        })
        .collect()
}

fn short(path: &Option<String>) -> String {
    match path {
        Some(p) => clean(
            p.strip_prefix("/nix/store/")
                .and_then(|r| r.get(33..))
                .unwrap_or(p),
        ),
        None => "-".into(),
    }
}

fn print_summary(s: &DeviceSummary, now: u64) {
    println!("{}", clean(&s.device));
    let Some(l) = &s.latest else {
        println!("  never seen");
        return;
    };
    let t = &l.telemetry;
    println!(
        "  seen       {}s ago (last seq {})",
        now.saturating_sub(l.at),
        l.last_seq
    );
    println!("  lathe      {}", clean(&t.run_state));
    println!("  current    {}", short(&t.current_system));
    if t.booted_system != t.current_system {
        println!("  booted     {}", short(&t.booted_system));
    }
    if t.profile_system != t.current_system {
        println!("  next boot  {}", short(&t.profile_system));
    }
    if t.available_system.is_some() && t.available_system != t.current_system {
        println!("  available  {}", short(&t.available_system));
    }
    println!(
        "  kernel     {} {}",
        clean(&t.kernel_release),
        clean(&t.kernel_version)
    );
    println!(
        "  up {}h{:02}m  load {:.2} {:.2} {:.2}  mem {}/{} MiB free  failed units {}",
        t.uptime_s / 3600,
        t.uptime_s % 3600 / 60,
        t.load[0],
        t.load[1],
        t.load[2],
        t.mem_available_kib / 1024,
        t.mem_total_kib / 1024,
        t.failed_units.map(|n| n.to_string()).unwrap_or("?".into())
    );
    for temp in &t.temps {
        println!(
            "  temp       {} {:.1} C",
            clean(&temp.name),
            temp.milli_c as f64 / 1000.0
        );
    }
    for r in t.results.iter().rev().take(5) {
        let o = match r.outcome {
            Outcome::Done => "done",
            Outcome::Deferred => "DEFERRED",
            Outcome::Rejected => "REJECTED",
            Outcome::Failed => "FAILED",
        };
        println!(
            "  result     seq {} {} {o}: {}",
            r.seq,
            clean(&r.what),
            clean(&r.detail)
        );
    }
    if !s.pending_commands.is_empty() {
        let seqs: Vec<String> = s
            .pending_commands
            .iter()
            .filter_map(|e| e.unverified_order().ok())
            .map(|o| format!("{} {}", clean(o.what()), o.seq()))
            .collect();
        println!("  queued     {}", seqs.join(", "));
    }
}

fn run(cli: Cli) -> R<()> {
    match cli.cmd {
        Cmd::Keygen { name, out } => {
            let k = SecretKey::generate(&name)?;
            let mut f = {
                use std::os::unix::fs::OpenOptionsExt;
                std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&out)
            }
            .map_err(|e| format!("{}: {e}", out.display()))?;
            writeln!(f, "{}", k.to_text()).map_err(|e| e.to_string())?;
            eprintln!(
                "emco-fleet: secret key in {} (keep it on this machine; back it up)",
                out.display()
            );
            eprintln!("emco-fleet: public key for services.emco-fleet-agent.trustedKeys:");
            println!("{}", k.public().to_text());
        }
        Cmd::Pubkey { key } => println!("{}", load_key(&key)?.public().to_text()),
        Cmd::Token { device } => {
            if !valid_name(&device) {
                return Err(format!("bad device id {device:?}"));
            }
            let t = generate()?;
            println!(
                "# on the board, in the agent's token file (root-only, outside the Nix store):"
            );
            println!("{t}");
            println!("# on the server, a line in the device tokens file:");
            println!("{device} {}", TokenHash::of(&t).to_text());
        }
        Cmd::Publish {
            sign,
            remote,
            toplevel,
            stage,
            note,
        } => {
            let key = load_key(&sign.key)?;
            let (toplevel, me) = local_toplevel(&sign.device, &toplevel)?;
            let order = Order::Manifest(Manifest {
                device: sign.device.clone(),
                seq: seq_or_now(sign.seq),
                host: me.host,
                system: me.system,
                toplevel,
                stage,
                created: unix_now(),
                note,
            });
            deliver(&sign, &remote, key.sign(&order), "PUT", "manifest")?;
        }
        Cmd::Command {
            sign,
            remote,
            action,
            toplevel,
            host,
            ttl_s,
        } => {
            let key = load_key(&sign.key)?;
            let top = match &toplevel {
                Some(t) if t.exists() => Some(local_toplevel(&sign.device, t)?.0),
                Some(t) => {
                    let t = t.to_string_lossy().into_owned();
                    let host = host
                        .as_deref()
                        .ok_or("the toplevel is not in the local store: give --host")?;
                    system_store_path(&t, host)?;
                    Some(t)
                }
                None => None,
            };
            let action = match (action.as_str(), top) {
                ("stage", Some(toplevel)) => Action::Stage { toplevel },
                ("switch", Some(toplevel)) => Action::Switch { toplevel },
                ("reboot", toplevel) => Action::Reboot { toplevel },
                (a, None) => return Err(format!("{a} needs --toplevel")),
                _ => unreachable!(),
            };
            let order = Order::Command(Command {
                device: sign.device.clone(),
                seq: seq_or_now(sign.seq),
                not_after: unix_now() + ttl_s,
                action,
            });
            deliver(&sign, &remote, key.sign(&order), "POST", "commands")?;
        }
        Cmd::Cancel { remote, device } => {
            remote.client()?.send(
                "DELETE",
                &format!("/api/v1/devices/{device}/commands"),
                None,
            )?;
            eprintln!("emco-fleet: dropped {device}'s queued commands");
        }
        Cmd::Devices {
            remote,
            device,
            json,
        } => {
            let c = remote.client()?;
            let now = unix_now();
            match device {
                Some(d) => {
                    let body = c.send("GET", &format!("/api/v1/devices/{d}"), None)?;
                    if json {
                        println!("{body}");
                    } else {
                        let d: DeviceDetail =
                            serde_json::from_str(&body).map_err(|e| e.to_string())?;
                        print_summary(&d.summary, now);
                        println!("  history    {} samples", d.history.len());
                    }
                }
                None => {
                    let body = c.send("GET", "/api/v1/devices", None)?;
                    if json {
                        println!("{body}");
                    } else {
                        let all: Vec<DeviceSummary> =
                            serde_json::from_str(&body).map_err(|e| e.to_string())?;
                        for s in &all {
                            print_summary(s, now);
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

fn main() {
    if let Err(e) = run(Cli::parse()) {
        eprintln!("emco-fleet: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod clean_tests {
    use super::clean;

    #[test]
    fn device_text_cannot_drive_the_terminal() {
        assert_eq!(clean("idle"), "idle");
        assert_eq!(clean("a\x1b[2Jb"), "a\\u{1b}[2Jb");
        assert_eq!(clean("x\u{9b}31m\x07\n"), "x\\u{9b}31m\\u{7}\\u{a}");
        assert_eq!(clean("Ø 6.18.37"), "Ø 6.18.37");
    }
}
