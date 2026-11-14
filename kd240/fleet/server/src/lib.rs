//! The KD240 fleet server.
//!
//! It keeps each device's latest telemetry and a bounded history, and it
//! stores and relays the build host's signed orders (update manifests and
//! stage/switch/reboot commands). It never holds the signing key, so a
//! compromised server can withhold or replay orders but cannot make one:
//! the board verifies every signature, its own device id and a persisted
//! sequence number. It also serves a static Nix binary cache at `/cache/`,
//! whose integrity rests on Nix's own signatures, not on this server.
//!
//! | Route | Who | |
//! |---|---|---|
//! | `POST /api/v1/devices/{id}/poll` | device | telemetry in, orders newer than `last_seq` out |
//! | `PUT /api/v1/devices/{id}/manifest` | admin | publish a signed manifest |
//! | `POST /api/v1/devices/{id}/commands` | admin | queue a signed command |
//! | `DELETE /api/v1/devices/{id}/commands` | admin | drop the queued commands |
//! | `GET /api/v1/devices` | admin | every device's summary |
//! | `GET /api/v1/devices/{id}` | admin | one device, with its history |
//! | `GET /cache/…` | anyone | the binary cache (`nix-cache-info`, `*.narinfo`, `nar/`) |
//! | `GET /healthz` | anyone | `ok` |
//!
//! Tokens are bearer tokens, compared in constant time (`emco_fleet_proto::token`).
//! State is one JSON file, `<data>/state.json`, written atomically.
#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::extract::{DefaultBodyLimit, Path as UrlPath, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use emco_fleet_proto::keys::{Envelope, PublicKey};
use emco_fleet_proto::msg::{
    DeviceDetail, DeviceSummary, Order, PollRequest, PollResponse, Sample,
};
use emco_fleet_proto::token::TokenHash;
use emco_fleet_proto::{unix_now, valid_name};
use serde::{Deserialize, Serialize};

/// Every option is also an environment variable, for the container.
#[derive(clap::Parser, Clone, Debug)]
#[command(
    name = "emco-fleet-server",
    version,
    about = "KD240 fleet telemetry and signed-update relay"
)]
pub struct Config {
    #[arg(long, env = "FLEET_LISTEN", default_value = "0.0.0.0:8080")]
    pub listen: String,
    /// Directory for state.json.
    #[arg(long, env = "FLEET_DATA_DIR", default_value = "/data")]
    pub data_dir: PathBuf,
    /// The static binary cache served at /cache/ (made with `nix copy --to file://…`).
    #[arg(long, env = "FLEET_CACHE_DIR", default_value = "/cache")]
    pub cache_dir: PathBuf,
    /// One admin token per line (raw, or `sha256:<hex>`); `#` comments.
    #[arg(long, env = "FLEET_ADMIN_TOKEN_FILE")]
    pub admin_token_file: PathBuf,
    /// `<device-id> <token>` per line (token raw or `sha256:<hex>`).
    #[arg(long, env = "FLEET_DEVICE_TOKENS_FILE")]
    pub device_tokens_file: PathBuf,
    /// Optional: the build host's public keys, one per line. With them the
    /// server refuses unsigned or mis-signed orders at publish time. The
    /// board checks regardless; this only catches mistakes early.
    #[arg(long, env = "FLEET_TRUSTED_KEYS_FILE")]
    pub trusted_keys_file: Option<PathBuf>,
    /// Samples of history kept per device.
    #[arg(long, env = "FLEET_HISTORY", default_value_t = 500)]
    pub history: usize,
    /// Minimum seconds between history samples (the latest is always kept).
    #[arg(long, env = "FLEET_HISTORY_EVERY_S", default_value_t = 300)]
    pub history_every_s: u64,
    /// Queued commands per device.
    #[arg(long, env = "FLEET_MAX_PENDING", default_value_t = 16)]
    pub max_pending: usize,
}

#[derive(Serialize, Deserialize, Default, Debug)]
struct Data {
    devices: BTreeMap<String, DeviceData>,
}

