//! The HTTP API and the embedded UI.
//!
//! Reads are open; anything that changes state (`POST`, `PUT`, `DELETE`)
//! needs `Authorization: Bearer <token>` unless the service was started with
//! `--no-auth`. The token is a lock against a passer-by on the shop LAN, not
//! a licence to operate remotely: the docs' rule stands that nobody runs the
//! machine without a person at it (`docs/30-serial-protocol.md` §5).
//!
//! | route | |
//! |---|---|
//! | `GET /` and `/ui/*` | the UI |
//! | `GET /egui/` | the egui emco_lathe screen (when built, `web/build.sh`) |
//! | `GET /api/profile` | the machine profile |
//! | `GET /api/state` | the latest snapshot (`session`, `sim`, `transport`) |
//! | `GET /api/events` | SSE: `state` snapshots and `core` session events |
//! | `GET /api/programs` | stored programs |
//! | `GET/PUT/DELETE /api/programs/{name}` | one program's text |
//! | `POST /api/programs/{name}/load` | parse, load into the session; diagnostics and backplot |
//! | `GET /api/program` | the loaded program: name, lines, diagnostics, backplot |
//! | `POST /api/command` | a `sender/INTERFACE.md` `Command` |
//! | `POST /api/sim/fault` | `{ "kind": "estop" }` etc. |
//! | `GET /api/capture`, `POST /api/capture/start|stop` | the listen-only capture |
//! | `GET /api/captures`, `GET /api/captures/{name}` | capture files |

use crate::capture::{self, Capture};
use crate::machine::Machine;
use crate::store::{Store, MAX_PROGRAM};
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, Request, State};
use axum::http::{header, HeaderMap, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use emco_sender_core::gcode::{Program, Severity};
use emco_sender_core::profile::MachineProfile;
use emco_sender_core::session::Command;
use serde::Deserialize;
use serde_json::{json, Value};
use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use tokio_stream::wrappers::{BroadcastStream, WatchStream};
use tokio_stream::{Stream, StreamExt};

/// Everything the handlers share.
pub struct App {
    pub machine: Machine,
    pub store: Store,
    pub capture: Capture,
    pub profile: MachineProfile,
    /// `None`: no auth (`--no-auth`).
    pub token: Option<String>,
    loaded: Mutex<Value>,
}

impl App {
    pub fn new(
        machine: Machine,
        store: Store,
        capture: Capture,
        profile: MachineProfile,
        token: Option<String>,
    ) -> App {
        App {
            machine,
            store,
            capture,
            profile,
            token,
            loaded: Mutex::new(Value::Null),
        }
    }
}

type Shared = Arc<App>;

fn err(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(json!({ "error": msg.into() }))).into_response()
}

fn io_err(e: std::io::Error) -> Response {
    let status = match e.kind() {
        std::io::ErrorKind::NotFound => StatusCode::NOT_FOUND,
        std::io::ErrorKind::InvalidInput => StatusCode::BAD_REQUEST,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    err(status, e.to_string())
}

/// Compares in constant time for equal lengths.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn auth(State(app): State<Shared>, req: Request, next: Next) -> Response {
    let changes = !matches!(*req.method(), Method::GET | Method::HEAD);
    if let (true, Some(token)) = (changes, &app.token) {
        let given = req
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "));
        if !given.is_some_and(|g| same(g.as_bytes(), token.as_bytes())) {
            return err(StatusCode::UNAUTHORIZED, "missing or wrong bearer token");
        }
    }
    next.run(req).await
}

/// The router.
pub fn router(app: Shared) -> Router {
    Router::new()
        .route("/", get(crate::ui::index))
        .route("/ui/{*path}", get(crate::ui::asset))
        .route("/egui", get(crate::ui::egui_redirect))
        .route("/egui/", get(crate::ui::egui_index))
        .route("/egui/{*path}", get(crate::ui::egui_asset))
        .route("/api/profile", get(profile))
        .route("/api/state", get(state))
        .route("/api/events", get(events))
        .route("/api/programs", get(list_programs))
        .route(
            "/api/programs/{name}",
            get(get_program).put(put_program).delete(delete_program),
        )
        .route("/api/programs/{name}/load", post(load_program))
        .route("/api/program", get(loaded_program))
        .route("/api/command", post(command))
        .route("/api/sim/fault", post(fault))
        .route("/api/capture", get(capture_status))
        .route("/api/capture/start", post(capture_start))
        .route("/api/capture/stop", post(capture_stop))
        .route("/api/captures", get(list_captures))
        .route("/api/captures/{name}", get(get_capture))
        .layer(DefaultBodyLimit::max(MAX_PROGRAM + 1024))
        .layer(middleware::from_fn_with_state(app.clone(), auth))
        .with_state(app)
}

async fn profile(State(app): State<Shared>) -> Json<MachineProfile> {
    Json(app.profile.clone())
}

async fn state(State(app): State<Shared>) -> Json<Value> {
    Json(app.machine.state.borrow().clone())
}

async fn events(State(app): State<Shared>) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let states = WatchStream::new(app.machine.state.clone())
        .map(|v| Ok(Event::default().event("state").data(v.to_string())));
    let core = BroadcastStream::new(app.machine.events.subscribe()).map(|r| {
        Ok(match r {
            Ok(v) => Event::default().event("core").data(v.to_string()),
            // A slow client: say how many events it missed, keep going.
            Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(n)) => {
                Event::default().event("lagged").data(n.to_string())
            }
        })
    });
    Sse::new(states.merge(core)).keep_alive(KeepAlive::default())
}

