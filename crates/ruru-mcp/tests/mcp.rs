//! MCP endpoint acceptance tests.

mod common;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use common::Harness;
use serde_json::json;

fn init(id: u64) -> serde_json::Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "initialize",
           "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}})
}

// --- AC1: initialize -> tools/list -> capture_raw returns a frame ---------

#[tokio::test]
async fn initialize_echoes_protocol_version_and_advertises_tools() {
    let h = Harness::start().await;
    let resp = h.mcp(init(1)).await;
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["jsonrpc"], "2.0");
    assert_eq!(body["id"], 1);
    assert_eq!(body["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(body["result"]["serverInfo"]["name"], "ruru-mcp");
    assert!(body["result"]["capabilities"]["tools"].is_object());

    // The server carries its own usage guidance, so any harness learns the when/how
    // without a bespoke skill.
    let instructions = body["result"]["instructions"].as_str().unwrap_or_default();
    assert!(
        instructions.contains("capture_whiteboard") && instructions.contains("capture_region"),
        "initialize should advertise usage instructions naming the tools, got: {instructions:?}"
    );
}

#[tokio::test]
async fn notifications_get_202_and_no_body() {
    let h = Harness::start().await;
    let resp = h
        .mcp(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
        .await;
    assert_eq!(resp.status(), 202);
    assert!(resp.bytes().await.unwrap().is_empty());
}

#[tokio::test]
async fn tools_list_has_the_four_tools() {
    let h = Harness::start().await;
    let body: serde_json::Value = h
        .mcp(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}))
        .await
        .json()
        .await
        .unwrap();

    let names: Vec<&str> = body["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    for expected in [
        "capture_whiteboard",
        "capture_region",
        "capture_raw",
        "list_capabilities",
    ] {
        assert!(
            names.contains(&expected),
            "missing tool {expected} in {names:?}"
        );
    }
}

#[tokio::test]
async fn capture_raw_returns_a_real_jpeg_frame() {
    let h = Harness::start().await;

    // The full AC1 flow, in order.
    h.mcp(init(1)).await;
    h.mcp(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
        .await;
    h.mcp(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}))
        .await;

    let body: serde_json::Value = h
        .mcp(json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
                    "params": {"name": "capture_raw", "arguments": {}}}))
        .await
        .json()
        .await
        .unwrap();

    let block = &body["result"]["content"][0];
    assert_eq!(block["type"], "image");
    assert_eq!(block["mimeType"], "image/jpeg");
    assert_ne!(body["result"]["isError"], json!(true));

    let bytes = STANDARD.decode(block["data"].as_str().unwrap()).unwrap();
    assert_eq!(
        &bytes[..2],
        &[0xFF, 0xD8],
        "decoded payload should be a JPEG"
    );
    assert!(bytes.len() > 500);
}

#[tokio::test]
async fn capture_whiteboard_and_region_return_images() {
    let h = Harness::start().await;

    let whiteboard: serde_json::Value = h
        .mcp(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                    "params": {"name": "capture_whiteboard"}}))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(whiteboard["result"]["content"][0]["type"], "image");

    let region: serde_json::Value = h
        .mcp(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
                    "params": {"name": "capture_region",
                               "arguments": {"x": 0.25, "y": 0.25, "w": 0.5, "h": 0.5}}}))
        .await
        .json()
        .await
        .unwrap();
    let data = region["result"]["content"][0]["data"].as_str().unwrap();
    let bytes = STANDARD.decode(data).unwrap();
    let img = image::load_from_memory(&bytes).unwrap();
    // 640x480 cropped to the middle 50% → ~320x240.
    assert!((300..=340).contains(&img.width()), "got {}", img.width());
}

/// capture_whiteboard also returns an unguessable view link, and that link is a
/// public capability URL — fetchable with no bearer and no session — serving the exact JPEG.
#[tokio::test]
async fn capture_whiteboard_returns_a_public_view_link() {
    let h = Harness::start().await;

    let body: serde_json::Value = h
        .mcp(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                    "params": {"name": "capture_whiteboard"}}))
        .await
        .json()
        .await
        .unwrap();
    let content = body["result"]["content"].as_array().unwrap();
    assert_eq!(
        content[0]["type"], "image",
        "first block is still the image"
    );

    // A text block carries the retained-capture link, pointing at this server.
    let text = content
        .iter()
        .find(|c| c["type"] == "text")
        .and_then(|c| c["text"].as_str())
        .expect("a text block with the view link");
    let url = text
        .split_whitespace()
        .find(|w| w.contains("/captures/"))
        .expect("a /captures/ url in the text");
    assert!(
        url.starts_with(&h.base),
        "link should point at this server: {url}"
    );

    // Public: a client with no bearer and no cookie fetches the exact JPEG.
    let resp = h.anon().get(url).send().await.unwrap();
    assert_eq!(
        resp.status(),
        200,
        "the capability link must be publicly fetchable"
    );
    assert_eq!(resp.headers()["content-type"], "image/jpeg");
    let jpeg = resp.bytes().await.unwrap();
    assert!(jpeg.starts_with(&[0xFF, 0xD8]), "should be a JPEG");

    // A bogus / wrong-shaped id is a clean 404 — no crash, no traversal.
    let bad = h
        .anon()
        .get(format!("{}/captures/deadbeef.jpg", h.base))
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), 404);
}

#[tokio::test]
async fn bad_region_is_a_tool_error_not_a_crash() {
    let h = Harness::start().await;
    let body: serde_json::Value = h
        .mcp(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                    "params": {"name": "capture_region",
                               "arguments": {"x": 0.8, "y": 0.0, "w": 0.5, "h": 0.5}}}))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(body["result"]["isError"], json!(true));
    assert_eq!(body["result"]["content"][0]["type"], "text");
}

#[tokio::test]
async fn list_capabilities_reports_a_reachable_source() {
    let h = Harness::start().await;
    let body: serde_json::Value = h
        .mcp(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                    "params": {"name": "list_capabilities"}}))
        .await
        .json()
        .await
        .unwrap();
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    let caps: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(caps["source"]["reachable"], true);
    // Uncalibrated by default.
    assert_eq!(caps["board"]["dewarp"], "none (uncalibrated)");
    assert_eq!(caps["board"]["calibrated"], false);
    assert_eq!(caps["downscale_max_edge"], 1568);
}

#[tokio::test]
async fn unknown_method_is_json_rpc_method_not_found() {
    let h = Harness::start().await;
    let body: serde_json::Value = h
        .mcp(json!({"jsonrpc": "2.0", "id": 9, "method": "does/not/exist"}))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(body["error"]["code"], -32601);
}

// --- AC2: /mcp rejects missing/invalid bearer, accepts valid --------------

#[tokio::test]
async fn missing_bearer_is_401() {
    let h = Harness::start().await;
    let resp = h
        .client
        .post(format!("{}/mcp", h.base))
        .json(&init(1))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn invalid_bearer_is_401() {
    let h = Harness::start().await;
    let resp = h
        .client
        .post(format!("{}/mcp", h.base))
        .bearer_auth("ruru_not-a-real-key")
        .json(&init(1))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn valid_bearer_is_accepted() {
    let h = Harness::start().await;
    let resp = h.mcp(init(1)).await;
    assert_eq!(resp.status(), 200);
}
