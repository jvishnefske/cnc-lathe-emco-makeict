//! The server through its router, in process.

use super::*;
use axum::body::Body;
use axum::http::Request;
use emco_fleet_proto::keys::SecretKey;
use emco_fleet_proto::msg::{Action, Command, Manifest, Telemetry};
use http_body_util::BodyExt;
use tower::ServiceExt;

const TOP: &str = "/nix/store/0123456789abcdfghijklmnpqrsvwxyz-nixos-system-kd240-25.11";

struct Fixture {
    _dir: tempfile::TempDir,
    cfg: Config,
    key: SecretKey,
}

fn fixture(trusted: bool) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let key = SecretKey::generate("fleet-1").unwrap();
    std::fs::write(dir.path().join("admin"), "# admin\nadmin-token\n").unwrap();
    std::fs::write(
        dir.path().join("devices"),
        format!(
            "kd240 {}\nother other-token\n",
            TokenHash::of("dev-token").to_text()
        ),
    )
    .unwrap();
    std::fs::write(dir.path().join("keys"), key.public().to_text()).unwrap();
    std::fs::create_dir_all(dir.path().join("cache")).unwrap();
    std::fs::write(
        dir.path().join("cache/nix-cache-info"),
        "StoreDir: /nix/store\n",
    )
    .unwrap();
    let cfg = Config {
        listen: String::new(),
        data_dir: dir.path().join("data"),
        cache_dir: dir.path().join("cache"),
        admin_token_file: dir.path().join("admin"),
        device_tokens_file: dir.path().join("devices"),
        trusted_keys_file: trusted.then(|| dir.path().join("keys")),
        history: 3,
        history_every_s: 0,
        max_pending: 2,
    };
    Fixture {
        _dir: dir,
        cfg,
        key,
    }
}

async fn call(
    app: &Shared,
    method: &str,
    uri: &str,
    token: Option<&str>,
    body: Option<String>,
) -> (StatusCode, String) {
    let mut req = Request::builder().method(method).uri(uri);
    if let Some(t) = token {
        req = req.header("Authorization", format!("Bearer {t}"));
    }
    if body.is_some() {
        req = req.header("Content-Type", "application/json");
    }
    let resp = router(app.clone())
        .oneshot(req.body(body.map(Body::from).unwrap_or_default()).unwrap())
        .await
        .unwrap();
    let code = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (code, String::from_utf8_lossy(&bytes).into_owned())
}

fn manifest(key: &SecretKey, seq: u64) -> Envelope {
    key.sign(&Order::Manifest(Manifest {
        device: "kd240".into(),
        seq,
        host: "kd240".into(),
        system: "aarch64-linux".into(),
        toplevel: TOP.into(),
        stage: false,
        created: 1,
        note: String::new(),
    }))
}

fn command(key: &SecretKey, seq: u64) -> Envelope {
    key.sign(&Order::Command(Command {
        device: "kd240".into(),
        seq,
        not_after: u64::MAX,
        action: Action::Reboot { toplevel: None },
    }))
}

fn poll_body(last_seq: u64) -> String {
    serde_json::to_string(&PollRequest {
        last_seq,
        telemetry: Telemetry {
            hostname: "kd240".into(),
            ..Default::default()
        },
    })
    .unwrap()
}

