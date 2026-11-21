//! `ruru-source` — owns the camera, fans frames out to everyone else.
//!
//! A UVC device has exactly one streaming owner, so a single thread here holds the
//! device and every consumer reaches frames through the Unix socket via `ruru-client`.
//!
//! This component is deliberately domain-agnostic: it knows cameras, frames and
//! controls, and nothing whatever about whiteboards. Dewarp, crop and calibration belong
//! to `ruru-mcp`, so that future consumers (gesture, security) are peers of the
//! whiteboard pipeline rather than dependents of it.

pub mod camera;
pub mod capture;
pub mod config;
pub mod controls_state;
pub mod server;

use anyhow::{Context, Result};
use camera::{Camera, MockCamera, mock::MockConfig};
use capture::CaptureHandle;
use config::{Backend, Config};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::net::UnixListener;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

/// A running source: capture thread plus accept loop.
pub struct RunningSource {
    socket_path: PathBuf,
    capture: CaptureHandle,
    shutdown: watch::Sender<bool>,
    accept: JoinHandle<()>,
    capture_thread: Option<std::thread::JoinHandle<()>>,
}

impl RunningSource {
    /// Build the configured camera and start serving.
    ///
    /// Returns once the socket exists with its final permissions, so a caller may connect
    /// as soon as this resolves.
    pub async fn start(config: Config) -> Result<Self> {
        let camera = build_camera(&config)?;
        Self::start_with_camera(config, camera).await
    }

    /// Start with a caller-supplied camera. Tests use this to hold onto a `MockStats`
    /// handle and to inject failure behaviour.
    pub async fn start_with_camera(config: Config, camera: Box<dyn Camera>) -> Result<Self> {
        config.validate()?;

        let listener = server::bind(
            &config.source.socket_path,
            config.source.socket_group.as_deref(),
        )?;

        let (capture, capture_thread) = capture::spawn(camera, &config);

        let server_config = server::ServerConfig {
            request_timeout: Duration::from_secs(config.source.request_timeout_secs),
            snapshot_timeout: Duration::from_secs(config.source.snapshot_timeout_secs),
        };

        let (shutdown, shutdown_rx) = watch::channel(false);
        let accept = tokio::spawn(accept_loop(
            listener,
            capture.clone(),
            server_config,
            shutdown_rx,
        ));

        info!(
            socket = %config.source.socket_path.display(),
            fps = config.camera.fps,
            "ruru-source ready"
        );

        Ok(Self {
            socket_path: config.source.socket_path.clone(),
            capture,
            shutdown,
            accept,
            capture_thread: Some(capture_thread),
        })
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Handle to the capture thread, for in-process callers (tests, and later `ruru-mcp`
    /// if it is ever colocated).
    pub fn capture(&self) -> &CaptureHandle {
        &self.capture
    }

    /// Stop accepting, close the device, and remove the socket.
    pub async fn shutdown(mut self) {
        let _ = self.shutdown.send(true);
        let _ = (&mut self.accept).await;

        self.capture.shutdown().await;
        if let Some(thread) = self.capture_thread.take() {
            // The capture thread only blocks on its command channel, so this is prompt.
            let _ = tokio::task::spawn_blocking(move || thread.join()).await;
        }

        self.remove_socket();
        info!("ruru-source stopped");
    }

    fn remove_socket(&self) {
        if self.socket_path.exists()
            && let Err(e) = std::fs::remove_file(&self.socket_path)
        {
            warn!(socket = %self.socket_path.display(), error = %e, "could not remove socket");
        }
    }
}

impl std::fmt::Debug for RunningSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunningSource")
            .field("socket_path", &self.socket_path)
            .finish_non_exhaustive()
    }
}

impl Drop for RunningSource {
    fn drop(&mut self) {
        // Best effort for callers that drop rather than await `shutdown` — chiefly tests.
        self.accept.abort();
        self.remove_socket();
    }
}

async fn accept_loop(
    listener: UnixListener,
    capture: CaptureHandle,
    config: server::ServerConfig,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            _ = shutdown.changed() => {
                debug!("accept loop shutting down");
                return;
            }
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    let capture = capture.clone();
                    tokio::spawn(server::handle_connection(stream, capture, config));
                }
                Err(e) => {
                    // Transient conditions like EMFILE would otherwise spin this loop.
                    warn!(error = %e, "accept failed");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            },
        }
    }
}

fn build_camera(config: &Config) -> Result<Box<dyn Camera>> {
    match config.camera.backend {
        Backend::Mock => {
            let camera = MockCamera::new(MockConfig {
                width: config.camera.width,
                height: config.camera.height,
                jpeg_quality: config.camera.mock_jpeg_quality,
            });
            Ok(Box::new(camera) as Box<dyn Camera>)
        }
        Backend::V4l2 => {
            let camera = camera::v4l2::V4l2Camera::new(&config.camera)
                .context("building the v4l2 backend")?;
            Ok(Box::new(camera) as Box<dyn Camera>)
        }
    }
}