#[derive(Serialize, Deserialize, Default, Debug)]
struct DeviceData {
    latest: Option<Sample>,
    #[serde(default)]
    history: VecDeque<Sample>,
    manifest: Option<Envelope>,
    #[serde(default)]
    commands: Vec<Envelope>,
}

pub struct App {
    cfg: Config,
    admin: Vec<TokenHash>,
    devices: Vec<(String, TokenHash)>,
    trusted: Vec<PublicKey>,
    data: Mutex<Data>,
}

pub type Shared = Arc<App>;

fn lines(path: &Path) -> Result<Vec<String>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(String::from)
        .collect())
}

impl App {
    pub fn open(cfg: Config) -> Result<Shared, String> {
        let admin = lines(&cfg.admin_token_file)?
            .iter()
            .map(|l| TokenHash::parse(l))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("{}: {e}", cfg.admin_token_file.display()))?;
        if admin.is_empty() {
            return Err(format!(
                "{}: no admin token",
                cfg.admin_token_file.display()
            ));
        }
        let mut devices = Vec::new();
        for l in lines(&cfg.device_tokens_file)? {
            let (id, tok) = l.split_once(char::is_whitespace).ok_or_else(|| {
                format!(
                    "{}: want `<device> <token>`",
                    cfg.device_tokens_file.display()
                )
            })?;
            if !valid_name(id) {
                return Err(format!(
                    "{}: bad device id {id:?}",
                    cfg.device_tokens_file.display()
                ));
            }
            let h = TokenHash::parse(tok)
                .map_err(|e| format!("{}: {id}: {e}", cfg.device_tokens_file.display()))?;
            devices.push((id.to_string(), h));
        }
        let trusted = match &cfg.trusted_keys_file {
            Some(p) => lines(p)?
                .iter()
                .map(|l| PublicKey::parse(l))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| format!("{}: {e}", p.display()))?,
            None => Vec::new(),
        };
        std::fs::create_dir_all(&cfg.data_dir)
            .map_err(|e| format!("{}: {e}", cfg.data_dir.display()))?;
        let state = cfg.data_dir.join("state.json");
        let data = match std::fs::read(&state) {
            Ok(b) => serde_json::from_slice(&b).map_err(|e| format!("{}: {e}", state.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Data::default(),
            Err(e) => return Err(format!("{}: {e}", state.display())),
        };
        Ok(Arc::new(App {
            cfg,
            admin,
            devices,
            trusted,
            data: Mutex::new(data),
        }))
    }

    fn known(&self, id: &str) -> bool {
        self.devices.iter().any(|(d, _)| d == id)
    }

    fn save(&self, data: &Data) -> Result<(), Error> {
        let path = self.cfg.data_dir.join("state.json");
        let tmp = self.cfg.data_dir.join("state.json.tmp");
        let bytes = serde_json::to_vec(data).map_err(|e| Error::internal(e.to_string()))?;
        std::fs::write(&tmp, bytes)
            .map_err(|e| Error::internal(format!("{}: {e}", tmp.display())))?;
        std::fs::rename(&tmp, &path)
            .map_err(|e| Error::internal(format!("{}: {e}", path.display())))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Data> {
        // A panic while holding the lock leaves plain data behind; carry on.
        self.data.lock().unwrap_or_else(|p| p.into_inner())
    }
}

// ---------------------------------------------------------------- errors

#[derive(Debug)]
pub struct Error(StatusCode, String);

impl Error {
    fn new(code: StatusCode, msg: impl Into<String>) -> Self {
        Error(code, msg.into())
    }
    fn internal(msg: String) -> Self {
        eprintln!("emco-fleet-server: {msg}");
        Error(StatusCode::INTERNAL_SERVER_ERROR, "internal error".into())
    }
    fn unauthorized() -> Self {
        Error(StatusCode::UNAUTHORIZED, "unauthorized".into())
    }
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        (self.0, self.1).into_response()
    }
}

// ------------------------------------------------------------------ auth

fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::trim)
}

