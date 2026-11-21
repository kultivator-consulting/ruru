//! Shared auth helpers for the two HTTP boundaries.

use crate::AppState;
use axum::http::{HeaderMap, header};

/// True if the request carries a valid `Authorization: Bearer <key>`.
///
/// Verification is SHA-256 + constant-time compare inside [`crate::secrets`]; a missing or
/// malformed header is simply false. The lock is held only for the comparison.
pub async fn bearer_ok(state: &AppState, headers: &HeaderMap) -> bool {
    let Some(value) = headers.get(header::AUTHORIZATION) else {
        return false;
    };
    let Ok(text) = value.to_str() else {
        return false;
    };
    // Case-insensitive scheme, per RFC 7235.
    let token = text
        .strip_prefix("Bearer ")
        .or_else(|| text.strip_prefix("bearer "));
    let Some(token) = token else {
        return false;
    };
    state.secrets.read().await.verify_bearer(token.trim())
}
