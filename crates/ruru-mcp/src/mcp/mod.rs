//! The MCP endpoint: hand-rolled JSON-RPC 2.0 over `POST /mcp`.
//!
//! Streamable HTTP in its request/response form — a POST returns a single JSON body; no
//! SSE, because these tools are one-shot. The bearer key is checked at the transport layer
//! before any JSON-RPC parsing, so an unauthenticated caller gets a plain 401 and learns
//! nothing about the protocol.

mod tools;

use crate::AppState;
use crate::auth;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};
use tracing::debug;

/// The MCP protocol revision we default to when a client omits one. We echo the client's
/// requested version rather than negotiating, so this only fills in a missing field.
const DEFAULT_PROTOCOL_VERSION: &str = "2025-06-18";

/// Server-level usage guidance, returned in `initialize` per the MCP spec. Harnesses inject
/// this into the model's context, so it carries the *when/how* a per-tool description can't
/// — written once here rather than duplicated into per-harness skill files.
const SERVER_INSTRUCTIONS: &str = "\
Ruru snapshots a physical office whiteboard so you can read it on request.

- `capture_whiteboard` is the normal read: the board dewarped to a head-on rectangle and \
downscaled to a legible size. Call it whenever the user asks what's on the board.
- `capture_region` pulls a sub-rectangle at full resolution — use it only when text is too \
small to read in the whole-board image. Its x/y/w/h are fractions (0..1) of the *dewarped* \
board (0,0 = top-left of the head-on board), not raw-camera pixels.
- The image is already perspective-corrected and calibrated. Don't try to un-warp, deskew \
or rotate it yourself.
- The board changes over time. Re-capture when you need the current state; don't rely on an \
image from earlier in the conversation.
- `list_capabilities` reports calibration state, board aspect and available modes; \
`capture_raw` is a debugging escape hatch (no dewarp or crop).";

#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    #[allow(dead_code)]
    jsonrpc: Option<String>,
    /// Absent ⇒ this is a notification; no response is sent.
    #[serde(default)]
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Option<Value>,
}

/// A JSON-RPC error object (the `error` member of a failure response).
struct RpcError {
    code: i64,
    message: String,
}

impl RpcError {
    fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
    fn method_not_found(method: &str) -> Self {
        Self::new(-32601, format!("method not found: {method}"))
    }
    fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(-32602, message)
    }
}

pub async fn handle(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    // Transport-level auth first: no valid bearer ⇒ 401, before we parse anything.
    if !auth::bearer_ok(&state, &headers).await {
        return (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Bearer")],
            "missing or invalid bearer key",
        )
            .into_response();
    }

    let request: JsonRpcRequest = match serde_json::from_slice(&body) {
        Ok(request) => request,
        Err(e) => {
            // Parse error — id is unknown, so null per the spec.
            return json_response(error_body(
                &Value::Null,
                RpcError::new(-32700, format!("parse error: {e}")),
            ));
        }
    };

    // A notification (no id) gets no response body, just 202.
    let Some(id) = request.id.clone() else {
        debug!(method = %request.method, "notification");
        return StatusCode::ACCEPTED.into_response();
    };

    match dispatch(&state, &request).await {
        Ok(result) => json_response(json!({"jsonrpc": "2.0", "id": id, "result": result})),
        Err(err) => json_response(error_body(&id, err)),
    }
}

async fn dispatch(state: &AppState, request: &JsonRpcRequest) -> Result<Value, RpcError> {
    match request.method.as_str() {
        "initialize" => Ok(initialize(request.params.as_ref())),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools::definitions() })),
        "tools/call" => tools::call(state, request.params.as_ref()).await,
        other => Err(RpcError::method_not_found(other)),
    }
}

fn initialize(params: Option<&Value>) -> Value {
    let protocol_version = params
        .and_then(|p| p.get("protocolVersion"))
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_PROTOCOL_VERSION);

    json!({
        "protocolVersion": protocol_version,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": "ruru-mcp", "version": env!("CARGO_PKG_VERSION") },
        "instructions": SERVER_INSTRUCTIONS,
    })
}

fn error_body(id: &Value, err: RpcError) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": err.code, "message": err.message },
    })
}

fn json_response(body: Value) -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        body.to_string(),
    )
        .into_response()
}
