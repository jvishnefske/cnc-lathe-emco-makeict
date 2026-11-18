//! The HTTP API through the router, with the real machine pump behind it.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use emco_kd240::api::{router, App};
use emco_kd240::capture::Capture;
use emco_kd240::machine::Machine;
use emco_kd240::store::Store;
use emco_sender_core::profile::MachineProfile;
use http_body_util::BodyExt;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use tower::ServiceExt;

const TOKEN: &str = "a-test-token-0123456789";

fn tempdir(tag: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "emco-kd240-api-{}-{tag}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn app(tag: &str) -> axum::Router {
    let dir = tempdir(tag);
    let profile = MachineProfile::pc_turn_50_default();
    let app = App::new(
        Machine::spawn(profile.clone()),
        Store::open(dir.join("programs")).unwrap(),
        Capture::new(None, dir.join("captures")).unwrap(),
        profile,
        Some(TOKEN.into()),
    );
    router(Arc::new(app))
}

async fn call(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: &str,
    auth: bool,
) -> (StatusCode, Value, String) {
    let mut req = Request::builder().method(method).uri(uri);
    if auth {
        req = req.header(header::AUTHORIZATION, format!("Bearer {TOKEN}"));
    }
    if body.starts_with('{') {
        req = req.header(header::CONTENT_TYPE, "application/json");
    }
    let res = app
        .clone()
        .oneshot(req.body(Body::from(body.to_owned())).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&bytes).into_owned();
    (
        status,
        serde_json::from_str(&text).unwrap_or(Value::Null),
        text,
    )
}

const PART: &str = "G21 G90\nG00 X10 Z2\nG01 Z-20 F120\nG02 X15 Z-25 R5\nG00 X30 Z5\nM30\n";

