//! Admin API acceptance tests plus the live preview proxy.

mod common;

use common::{Harness, PASSWORD};
use serde_json::json;

// --- AC3: admin login + session cookie gates the admin API ----------------

#[tokio::test]
async fn admin_routes_require_a_session() {
    let h = Harness::start().await;
    // No cookie yet.
    let anon = h.anon();
    for path in ["/api/me", "/api/status", "/api/controls"] {
        let resp = anon
            .get(format!("{}{}", h.base, path))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 401, "{path} should require a session");
    }
}

#[tokio::test]
async fn wrong_password_is_rejected() {
    let h = Harness::start().await;
    let resp = h
        .client
        .post(format!("{}/api/login", h.base))
        .json(&json!({ "password": "not-the-password" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    // And the session was not established.
    let me = h
        .client
        .get(format!("{}/api/me", h.base))
        .send()
        .await
        .unwrap();
    assert_eq!(me.status(), 401);
}

#[tokio::test]
async fn login_opens_the_admin_api() {
    let h = Harness::start().await;
    assert_eq!(h.login().await.status(), 200);

    let me = h
        .client
        .get(format!("{}/api/me", h.base))
        .send()
        .await
        .unwrap();
    assert_eq!(me.status(), 200);

    // Status proxies through to ruru-source.
    let status: serde_json::Value = h
        .client
        .get(format!("{}/api/status", h.base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(status["camera_backend"], "mock");
}

#[tokio::test]
async fn logout_closes_the_session() {
    let h = Harness::start().await;
    h.login().await;
    assert_eq!(
        h.client
            .get(format!("{}/api/me", h.base))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );

    h.client
        .post(format!("{}/api/logout", h.base))
        .send()
        .await
        .unwrap();
    assert_eq!(
        h.client
            .get(format!("{}/api/me", h.base))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
}

#[tokio::test]
async fn controls_proxy_through_to_the_source() {
    let h = Harness::start().await;
    h.login().await;

    // Set brightness, then read it back.
    let set: serde_json::Value = h
        .client
        .put(format!("{}/api/controls/brightness", h.base))
        .json(&json!({ "value": 64 }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(set["value"], 64);

    let controls: serde_json::Value = h
        .client
        .get(format!("{}/api/controls", h.base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let brightness = controls
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "brightness")
        .unwrap();
    assert_eq!(brightness["value"], 64);
}

#[tokio::test]
async fn enable_toggle_proxies_through() {
    let h = Harness::start().await;
    h.login().await;

    let status: serde_json::Value = h
        .client
        .post(format!("{}/api/enable", h.base))
        .json(&json!({ "enabled": false }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(status["enabled"], false);
    assert_eq!(status["open"], false, "disabling closes the device");
}

// --- AC4: key mint/rotate returns the key once; only the hash is stored ----

#[tokio::test]
async fn rotate_returns_the_key_once_and_stores_only_the_hash() {
    let h = Harness::start().await;
    h.login().await;

    let old_bearer = h.bearer.clone();
    let body: serde_json::Value = h
        .client
        .post(format!("{}/api/keys/rotate", h.base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let new_key = body["key"].as_str().unwrap().to_string();
    assert!(new_key.starts_with("ruru_"));
    assert_ne!(new_key, old_bearer);

    // The on-disk secrets file must hold only the hash, never the returned key.
    let on_disk = std::fs::read_to_string(&h.secrets_path).unwrap();
    assert!(
        !on_disk.contains(&new_key),
        "the raw key must never be persisted"
    );
    assert!(on_disk.contains("bearer_key_sha256"));

    // The new key works on /mcp; the old one no longer does.
    let with = |key: &str| {
        h.client
            .post(format!("{}/mcp", h.base))
            .bearer_auth(key)
            .json(&json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}))
    };
    assert_eq!(with(&new_key).send().await.unwrap().status(), 200);
    assert_eq!(with(&old_bearer).send().await.unwrap().status(), 401);
}

#[tokio::test]
async fn rotate_requires_a_session() {
    let h = Harness::start().await;
    let resp = h
        .anon()
        .post(format!("{}/api/keys/rotate", h.base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

// --- live preview proxy (feeds AC5's preview) -----------------------------

#[tokio::test]
async fn preview_streams_multipart_mjpeg() {
    let h = Harness::start().await;
    h.login().await;

    let mut resp = h
        .client
        .get(format!("{}/api/preview.mjpeg", h.base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers()["content-type"],
        "multipart/x-mixed-replace; boundary=frame"
    );

    // Pull enough of the stream to see a frame boundary, then drop it (which detaches us
    // from the source). `chunk()` avoids pulling in a Stream extension trait.
    let mut buf = Vec::new();
    while buf.len() < 4096 {
        match resp.chunk().await.unwrap() {
            Some(chunk) => buf.extend_from_slice(&chunk),
            None => break,
        }
    }
    let text = String::from_utf8_lossy(&buf);
    assert!(text.contains("--frame"), "expected a multipart boundary");
    assert!(text.contains("Content-Type: image/jpeg"));
}

#[tokio::test]
async fn preview_requires_a_session() {
    let h = Harness::start().await;
    let resp = h
        .anon()
        .get(format!("{}/api/preview.mjpeg", h.base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn login_uses_the_configured_password() {
    // Guards against an accidental hard-coded or empty-password bypass.
    let h = Harness::start().await;
    assert_ne!(PASSWORD, "");
    let resp = h
        .client
        .post(format!("{}/api/login", h.base))
        .json(&json!({ "password": "" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

// --- change password (needs the current password; takes effect live) ----

#[tokio::test]
async fn change_password_requires_a_session() {
    let h = Harness::start().await;
    let resp = h
        .anon()
        .post(format!("{}/api/password", h.base))
        .json(&json!({ "current": PASSWORD, "new": "irrelevant" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn change_password_rejects_a_wrong_current() {
    let h = Harness::start().await;
    h.login().await;
    let resp = h
        .client
        .post(format!("{}/api/password", h.base))
        .json(&json!({ "current": "not-the-password", "new": "new-secret-1" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);

    // The password must be unchanged: the original still logs in.
    h.client
        .post(format!("{}/api/logout", h.base))
        .send()
        .await
        .unwrap();
    let relogin = h
        .client
        .post(format!("{}/api/login", h.base))
        .json(&json!({ "password": PASSWORD }))
        .send()
        .await
        .unwrap();
    assert_eq!(relogin.status(), 200);
}

#[tokio::test]
async fn change_password_takes_effect_and_stores_only_a_hash() {
    let h = Harness::start().await;
    h.login().await;

    const NEW: &str = "a-brand-new-password-42";
    let resp = h
        .client
        .post(format!("{}/api/password", h.base))
        .json(&json!({ "current": PASSWORD, "new": NEW }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Never the plaintext on disk — only the argon2id hash.
    let on_disk = std::fs::read_to_string(&h.secrets_path).unwrap();
    assert!(
        !on_disk.contains(NEW),
        "the plaintext password must never be stored"
    );

    // The old password stops working; the new one logs in.
    h.client
        .post(format!("{}/api/logout", h.base))
        .send()
        .await
        .unwrap();
    let old = h
        .client
        .post(format!("{}/api/login", h.base))
        .json(&json!({ "password": PASSWORD }))
        .send()
        .await
        .unwrap();
    assert_eq!(old.status(), 401, "the old password must stop working");
    let new = h
        .client
        .post(format!("{}/api/login", h.base))
        .json(&json!({ "password": NEW }))
        .send()
        .await
        .unwrap();
    assert_eq!(new.status(), 200, "the new password must work");
}

#[tokio::test]
async fn change_password_rejects_an_empty_new() {
    let h = Harness::start().await;
    h.login().await;
    let resp = h
        .client
        .post(format!("{}/api/password", h.base))
        .json(&json!({ "current": PASSWORD, "new": "" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

// --- hardening: session revocation, login throttling, headers -------------

#[tokio::test]
async fn changing_the_password_revokes_other_sessions_but_not_the_callers() {
    let h = Harness::start().await;
    h.login().await;

    // A second, independent browser session — e.g. one an attacker copied.
    let other = reqwest::Client::builder()
        .cookie_store(true)
        .build()
        .unwrap();
    let login = other
        .post(format!("{}/api/login", h.base))
        .json(&json!({ "password": PASSWORD }))
        .send()
        .await
        .unwrap();
    assert_eq!(login.status(), 200);

    let resp = h
        .client
        .post(format!("{}/api/password", h.base))
        .json(&json!({ "current": PASSWORD, "new": "rotated-password-123" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let me = |c: reqwest::Client| {
        let url = format!("{}/api/me", h.base);
        async move { c.get(url).send().await.unwrap().status() }
    };
    assert_eq!(me(other).await, 401, "other sessions must be revoked");
    assert_eq!(
        me(h.client.clone()).await,
        200,
        "the caller is re-issued a fresh session"
    );
}

#[tokio::test]
async fn repeated_failed_logins_are_locked_out() {
    let h = Harness::start().await;
    let attempt = |password: &'static str| {
        let c = h.anon();
        let url = format!("{}/api/login", h.base);
        async move {
            c.post(url)
                .json(&json!({ "password": password }))
                .send()
                .await
                .unwrap()
        }
    };
    for _ in 0..5 {
        assert_eq!(attempt("wrong").await.status(), 401);
    }
    let locked = attempt("wrong").await;
    assert_eq!(locked.status(), 429);
    assert!(locked.headers().contains_key("retry-after"));
    // Even the right password is refused while locked out.
    assert_eq!(attempt(PASSWORD).await.status(), 429);
}

#[tokio::test]
async fn responses_carry_anti_framing_headers() {
    let h = Harness::start().await;
    let resp = h
        .anon()
        .get(format!("{}/healthz", h.base))
        .send()
        .await
        .unwrap();
    let headers = resp.headers();
    assert_eq!(headers["x-frame-options"], "DENY");
    assert_eq!(headers["content-security-policy"], "frame-ancestors 'none'");
    assert_eq!(headers["x-content-type-options"], "nosniff");
}
