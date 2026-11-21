//! `ruru-mcp` — the whiteboard MCP service.
//!
//! Exposes the HTTP MCP endpoint an AI agent calls, an admin panel, and the whiteboard
//! pipeline (dewarp/crop/downscale). It is a **client** of `ruru-source` via `ruru-client`
//! and never touches the camera directly — dewarp, crop and calibration live here, never
//! in the source.
//!
//! Three trust boundaries, three mechanisms: `/mcp` takes a bearer key; the admin panel
//! takes a password behind an encrypted session cookie; the source socket's access
//! control is the filesystem's job, so this service is just a socket client there.

mod admin;
mod auth;
pub mod calibration;
pub mod captures;
pub mod config;
mod dewarp;
mod mcp;
mod pipeline;
pub mod secrets;
mod security;
mod web;

use anyhow::{Context, Result};
use axum::Router;
use axum::extract::FromRef;
use axum::middleware;
use axum::routing::{get, post, put};
use axum_extra::extract::cookie::Key;
use calibration::Calibration;
use config::Config;
use ruru_client::SourceClient;
use secrets::Secrets;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::{RwLock, Semaphore};
use tracing::{info, warn};

/// Shared handler state. Cheap to clone (everything inside is `Arc` or already cheap).
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    /// Secrets can change at runtime (key rotation, password change), hence the lock.
    pub secrets: Arc<RwLock<Secrets>>,
    /// Board calibration (corners + aspect); the admin edits it at runtime.
    pub calibration: Arc<RwLock<Calibration>>,
    /// Stable for the process lifetime — the cookie key does not rotate.
    pub cookie_key: Key,
    pub source: SourceClient,
    /// Retained-capture store, or `None` when capture retention is not configured.
    pub captures: Option<Arc<captures::CaptureStore>>,
    /// Throttles `/api/login` (per-IP lockout + bounded concurrent argon2 verifies).
    pub login_guard: Arc<security::LoginGuard>,
    /// Bounds concurrent image-processing tool calls on `/mcp` (dewarping 4K is CPU-heavy).
    pub capture_permits: Arc<Semaphore>,
}

impl AppState {
    /// Wire up state from its loaded parts. The cookie key comes from `secrets`.
    pub fn new(
        config: Config,
        secrets: Secrets,
        calibration: Calibration,
        source: SourceClient,
        captures: Option<Arc<captures::CaptureStore>>,
    ) -> Result<Self> {
        let cookie_key = secrets.cookie_key()?;
        let permits = config.server.max_concurrent_captures.max(1);
        Ok(Self {
            config: Arc::new(config),
            secrets: Arc::new(RwLock::new(secrets)),
            calibration: Arc::new(RwLock::new(calibration)),
            cookie_key,
            source,
            captures,
            login_guard: Arc::new(security::LoginGuard::default()),
            capture_permits: Arc::new(Semaphore::new(permits)),
        })
    }
}

// Lets `PrivateCookieJar` pull the encryption key out of state.
impl FromRef<AppState> for Key {
    fn from_ref(state: &AppState) -> Self {
        state.cookie_key.clone()
    }
}

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/mcp", post(mcp::handle))
        .route("/api/login", post(admin::login))
        .route("/api/logout", post(admin::logout))
        .route("/api/me", get(admin::me))
        .route("/api/status", get(admin::status))
        .route("/api/controls", get(admin::list_controls))
        .route("/api/controls/{name}", put(admin::set_control))
        .route("/api/enable", post(admin::set_enabled))
        .route("/api/keys/rotate", post(admin::rotate_key))
        .route("/api/password", post(admin::change_password))
        .route("/api/preview.mjpeg", get(admin::preview))
        .route(
            "/api/calibration",
            get(admin::get_calibration).put(admin::put_calibration),
        )
        .route("/api/dewarp-preview", post(admin::dewarp_preview))
        // Public, unauthenticated by design: an unguessable capability link the user can open
        // in a browser to see a retained capture. The id is the access control.
        .route("/captures/{file}", get(captures::serve))
        // The SPA is the fallback: anything not matched above is a static asset or a
        // client-side route.
        .fallback(web::static_handler)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            security::request_timeout,
        ))
        .layer(middleware::from_fn(security::security_headers))
        .with_state(state)
}

async fn healthz() -> &'static str {
    "ok"
}

/// Load secrets, wire state, and serve until a shutdown signal.
pub async fn serve(config: Config) -> Result<()> {
    let secrets = Secrets::load(&config.secrets.path)?;
    if !secrets.has_password() {
        warn!(
            "no admin password set — run `ruru-mcp set-password`; the admin panel is \
             locked until then"
        );
    }
    if !secrets.has_bearer() {
        warn!(
            "no bearer key minted — run `ruru-mcp mint-key`; /mcp rejects every request \
             until then"
        );
    }
    let calibration = Calibration::load_or_default(&config.calibration.path, &config.board)?;
    if calibration.is_calibrated() {
        info!(aspect = ?calibration.aspect, "board calibration loaded");
    } else {
        info!("board is not calibrated — captures fall back to no-warp until corners are set");
    }

    let captures = captures::CaptureStore::from_config(&config.captures)?.map(Arc::new);
    match &captures {
        Some(_) => info!(
            retain_days = config.captures.retain_days,
            retain_count = config.captures.retain_count,
            "capture retention on — snapshots served as links under /captures/"
        ),
        None => info!("capture retention off — set [captures] dir + public_base_url to enable"),
    }

    let source = SourceClient::new(&config.source.socket_path);
    let state = AppState::new(config.clone(), secrets, calibration, source, captures)?;

    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind(config.server.bind)
        .await
        .with_context(|| format!("binding {}", config.server.bind))?;
    info!(
        bind = %config.server.bind,
        source = %config.source.socket_path.display(),
        "ruru-mcp listening"
    );
    // Peer addresses feed the per-IP login throttle.
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .context("server error")?;
    Ok(())
}

async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = match signal(SignalKind::terminate()) {
        Ok(s) => s,
        Err(_) => {
            let _ = tokio::signal::ctrl_c().await;
            return;
        }
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
    info!("shutdown signal received");
}