#[tokio::test]
async fn ui_and_reads_are_open_changes_need_the_token() {
    let app = app("auth");
    let (s, _, html) = call(&app, "GET", "/", "", false).await;
    assert_eq!(s, StatusCode::OK);
    assert!(
        html.contains("rxjs-7.8.2.umd.min.js"),
        "the UI loads the vendored RxJS"
    );
    let (s, _, js) = call(&app, "GET", "/ui/vendor/rxjs-7.8.2.umd.min.js", "", false).await;
    assert_eq!(s, StatusCode::OK);
    assert!(js.len() > 80_000);
    assert_eq!(
        call(&app, "GET", "/ui/../Cargo.toml", "", false).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(&app, "GET", "/api/programs", "", false).await.0,
        StatusCode::OK
    );
    assert_eq!(
        call(&app, "PUT", "/api/programs/part1", PART, false)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let mut bad = Request::builder().method("PUT").uri("/api/programs/part1");
    bad = bad.header(header::AUTHORIZATION, "Bearer wrong-token-0123456789");
    let res = app
        .clone()
        .oneshot(bad.body(Body::from(PART)).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        call(&app, "POST", "/api/command", r#"{"kind":"run"}"#, false)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
}

async fn get_with_type(app: &axum::Router, uri: &str) -> (StatusCode, String, Vec<u8>) {
    let res = app
        .clone()
        .oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let ty = res
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let bytes = res.into_body().collect().await.unwrap().to_bytes().to_vec();
    (status, ty, bytes)
}

#[tokio::test]
async fn the_egui_screen_is_served_at_egui_when_built() {
    let app = app("egui");
    let res = app
        .clone()
        .oneshot(Request::get("/egui").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::PERMANENT_REDIRECT);
    assert_eq!(res.headers()[header::LOCATION], "/egui/");
    let (s, ty, html) = get_with_type(&app, "/egui/").await;
    assert!(ty.starts_with("text/html"), "{ty}");
    let html = String::from_utf8(html).unwrap();
    assert_eq!(
        get_with_type(&app, "/egui/../Cargo.toml").await.0,
        StatusCode::NOT_FOUND
    );
    if !emco_kd240::ui::EGUI_BUILT {
        // Not built into this binary: the page says how, and nothing else is served.
        assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
        assert!(html.contains("web/build.sh"), "{html}");
        assert_eq!(
            get_with_type(&app, "/egui/emco_egui_bg.wasm").await.0,
            StatusCode::NOT_FOUND
        );
        return;
    }
    assert_eq!(s, StatusCode::OK);
    assert!(html.contains("emco_canvas") && html.contains("./emco_egui.js"));
    let (s, ty, js) = get_with_type(&app, "/egui/emco_egui.js").await;
    assert_eq!(
        (s, ty.as_str()),
        (StatusCode::OK, "text/javascript; charset=utf-8")
    );
    assert!(String::from_utf8_lossy(&js).contains("emco_egui_bg.wasm"));
    let (s, ty, wasm) = get_with_type(&app, "/egui/emco_egui_bg.wasm").await;
    assert_eq!((s, ty.as_str()), (StatusCode::OK, "application/wasm"));
    assert_eq!(&wasm[..4], b"\0asm", "a WebAssembly module");
}

#[tokio::test]
async fn store_load_and_run_a_program() {
    let app = app("run");
    assert_eq!(
        call(&app, "PUT", "/api/programs/part1", PART, true).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        call(&app, "PUT", "/api/programs/..%2Fescape", PART, true)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    let (_, list, _) = call(&app, "GET", "/api/programs", "", false).await;
    assert_eq!(list[0]["name"], "part1");
    let (s, _, text) = call(&app, "GET", "/api/programs/part1", "", false).await;
    assert_eq!((s, text.as_str()), (StatusCode::OK, PART));

    let (s, doc, _) = call(&app, "POST", "/api/programs/part1/load", "", true).await;
    assert_eq!(s, StatusCode::OK, "{doc}");
    assert_eq!(doc["errors"], 0);
    assert_eq!(doc["lines"].as_array().unwrap().len(), 6);
    let segs = doc["backplot"]["segments"].as_array().unwrap();
    assert!(segs.iter().any(|s| s["kind"] == "arc"), "{segs:?}");
    let (_, loaded, _) = call(&app, "GET", "/api/program", "", false).await;
    assert_eq!(loaded["name"], "part1");

    // `run` is only taken from idle, so wait for homing to finish first.
    assert_eq!(
        call(&app, "POST", "/api/command", r#"{"kind":"home"}"#, true)
            .await
            .0,
        StatusCode::ACCEPTED
    );
    let mut homed = false;
    for _ in 0..1500 {
        let (_, st, _) = call(&app, "GET", "/api/state", "", false).await;
        if st["session"]["run"] == "idle" && st["session"]["dro"]["homed"] == true {
            homed = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(homed, "homing never finished");
    assert_eq!(
        call(&app, "POST", "/api/command", r#"{"kind":"run"}"#, true)
            .await
            .0,
        StatusCode::ACCEPTED
    );
    let mut ran = false;
    for _ in 0..200 {
        let (_, st, _) = call(&app, "GET", "/api/state", "", false).await;
        if st["session"]["currentLine"].as_i64().unwrap_or(-1) >= 0 {
            ran = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(ran, "the program never started on the virtual machine");
    let (s, body, _) = call(&app, "POST", "/api/programs/part1/load", "", true).await;
    assert_eq!(s, StatusCode::CONFLICT, "no reload while running: {body}");
    assert_eq!(
        call(&app, "POST", "/api/command", r#"{"kind":"abort"}"#, true)
            .await
            .0,
        StatusCode::ACCEPTED
    );
}

#[tokio::test]
async fn bad_input_is_refused_with_a_reason() {
    let app = app("bad");
    let (s, body, _) = call(&app, "POST", "/api/command", r#"{"kind":"launch"}"#, true).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("Command"));
    assert_eq!(
        call(&app, "POST", "/api/sim/fault", r#"{"kind":"fire"}"#, true)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        call(&app, "POST", "/api/sim/fault", r#"{"kind":"estop"}"#, true)
            .await
            .0,
        StatusCode::ACCEPTED
    );
    call(&app, "PUT", "/api/programs/broken", "G01 X\nM30\n", true).await;
    let (s, doc, _) = call(&app, "POST", "/api/programs/broken/load", "", true).await;
    if s == StatusCode::UNPROCESSABLE_ENTITY {
        assert!(doc["errors"].as_u64().unwrap() > 0);
    }
    let (s, body, _) = call(
        &app,
        "POST",
        "/api/capture/start",
        r#"{"baud":9600,"framing":"8N1"}"#,
        true,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert!(body["error"].as_str().unwrap().contains("--rs485"));
    assert_eq!(
        call(&app, "GET", "/api/captures/notes.txt", "", false)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    let (_, cap, _) = call(&app, "GET", "/api/capture", "", false).await;
    assert_eq!(cap["status"]["running"], false);
}

#[tokio::test]
async fn events_are_server_sent() {
    let app = app("sse");
    let res = app
        .oneshot(Request::get("/api/events").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers()[header::CONTENT_TYPE], "text/event-stream");
    let mut body = res.into_body();
    let frame = tokio::time::timeout(std::time::Duration::from_secs(5), body.frame())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let text = String::from_utf8_lossy(frame.data_ref().unwrap()).into_owned();
    assert!(text.starts_with("event: state"), "{text}");
}
