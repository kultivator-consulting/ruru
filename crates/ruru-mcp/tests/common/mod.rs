//! Integration harness: a real ruru-mcp server on an ephemeral port, backed by a real
//! in-process mock `ruru-source`. Everything is driven over HTTP through `reqwest`, so the
//! tests exercise exactly the surface an MCP client and the admin SPA will hit.
//!
//! Each test binary (`mcp`, `admin`) uses a different subset of this module, so from any
//! single binary's view some helpers look unused.
#![allow(dead_code)]

use ruru_client::SourceClient;
use ruru_mcp::AppState;
use ruru_mcp::config::Config;
use ruru_mcp::secrets::Secrets;
use ruru_source::RunningSource;
use ruru_source::camera::mock::MockConfig;
use ruru_source::camera::{Camera, MockCamera};
use std::sync::Arc;
use tempfile::TempDir;

pub const PASSWORD: &str = "correct-horse-battery";

pub struct Harness {
    pub base: String,
    pub bearer: String,
    pub secrets_path: std::path::PathBuf,
    pub calibration_path: std::path::PathBuf,
    pub client: reqwest::Client,
    /// The server's state, for driving it directly (e.g. `begin_shutdown`).
    #[allow(dead_code)] // only some test binaries use it
    pub state: AppState,
    _source: Option<RunningSource>,
    _dir: TempDir,
    server: tokio::task::JoinHandle<()>,
}

impl Harness {
    /// Start with the source enabled and a bearer + password provisioned.
    pub async fn start() -> Self {
        Self::start_with(true).await
    }

    pub async fn start_with(source_enabled: bool) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("source.sock");

        // Real mock source over a real Unix socket.
        let mut source_cfg = ruru_source::config::Config::parse(&format!(
            "[source]\nsocket_path = \"{}\"\nidle_grace_secs = 2\nenabled = {}\n\
             [camera]\nwidth = 640\nheight = 480\nfps = 30\nwarmup_frames = 1\nmock_jpeg_quality = 70\n",
            socket.display(),
            source_enabled
        ))
        .expect("source config");
        source_cfg.source.socket_path = socket.clone();
        let camera = MockCamera::new(MockConfig {
            width: 640,
            height: 480,
            jpeg_quality: 70,
        });
        let source =
            RunningSource::start_with_camera(source_cfg, Box::new(camera) as Box<dyn Camera>)
                .await
                .expect("source starts");

        // Secrets: fresh cookie key, a minted bearer, and an admin password.
        let secrets_path = dir.path().join("mcp-secrets.toml");
        let mut secrets = Secrets::load_or_init(&secrets_path).expect("secrets");
        let bearer = secrets.mint_bearer();
        secrets.set_password(PASSWORD).expect("set password");
        secrets.save(&secrets_path).expect("save secrets");

        // Ephemeral port up front — capture links need the real origin baked into the config.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());

        let calibration_path = dir.path().join("calibration.json");
        let captures_dir = dir.path().join("captures");
        let config = Config::parse(&format!(
            "[server]\n[source]\nsocket_path = \"{}\"\n[pipeline]\n[secrets]\npath = \"{}\"\n\
             [calibration]\npath = \"{}\"\n[captures]\ndir = \"{}\"\npublic_base_url = \"{}\"\n",
            socket.display(),
            secrets_path.display(),
            calibration_path.display(),
            captures_dir.display(),
            base,
        ))
        .expect("mcp config");

        let calibration =
            ruru_mcp::calibration::Calibration::load_or_default(&calibration_path, &config.board)
                .expect("calibration");
        let captures = ruru_mcp::captures::CaptureStore::from_config(&config.captures)
            .expect("captures store")
            .map(Arc::new);

        let state = AppState::new(
            config,
            secrets,
            calibration,
            SourceClient::new(&socket),
            captures,
        )
        .expect("app state");

        let app = ruru_mcp::build_router(state.clone())
            .into_make_service_with_connect_info::<std::net::SocketAddr>();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });

        Self {
            base,
            bearer,
            secrets_path,
            calibration_path,
            state,
            client: reqwest::Client::builder()
                .cookie_store(true)
                .build()
                .unwrap(),
            _source: Some(source),
            _dir: dir,
            server,
        }
    }

    /// Log in on the shared (cookie-storing) client, so later calls carry the session.
    pub async fn login(&self) -> reqwest::Response {
        self.client
            .post(format!("{}/api/login", self.base))
            .json(&serde_json::json!({ "password": PASSWORD }))
            .send()
            .await
            .expect("login sent")
    }

    /// A fresh client with no cookie — for testing the unauthenticated case.
    pub fn anon(&self) -> reqwest::Client {
        reqwest::Client::new()
    }

    /// POST a JSON-RPC message to `/mcp` with the valid bearer.
    pub async fn mcp(&self, body: serde_json::Value) -> reqwest::Response {
        self.client
            .post(format!("{}/mcp", self.base))
            .bearer_auth(&self.bearer)
            .json(&body)
            .send()
            .await
            .expect("request sent")
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.server.abort();
    }
}
