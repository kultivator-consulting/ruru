//! Shared harness: an in-process source over a temp-dir socket.

use ruru_client::SourceClient;
use ruru_source::RunningSource;
use ruru_source::camera::mock::{MockConfig, MockStats};
use ruru_source::camera::{Camera, MockCamera};
use ruru_source::config::Config;
use std::path::Path;
use std::sync::Arc;
use tempfile::TempDir;

pub const WIDTH: u16 = 320;
pub const HEIGHT: u16 = 240;

/// Base config: small frames and a short idle grace, so tests stay quick.
pub fn base_config(socket: &Path) -> Config {
    Config::parse(&format!(
        r#"
        [source]
        socket_path = "{}"
        idle_grace_secs = 1
        snapshot_timeout_secs = 5

        [camera]
        width = {WIDTH}
        height = {HEIGHT}
        fps = 30
        warmup_frames = 1
        mock_jpeg_quality = 60
        "#,
        socket.display()
    ))
    .expect("test config should be valid")
}

pub struct Harness {
    pub source: Option<RunningSource>,
    pub client: SourceClient,
    pub stats: Arc<MockStats>,
    _dir: TempDir,
}

impl Harness {
    pub async fn start(config: Config) -> Self {
        Self::start_with(config, |cam| cam).await
    }

    /// Start with a chance to reconfigure the mock — e.g. to inject open failures.
    pub async fn start_with(config: Config, adjust: impl FnOnce(MockCamera) -> MockCamera) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut config = config;
        // Rewrite the socket into this harness's own directory so tests never collide.
        let socket = dir.path().join("source.sock");
        config.source.socket_path = socket.clone();

        let camera = adjust(MockCamera::new(MockConfig {
            width: config.camera.width,
            height: config.camera.height,
            jpeg_quality: config.camera.mock_jpeg_quality,
        }));
        let stats = camera.stats();

        let source = RunningSource::start_with_camera(config, Box::new(camera) as Box<dyn Camera>)
            .await
            .expect("source should start");

        Self {
            client: SourceClient::new(&socket),
            source: Some(source),
            stats,
            _dir: dir,
        }
    }

    pub fn socket_path(&self) -> &Path {
        self.source.as_ref().expect("running").socket_path()
    }

    pub async fn shutdown(mut self) {
        if let Some(source) = self.source.take() {
            source.shutdown().await;
        }
    }
}

/// Poll an async condition until it holds, or fail the test.
///
/// State here is driven by a separate OS thread and real timers, so tests observe it by
/// waiting for it rather than assuming a fixed sleep is long enough.
#[macro_export]
macro_rules! eventually {
    ($label:expr, $timeout:expr, $cond:expr) => {{
        let deadline = std::time::Instant::now() + $timeout;
        loop {
            if $cond {
                break;
            }
            if std::time::Instant::now() >= deadline {
                panic!("timed out after {:?} waiting for: {}", $timeout, $label);
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }};
}

/// gid of `name`, if the group exists and this process could actually chown to it.
pub fn usable_group(name: &str) -> Option<u32> {
    let group = nix::unistd::Group::from_name(name).ok().flatten()?;
    let gid = group.gid;

    // root may chown to any group; everyone else only to one they belong to.
    if nix::unistd::geteuid().is_root() {
        return Some(gid.as_raw());
    }
    let member = nix::unistd::getgroups()
        .map(|gs| gs.contains(&gid))
        .unwrap_or(false);
    member.then(|| gid.as_raw())
}
