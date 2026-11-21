//! The admin API under `/api`.
//!
//! Boundary: an argon2id password logs in and mints an encrypted session cookie
//! (`PrivateCookieJar`); every other route requires that cookie via the [`Session`] guard.
//! The camera-control and preview routes are thin proxies to `ruru-source` through
//! `ruru-client` — this service never touches the device.

use crate::AppState;
use crate::calibration::Calibration;
use crate::pipeline;
use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::{ConnectInfo, FromRef, FromRequestParts, Path, State};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use axum_extra::extract::PrivateCookieJar;
use axum_extra::extract::cookie::{Cookie, Key, SameSite};
use ruru_client::ClientError;
use serde::Deserialize;
use serde_json::json;
use std::net::SocketAddr;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{info, warn};

use crate::security::LoginCheck;

const SESSION_COOKIE: &str = "ruru_session";

/// Extractor that admits a request only if it carries a valid, current session cookie.
///
/// The cookie is encrypted and authenticated with the server key, so its contents can't be
/// forged. It carries `v1.<generation>.<issued-at>`; a session is valid only while its
/// generation matches the secrets' current one (bumped on every password change) and it is
/// younger than `[server].session_max_age_secs`. That bounds the life of a stolen cookie.
pub struct Session;

impl FromRequestParts<AppState> for Session {
    type Rejection = StatusCode;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let jar = PrivateCookieJar::<Key>::from_request_parts(parts, &Key::from_ref(state))
            .await
            .expect("PrivateCookieJar extraction is infallible");
        let Some(cookie) = jar.get(SESSION_COOKIE) else {
            return Err(StatusCode::UNAUTHORIZED);
        };
        let Some((generation, issued_at)) = parse_session(cookie.value()) else {
            return Err(StatusCode::UNAUTHORIZED);
        };
        let current = state.secrets.read().await.session_generation();
        let age = now_secs().saturating_sub(issued_at);
        if generation == current && age <= state.config.server.session_max_age_secs {
            Ok(Session)
        } else {
            Err(StatusCode::UNAUTHORIZED)
        }
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn parse_session(value: &str) -> Option<(u64, u64)> {
    let mut parts = value.split('.');
    if parts.next()? != "v1" {
        return None;
    }
    let generation = parts.next()?.parse().ok()?;
    let issued_at = parts.next()?.parse().ok()?;
    parts.next().is_none().then_some((generation, issued_at))
}

fn session_cookie(generation: u64) -> Cookie<'static> {
    Cookie::build((SESSION_COOKIE, format!("v1.{generation}.{}", now_secs())))
        .path("/")
        .http_only(true)
        // Strict: the admin panel is never embedded or linked cross-site.
        .same_site(SameSite::Strict)
        // `Secure` is intentionally omitted — LAN-only plain HTTP can't set it, and a
        // Secure cookie would simply never be sent back. The firewall is the boundary.
        // No Max-Age: a browser-session cookie, cleared when the browser closes.
        .build()
}

#[derive(Deserialize)]
pub struct LoginRequest {
    password: String,
}

pub async fn login(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    jar: PrivateCookieJar,
    Json(body): Json<LoginRequest>,
) -> Result<(PrivateCookieJar, Json<serde_json::Value>), Response> {
    let ip = peer.ip();
    let _permit = match state.login_guard.check(ip) {
        LoginCheck::Allowed(permit) => permit,
        LoginCheck::LockedOut(retry) => {
            warn!(%ip, "admin login refused: too many failed attempts");
            return Err(too_many_requests(retry.as_secs().max(1)));
        }
        LoginCheck::Busy => return Err(too_many_requests(1)),
    };

    // argon2id is deliberately slow; run it off the async workers.
    let secrets = state.secrets.read().await.clone();
    let ok = tokio::task::spawn_blocking(move || secrets.verify_password(&body.password))
        .await
        .unwrap_or(false);

    if ok {
        state.login_guard.record_success(ip);
        info!(%ip, "admin login");
        let generation = state.secrets.read().await.session_generation();
        Ok((
            jar.add(session_cookie(generation)),
            Json(json!({"authenticated": true})),
        ))
    } else {
        state.login_guard.record_failure(ip);
        warn!(%ip, "failed admin login");
        Err(StatusCode::UNAUTHORIZED.into_response())
    }
}

fn too_many_requests(retry_after_secs: u64) -> Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        [(
            axum::http::header::RETRY_AFTER,
            retry_after_secs.to_string(),
        )],
        Json(json!({ "error": "too many login attempts; try again later" })),
    )
        .into_response()
}