async fn list_programs(State(app): State<Shared>) -> Response {
    match app.store.list() {
        Ok(v) => Json(v).into_response(),
        Err(e) => io_err(e),
    }
}

async fn get_program(State(app): State<Shared>, Path(name): Path<String>) -> Response {
    match app.store.read(&name) {
        Ok(text) => ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], text).into_response(),
        Err(e) => io_err(e),
    }
}

async fn put_program(State(app): State<Shared>, Path(name): Path<String>, body: Bytes) -> Response {
    if std::str::from_utf8(&body).is_err() {
        return err(StatusCode::BAD_REQUEST, "a program must be UTF-8 text");
    }
    match app.store.write(&name, &body) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => io_err(e),
    }
}

async fn delete_program(State(app): State<Shared>, Path(name): Path<String>) -> Response {
    match app.store.delete(&name) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => io_err(e),
    }
}

/// The loaded-program document: what the UI's program pane and toolpath show.
fn describe(name: &str, p: &Program) -> Value {
    let lines: Vec<String> = (0..p.line_count()).map(|i| p.line(i)).collect();
    let errors = p
        .diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .count();
    json!({
        "name": name,
        "lines": lines,
        "diagnostics": p.diagnostics,
        "errors": errors,
        "backplot": emco_sender_core::geom::backplot(p, None),
    })
}

async fn load_program(State(app): State<Shared>, Path(name): Path<String>) -> Response {
    if app.machine.busy() {
        return err(
            StatusCode::CONFLICT,
            format!(
                "the machine is {}: abort or finish first",
                app.machine.run_state()
            ),
        );
    }
    let bytes = match app.store.read(&name) {
        Ok(b) => b,
        Err(e) => return io_err(e),
    };
    let Ok(text) = String::from_utf8(bytes) else {
        return err(StatusCode::BAD_REQUEST, "program is not UTF-8");
    };
    let p = Program::parse(&text);
    let doc = describe(&name, &p);
    if doc["errors"].as_u64().unwrap_or(0) > 0 {
        return (StatusCode::UNPROCESSABLE_ENTITY, Json(doc)).into_response();
    }
    app.machine.load(p);
    if let Ok(mut l) = app.loaded.lock() {
        *l = doc.clone();
    }
    Json(doc).into_response()
}

async fn loaded_program(State(app): State<Shared>) -> Json<Value> {
    Json(app.loaded.lock().map(|l| l.clone()).unwrap_or(Value::Null))
}

async fn command(State(app): State<Shared>, body: Bytes) -> Response {
    let cmd: Command = match serde_json::from_slice(&body) {
        Ok(c) => c,
        Err(e) => return err(StatusCode::BAD_REQUEST, format!("not a Command: {e}")),
    };
    app.machine.command(cmd);
    StatusCode::ACCEPTED.into_response()
}

#[derive(Deserialize)]
struct Fault {
    kind: String,
}

async fn fault(State(app): State<Shared>, Json(f): Json<Fault>) -> Response {
    if app.machine.fault(&f.kind) {
        StatusCode::ACCEPTED.into_response()
    } else {
        err(
            StatusCode::BAD_REQUEST,
            format!("fault must be one of {:?}", crate::machine::FAULTS),
        )
    }
}

async fn capture_status(State(app): State<Shared>) -> Json<Value> {
    Json(json!({
        "status": app.capture.status(),
        "baudRates": capture::BAUD_RATES,
    }))
}

async fn capture_start(State(app): State<Shared>, Json(req): Json<capture::Request>) -> Response {
    let cap = app.clone();
    match tokio::task::spawn_blocking(move || cap.capture.start(req)).await {
        Ok(Ok(s)) => Json(s).into_response(),
        Ok(Err(e)) => err(StatusCode::CONFLICT, e),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn capture_stop(State(app): State<Shared>) -> Response {
    let cap = app.clone();
    match tokio::task::spawn_blocking(move || cap.capture.stop()).await {
        Ok(s) => Json(s).into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn list_captures(State(app): State<Shared>) -> Response {
    match app.capture.list() {
        Ok(v) => Json(
            v.into_iter()
                .map(|(name, bytes)| json!({ "name": name, "bytes": bytes }))
                .collect::<Vec<_>>(),
        )
        .into_response(),
        Err(e) => io_err(e),
    }
}

async fn get_capture(State(app): State<Shared>, Path(name): Path<String>) -> Response {
    if !capture::valid_capture_name(&name) {
        return err(StatusCode::BAD_REQUEST, "not a capture name");
    }
    match std::fs::read(app.capture.dir().join(&name)) {
        Ok(b) => {
            let mut h = HeaderMap::new();
            h.insert(
                header::CONTENT_TYPE,
                "application/octet-stream".parse().expect("static header"),
            );
            if let Ok(v) = format!("attachment; filename=\"{name}\"").parse() {
                h.insert(header::CONTENT_DISPOSITION, v);
            }
            (h, b).into_response()
        }
        Err(e) => io_err(e),
    }
}
