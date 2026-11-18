//! The UIs, embedded in the binary.
//!
//! * `/` and `/ui/*`: `ui/`, plain ES modules and the vendored RxJS UMD
//!   bundle, with no build step, so what is in the repository is exactly
//!   what is served.
//! * `/egui/`: the emco_lathe screen (`web/`, egui compiled to wasm32),
//!   embedded only when it has been built (`build.rs`); otherwise the page
//!   says how to build it.

use axum::extract::Path;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};

const FILES: &[(&str, &str, &[u8])] = &[
    (
        "index.html",
        "text/html; charset=utf-8",
        include_bytes!("../ui/index.html"),
    ),
    (
        "style.css",
        "text/css; charset=utf-8",
        include_bytes!("../ui/style.css"),
    ),
    (
        "app.js",
        "text/javascript; charset=utf-8",
        include_bytes!("../ui/app.js"),
    ),
    (
        "streams.js",
        "text/javascript; charset=utf-8",
        include_bytes!("../ui/streams.js"),
    ),
    (
        "toolpath.js",
        "text/javascript; charset=utf-8",
        include_bytes!("../ui/toolpath.js"),
    ),
    (
        "vendor/rxjs-7.8.2.umd.min.js",
        "text/javascript; charset=utf-8",
        include_bytes!("../ui/vendor/rxjs-7.8.2.umd.min.js"),
    ),
    (
        "vendor/rxjs-LICENSE.txt",
        "text/plain; charset=utf-8",
        include_bytes!("../ui/vendor/rxjs-LICENSE.txt"),
    ),
];

fn serve(path: &str) -> Response {
    match FILES.iter().find(|(p, _, _)| *p == path) {
        Some((_, ty, body)) => (
            [
                (header::CONTENT_TYPE, *ty),
                (header::CACHE_CONTROL, "no-cache"),
            ],
            *body,
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

pub async fn index() -> Response {
    serve("index.html")
}

pub async fn asset(Path(path): Path<String>) -> Response {
    serve(&path)
}

// -----------------------------------------------------------------------------
// The egui screen
// -----------------------------------------------------------------------------

#[cfg(egui_dist)]
const EGUI_FILES: &[(&str, &str, &[u8])] = &[
    (
        "index.html",
        "text/html; charset=utf-8",
        include_bytes!(concat!(env!("EMCO_EGUI_DIST_DIR"), "/index.html")),
    ),
    (
        "emco_egui.js",
        "text/javascript; charset=utf-8",
        include_bytes!(concat!(env!("EMCO_EGUI_DIST_DIR"), "/emco_egui.js")),
    ),
    (
        "emco_egui_bg.wasm",
        "application/wasm",
        include_bytes!(concat!(env!("EMCO_EGUI_DIST_DIR"), "/emco_egui_bg.wasm")),
    ),
];

/// True when this binary has the egui screen in it.
pub const EGUI_BUILT: bool = cfg!(egui_dist);

#[cfg(egui_dist)]
fn serve_egui(path: &str) -> Response {
    match EGUI_FILES.iter().find(|(p, _, _)| *p == path) {
        Some((_, ty, body)) => (
            [
                (header::CONTENT_TYPE, *ty),
                (header::CACHE_CONTROL, "no-cache"),
            ],
            *body,
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

#[cfg(not(egui_dist))]
fn serve_egui(path: &str) -> Response {
    if path != "index.html" {
        return StatusCode::NOT_FOUND.into_response();
    }
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        "<!doctype html><meta charset=utf-8><title>egui screen not built</title>\
         <p>The egui screen was not built into this binary. Run <code>kd240/web/build.sh</code>, \
         then rebuild <code>emco-kd240</code>. The RxJS UI is at <a href=\"/\">/</a>.</p>",
    )
        .into_response()
}

/// `/egui`: the page's assets are relative, so it lives at `/egui/`.
pub async fn egui_redirect() -> Response {
    (
        StatusCode::PERMANENT_REDIRECT,
        [(header::LOCATION, "/egui/")],
    )
        .into_response()
}

pub async fn egui_index() -> Response {
    serve_egui("index.html")
}

pub async fn egui_asset(Path(path): Path<String>) -> Response {
    serve_egui(&path)
}