pub async fn logout(jar: PrivateCookieJar) -> (PrivateCookieJar, Json<serde_json::Value>) {
    // The removal cookie must carry the same Path as the one we set, or the browser
    // (and reqwest's store) won't consider it a match and the session survives logout.
    (
        jar.remove(Cookie::build((SESSION_COOKIE, "")).path("/").build()),
        Json(json!({"authenticated": false})),
    )
}

/// Cheap "am I logged in?" probe the SPA calls on boot. The [`Session`] guard turns a
/// missing cookie into 401, which the SPA reads as "show the login page".
pub async fn me(_session: Session) -> Json<serde_json::Value> {
    Json(json!({"authenticated": true}))
}

pub async fn status(
    _session: Session,
    State(state): State<AppState>,
) -> Result<Response, ApiError> {
    let status = state.source.status().await?;
    Ok(Json(status).into_response())
}

pub async fn list_controls(
    _session: Session,
    State(state): State<AppState>,
) -> Result<Response, ApiError> {
    let controls = state.source.list_controls().await?;
    Ok(Json(controls).into_response())
}

#[derive(Deserialize)]
pub struct SetControlRequest {
    value: i64,
}

pub async fn set_control(
    _session: Session,
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(body): Json<SetControlRequest>,
) -> Result<Response, ApiError> {
    // Persist by default: setting a control in the admin panel is defining a durable default.
    // ruru-source records it in its control-overrides state file and re-applies it on every
    // open, so it survives a restart. ruru-mcp only drives the change — the device state, and
    // its persistence, are the source's to own.
    let control = state.source.set_control(&name, body.value, true).await?;
    Ok(Json(control).into_response())
}

#[derive(Deserialize)]
pub struct EnableRequest {
    enabled: bool,
}

pub async fn set_enabled(
    _session: Session,
    State(state): State<AppState>,
    Json(body): Json<EnableRequest>,
) -> Result<Response, ApiError> {
    let status = state.source.set_enabled(body.enabled).await?;
    Ok(Json(status).into_response())
}

/// Rotate the bearer key. Returns the new key **once**; only its hash is persisted.
pub async fn rotate_key(
    _session: Session,
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let token = {
        let mut secrets = state.secrets.write().await;
        let token = secrets.mint_bearer();
        secrets
            .save(&state.config.secrets.path)
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        token
    };
    info!("bearer key rotated via admin");
    Ok(Json(json!({ "key": token })))
}

#[derive(Deserialize)]
pub struct ChangePasswordRequest {
    current: String,
    new: String,
}

/// Change the admin password. Requires the session cookie **and** the current password;
/// updates the argon2id hash in the live in-memory secrets and the `0600` file, so it takes
/// effect on the next login. Unlike the CLI `set-password`, this reaches the *running*
/// server's secrets, so no restart is needed.
///
/// Every other session is revoked (the session generation bumps); the caller gets a fresh
/// cookie so they stay signed in.
pub async fn change_password(
    _session: Session,
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Json(body): Json<ChangePasswordRequest>,
) -> Result<(PrivateCookieJar, Json<serde_json::Value>), ApiError> {
    if body.new.is_empty() {
        return Err(ApiError::BadRequest(
            "new password must not be empty".into(),
        ));
    }
    let mut secrets = state.secrets.write().await;
    if !secrets.verify_password(&body.current) {
        warn!("admin password change rejected: wrong current password");
        return Err(ApiError::Forbidden("current password is incorrect".into()));
    }
    secrets
        .set_password(&body.new)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    secrets
        .save(&state.config.secrets.path)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    info!("admin password changed; other admin sessions revoked");
    let generation = secrets.session_generation();
    Ok((
        jar.add(session_cookie(generation)),
        Json(json!({ "ok": true })),
    ))
}

// --- calibration ----------------------------------------------------------

pub async fn get_calibration(
    _session: Session,
    State(state): State<AppState>,
) -> Json<Calibration> {
    Json(state.calibration.read().await.clone())
}

/// Persist new corners + aspect, and hot-update the running state so the next capture uses
/// them. Validated before it touches disk or state.
pub async fn put_calibration(
    _session: Session,
    State(state): State<AppState>,
    Json(new): Json<Calibration>,
) -> Result<Json<Calibration>, ApiError> {
    new.validate()
        .map_err(|e| ApiError::BadRequest(e.to_string()))?;
    new.save(&state.config.calibration.path)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    *state.calibration.write().await = new.clone();
    info!(calibrated = new.is_calibrated(), aspect = ?new.aspect, "calibration saved");
    Ok(Json(new))
}

#[derive(Deserialize)]
pub struct DewarpPreviewRequest {
    corners: [[f32; 2]; 4],
    aspect: [u32; 2],
}

