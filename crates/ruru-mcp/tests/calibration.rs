//! Acceptance tests — calibration persistence, dewarp preview, region in dewarped
//! space, uncalibrated fallback, and the high_contrast mode.

mod common;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use common::Harness;
use serde_json::json;

/// A non-degenerate, roughly-centered quad (TL, TR, BR, BL).
fn corners() -> serde_json::Value {
    json!([[0.05, 0.06], [0.95, 0.05], [0.94, 0.95], [0.06, 0.94]])
}

async fn capture_image(h: &Harness, args: serde_json::Value) -> (serde_json::Value, Vec<u8>) {
    let body: serde_json::Value = h
        .mcp(json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"capture_whiteboard","arguments":args}}))
        .await
        .json()
        .await
        .unwrap();
    let bytes = body["result"]["content"][0]["data"]
        .as_str()
        .map(|d| STANDARD.decode(d).unwrap())
        .unwrap_or_default();
    (body, bytes)
}

// --- session gating -------------------------------------------------------

#[tokio::test]
async fn calibration_requires_a_session() {
    let h = Harness::start().await;
    let anon = h.anon();
    assert_eq!(
        anon.get(format!("{}/api/calibration", h.base))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        anon.put(format!("{}/api/calibration", h.base))
            .json(&json!({"corners": corners(), "aspect": [3, 2]}))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
}

// --- AC: corners + aspect persist across a restart ------------------------

#[tokio::test]
async fn calibration_round_trips_and_persists() {
    let h = Harness::start().await;
    h.login().await;

    // Default: uncalibrated, config aspect.
    let initial: serde_json::Value = h
        .client
        .get(format!("{}/api/calibration", h.base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(initial["corners"], json!(null));
    assert_eq!(initial["aspect"], json!([3, 2]));

    // Save corners + a measured aspect.
    let saved = h
        .client
        .put(format!("{}/api/calibration", h.base))
        .json(&json!({"corners": corners(), "aspect": [16, 10]}))
        .send()
        .await
        .unwrap();
    assert_eq!(saved.status(), 200);

    // Read back.
    let after: serde_json::Value = h
        .client
        .get(format!("{}/api/calibration", h.base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(after["aspect"], json!([16, 10]));
    assert_eq!(after["corners"][1][0], 0.95);

    // Persist across restart: a fresh load of the on-disk file has the same calibration.
    let reloaded = std::fs::read_to_string(&h.calibration_path).unwrap();
    assert!(reloaded.contains("16"), "aspect not persisted: {reloaded}");
    assert!(reloaded.contains("0.95"), "corners not persisted");
}

#[tokio::test]
async fn out_of_range_corners_are_rejected() {
    let h = Harness::start().await;
    h.login().await;
    let resp = h
        .client
        .put(format!("{}/api/calibration", h.base))
        .json(
            &json!({"corners": [[0.0, 0.0], [1.5, 0.0], [1.0, 1.0], [0.0, 1.0]], "aspect": [3, 2]}),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

// --- AC: dewarp preview ---------------------------------------------------

#[tokio::test]
async fn dewarp_preview_returns_a_jpeg() {
    let h = Harness::start().await;
    h.login().await;
    let resp = h
        .client
        .post(format!("{}/api/dewarp-preview", h.base))
        .json(&json!({"corners": corners(), "aspect": [2, 1]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.headers()["content-type"], "image/jpeg");
    let bytes = resp.bytes().await.unwrap();
    let img = image::load_from_memory(&bytes).unwrap();
    // aspect 2:1, mock is 640x480 → long edge 640 → 640x320.
    assert_eq!((img.width(), img.height()), (640, 320));
}

// --- AC: uncalibrated cleanly falls back to no-warp -----------------------

#[tokio::test]
async fn uncalibrated_whiteboard_falls_back_without_error() {
    let h = Harness::start().await;
    let (body, bytes) = capture_image(&h, json!({})).await;
    assert_ne!(
        body["result"]["isError"],
        json!(true),
        "should not error uncalibrated"
    );
    let img = image::load_from_memory(&bytes).unwrap();
    // No warp — just the raw 640x480 (within the 1568 budget, so unchanged).
    assert_eq!((img.width(), img.height()), (640, 480));
}

// --- AC: capture_region at full res in dewarped space ---------------------

#[tokio::test]
async fn region_is_cropped_in_dewarped_space_at_full_res() {
    let h = Harness::start().await;
    h.login().await;
    // Calibrate to a 2:1 board.
    h.client
        .put(format!("{}/api/calibration", h.base))
        .json(&json!({"corners": corners(), "aspect": [2, 1]}))
        .send()
        .await
        .unwrap();

    // Dewarped board is 640x320; the centre 50% region → 320x160.
    let body: serde_json::Value = h
        .mcp(
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
            "name":"capture_region","arguments":{"x":0.25,"y":0.25,"w":0.5,"h":0.5}}}),
        )
        .await
        .json()
        .await
        .unwrap();
    let bytes = STANDARD
        .decode(body["result"]["content"][0]["data"].as_str().unwrap())
        .unwrap();
    let img = image::load_from_memory(&bytes).unwrap();
    assert_eq!((img.width(), img.height()), (320, 160));
}

// --- high_contrast mode ---------------------------------------------------

#[tokio::test]
async fn high_contrast_mode_returns_grayscale() {
    let h = Harness::start().await;
    let (body, bytes) = capture_image(&h, json!({"mode": "high_contrast"})).await;
    assert_ne!(body["result"]["isError"], json!(true));
    let img = image::load_from_memory(&bytes).unwrap().to_rgb8();
    // Grayscale ⇒ R≈G≈B on a sampled pixel (JPEG chroma is near-zero, allow a little slack).
    let p = img.get_pixel(img.width() / 2, img.height() / 2).0;
    let spread = *p.iter().max().unwrap() as i32 - *p.iter().min().unwrap() as i32;
    assert!(spread <= 6, "expected gray, got {p:?}");
}

#[tokio::test]
async fn unknown_mode_is_a_tool_error() {
    let h = Harness::start().await;
    let (body, _) = capture_image(&h, json!({"mode": "sepia"})).await;
    assert_eq!(body["result"]["isError"], json!(true));
}
