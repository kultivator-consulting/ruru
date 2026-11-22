//! The Unix-socket front door.
//!
//! Access control on this hop is the filesystem's job — mode `0660` owned by a known
//! group — not an application token. That is deliberate: the source holds no secrets, and
//! the OS enforces this boundary more reliably than anything we could write.

use crate::camera::CameraError;
use crate::capture::{CaptureError, CaptureHandle, FrameEvent};
use anyhow::{Context, Result, bail};
use ruru_client::proto::{self, ErrorCode, Frame, Message, Op, PROTO_VERSION, Request, Response};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::broadcast::error::RecvError;
use tracing::{debug, info, warn};

#[derive(Debug, Clone, Copy)]
pub struct ServerConfig {
    pub request_timeout: Duration,
    pub snapshot_timeout: Duration,
}

/// Create the listening socket with the right ownership, or fail loudly.
pub fn bind(path: &Path, group: Option<&str>) -> Result<UnixListener> {
    let parent = path
        .parent()
        .with_context(|| format!("socket path {} has no parent directory", path.display()))?;
    if !parent.is_dir() {
        bail!(
            "socket directory {} does not exist (in production systemd's RuntimeDirectory \
             creates it as 0750 root:ruru)",
            parent.display()
        );
    }

    reclaim_stale_socket(path)?;

    let listener = UnixListener::bind(path)
        .with_context(|| format!("binding source socket {}", path.display()))?;

    // Never leave a half-configured socket on disk: until ownership is settled, this
    // socket is exactly the thing we refuse to serve on.
    match apply_ownership(path, group) {
        Ok(()) => Ok(listener),
        Err(e) => {
            drop(listener);
            let _ = std::fs::remove_file(path);
            Err(e)
        }
    }
}

/// Mode is unconditional; group ownership only when configured.
fn apply_ownership(path: &Path, group: Option<&str>) -> Result<()> {
    // Permissions land immediately after bind and before the first accept. The window is
    // sub-millisecond, and in production the parent directory (0750 root:ruru) is what
    // actually closes it (systemd's `RuntimeDirectory=`, see `deploy/`).
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o660))
        .with_context(|| format!("setting mode 0660 on {}", path.display()))?;

    let Some(name) = group else {
        info!(
            socket = %path.display(),
            mode = "0660",
            "socket ready; source.socket_group is unset, so it keeps this process's \
             own group (expected in dev, not in production)"
        );
        return Ok(());
    };

    let gid = lookup_group(name)?;
    nix::unistd::chown(path, None, Some(gid)).with_context(|| {
        format!(
            "changing group of {} to `{name}` (gid {gid}). The source socket's only \
             access control is its ownership, so this is fatal rather than a warning — \
             either create the group and add this service user to it, or unset \
             source.socket_group.",
            path.display()
        )
    })?;
    info!(socket = %path.display(), group = name, mode = "0660", "socket ready");
    Ok(())
}

/// Remove a leftover socket file, but never one that something is still serving.
fn reclaim_stale_socket(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }

    if std::os::unix::net::UnixStream::connect(path).is_ok() {
        bail!(
            "{} is already being served by another ruru-source — refusing to start, \
             because two daemons cannot both own the camera",
            path.display()
        );
    }

    std::fs::remove_file(path)
        .with_context(|| format!("removing stale socket {}", path.display()))?;
    debug!(socket = %path.display(), "removed stale socket");
    Ok(())
}

fn lookup_group(name: &str) -> Result<nix::unistd::Gid> {
    let group = nix::unistd::Group::from_name(name)
        .with_context(|| format!("looking up group `{name}`"))?
        .with_context(|| format!("no such group `{name}` on this system"))?;
    Ok(group.gid)
}

/// Serve one connection: read its single request, answer it, stream if asked.
pub async fn handle_connection(mut stream: UnixStream, capture: CaptureHandle, cfg: ServerConfig) {
    let request = match read_request(&mut stream, cfg.request_timeout).await {
        Ok(request) => request,
        Err(response) => {
            let _ = proto::write_json(&mut stream, &response).await;
            return;
        }
    };

    let result = match request.op {
        Op::Snapshot => serve_snapshot(&mut stream, &capture, cfg).await,
        Op::Subscribe => serve_subscribe(&mut stream, &capture).await,
        Op::Status => {
            let response = match capture.status().await {
                Ok(status) => Response::ok().with_status(status),
                Err(e) => capture_error_response(e),
            };
            proto::write_json(&mut stream, &response).await
        }
        Op::Enable { enabled } => {
            let response = match capture.set_enabled(enabled).await {
                Ok(status) => Response::ok().with_status(status),
                Err(e) => capture_error_response(e),
            };
            proto::write_json(&mut stream, &response).await
        }
        Op::ListControls => {
            let response = match capture.list_controls().await {
                Ok(controls) => Response::ok().with_controls(controls),
                Err(e) => capture_error_response(e),
            };
            proto::write_json(&mut stream, &response).await
        }
        Op::GetControl { name } => {
            let response = match capture.get_control(name).await {
                Ok(control) => Response::ok().with_control(control),
                Err(e) => capture_error_response(e),
            };
            proto::write_json(&mut stream, &response).await
        }
        Op::SetControl {
            name,
            value,
            persist,
        } => {
            let response = match capture.set_control(name, value, persist).await {
                Ok(control) => Response::ok().with_control(control),
                Err(e) => capture_error_response(e),
            };
            proto::write_json(&mut stream, &response).await
        }
    };

    if let Err(e) = result {
        debug!(error = %e, "connection ended");
    }
}

