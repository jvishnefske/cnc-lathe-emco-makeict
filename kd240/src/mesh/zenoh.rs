//! [`ZenohBus`]: the [`Bus`] on the board, over middleware's Zenoh backend
//! (`middleware::zenoh::ZenohTransport`, the same one the node's gateway
//! publishes with, attachments included).
//!
//! The session lives on a thread of its own with a one-worker Tokio
//! runtime, so the pump thread never waits on the network: a publication
//! is handed over on a channel, and received ones come back on another.
//! The thread connects to the router (in client mode, as middleware's
//! services do), retrying every [`RETRY`] until it succeeds.
//!
//! Commands are not kept for later. Until the session is up, publishing
//! is refused; a publication still queued after [`MAX_AGE`] is dropped
//! rather than sent late. Every publication dropped or failed after it was
//! accepted is counted ([`Bus::failures`]), so the transport stops
//! trusting a program upload that one of them belonged to.

use super::Bus;
use middleware::transport::{Publisher, Subscribe, Subscriber};
use middleware::zenoh::{ZenohError, ZenohSubscriber, ZenohTransport};
use middleware::{Key, KeyFilter};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};
use tokio::sync::mpsc as amp;

/// Between connection attempts.
pub const RETRY: Duration = Duration::from_secs(2);
/// A publication older than this when its turn comes is dropped.
pub const MAX_AGE: Duration = Duration::from_secs(1);
/// The largest publication received; a bigger one is dropped.
pub const MAX_SAMPLE: usize = 64 * 1024;

type Outgoing = (Instant, String, Vec<u8>);

/// See the module docs.
pub struct ZenohBus {
    out: amp::UnboundedSender<Outgoing>,
    inbox: mpsc::Receiver<(String, Vec<u8>)>,
    up: Arc<AtomicBool>,
    failures: Arc<AtomicU64>,
}

