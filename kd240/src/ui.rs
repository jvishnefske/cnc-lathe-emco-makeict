//! The UI, embedded in the binary: `ui/` is plain ES modules and the
//! vendored RxJS UMD bundle, with no build step, so what is in the
//! repository is exactly what is served.

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
