//! The server through its router, in process.

use super::*;
use axum::body::Body;
use axum::http::Request;
use emco_fleet_proto::keys::SecretKey;
use emco_fleet_proto::msg::{Action, Command, Manifest, Telemetry};
use http_body_util::BodyExt;
use tower::ServiceExt;

const TOP: &str = "/nix/store/0123456789abcdfghijklmnpqrsvwxyz-nixos-system-kd240-25.11";

fn bundle() -> emco_fleet_proto::bundle::Bundle {
    use emco_fleet_proto::bundle::{Bundle, BundleFile, FILES, FORMAT};
    Bundle {
        format: FORMAT.into(),
        class: "kd240".into(),
        version: TOP.into(),
        created: 1,
        files: FILES
            .iter()
            .map(|n| BundleFile {
                name: n.to_string(),
                size: 10,
                sha256: "a".repeat(64),
            })
            .collect(),
    }
}

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
    let cfg = Config {
        listen: String::new(),
        data_dir: dir.path().join("data"),
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
        bundle: bundle(),
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
        action: Action::Reboot { bundle: None },
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
async fn bundle_files_are_stored_by_their_hash() {
    use sha2::{Digest, Sha256};
    let f = fixture(false);
    let app = App::open(f.cfg.clone()).unwrap();
    let data = "the kernel".to_string();
    let sha = emco_fleet_proto::keys::hex(&Sha256::digest(&data));
    let uri = format!("/api/v1/files/{sha}");

    // Only an admin uploads, and only bytes that match the name.
    assert_eq!(
        call(&app, "PUT", &uri, Some("dev-token"), Some(data.clone()))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let wrong = format!("/api/v1/files/{}", "b".repeat(64));
    let (code, body) = call(&app, "PUT", &wrong, Some("admin-token"), Some(data.clone())).await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.contains(&sha), "{body}");
    assert_eq!(
        call(&app, "GET", &wrong, Some("dev-token"), None).await.0,
        StatusCode::NOT_FOUND,
        "a refused upload leaves nothing behind"
    );
    let leftovers: Vec<_> = std::fs::read_dir(f.cfg.data_dir.join("files"))
        .unwrap()
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");

    assert_eq!(
        call(&app, "PUT", &uri, Some("admin-token"), Some(data.clone()))
            .await
            .0,
        StatusCode::CREATED
    );
    assert_eq!(
        call(&app, "PUT", &uri, Some("admin-token"), Some(data.clone()))
            .await
            .0,
        StatusCode::NO_CONTENT,
        "already there"
    );

    // Any device (or an admin) downloads; nobody else.
    for tok in ["dev-token", "other-token", "admin-token"] {
        assert_eq!(
            call(&app, "GET", &uri, Some(tok), None).await,
            (StatusCode::OK, data.clone()),
            "{tok}"
        );
    }
    assert_eq!(
        call(&app, "GET", &uri, None, None).await.0,
        StatusCode::UNAUTHORIZED
    );
    for bad in ["../admin", "ABC", &"A".repeat(64)] {
        let code = call(
            &app,
            "GET",
            &format!("/api/v1/files/{bad}"),
            Some("dev-token"),
            None,
        )
        .await
        .0;
        assert!(code.is_client_error(), "{bad}: {code}");
        assert_ne!(code, StatusCode::OK);
    }
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