/// A client-mode Zenoh configuration connecting to `router` only.
pub fn client_config(router: &str) -> Result<zenoh::Config, String> {
    if router.contains(['"', '\\']) {
        return Err(format!("bad router locator {router:?}"));
    }
    let mut c = zenoh::Config::default();
    let set = |c: &mut zenoh::Config, k: &str, v: &str| {
        c.insert_json5(k, v)
            .map_err(|e| format!("zenoh config {k}: {e}"))
    };
    set(&mut c, "mode", r#""client""#)?;
    set(&mut c, "connect/endpoints", &format!(r#"["{router}"]"#))?;
    set(&mut c, "scouting/multicast/enabled", "false")?;
    Ok(c)
}

impl ZenohBus {
    /// Connects to the Zenoh router at `router` (`tcp/127.0.0.1:7447`) in
    /// the background and subscribes below each of `prefixes`.
    pub fn connect(router: &str, prefixes: Vec<String>) -> Result<ZenohBus, String> {
        let config = client_config(router)?;
        let router = router.to_owned();
        Ok(Self::start(prefixes, move || {
            let config = config.clone();
            let router = router.clone();
            async move {
                ZenohTransport::open(config)
                    .await
                    .map_err(|e| format!("zenoh router {router}: {e}"))
            }
        }))
    }

    /// The bus over transports `open` makes (a test passes an isolated
    /// session with local delivery).
    pub fn start<F, Fut>(prefixes: Vec<String>, open: F) -> ZenohBus
    where
        F: Fn() -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<ZenohTransport, String>>,
    {
        let (out, out_rx) = amp::unbounded_channel();
        let (in_tx, inbox) = mpsc::channel();
        let up = Arc::new(AtomicBool::new(false));
        let flag = up.clone();
        let failures = Arc::new(AtomicU64::new(0));
        let failed = failures.clone();
        std::thread::Builder::new()
            .name("mesh-zenoh".into())
            .spawn(move || {
                // Zenoh refuses Tokio's current-thread scheduler.
                let rt = match tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(1)
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => return eprintln!("mesh: no runtime: {e}"),
                };
                rt.block_on(serve(prefixes, open, out_rx, in_tx, flag, failed));
            })
            .expect("spawn the mesh thread");
        ZenohBus {
            out,
            inbox,
            up,
            failures,
        }
    }

    /// True once the session is open and subscribed.
    pub fn is_up(&self) -> bool {
        self.up.load(Ordering::Acquire)
    }
}

impl Bus for ZenohBus {
    fn publish(&mut self, key: &str, body: &[u8]) -> Result<(), String> {
        if !self.is_up() {
            return Err("not connected to the zenoh router".into());
        }
        Key::new(key).map_err(|e| format!("{e:?}"))?;
        self.out
            .send((Instant::now(), key.into(), body.into()))
            .map_err(|_| "the mesh thread has stopped".to_string())
    }

    fn try_recv(&mut self) -> Option<(String, Vec<u8>)> {
        self.inbox.try_recv().ok()
    }

    fn failures(&self) -> u64 {
        self.failures.load(Ordering::Acquire)
    }
}

async fn subscribe(t: &mut ZenohTransport, prefix: &str) -> Result<ZenohSubscriber, String> {
    let key = Key::new(prefix).map_err(|e| format!("{prefix}: {e:?}"))?;
    t.subscribe(KeyFilter::Prefix(key))
        .await
        .map_err(|e: ZenohError| format!("subscribe {prefix}/**: {e}"))
}

/// Receives on `sub` into `buf`, as an owned pair; `None` once it closes.
async fn next(sub: &mut ZenohSubscriber, buf: &mut [u8]) -> Option<(String, Vec<u8>)> {
    loop {
        match sub.recv(buf).await {
            Ok(s) => return Some((s.key.as_str().to_owned(), s.payload.to_vec())),
            Err(ZenohError::Closed) => return None,
            Err(e) => eprintln!("mesh: a sample was dropped: {e}"),
        }
    }
}

async fn serve<F, Fut>(
    prefixes: Vec<String>,
    open: F,
    mut out: amp::UnboundedReceiver<Outgoing>,
    inbox: mpsc::Sender<(String, Vec<u8>)>,
    up: Arc<AtomicBool>,
    failures: Arc<AtomicU64>,
) where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<ZenohTransport, String>>,
{
    let (mut t, mut subs) = loop {
        let opened = async {
            let mut t = open().await?;
            let mut subs = Vec::new();
            for p in &prefixes {
                subs.push(subscribe(&mut t, p).await?);
            }
            Ok::<_, String>((t, subs))
        };
        match opened.await {
            Ok(x) => break x,
            Err(e) => {
                eprintln!("mesh: {e}; retrying in {} s", RETRY.as_secs());
                tokio::time::sleep(RETRY).await;
            }
        }
    };
    // Anything handed over before the session was up is stale.
    while out.try_recv().is_ok() {}
    up.store(true, Ordering::Release);
    eprintln!(
        "mesh: connected, subscribed to {}",
        prefixes.join("/**, ") + "/**"
    );

    // One task per subscriber feeding the inbox; this one publishes.
    let mut tasks = tokio::task::JoinSet::new();
    for mut sub in subs.drain(..) {
        let inbox = inbox.clone();
        tasks.spawn(async move {
            let mut buf = vec![0u8; MAX_SAMPLE];
            while let Some(s) = next(&mut sub, &mut buf).await {
                if inbox.send(s).is_err() {
                    return;
                }
            }
        });
    }
    while let Some((at, key, body)) = out.recv().await {
        if at.elapsed() > MAX_AGE {
            eprintln!("mesh: dropped a stale publication on {key}");
            failures.fetch_add(1, Ordering::AcqRel);
            continue;
        }
        let Ok(k) = Key::new(&key) else {
            failures.fetch_add(1, Ordering::AcqRel);
            continue;
        };
        if let Err(e) = t.publish(k, &body).await {
            eprintln!("mesh: publish {key}: {e}");
            failures.fetch_add(1, Ordering::AcqRel);
        }
    }
    tasks.abort_all();
}

#[cfg(test)]
mod tests {
    use super::*;
    use middleware::zenoh::SessionConfig;

    fn wait<T>(mut f: impl FnMut() -> Option<T>) -> T {
        for _ in 0..500 {
            if let Some(v) = f() {
                return v;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("timed out");
    }

    #[test]
    fn client_configs_name_one_router() {
        assert!(client_config("tcp/127.0.0.1:7447").is_ok());
        assert!(client_config("tcp/\"x").is_err());
    }

    /// One isolated Zenoh session (no network) with local delivery: the bus
    /// and the test's own transport talk through it.
    #[test]
    fn publications_cross_an_in_process_session_both_ways() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut peer = rt.block_on(async {
            ZenohTransport::open(SessionConfig::new().build().unwrap())
                .await
                .unwrap()
                .with_local_delivery(true)
        });
        let for_bus = peer.clone();
        let mut bus = ZenohBus::start(vec!["app/proc/aa".into()], move || {
            let t = for_bus.clone();
            async move { Ok(t) }
        });
        assert!(bus.publish("app/proc/aa/cmd", &[2]).is_err() || bus.is_up());
        wait(|| bus.is_up().then_some(()));

        let mut cmds = rt
            .block_on(peer.subscribe(KeyFilter::Exact(Key::new("app/proc/aa/cmd").unwrap())))
            .unwrap();
        // The test's peer publishes a state; the bus receives it, and
        // nothing outside its prefix.
        rt.block_on(async {
            peer.publish(Key::new("app/proc/bb/state").unwrap(), &[9])
                .await
                .unwrap();
            peer.publish(Key::new("app/proc/aa/state").unwrap(), &[1, 2, 3])
                .await
                .unwrap();
        });
        let got = wait(|| bus.try_recv());
        assert_eq!(got, ("app/proc/aa/state".to_string(), vec![1, 2, 3]));

        // The bus publishes a command; the peer receives it.
        bus.publish("app/proc/aa/cmd", &[2]).unwrap();
        let mut buf = vec![0u8; 256];
        let s = rt.block_on(async {
            tokio::time::timeout(Duration::from_secs(5), cmds.recv(&mut buf))
                .await
                .expect("the command arrives")
                .unwrap()
        });
        assert_eq!((s.key.as_str(), s.payload), ("app/proc/aa/cmd", &[2u8][..]));
        assert!(bus.publish("not a key", &[0]).is_err());
    }

    /// The real path: [`ZenohBus::connect`] opens a client session on its
    /// own thread and reaches a peer listening on loopback, as it reaches
    /// the board's zenohd.
    #[test]
    fn connect_reaches_a_router_on_loopback() {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let endpoint = format!("tcp/127.0.0.1:{port}");
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut router = rt.block_on(async {
            ZenohTransport::open(SessionConfig::new().listen(&endpoint).build().unwrap())
                .await
                .unwrap()
        });
        let mut cmds = rt
            .block_on(router.subscribe(KeyFilter::Exact(Key::new("app/proc/aa/cmd").unwrap())))
            .unwrap();
        let mut bus = ZenohBus::connect(&endpoint, vec!["app/proc/aa".into()]).unwrap();
        wait(|| bus.is_up().then_some(()));
        // Until a publication arrives the subscription may still be
        // propagating: publish the state until one does.
        let got = wait(|| {
            rt.block_on(router.publish(Key::new("app/proc/aa/state").unwrap(), &[4]))
                .unwrap();
            std::thread::sleep(Duration::from_millis(20));
            bus.try_recv()
        });
        assert_eq!(got.1, vec![4]);
        bus.publish("app/proc/aa/cmd", &[6]).unwrap();
        let mut buf = vec![0u8; 256];
        let s = rt.block_on(async {
            tokio::time::timeout(Duration::from_secs(5), cmds.recv(&mut buf))
                .await
                .expect("the command arrives")
                .unwrap()
        });
        assert_eq!(s.payload, &[6u8][..]);
    }
}
