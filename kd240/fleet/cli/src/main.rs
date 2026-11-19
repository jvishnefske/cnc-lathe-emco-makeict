//! `emco-fleet`: the build host's tool (`kd240/fleet/README.md`).
//!
//! ```text
//! emco-fleet pubkey  --pkcs11 emco-fleet          # the card's key, for trustedKeys
//! emco-fleet keygen  --name emco-fleet-1 --out ~/.config/emco-fleet/fleet.sec
//! emco-fleet token   --device kd240
//! nix build .#kd240-bundle -o result-bundle
//! emco-fleet publish --pkcs11 emco-fleet --device kd240 --bundle result-bundle [--stage]
//! emco-fleet command --pkcs11 emco-fleet --device kd240 reboot --bundle result-bundle
//! emco-fleet devices
//! ```
//!
//! The secret key is used here (or stays in its card). Only signed
//! envelopes and the bundle's files leave.
#![forbid(unsafe_code)]

mod bundle_dir;
mod signing;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use bundle_dir::BundleDir;
use clap::{Args, Parser, Subcommand};
use emco_fleet_proto::keys::{sign_order, Envelope, SecretKey};
use emco_fleet_proto::msg::{
    Action, Command, DeviceDetail, DeviceSummary, Manifest, Order, Outcome,
};
use emco_fleet_proto::token::{generate, TokenHash};
use emco_fleet_proto::{unix_now, valid_name};
use signing::KeySource;

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
struct Key {
    /// The signing key's label in a PKCS#11 token (PKCS11_MODULE,
    /// PKCS11_TOKEN, PKCS11_PIN as for mwctl).
    #[arg(long, env = "FLEET_PKCS11_KEY")]
    pkcs11: Option<String>,
    /// A software ed25519 key from `keygen`, instead of a token.
    #[arg(long, env = "FLEET_KEY_FILE")]
    key: Option<PathBuf>,
    /// The name the board's trusted key has. Default: the PKCS#11 label.
    #[arg(long, env = "FLEET_KEY_NAME")]
    key_name: Option<String>,
}

impl Key {
    fn source(&self) -> R<KeySource> {
        KeySource::from_args(
            self.key.as_deref(),
            self.pkcs11.as_deref(),
            self.key_name.as_deref(),
        )
    }
}