fn require_admin(app: &App, headers: &HeaderMap) -> Result<(), Error> {
    let tok = bearer(headers).ok_or_else(Error::unauthorized)?;
    // Check every entry, so the time taken does not say which one matched.
    let ok = app.admin.iter().fold(false, |acc, h| h.matches(tok) | acc);
    ok.then_some(()).ok_or_else(Error::unauthorized)
}

fn require_device(app: &App, headers: &HeaderMap, id: &str) -> Result<(), Error> {
    let tok = bearer(headers).ok_or_else(Error::unauthorized)?;
    let ok = app
        .devices
        .iter()
        .fold(false, |acc, (d, h)| (h.matches(tok) & (d == id)) | acc);
    ok.then_some(()).ok_or_else(Error::unauthorized)
}

// --------------------------------------------------------------- routes

pub fn router(app: Shared) -> Router {
    let cache = tower_http::services::ServeDir::new(&app.cfg.cache_dir);
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/api/v1/devices", get(list_devices))
        .route("/api/v1/devices/{id}", get(device_detail))
        .route("/api/v1/devices/{id}/poll", post(poll))
        .route("/api/v1/devices/{id}/manifest", put(put_manifest))
        .route(
            "/api/v1/devices/{id}/commands",
            post(post_command).delete(clear_commands),
        )
        .nest_service("/cache", cache)
        .layer(DefaultBodyLimit::max(64 * 1024))
        .with_state(app)
}

fn summary(id: &str, d: Option<&DeviceData>) -> DeviceSummary {
    DeviceSummary {
        device: id.to_string(),
        latest: d.and_then(|d| d.latest.clone()),
        manifest: d.and_then(|d| d.manifest.clone()),
        pending_commands: d.map(|d| d.commands.clone()).unwrap_or_default(),
    }
}

async fn list_devices(
    State(app): State<Shared>,
    headers: HeaderMap,
) -> Result<Json<Vec<DeviceSummary>>, Error> {
    require_admin(&app, &headers)?;
    let data = app.lock();
    let ids: BTreeSet<&str> = app
        .devices
        .iter()
        .map(|(d, _)| d.as_str())
        .chain(data.devices.keys().map(String::as_str))
        .collect();
    Ok(Json(
        ids.into_iter()
            .map(|id| summary(id, data.devices.get(id)))
            .collect(),
    ))
}

