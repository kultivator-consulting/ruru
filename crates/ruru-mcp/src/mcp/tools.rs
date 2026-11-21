//! MCP tool definitions and dispatch.
//!
//! A **tool** failure (the camera is down, a bad region) is reported as a normal result
//! with `isError: true` and a text block — the model sees it and can react. Only a
//! malformed `tools/call` request (no tool name) is a JSON-RPC protocol error.

use super::RpcError;
use crate::AppState;
use crate::pipeline::{self, Encoded, Mode, Region};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};

/// Shared `inputSchema` fragment for the optional enhancement mode.
fn mode_property() -> Value {
    json!({
        "mode": {
            "type": "string",
            "enum": ["dewarped", "high_contrast"],
            "description": "output mode (default dewarped); high_contrast = grayscale + contrast stretch"
        }
    })
}

/// The advertised tools, for `tools/list`.
pub fn definitions() -> Vec<Value> {
    let empty_object = json!({ "type": "object", "properties": {}, "additionalProperties": false });
    vec![
        json!({
            "name": "capture_whiteboard",
            "description": "Snapshot the whiteboard: dewarped to a head-on rectangle and \
                            downscaled to a legible, model-friendly size. The normal read.",
            "inputSchema": {
                "type": "object",
                "properties": mode_property(),
                "additionalProperties": false
            },
        }),
        json!({
            "name": "capture_region",
            "description": "Snapshot a rectangular region of the dewarped board at full \
                            resolution. Reach for this only when text is too small to read \
                            in capture_whiteboard's downscaled image. Coordinates are \
                            fractions of the dewarped board (0..1), origin top-left.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "x": { "type": "number", "minimum": 0, "maximum": 1, "description": "left edge (0..1)" },
                    "y": { "type": "number", "minimum": 0, "maximum": 1, "description": "top edge (0..1)" },
                    "w": { "type": "number", "minimum": 0, "maximum": 1, "description": "width (0..1)" },
                    "h": { "type": "number", "minimum": 0, "maximum": 1, "description": "height (0..1)" },
                    "mode": mode_property()["mode"]
                },
                "required": ["x", "y", "w", "h"],
                "additionalProperties": false
            },
        }),
        json!({
            "name": "capture_raw",
            "description": "The raw camera frame, exactly as captured — no dewarp, crop or \
                            downscale. For debugging framing and focus.",
            "inputSchema": empty_object,
        }),
        json!({
            "name": "list_capabilities",
            "description": "Report what this board service can currently do: geometry, \
                            calibration state, downscale budget, and live source status.",
            "inputSchema": empty_object,
        }),
    ]
}

pub async fn call(state: &AppState, params: Option<&Value>) -> Result<Value, RpcError> {
    let params = params.ok_or_else(|| RpcError::invalid_params("tools/call needs params"))?;
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::invalid_params("tools/call needs a tool `name`"))?;
    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));

    let mode = match Mode::parse(args.get("mode").and_then(Value::as_str)) {
        Ok(m) => m,
        Err(e) => return Ok(tool_error(e.to_string())),
    };

    // Image-processing tools share a small permit pool; a call over the limit gets a tool
    // error the model can react to, rather than piling more 4K dewarps onto the CPU.
    let _permit = if matches!(name, "capture_whiteboard" | "capture_region") {
        match state.capture_permits.clone().try_acquire_owned() {
            Ok(permit) => Some(permit),
            Err(_) => {
                return Ok(tool_error(
                    "busy: other captures are in progress; try again in a moment",
                ));
            }
        }
    } else {
        None
    };

    let result = match name {
        "capture_raw" => from_capture(pipeline::capture_raw(&state.source).await),
        "capture_whiteboard" => {
            let cal = state.calibration.read().await.clone();
            from_capture_retained(
                state,
                pipeline::capture_whiteboard(&state.source, &state.config, &cal, mode).await,
            )
        }
        "capture_region" => match parse_region(&args) {
            Ok(region) => {
                let cal = state.calibration.read().await.clone();
                from_capture_retained(
                    state,
                    pipeline::capture_region(&state.source, &state.config, &cal, region, mode)
                        .await,
                )
            }
            Err(message) => tool_error(message),
        },
        "list_capabilities" => list_capabilities(state).await,
        other => tool_error(format!("unknown tool: {other}")),
    };
    Ok(result)
}

