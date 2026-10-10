//! The dashboard, served by the hub at `/ui` to any browser on this machine. The built frontend (`frontend/dist`,
//! made by `npm run build`) is embedded in the binary when it is built; a folder named by `MM_UI_DIR`, or `ui`
//! beside the executable or in the home folder, takes precedence (so the page can be replaced without a rebuild).
//! The files hold no secrets: the page asks for the owner's token and keeps it in the tab's sessionStorage.

use std::path::{Component, Path, PathBuf};

use axum::extract::{Path as UrlPath, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::Router;
use rust_embed::RustEmbed;

use crate::hub::Hub;

#[derive(RustEmbed)]
#[folder = "$CARGO_MANIFEST_DIR/../frontend/dist"]
#[allow_missing = true]
struct Dist;

fn mime(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// Folders that may hold a replacement build, in order.
fn override_dirs(home: &Path) -> Vec<PathBuf> {
    let mut v = vec![];
    if let Some(d) = std::env::var_os("MM_UI_DIR") {
        v.push(PathBuf::from(d));
    }
    if let Some(d) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|p| p.join("ui")))
    {
        v.push(d);
    }
    v.push(home.join("ui"));
    v
}

fn load(home: &Path, rel: &str) -> Option<Vec<u8>> {
    let clean = Path::new(rel);
    if clean.components().any(|c| !matches!(c, Component::Normal(_))) {
        return None;
    }
    for d in override_dirs(home) {
        if d.join("index.html").is_file() {
            return std::fs::read(d.join(clean)).ok();
        }
    }
    Dist::get(rel).map(|f| f.data.into_owned())
}

fn serve_file(home: &Path, rel: &str) -> Response {
    let rel = if rel.is_empty() { "index.html" } else { rel };
    match load(home, rel) {
        Some(bytes) => {
            let cache = if rel.starts_with("assets/") {
                "public, max-age=31536000, immutable"
            } else {
                "no-cache"
            };
            (
                [
                    (header::CONTENT_TYPE, mime(rel)),
                    (header::CACHE_CONTROL, cache),
                    (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
                    (header::REFERRER_POLICY, "no-referrer"),
                    (
                        header::CONTENT_SECURITY_POLICY,
                        "default-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'",
                    ),
                ],
                bytes,
            )
                .into_response()
        }
        None if rel == "index.html" => (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            "The dashboard is not built into this binary. Run `npm ci && npm run build` in frontend/ and rebuild, \
             or put the built files in a folder named ui beside the executable (or set MM_UI_DIR).",
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn index(State(hub): State<Hub>) -> Response {
    serve_file(&hub.engine.home, "")
}

async fn file(State(hub): State<Hub>, UrlPath(rel): UrlPath<String>) -> Response {
    serve_file(&hub.engine.home, &rel)
}

/// `/ui` routes: open (static files only; the API behind them wants the token).
pub fn routes() -> Router<Hub> {
    Router::new()
        .route("/ui", get(|| async { Redirect::permanent("/ui/") }))
        .route("/ui/", get(index))
        .route("/ui/*rel", get(file))
}
