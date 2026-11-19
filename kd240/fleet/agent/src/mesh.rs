//! The process payload's run state over Zenoh, for the interlock
//! ([`emco_fleet_proto::policy::local_run_state`]).
//!
//! The process model republishes its state every second
//! (`cnc_model::proc::STATE_HEARTBEAT_MS`), so a subscriber that arrives
//! late hears it within one: [`read_state`] opens a client session to the
//! board's router, subscribes to `app/proc/<node>/state` and waits up to
//! [`WAIT`] for one publication. It only listens: the agent sends the
//! lathe nothing.
//!
//! **Any sample on that key is believed.** Publications are not
//! authenticated (the same gap as the unit's commands): anything that can
//! publish on the board's router can claim to be the process model. The
//! interlock is built so that this can only *add* a deferral, never remove
//! one: a reboot needs this state *and* the lathe service's to be quiet
//! (`emco_fleet_proto::policy::local_run_state`), so a forged `idle` changes
//! nothing and a forged `running` merely postpones an update. The board's
//! zenohd listens on loopback only (kd240/nix/mesh.nix), so only the
//! board's own processes can publish there.

use std::time::Duration;

/// How long to wait for the process model's state: two heartbeats and a
/// margin.
pub const WAIT: Duration = Duration::from_millis(2500);

/// Where the process payload is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessProbe {
    /// The Zenoh router, `tcp/127.0.0.1:7447`.
    pub router: String,
    /// The process payload's node id, 16 hex digits.
    pub node: String,
}

impl ProcessProbe {
    /// Checks the node id; the router is checked when it is used.
    pub fn new(router: &str, node: &str) -> Result<ProcessProbe, String> {
        let node = node.trim().to_ascii_lowercase();
        if node.len() != 16 || !node.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(format!("process node id {node:?} is not 16 hex digits"));
        }
        if router.contains(['"', '\\']) || router.is_empty() {
            return Err(format!("bad zenoh router {router:?}"));
        }
        Ok(ProcessProbe {
            router: router.to_string(),
            node,
        })
    }

    /// `app/proc/<node>/state`.
    pub fn key(&self) -> String {
        format!("app/proc/{}/state", self.node)
    }
}

fn config(router: &str, mode: &str, listen: bool) -> Result<zenoh::Config, String> {
    let mut c = zenoh::Config::default();
    let ends = format!(r#"["{router}"]"#);
    let set = |c: &mut zenoh::Config, k: &str, v: &str| {
        c.insert_json5(k, v)
            .map_err(|e| format!("zenoh config {k}: {e}"))
    };
    set(&mut c, "mode", &format!(r#""{mode}""#))?;
    set(
        &mut c,
        if listen {
            "listen/endpoints"
        } else {
            "connect/endpoints"
        },
        &ends,
    )?;
    set(&mut c, "scouting/multicast/enabled", "false")?;
    Ok(c)
}

/// The process model's latest state publication, waiting up to `wait`.
pub fn read_state(p: &ProcessProbe, wait: Duration) -> Result<Vec<u8>, String> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    let cfg = config(&p.router, "client", false)?;
    let key = p.key();
    rt.block_on(async move {
        let session = tokio::time::timeout(wait, zenoh::open(cfg))
            .await
            .map_err(|_| format!("zenoh router {}: timed out", p.router))?
            .map_err(|e| format!("zenoh router {}: {e}", p.router))?;
        let sub = session
            .declare_subscriber(&key)
            .await
            .map_err(|e| format!("subscribe {key}: {e}"))?;
        let got = tokio::time::timeout(wait, sub.recv_async()).await;
        let _ = session.close().await;
        match got {
            Ok(Ok(sample)) => Ok(sample.payload().to_bytes().to_vec()),
            Ok(Err(e)) => Err(format!("{key}: {e}")),
            Err(_) => Err(format!("no state on {key} within {} ms", wait.as_millis())),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probes_check_the_node_id() {
        let p = ProcessProbe::new("tcp/127.0.0.1:7447", "0011AABBCCDDEEFF").unwrap();
        assert_eq!(p.key(), "app/proc/0011aabbccddeeff/state");
        assert!(ProcessProbe::new("tcp/127.0.0.1:7447", "0011").is_err());
        assert!(ProcessProbe::new("tcp/\"", "0011aabbccddeeff").is_err());
    }

    /// A process model on loopback publishing its state every 100 ms: the
    /// agent hears one. Nothing listening: an error, within the wait.
    #[test]
    fn the_state_is_read_from_a_router_on_loopback() {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let endpoint = format!("tcp/127.0.0.1:{port}");
        let probe = ProcessProbe::new(&endpoint, "00000000000000aa").unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let cfg = config(&endpoint, "peer", true).unwrap();
        let key = probe.key();
        let node = rt.block_on(async { zenoh::open(cfg).await }).unwrap();
        let publisher = rt.spawn(async move {
            loop {
                let _ = node.put(&key, vec![2u8; 11]).await;
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        });
        let body = read_state(&probe, Duration::from_secs(5)).unwrap();
        assert_eq!(body, vec![2u8; 11]);
        publisher.abort();

        let nobody = ProcessProbe::new("tcp/127.0.0.1:9", "00000000000000aa").unwrap();
        let t = std::time::Instant::now();
        assert!(read_state(&nobody, Duration::from_millis(500)).is_err());
        assert!(t.elapsed() < Duration::from_secs(5));
    }
}
