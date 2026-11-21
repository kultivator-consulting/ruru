//! Serves the embedded Vite+Vue SPA.
//!
//! `web/dist` is baked into the binary at release time via `rust-embed` (in debug it is
//! read from disk, so a `vite build` shows up on refresh without a Rust rebuild). Unknown
//! non-asset paths fall back to `index.html` so client-side routing (Vue Router history
//! mode) works on deep links.

use axum::body::Body;
use axum::http::{StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "../../web/dist"]
struct Assets;

pub async fn static_handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');

    // API and MCP live on their own routes; an unmatched request under those prefixes is a
    // genuine 404, not a client-side route to paper over with index.html.
    if path == "mcp" || path.starts_with("api/") {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }

    let path = if path.is_empty() { "index.html" } else { path };

    if let Some(content) = Assets::get(path) {
        return serve(path, content.data);
    }

    // SPA fallback for deep links.
    match Assets::get("index.html") {
        Some(content) => serve("index.html", content.data),
        None => (
            StatusCode::NOT_FOUND,
            "SPA not built — run `make web-build` (or `vite build`) to populate web/dist",
        )
            .into_response(),
    }
}

fn serve(path: &str, data: std::borrow::Cow<'static, [u8]>) -> Response {
    let mime = mime_for(path);
    // Hashed asset filenames are safe to cache hard; index.html must not be, or a deploy
    // won't be picked up.
    let cache = if path == "index.html" {
        "no-cache"
    } else {
        "public, max-age=31536000, immutable"
    };
    (
        [
            (header::CONTENT_TYPE, mime),
            (header::CACHE_CONTROL, cache.to_string()),
        ],
        Body::from(data.into_owned()),
    )
        .into_response()
}

fn mime_for(path: &str) -> String {
    mime_guess::from_path(path)
        .first_or_octet_stream()
        .to_string()
}