#[tokio::test]
async fn tokens_are_checked() {
    let f = fixture(false);
    let app = App::open(f.cfg.clone()).unwrap();
    assert_eq!(
        call(&app, "GET", "/api/v1/devices", None, None).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&app, "GET", "/api/v1/devices", Some("nope"), None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    // A device token is not an admin token.
    assert_eq!(
        call(&app, "GET", "/api/v1/devices", Some("dev-token"), None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&app, "GET", "/api/v1/devices", Some("admin-token"), None)
            .await
            .0,
        StatusCode::OK
    );
    // A device token works for its own device only.
    let p = "/api/v1/devices/kd240/poll";
    assert_eq!(
        call(&app, "POST", p, Some("other-token"), Some(poll_body(0)))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&app, "POST", p, Some("admin-token"), Some(poll_body(0)))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&app, "POST", p, Some("dev-token"), Some(poll_body(0)))
            .await
            .0,
        StatusCode::OK
    );
    // Publishing needs the admin token.
    let m = serde_json::to_string(&manifest(&f.key, 1)).unwrap();
    assert_eq!(
        call(
            &app,
            "PUT",
            "/api/v1/devices/kd240/manifest",
            Some("dev-token"),
            Some(m)
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn publish_poll_and_acknowledge() {
    let f = fixture(true);
    let app = App::open(f.cfg.clone()).unwrap();
    let adm = Some("admin-token");
    let m = serde_json::to_string(&manifest(&f.key, 10)).unwrap();
    assert_eq!(
        call(
            &app,
            "PUT",
            "/api/v1/devices/kd240/manifest",
            adm,
            Some(m.clone())
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    // The same seq again is a conflict.
    assert_eq!(
        call(&app, "PUT", "/api/v1/devices/kd240/manifest", adm, Some(m))
            .await
            .0,
        StatusCode::CONFLICT
    );
    let c = serde_json::to_string(&command(&f.key, 11)).unwrap();
    assert_eq!(
        call(&app, "POST", "/api/v1/devices/kd240/commands", adm, Some(c))
            .await
            .0,
        StatusCode::ACCEPTED
    );

    let (code, body) = call(
        &app,
        "POST",
        "/api/v1/devices/kd240/poll",
        Some("dev-token"),
        Some(poll_body(0)),
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    let r: PollResponse = serde_json::from_str(&body).unwrap();
    assert!(r.manifest.is_some());
    assert_eq!(r.commands.len(), 1);

    // Once the board reports seq 11, nothing is pending.
    let (_, body) = call(
        &app,
        "POST",
        "/api/v1/devices/kd240/poll",
        Some("dev-token"),
        Some(poll_body(11)),
    )
    .await;
    assert_eq!(
        serde_json::from_str::<PollResponse>(&body).unwrap(),
        PollResponse::default()
    );
    // ...and an older command is refused at publish time.
    let c = serde_json::to_string(&command(&f.key, 11)).unwrap();
    assert_eq!(
        call(&app, "POST", "/api/v1/devices/kd240/commands", adm, Some(c))
            .await
            .0,
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn tampered_or_misdirected_orders_are_refused_when_keys_are_configured() {
    let f = fixture(true);
    let app = App::open(f.cfg.clone()).unwrap();
    let adm = Some("admin-token");
    let mut m = manifest(&f.key, 10);
    m.payload = m.payload.replace("\"stage\":false", "\"stage\":true");
    let (code, body) = call(
        &app,
        "PUT",
        "/api/v1/devices/kd240/manifest",
        adm,
        Some(serde_json::to_string(&m).unwrap()),
    )
    .await;
    assert_eq!(
        (code, body.as_str()),
        (StatusCode::BAD_REQUEST, "bad signature")
    );
    // A manifest for kd240 put to another device.
    let m = serde_json::to_string(&manifest(&f.key, 10)).unwrap();
    assert_eq!(
        call(
            &app,
            "PUT",
            "/api/v1/devices/other/manifest",
            adm,
            Some(m.clone())
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        call(&app, "PUT", "/api/v1/devices/nobody/manifest", adm, Some(m))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    // A command where a manifest goes.
    let c = serde_json::to_string(&command(&f.key, 12)).unwrap();
    assert_eq!(
        call(&app, "PUT", "/api/v1/devices/kd240/manifest", adm, Some(c))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn queue_is_bounded_and_clearable() {
    let f = fixture(false);
    let app = App::open(f.cfg.clone()).unwrap();
    let adm = Some("admin-token");
    for (seq, want) in [
        (3, StatusCode::ACCEPTED),
        (2, StatusCode::ACCEPTED),
        (2, StatusCode::CONFLICT),
        (4, StatusCode::TOO_MANY_REQUESTS),
    ] {
        let c = serde_json::to_string(&command(&f.key, seq)).unwrap();
        assert_eq!(
            call(&app, "POST", "/api/v1/devices/kd240/commands", adm, Some(c))
                .await
                .0,
            want,
            "seq {seq}"
        );
    }
    let (_, body) = call(
        &app,
        "POST",
        "/api/v1/devices/kd240/poll",
        Some("dev-token"),
        Some(poll_body(0)),
    )
    .await;
    let seqs: Vec<u64> = serde_json::from_str::<PollResponse>(&body)
        .unwrap()
        .commands
        .iter()
        .map(|e| e.unverified_order().unwrap().seq())
        .collect();
    assert_eq!(seqs, [2, 3]);
    assert_eq!(
        call(&app, "DELETE", "/api/v1/devices/kd240/commands", adm, None)
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    let (_, body) = call(
        &app,
        "POST",
        "/api/v1/devices/kd240/poll",
        Some("dev-token"),
        Some(poll_body(0)),
    )
    .await;
    assert!(serde_json::from_str::<PollResponse>(&body)
        .unwrap()
        .commands
        .is_empty());
}

#[tokio::test]
async fn history_is_bounded_and_state_persists() {
    let f = fixture(false);
    let app = App::open(f.cfg.clone()).unwrap();
    for i in 0..5 {
        call(
            &app,
            "POST",
            "/api/v1/devices/kd240/poll",
            Some("dev-token"),
            Some(poll_body(i)),
        )
        .await;
    }
    let m = serde_json::to_string(&manifest(&f.key, 10)).unwrap();
    call(
        &app,
        "PUT",
        "/api/v1/devices/kd240/manifest",
        Some("admin-token"),
        Some(m),
    )
    .await;
    drop(app);

    let app = App::open(f.cfg.clone()).unwrap();
    let (code, body) = call(
        &app,
        "GET",
        "/api/v1/devices/kd240",
        Some("admin-token"),
        None,
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    let d: DeviceDetail = serde_json::from_str(&body).unwrap();
    assert_eq!(
        d.history.iter().map(|s| s.last_seq).collect::<Vec<_>>(),
        [2, 3, 4]
    );
    assert!(d.summary.manifest.is_some());
    assert_eq!(d.summary.latest.unwrap().telemetry.hostname, "kd240");

    let (_, body) = call(&app, "GET", "/api/v1/devices", Some("admin-token"), None).await;
    let all: Vec<DeviceSummary> = serde_json::from_str(&body).unwrap();
    assert_eq!(
        all.iter().map(|s| s.device.as_str()).collect::<Vec<_>>(),
        ["kd240", "other"]
    );
}

#[tokio::test]
async fn serves_the_cache_without_a_token() {
    let f = fixture(false);
    let app = App::open(f.cfg.clone()).unwrap();
    let (code, body) = call(&app, "GET", "/cache/nix-cache-info", None, None).await;
    assert_eq!(
        (code, body.as_str()),
        (StatusCode::OK, "StoreDir: /nix/store\n")
    );
    assert_eq!(
        call(&app, "GET", "/cache/../admin", None, None).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(&app, "GET", "/healthz", None, None).await.0,
        StatusCode::OK
    );
}

#[test]
fn bad_config_files_are_refused() {
    let f = fixture(false);
    std::fs::write(&f.cfg.admin_token_file, "# nothing\n").unwrap();
    assert!(App::open(f.cfg.clone()).is_err());
    std::fs::write(&f.cfg.admin_token_file, "x\n").unwrap();
    std::fs::write(&f.cfg.device_tokens_file, "../etc tok\n").unwrap();
    assert!(App::open(f.cfg.clone()).is_err());
    std::fs::write(&f.cfg.device_tokens_file, "kd240\n").unwrap();
    assert!(App::open(f.cfg.clone()).is_err());
}