fn parse_region(args: &Value) -> Result<Region, String> {
    let field = |key: &str| -> Result<f32, String> {
        args.get(key)
            .and_then(Value::as_f64)
            .map(|v| v as f32)
            .ok_or_else(|| format!("region needs a numeric `{key}`"))
    };
    Ok(Region {
        x: field("x")?,
        y: field("y")?,
        w: field("w")?,
        h: field("h")?,
    })
}

/// For `capture_raw`: an image block, no retained link (raw frames are a debug aid, not
/// something a user wants to reference later).
fn from_capture(result: Result<Encoded, pipeline::PipelineError>) -> Value {
    match result {
        Ok(encoded) => {
            log_capture(&encoded);
            image_content(&encoded.jpeg, None)
        }
        Err(e) => tool_error(e.to_string()),
    }
}

/// For `capture_whiteboard` / `capture_region`: the image block plus, when retention is
/// configured, a text block with an unguessable link to the stored frame so the agent can
/// hand it to the user. A store failure is logged and the capture still returns.
fn from_capture_retained(
    state: &AppState,
    result: Result<Encoded, pipeline::PipelineError>,
) -> Value {
    match result {
        Ok(encoded) => {
            log_capture(&encoded);
            let link = state.captures.as_ref().and_then(|store| {
                store
                    .store(&encoded.jpeg)
                    .map_err(|e| tracing::warn!(error = %e, "could not retain capture; returning it without a link"))
                    .ok()
            });
            image_content(&encoded.jpeg, link.as_deref())
        }
        Err(e) => tool_error(e.to_string()),
    }
}

fn log_capture(encoded: &Encoded) {
    tracing::debug!(
        width = encoded.width,
        height = encoded.height,
        bytes = encoded.jpeg.len(),
        "capture produced a frame"
    );
}

fn image_content(jpeg: &[u8], link: Option<&str>) -> Value {
    let mut content = vec![json!({
        "type": "image",
        "data": STANDARD.encode(jpeg),
        "mimeType": "image/jpeg"
    })];
    if let Some(url) = link {
        content.push(json!({
            "type": "text",
            "text": format!("Saved for reference — open or share this exact capture: {url}")
        }));
    }
    json!({ "content": content })
}

fn tool_error(message: impl Into<String>) -> Value {
    json!({
        "content": [{ "type": "text", "text": message.into() }],
        "isError": true
    })
}

async fn list_capabilities(state: &AppState) -> Value {
    let config = &state.config;
    let cal = state.calibration.read().await.clone();

    // Source status is best-effort — the board service should still describe itself even
    // if the frame source is momentarily down.
    let source = match state.source.status().await {
        Ok(status) => json!({
            "reachable": true,
            "enabled": status.enabled,
            "open": status.open,
            "backend": status.camera_backend,
            "fps": status.fps,
        }),
        Err(e) => json!({ "reachable": false, "error": e.to_string() }),
    };

    let capabilities = json!({
        "tools": ["capture_whiteboard", "capture_region", "capture_raw", "list_capabilities"],
        "modes": ["dewarped", "high_contrast"],
        "board": {
            "aspect": cal.aspect,
            "calibrated": cal.is_calibrated(),
            "dewarp": if cal.is_calibrated() { "homography" } else { "none (uncalibrated)" },
        },
        "downscale_max_edge": config.pipeline.max_edge,
        "jpeg_quality": config.pipeline.jpeg_quality,
        "source": source,
    });

    // Returned as a pretty-printed text block — the model reads JSON fine, and a human
    // debugging with a raw MCP client can read it too.
    json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string_pretty(&capabilities).unwrap_or_else(|_| capabilities.to_string())
        }]
    })
}