#[derive(Args)]
struct Signing {
    #[command(flatten)]
    key: Key,
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
    /// Print the public key for the board's trustedKeys.
    Pubkey {
        #[command(flatten)]
        key: Key,
    },
    /// Make a device token: the token for the board, a line for the server.
    Token {
        #[arg(long)]
        device: String,
    },
    /// Upload a built bundle's files and publish a signed manifest for it.
    Publish {
        #[command(flatten)]
        sign: Signing,
        #[command(flatten)]
        remote: Remote,
        /// The bundle directory (`nix build .#kd240-bundle`).
        #[arg(long)]
        bundle: PathBuf,
        /// Also write it into the board's inactive slot now.
        #[arg(long)]
        stage: bool,
        #[arg(long, default_value = "")]
        note: String,
    },
    /// Sign and send a command: stage, or reboot (into a bundle, or not).
    Command {
        #[command(flatten)]
        sign: Signing,
        #[command(flatten)]
        remote: Remote,
        #[arg(value_parser = ["stage", "reboot"])]
        action: String,
        /// The bundle directory (stage; optional for reboot: boot it once,
        /// committed when healthy).
        #[arg(long)]
        bundle: Option<PathBuf>,
        /// Seconds the command stays valid (a deferred reboot expires).
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

    /// Upload the bundle's files the server does not have yet.
    fn upload(&self, b: &BundleDir) -> R<()> {
        let auth = format!("Bearer {}", self.token);
        for f in &b.bundle.files {
            let url = self.url(&format!("/api/v1/files/{}", f.sha256));
            let p = b.path(&f.name);
            let file = std::fs::File::open(&p).map_err(|e| format!("{}: {e}", p.display()))?;
            let resp = self
                .http
                .put(&url)
                .config()
                .timeout_global(None)
                .build()
                .header("Authorization", &auth)
                .header("Content-Type", "application/octet-stream")
                .send(&file)
                .map_err(|e| format!("{url}: {e}"))?;
            let fresh = resp.status() == 201;
            self.check(&format!("PUT {url}"), resp)?;
            if fresh {
                eprintln!("emco-fleet: uploaded {} ({} bytes)", f.name, f.size);
            }
        }
        Ok(())
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

fn seq_or_now(seq: Option<u64>) -> u64 {
    seq.unwrap_or_else(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    })
}

fn sign_env(sign: &Signing, order: &Order) -> R<Envelope> {
    let signer = sign.key.source()?.open()?;
    sign_order(signer.as_ref(), order)
}

/// Open a bundle directory and upload its files (unless only printing).
fn prepare(sign: &Signing, remote: &Remote, dir: &Path) -> R<BundleDir> {
    let b = BundleDir::open(dir)?;
    if b.bundle.class != "kd240" {
        eprintln!(
            "emco-fleet: note: the bundle is for class {:?}",
            b.bundle.class
        );
    }
    if !sign.print {
        remote.client()?.upload(&b)?;
    }
    Ok(b)
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

/// Characters that change how a line reads without being visible: format
/// characters (bidi embeddings, overrides and isolates, zero-width marks,
/// BOM), line and paragraph separators, and private-use code points.
fn invisible(c: char) -> bool {
    matches!(c,
        '\u{00AD}' | '\u{061C}' | '\u{180E}'
        | '\u{200B}'..='\u{200F}'
        | '\u{2028}'..='\u{202E}'
        | '\u{2060}'..='\u{206F}'
        | '\u{FEFF}'
        | '\u{FFF9}'..='\u{FFFB}'
        | '\u{E000}'..='\u{F8FF}'
        | '\u{F0000}'..='\u{10FFFF}'
        | '\u{E0000}'..='\u{E007F}')
}

/// Device-reported text, made safe for the admin's terminal: control
/// characters (C0, DEL, C1, so no ESC/CSI sequences) and [`invisible`] ones
/// are shown escaped, so a board can't redraw or reorder what is printed.
fn clean(s: &str) -> String {
    s.chars()
        .flat_map(|c| -> Box<dyn Iterator<Item = char>> {
            if c.is_control() || invisible(c) {
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
    if let Some(sl) = &t.slots {
        let o = |s: &Option<String>| clean(s.as_deref().unwrap_or("?"));
        println!(
            "  slots      booted {}  committed {}{}",
            o(&sl.booted),
            o(&sl.active),
            sl.try_slot
                .as_ref()
                .map(|t| format!("  TRY {}", clean(t)))
                .unwrap_or_default()
        );
        println!("  slot a     {}", short(&sl.a));
        println!("  slot b     {}", short(&sl.b));
        if sl.available.is_some() {
            println!(
                "  available  {}{}",
                short(&sl.available),
                if sl.staged { " (staged)" } else { "" }
            );
        }
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
        Cmd::Pubkey { key } => {
            let s = key.source()?.open()?;
            let pk = s.public_key()?;
            eprintln!(
                "emco-fleet: {} key, fingerprint {}",
                pk.algorithm(),
                pk.fingerprint()
            );
            println!("{}", pk.to_text());
        }
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
            bundle,
            stage,
            note,
        } => {
            let b = prepare(&sign, &remote, &bundle)?;
            let order = Order::Manifest(Manifest {
                device: sign.device.clone(),
                seq: seq_or_now(sign.seq),
                bundle: b.bundle,
                stage,
                created: unix_now(),
                note,
            });
            deliver(&sign, &remote, sign_env(&sign, &order)?, "PUT", "manifest")?;
        }
        Cmd::Command {
            sign,
            remote,
            action,
            bundle,
            ttl_s,
        } => {
            let b = match &bundle {
                Some(dir) => Some(prepare(&sign, &remote, dir)?.bundle),
                None => None,
            };
            let action = match (action.as_str(), b) {
                ("stage", Some(bundle)) => Action::Stage { bundle },
                ("reboot", bundle) => Action::Reboot { bundle },
                (a, None) => return Err(format!("{a} needs --bundle")),
                _ => unreachable!(),
            };
            let order = Order::Command(Command {
                device: sign.device.clone(),
                seq: seq_or_now(sign.seq),
                not_after: unix_now() + ttl_s,
                action,
            });
            deliver(&sign, &remote, sign_env(&sign, &order)?, "POST", "commands")?;
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
        // Right-to-left override: "done" would display in place of "enod".
        assert_eq!(clean("\u{202e}enod"), "\\u{202e}enod");
        assert_eq!(
            clean("ok\u{200b}\u{2066}\u{feff}"),
            "ok\\u{200b}\\u{2066}\\u{feff}"
        );
        assert_eq!(clean("\u{e000}\u{2028}"), "\\u{e000}\\u{2028}");
    }
}