async fn read_request(
    stream: &mut UnixStream,
    timeout: Duration,
) -> Result<Request, Box<Response>> {
    let message = tokio::time::timeout(timeout, proto::read_message(stream))
        .await
        .map_err(|_| {
            bad_request(
                ErrorCode::BadRequest,
                format!("no request within {}s", timeout.as_secs()),
            )
        })?
        .map_err(|e| bad_request(ErrorCode::BadRequest, format!("unreadable request: {e}")))?
        .ok_or_else(|| bad_request(ErrorCode::BadRequest, "connection closed before a request"))?;

    let bytes = match message {
        Message::Json(bytes) => bytes,
        Message::Frame(_) => {
            return Err(bad_request(
                ErrorCode::BadRequest,
                "expected a control message, got a frame",
            ));
        }
    };

    let request: Request = serde_json::from_slice(&bytes)
        .map_err(|e| bad_request(ErrorCode::BadRequest, format!("malformed request: {e}")))?;

    if request.v != PROTO_VERSION {
        return Err(bad_request(
            ErrorCode::ProtoVersion,
            format!(
                "this source speaks protocol v{PROTO_VERSION}, the client asked for v{}",
                request.v
            ),
        ));
    }

    Ok(request)
}

/// Boxed so the `Err` arm stays small (`Response` is a few hundred bytes).
fn bad_request(code: ErrorCode, message: impl Into<String>) -> Box<Response> {
    Box::new(Response::err(code, message))
}

async fn serve_snapshot(
    stream: &mut UnixStream,
    capture: &CaptureHandle,
    cfg: ServerConfig,
) -> Result<(), proto::CodecError> {
    // Subscribe first: a frame captured between attaching and subscribing would be lost,
    // and for a one-shot that is the difference between a frame and a timeout.
    let mut frames = capture.subscribe_frames();

    let _attachment = match capture.attach().await {
        Ok(attachment) => attachment,
        Err(e) => return proto::write_json(stream, &capture_error_response(e)).await,
    };

    let frame = match tokio::time::timeout(cfg.snapshot_timeout, next_frame(&mut frames)).await {
        Ok(Ok(frame)) => frame,
        Ok(Err(end)) => return proto::write_json(stream, &stream_end_response(end)).await,
        Err(_) => {
            let response = Response::err(
                ErrorCode::CameraError,
                format!(
                    "no frame within {}s — the device may be failing to open",
                    cfg.snapshot_timeout.as_secs()
                ),
            );
            return proto::write_json(stream, &response).await;
        }
    };

    proto::write_json(stream, &Response::ok()).await?;
    proto::write_frame(stream, &frame).await
}

async fn serve_subscribe(
    stream: &mut UnixStream,
    capture: &CaptureHandle,
) -> Result<(), proto::CodecError> {
    let mut frames = capture.subscribe_frames();

    let _attachment = match capture.attach().await {
        Ok(attachment) => attachment,
        Err(e) => return proto::write_json(stream, &capture_error_response(e)).await,
    };

    proto::write_json(stream, &Response::ok()).await?;

    loop {
        match next_frame(&mut frames).await {
            Ok(frame) => {
                if let Err(e) = proto::write_frame(stream, &frame).await {
                    // Almost always the client hanging up; the attachment drops on return.
                    debug!(error = %e, "subscriber write failed; ending subscription");
                    return Ok(());
                }
            }
            Err(end) => {
                return proto::write_json(stream, &stream_end_response(end)).await;
            }
        }
    }
}

enum StreamEnd {
    Disabled,
    SourceStopped,
}

/// Next frame, skipping past any backlog this consumer failed to keep up with.
async fn next_frame(
    frames: &mut tokio::sync::broadcast::Receiver<FrameEvent>,
) -> Result<Frame, StreamEnd> {
    loop {
        match frames.recv().await {
            Ok(FrameEvent::Frame(frame)) => return Ok(frame),
            Ok(FrameEvent::Disabled) => return Err(StreamEnd::Disabled),
            Err(RecvError::Lagged(skipped)) => {
                // Latest-frame-wins: a slow consumer loses frames rather than stalling
                // the capture loop for everyone else.
                warn!(
                    skipped,
                    "subscriber fell behind; skipping to the newest frame"
                );
            }
            Err(RecvError::Closed) => return Err(StreamEnd::SourceStopped),
        }
    }
}

fn stream_end_response(end: StreamEnd) -> Response {
    match end {
        StreamEnd::Disabled => Response::err(
            ErrorCode::Disabled,
            "the source was disabled; the subscription has ended",
        ),
        StreamEnd::SourceStopped => {
            Response::err(ErrorCode::Internal, "the source is shutting down")
        }
    }
}

fn capture_error_response(e: CaptureError) -> Response {
    match e {
        CaptureError::Disabled => Response::err(
            ErrorCode::Disabled,
            "the source is disabled; enable it before capturing",
        ),
        CaptureError::Gone => {
            Response::err(ErrorCode::Internal, "the capture thread is not running")
        }
        CaptureError::Camera(CameraError::UnknownControl(name)) => Response::err(
            ErrorCode::UnknownControl,
            format!("no control named `{name}`"),
        ),
        CaptureError::Camera(e @ CameraError::ControlRange { .. }) => {
            Response::err(ErrorCode::ControlRange, e.to_string())
        }
        CaptureError::Camera(e) => Response::err(ErrorCode::CameraError, e.to_string()),
    }
}