async fn device_detail(
    State(app): State<Shared>,
    headers: HeaderMap,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<DeviceDetail>, Error> {
    require_admin(&app, &headers)?;
    let data = app.lock();
    if !app.known(&id) && !data.devices.contains_key(&id) {
        return Err(Error::new(StatusCode::NOT_FOUND, "no such device"));
    }
    let d = data.devices.get(&id);
    Ok(Json(DeviceDetail {
        summary: summary(&id, d),
        history: d
            .map(|d| d.history.iter().cloned().collect())
            .unwrap_or_default(),
    }))
}

async fn poll(
    State(app): State<Shared>,
    headers: HeaderMap,
    UrlPath(id): UrlPath<String>,
    body: axum::body::Bytes,
) -> Result<Json<PollResponse>, Error> {
    require_device(&app, &headers, &id)?;
    let req: PollRequest = serde_json::from_slice(&body)
        .map_err(|e| Error::new(StatusCode::BAD_REQUEST, format!("bad poll: {e}")))?;
    let now = unix_now();
    let mut data = app.lock();
    let d = data.devices.entry(id.clone()).or_default();
    let sample = Sample {
        at: now,
        last_seq: req.last_seq,
        telemetry: req.telemetry,
    };
    let mut dirty = false;
    let due = d
        .history
        .back()
        .map(|s| now.saturating_sub(s.at) >= app.cfg.history_every_s)
        .unwrap_or(true);
    if due && app.cfg.history > 0 {
        d.history.push_back(sample.clone());
        while d.history.len() > app.cfg.history {
            d.history.pop_front();
        }
        dirty = true;
    }
    d.latest = Some(sample);
    // The board has accepted (or consumed) everything up to last_seq.
    let before = d.commands.len();
    d.commands.retain(|e| {
        e.unverified_order()
            .map(|o| o.seq() > req.last_seq)
            .unwrap_or(false)
    });
    dirty |= d.commands.len() != before;
    let manifest = d.manifest.clone().filter(|m| {
        m.unverified_order()
            .map(|o| o.seq() > req.last_seq)
            .unwrap_or(false)
    });
    let resp = PollResponse {
        manifest,
        commands: d.commands.clone(),
    };
    if dirty {
        app.save(&data)?;
    }
    Ok(Json(resp))
}

/// The order in a published envelope, checked as far as the server can.
fn check_publish(app: &App, id: &str, env: &Envelope) -> Result<Order, Error> {
    if !app.known(id) {
        return Err(Error::new(StatusCode::NOT_FOUND, "no such device"));
    }
    let order = if app.trusted.is_empty() {
        env.unverified_order()
            .map_err(|e| Error::new(StatusCode::BAD_REQUEST, format!("bad order: {e}")))?
    } else {
        env.verify(&app.trusted)
            .map_err(|e| Error::new(StatusCode::BAD_REQUEST, e.to_string()))?
    };
    if order.device() != id {
        return Err(Error::new(
            StatusCode::BAD_REQUEST,
            format!("order is for {:?}", order.device()),
        ));
    }
    Ok(order)
}

fn last_seq_of(d: &DeviceData) -> u64 {
    d.latest.as_ref().map(|s| s.last_seq).unwrap_or(0)
}

async fn put_manifest(
    State(app): State<Shared>,
    headers: HeaderMap,
    UrlPath(id): UrlPath<String>,
    Json(env): Json<Envelope>,
) -> Result<StatusCode, Error> {
    require_admin(&app, &headers)?;
    let order = check_publish(&app, &id, &env)?;
    if !matches!(order, Order::Manifest(_)) {
        return Err(Error::new(StatusCode::BAD_REQUEST, "not a manifest"));
    }
    let mut data = app.lock();
    let d = data.devices.entry(id).or_default();
    let current = d
        .manifest
        .as_ref()
        .and_then(|m| m.unverified_order().ok())
        .map(|o| o.seq())
        .unwrap_or(0);
    let floor = current.max(last_seq_of(d));
    if order.seq() <= floor {
        return Err(Error::new(
            StatusCode::CONFLICT,
            format!("seq {} is not above {floor}", order.seq()),
        ));
    }
    d.manifest = Some(env);
    app.save(&data)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn post_command(
    State(app): State<Shared>,
    headers: HeaderMap,
    UrlPath(id): UrlPath<String>,
    Json(env): Json<Envelope>,
) -> Result<StatusCode, Error> {
    require_admin(&app, &headers)?;
    let order = check_publish(&app, &id, &env)?;
    if !matches!(order, Order::Command(_)) {
        return Err(Error::new(StatusCode::BAD_REQUEST, "not a command"));
    }
    let mut data = app.lock();
    let d = data.devices.entry(id).or_default();
    let floor = last_seq_of(d);
    if order.seq() <= floor {
        return Err(Error::new(
            StatusCode::CONFLICT,
            format!("seq {} is not above {floor}", order.seq()),
        ));
    }
    if d.commands.iter().any(|e| {
        e.unverified_order()
            .map(|o| o.seq() == order.seq())
            .unwrap_or(false)
    }) {
        return Err(Error::new(
            StatusCode::CONFLICT,
            format!("seq {} is already queued", order.seq()),
        ));
    }
    if d.commands.len() >= app.cfg.max_pending {
        return Err(Error::new(
            StatusCode::TOO_MANY_REQUESTS,
            "too many queued commands",
        ));
    }
    d.commands.push(env);
    d.commands
        .sort_by_key(|e| e.unverified_order().map(|o| o.seq()).unwrap_or(0));
    app.save(&data)?;
    Ok(StatusCode::ACCEPTED)
}

async fn clear_commands(
    State(app): State<Shared>,
    headers: HeaderMap,
    UrlPath(id): UrlPath<String>,
) -> Result<StatusCode, Error> {
    require_admin(&app, &headers)?;
    let mut data = app.lock();
    if let Some(d) = data.devices.get_mut(&id) {
        d.commands.clear();
        app.save(&data)?;
    }
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests;
