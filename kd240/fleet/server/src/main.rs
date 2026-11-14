//! `emco-fleet-server`: see the crate docs and `kd240/fleet/README.md`.
#![forbid(unsafe_code)]

use clap::Parser;
use emco_fleet_server::{router, App, Config};

#[tokio::main]
async fn main() {
    let cfg = Config::parse();
    let listen = cfg.listen.clone();
    let app = match App::open(cfg) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("emco-fleet-server: {e}");
            std::process::exit(1);
        }
    };
    let listener = match tokio::net::TcpListener::bind(&listen).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("emco-fleet-server: cannot listen on {listen}: {e}");
            std::process::exit(1);
        }
    };
    eprintln!("emco-fleet-server: listening on {listen}");
    let shutdown = async {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    };
    if let Err(e) = axum::serve(listener, router(app))
        .with_graceful_shutdown(shutdown)
        .await
    {
        eprintln!("emco-fleet-server: {e}");
        std::process::exit(1);
    }
}