/// Dewarp a fresh snapshot with the **supplied** corners (not the persisted ones) and
/// return the JPEG — the live preview the calibration UI shows on drag-release.
pub async fn dewarp_preview(
    _session: Session,
    State(state): State<AppState>,
    Json(req): Json<DewarpPreviewRequest>,
) -> Result<Response, ApiError> {
    let _permit = state
        .capture_permits
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError::Busy)?;
    let encoded = pipeline::dewarp_preview(&state.source, &state.config, req.corners, req.aspect)
        .await
        .map_err(pipeline_error)?;
    Ok((
        [(axum::http::header::CONTENT_TYPE, "image/jpeg")],
        encoded.jpeg,
    )
        .into_response())
}

fn pipeline_error(e: pipeline::PipelineError) -> ApiError {
    use pipeline::PipelineError;
    match e {
        PipelineError::Source(c) if c.is_disabled() => ApiError::Disabled(c.to_string()),
        PipelineError::Source(c) => ApiError::Source(c.to_string()),
        PipelineError::BadRegion(m) | PipelineError::BadMode(m) => ApiError::BadRequest(m),
        PipelineError::DegenerateCorners => {
            ApiError::BadRequest("calibration corners are degenerate".into())
        }
        other => ApiError::Internal(other.to_string()),
    }
}

/// Live preview: proxy `ruru-source`'s `subscribe()` stream as `multipart/x-mixed-replace`
/// so an `<img>` shows live video. The subscription is held only while this response body
/// is alive — when the browser closes the tab, axum drops the stream, the `FrameStream`
/// drops, and `ruru-source` detaches us (its ref-count falls, so it can idle-close). That
/// is the surveillance-posture guarantee: the camera is held open only while someone is
/// actually looking.
pub async fn preview(_session: Session, State(state): State<AppState>) -> Response {
    let mut sub = match state.source.subscribe().await {
        Ok(sub) => sub,
        Err(e) => return ApiError::from(e).into_response(),
    };

    let stream = async_stream::stream! {
        // Ends on the first non-frame: a clean stop (source disabled/stopped) or a read
        // error. Either way the <img> stops rather than hanging.
        while let Ok(Some(frame)) = sub.next().await {
            let mut part = Vec::with_capacity(frame.data.len() + 96);
            part.extend_from_slice(
                format!(
                    "--frame\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n",
                    frame.data.len()
                )
                .as_bytes(),
            );
            part.extend_from_slice(&frame.data);
            part.extend_from_slice(b"\r\n");
            yield Ok::<Bytes, std::io::Error>(Bytes::from(part));
        }
    };

    (
        [
            (
                axum::http::header::CONTENT_TYPE,
                "multipart/x-mixed-replace; boundary=frame",
            ),
            (axum::http::header::CACHE_CONTROL, "no-store"),
        ],
        Body::from_stream(stream),
    )
        .into_response()
}

// --- errors ---------------------------------------------------------------

pub enum ApiError {
    /// The source refused because it is disabled — a normal, recoverable state.
    Disabled(String),
    /// Anything else from the source (unreachable, bad control, internal).
    Source(String),
    /// The client sent something invalid (bad calibration, bad region/mode).
    BadRequest(String),
    /// Authenticated, but this specific action failed its own check (wrong current password).
    Forbidden(String),
    /// Too many image-processing requests in flight.
    Busy,
    Internal(String),
}

impl From<ClientError> for ApiError {
    fn from(e: ClientError) -> Self {
        if e.is_disabled() {
            ApiError::Disabled(e.to_string())
        } else {
            ApiError::Source(e.to_string())
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            // 409: the request was fine, the source is just off right now.
            ApiError::Disabled(m) => (StatusCode::CONFLICT, m),
            ApiError::Source(m) => (StatusCode::BAD_GATEWAY, m),
            ApiError::BadRequest(m) => (StatusCode::BAD_REQUEST, m),
            ApiError::Forbidden(m) => (StatusCode::FORBIDDEN, m),
            ApiError::Busy => (
                StatusCode::SERVICE_UNAVAILABLE,
                "busy processing other captures; try again".to_string(),
            ),
            ApiError::Internal(m) => (StatusCode::INTERNAL_SERVER_ERROR, m),
        };
        (status, Json(json!({ "error": message }))).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::parse_session;

    #[test]
    fn parses_only_well_formed_v1_sessions() {
        assert_eq!(parse_session("v1.3.1700000000"), Some((3, 1_700_000_000)));
        // The pre-hardening constant value, and anything malformed, is not a session.
        assert_eq!(parse_session("authenticated"), None);
        assert_eq!(parse_session("v2.3.1700000000"), None);
        assert_eq!(parse_session("v1.3"), None);
        assert_eq!(parse_session("v1.3.170.extra"), None);
        assert_eq!(parse_session("v1.x.1700000000"), None);
    }
}
