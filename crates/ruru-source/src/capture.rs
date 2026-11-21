//! The single-owner capture loop.
//!
//! Exactly one OS thread ever holds the [`Camera`], because a UVC device has exactly one
//! streaming owner. Async callers reach it by channel round-trip: commands go in over a
//! `std::sync::mpsc` (chosen over `tokio::sync::mpsc` because it has `recv_timeout`, which
//! is what lets one blocking call serve both frame pacing and the idle-close deadline),
//! and replies come back over a `oneshot`.
//!
//! Frames go out over a `broadcast`, so one capture feeds every subscriber. Payloads are
//! `Bytes`, so fan-out to N consumers is N refcount bumps rather than N copies.

use crate::camera::{Camera, CameraError};
use crate::config::Config;
use crate::controls_state::{self, Overrides};
use ruru_client::proto::{ControlDesc, FourCc, Frame, SourceStatus};
use std::collections::VecDeque;
use std::ops::ControlFlow;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, TryRecvError, channel};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{broadcast, oneshot};
use tracing::{debug, error, info, warn};

/// What subscribers observe. A deliberate shutdown is an event, not silence — otherwise
/// a client cannot tell "disabled" from "the camera is just slow".
#[derive(Debug, Clone)]
pub enum FrameEvent {
    Frame(Frame),
    /// The master gate closed; live subscriptions end here.
    Disabled,
}

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("capture thread is not running")]
    Gone,

    #[error("source is disabled")]
    Disabled,

    #[error(transparent)]
    Camera(#[from] CameraError),
}

type Reply<T> = oneshot::Sender<Result<T, CaptureError>>;

enum Command {
    Attach(Reply<()>),
    Detach,
    Status(oneshot::Sender<SourceStatus>),
    SetEnabled {
        enabled: bool,
        reply: oneshot::Sender<SourceStatus>,
    },
    ListControls(Reply<Vec<ControlDesc>>),
    GetControl {
        name: String,
        reply: Reply<ControlDesc>,
    },
    SetControl {
        name: String,
        value: i64,
        persist: bool,
        reply: Reply<ControlDesc>,
    },
    Shutdown(oneshot::Sender<()>),
}

/// Async-side handle to the capture thread. Cheap to clone; hand one to each connection.
#[derive(Clone)]
pub struct CaptureHandle {
    cmd_tx: Sender<Command>,
    frames: broadcast::Sender<FrameEvent>,
}

impl CaptureHandle {
    /// Start receiving frames. Subscribe *before* attaching, or a frame captured in
    /// between the two would be missed.
    pub fn subscribe_frames(&self) -> broadcast::Receiver<FrameEvent> {
        self.frames.subscribe()
    }

    /// Register interest in frames. The device opens if it was idle, and stays open until
    /// the returned guard drops (plus the idle grace).
    pub async fn attach(&self) -> Result<Attachment, CaptureError> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(Command::Attach(tx))
            .map_err(|_| CaptureError::Gone)?;
        rx.await.map_err(|_| CaptureError::Gone)??;
        Ok(Attachment {
            cmd_tx: self.cmd_tx.clone(),
        })
    }

    pub async fn status(&self) -> Result<SourceStatus, CaptureError> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(Command::Status(tx))
            .map_err(|_| CaptureError::Gone)?;
        rx.await.map_err(|_| CaptureError::Gone)
    }

    pub async fn set_enabled(&self, enabled: bool) -> Result<SourceStatus, CaptureError> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(Command::SetEnabled { enabled, reply: tx })
            .map_err(|_| CaptureError::Gone)?;
        rx.await.map_err(|_| CaptureError::Gone)
    }

    pub async fn list_controls(&self) -> Result<Vec<ControlDesc>, CaptureError> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(Command::ListControls(tx))
            .map_err(|_| CaptureError::Gone)?;
        rx.await.map_err(|_| CaptureError::Gone)?
    }

    pub async fn get_control(&self, name: String) -> Result<ControlDesc, CaptureError> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(Command::GetControl { name, reply: tx })
            .map_err(|_| CaptureError::Gone)?;
        rx.await.map_err(|_| CaptureError::Gone)?
    }

    pub async fn set_control(
        &self,
        name: String,
        value: i64,
        persist: bool,
    ) -> Result<ControlDesc, CaptureError> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(Command::SetControl {
                name,
                value,
                persist,
                reply: tx,
            })
            .map_err(|_| CaptureError::Gone)?;
        rx.await.map_err(|_| CaptureError::Gone)?
    }

    /// Ask the capture thread to close the device and exit.
    pub async fn shutdown(&self) {
        let (tx, rx) = oneshot::channel();
        if self.cmd_tx.send(Command::Shutdown(tx)).is_ok() {
            let _ = rx.await;
        }
    }
}

/// Holds the reference count up. Detaching on drop is what makes the count correct even
/// when a client vanishes mid-stream or a task is cancelled.
pub struct Attachment {
    cmd_tx: Sender<Command>,
}

impl Drop for Attachment {
    fn drop(&mut self) {
        let _ = self.cmd_tx.send(Command::Detach);
    }
}

// ---------------------------------------------------------------------------
// Capture thread
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct CaptureConfig {
    fps: u32,
    warmup_frames: u32,
    idle_grace: Duration,
    reopen_backoff: Duration,
    /// The `[camera.controls]` from the config file, in file order. Immutable — operator
    /// tuning goes into `overrides`, layered on top, so the config stays the base truth.
    base_controls: Vec<(String, i64)>,
    /// Where `overrides` is persisted, if persistence is configured.
    controls_state_path: Option<PathBuf>,
    fourcc: FourCc,
}

/// Parked wait when nothing is pending — a command wakes the thread immediately, so this
/// only bounds how long a fully idle source sleeps between no-op checks.
const IDLE_PARK: Duration = Duration::from_secs(3600);

struct CaptureState {
    cam: Box<dyn Camera>,
    cfg: CaptureConfig,
    frames: broadcast::Sender<FrameEvent>,
    refcount: usize,
    enabled: bool,
    pinned: bool,
    seq: u64,
    last_seq: u64,
    idle_since: Option<Instant>,
    backoff_until: Option<Instant>,
    next_frame_at: Instant,
    fps: FpsTracker,
    last_error: Option<String>,
    /// Locked controls that failed to apply on the most recent open. Persistent config
    /// problem, refreshed each open — kept separate from `last_error` so a successful
    /// capture does not erase it.
    control_warnings: Vec<String>,
    /// Operator-set control values, layered over `cfg.base_controls` on every open and
    /// persisted to `cfg.controls_state_path`. Loaded from that file at startup.
    overrides: Overrides,
}

pub fn spawn(
    camera: Box<dyn Camera>,
    config: &Config,
) -> (CaptureHandle, std::thread::JoinHandle<()>) {
    let (cmd_tx, cmd_rx) = channel();
    let (frames, _) = broadcast::channel(config.source.broadcast_capacity);

    // Operator overrides persisted from a previous run, layered over the config on open.
    let overrides = match &config.source.controls_state_path {
        Some(path) => {
            let loaded = controls_state::load(path);
            if !loaded.is_empty() {
                info!(count = loaded.len(), path = %path.display(), "loaded persisted control overrides");
            }
            loaded
        }
        None => Overrides::new(),
    };

    let cfg = CaptureConfig {
        fps: config.camera.fps,
        warmup_frames: config.camera.warmup_frames,
        idle_grace: Duration::from_secs(config.source.idle_grace_secs),
        reopen_backoff: Duration::from_secs(config.camera.reopen_backoff_secs),
        base_controls: config
            .camera
            .controls
            .iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect(),
        controls_state_path: config.source.controls_state_path.clone(),
        fourcc: config
            .camera
            .fourcc
            .parse()
            .unwrap_or(ruru_client::proto::FourCc::MJPG),
    };

    let state = CaptureState {
        cam: camera,
        cfg,
        frames: frames.clone(),
        refcount: 0,
        enabled: config.source.enabled,
        pinned: config.source.pin,
        seq: 0,
        last_seq: 0,
        idle_since: None,
        backoff_until: None,
        next_frame_at: Instant::now(),
        fps: FpsTracker::default(),
        last_error: None,
        control_warnings: Vec::new(),
        overrides,
    };

    let pinned = state.pinned;
    let enabled = state.enabled;

    let join = std::thread::Builder::new()
        .name("ruru-capture".into())
        .spawn(move || run(state, cmd_rx))
        .expect("spawning the capture thread");

    if pinned {
        info!("camera pin is set: the device will be held open with no subscribers");
    }
    if !enabled {
        warn!("CAMERA DISABLED at startup: capture is refused until the source is enabled");
    }

    (CaptureHandle { cmd_tx, frames }, join)
}

fn run(mut state: CaptureState, cmd_rx: Receiver<Command>) {
    let interval = Duration::from_secs_f64(1.0 / state.cfg.fps as f64);
    state.next_frame_at = Instant::now();

    loop {
        // Anything already queued is applied before deciding what to do this pass.
        loop {
            match cmd_rx.try_recv() {
                Ok(cmd) => {
                    if state.handle(cmd).is_break() {
                        state.finish();
                        return;
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    state.finish();
                    return;
                }
            }
        }

        let wait = state.tick(interval);
        if wait.is_zero() {
            continue;
        }

        match cmd_rx.recv_timeout(wait) {
            Ok(cmd) => {
                if state.handle(cmd).is_break() {
                    state.finish();
                    return;
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                state.finish();
                return;
            }
        }
    }
}

impl CaptureState {
    /// Advance one step and report how long the thread may sleep before the next one.
    /// `Duration::ZERO` means "go round again immediately".
    fn tick(&mut self, interval: Duration) -> Duration {
        let now = Instant::now();
        let wanted = self.enabled && (self.pinned || self.refcount > 0);

        if !wanted {
            return self.tick_idle(now);
        }

        // Something wants frames, so any idle countdown is void.
        self.idle_since = None;

        if !self.cam.is_open() {
            if let Some(until) = self.backoff_until
                && now < until
            {
                return until - now;
            }
            if let Err(e) = self.open_device() {
                let message = e.to_string();
                warn!(error = %message, "device open failed; backing off before retry");
                self.last_error = Some(message);
                self.backoff_until = Some(now + self.cfg.reopen_backoff);
                return self.cfg.reopen_backoff;
            }
            self.backoff_until = None;
            self.next_frame_at = Instant::now();
        }

        if now < self.next_frame_at {
            return self.next_frame_at - now;
        }

        self.capture_once(interval)
    }

    fn tick_idle(&mut self, now: Instant) -> Duration {
        if !self.cam.is_open() {
            self.idle_since = None;
            return IDLE_PARK;
        }

        let started = *self.idle_since.get_or_insert(now);
        let deadline = started + self.cfg.idle_grace;
        if now >= deadline {
            info!(
                grace_secs = self.cfg.idle_grace.as_secs(),
                "no subscribers; closing device"
            );
            self.cam.close();
            self.fps.reset();
            self.idle_since = None;
            return IDLE_PARK;
        }
        deadline - now
    }

    fn capture_once(&mut self, interval: Duration) -> Duration {
        match self.cam.grab_frame() {
            Ok(captured) => {
                let timestamp_us = captured.timestamp_us.unwrap_or_else(now_us);
                let frame = Frame {
                    seq: self.seq,
                    timestamp_us,
                    width: captured.width,
                    height: captured.height,
                    fourcc: captured.fourcc,
                    data: captured.data,
                };
                self.last_seq = self.seq;
                self.seq += 1;
                self.fps.record(Instant::now());
                self.last_error = None;

                // Err only means nobody is listening right now, which is normal when a
                // snapshot's attach lands before its subscription does.
                let _ = self.frames.send(FrameEvent::Frame(frame));

                // Never burst to catch up after a stall — that would spike the device.
                self.next_frame_at = (self.next_frame_at + interval).max(Instant::now());
                Duration::ZERO
            }
            Err(e) => {
                // The replug path: a capture failure means the handle is no longer good,
                // so drop it and let the next pass re-resolve and reopen from scratch.
                let message = e.to_string();
                warn!(error = %message, "capture failed; closing device and backing off");
                self.last_error = Some(message);
                self.cam.close();
                self.fps.reset();
                self.backoff_until = Some(Instant::now() + self.cfg.reopen_backoff);
                self.cfg.reopen_backoff
            }
        }
    }

    fn open_device(&mut self) -> Result<(), CameraError> {
        self.cam.ensure_open()?;
        info!(backend = self.cam.backend(), fourcc = %self.cfg.fourcc, "device opened");

        self.apply_locked_controls();

        if let Err(e) = self.warmup() {
            self.cam.close();
            return Err(e);
        }
        Ok(())
    }

    /// Reapply the configured controls on every open — a device that was replugged comes
    /// back at its defaults, which would silently un-lock focus and exposure.
    ///
    /// Controls are applied in config-file order (preserved by the `IndexMap` behind
    /// `camera.controls`), because three Brio controls are `inactive` until a companion
    /// auto control is switched off first. Each write is then re-read: setting an inactive
    /// control is *accepted* but does not take, so trusting the call to succeed would let
    /// a mis-ordered config silently ship with autofocus still on.
    /// The controls to apply on open: the config's `[camera.controls]` in file order, with
    /// any operator override substituted in place, then override-only controls in the order
    /// they were set. One ordered list, so a gate still precedes the control it gates.
    fn effective_controls(&self) -> Vec<(String, i64)> {
        let mut out: Vec<(String, i64)> =
            Vec::with_capacity(self.cfg.base_controls.len() + self.overrides.len());
        for (name, value) in &self.cfg.base_controls {
            out.push((
                name.clone(),
                self.overrides.get(name).copied().unwrap_or(*value),
            ));
        }
        for (name, value) in &self.overrides {
            if !self.cfg.base_controls.iter().any(|(n, _)| n == name) {
                out.push((name.clone(), *value));
            }
        }
        out
    }

    fn apply_locked_controls(&mut self) {
        // Fresh each open — a control that failed last time may succeed now (e.g. after a
        // replug), and one that succeeds now must not carry a stale warning.
        self.control_warnings.clear();
        let effective = self.effective_controls();
        for (name, value) in &effective {
            match self.cam.set_control(name, *value) {
                Ok(applied) if applied.value == *value => {
                    debug!(control = %name, value, "locked control applied");
                }
                // Within one step of the request: the device snapped to its grid, which is
                // expected and fine — the returned value is the truth.
                Ok(applied) if (applied.value - *value).abs() < applied.step.max(1) => {
                    debug!(
                        control = %name,
                        requested = value,
                        applied = applied.value,
                        "locked control snapped to the device's step grid"
                    );
                }
                // The write was accepted but the value did not change — almost always an
                // inactive control whose auto gate is still on, i.e. a misordered config
                // (the gate listed after the control, or omitted). Loud, because an
                // unlocked focus or exposure makes captures incomparable with no other
                // symptom.
                Ok(applied) => {
                    warn!(
                        control = %name,
                        requested = value,
                        actual = applied.value,
                        "locked control did not take — it may be inactive until its auto \
                         control is switched off first; check the order in [camera.controls]"
                    );
                    self.control_warnings.push(format!(
                        "`{name}` did not take (requested {value}, still {})",
                        applied.value
                    ));
                }
                Err(e) => {
                    // Not fatal: refusing to serve because one control name is wrong would
                    // be worse than serving with it unset. Surfaced in
                    // status.control_warnings so it cannot pass unnoticed.
                    warn!(control = %name, error = %e, "could not apply locked control");
                    self.control_warnings.push(format!("`{name}`: {e}"));
                }
            }
        }
    }

    /// Record an operator override (re-read device value) and write it through to the state
    /// file. `insert` on the `IndexMap` updates in place when present, so re-tuning a control
    /// keeps its position and the file does not grow. A write failure is logged, not returned:
    /// the control did take on the device — only its durability failed — and turning the whole
    /// `set_control` into an error would be worse than a live-but-unsaved value.
    fn persist_override(&mut self, name: &str, value: i64) {
        self.overrides.insert(name.to_string(), value);
        match &self.cfg.controls_state_path {
            Some(path) => match controls_state::save(path, &self.overrides) {
                Ok(()) => debug!(control = %name, value, "override persisted"),
                Err(e) => {
                    warn!(control = %name, error = %e, "control applied but the override could not be persisted")
                }
            },
            None => debug!(
                control = %name,
                "persist requested but no controls_state_path is configured — applied for this run only"
            ),
        }
    }

    fn warmup(&mut self) -> Result<(), CameraError> {
        for n in 0..self.cfg.warmup_frames {
            self.cam.grab_frame().map_err(|e| {
                warn!(frame = n, error = %e, "warmup capture failed");
                e
            })?;
        }
        if self.cfg.warmup_frames > 0 {
            debug!(frames = self.cfg.warmup_frames, "warmup complete");
        }
        Ok(())
    }

    fn handle(&mut self, cmd: Command) -> ControlFlow<()> {
        match cmd {
            Command::Attach(reply) => {
                let result = if self.enabled {
                    self.refcount += 1;
                    debug!(subscribers = self.refcount, "attach");
                    Ok(())
                } else {
                    Err(CaptureError::Disabled)
                };
                let _ = reply.send(result);
            }

            Command::Detach => {
                self.refcount = self.refcount.saturating_sub(1);
                debug!(subscribers = self.refcount, "detach");
            }

            Command::Status(reply) => {
                let _ = reply.send(self.status());
            }

            Command::SetEnabled { enabled, reply } => {
                if enabled != self.enabled {
                    self.enabled = enabled;
                    if enabled {
                        // Deliberately loud in both directions: an always-on camera
                        // pointed at an office is a surveillance capability, and the
                        // epic requires its state changes be visible, not silent.
                        warn!("CAMERA ENABLED: capture is now permitted");
                        self.next_frame_at = Instant::now();
                    } else {
                        warn!("CAMERA DISABLED: closing device and ending subscriptions");
                        self.cam.close();
                        self.fps.reset();
                        self.idle_since = None;
                        let _ = self.frames.send(FrameEvent::Disabled);
                    }
                }
                let _ = reply.send(self.status());
            }

            Command::ListControls(reply) => {
                let result = self.with_device(|cam| cam.query_controls());
                let _ = reply.send(result);
            }

            Command::GetControl { name, reply } => {
                let result = self.with_device(|cam| cam.get_control(&name));
                let _ = reply.send(result);
            }

            Command::SetControl {
                name,
                value,
                persist,
                reply,
            } => {
                let result = self.with_device(|cam| cam.set_control(&name, value));
                if let Ok(applied) = &result {
                    info!(control = %name, value = applied.value, persist, "control set");
                    if persist {
                        self.persist_override(&name, applied.value);
                    }
                }
                let _ = reply.send(result);
            }

            Command::Shutdown(ack) => {
                let _ = ack.send(());
                return ControlFlow::Break(());
            }
        }
        ControlFlow::Continue(())
    }

    /// Run a device operation, opening the device first if it is closed.
    ///
    /// Opening here without an attachment is fine: the next idle pass starts the grace
    /// countdown and closes it again, exactly as it would after a snapshot.
    fn with_device<T>(
        &mut self,
        f: impl FnOnce(&mut dyn Camera) -> Result<T, CameraError>,
    ) -> Result<T, CaptureError> {
        if !self.enabled {
            return Err(CaptureError::Disabled);
        }
        if let Err(e) = self.cam.ensure_open() {
            self.last_error = Some(e.to_string());
            return Err(e.into());
        }
        f(&mut *self.cam).map_err(Into::into)
    }

    fn status(&self) -> SourceStatus {
        SourceStatus {
            enabled: self.enabled,
            open: self.cam.is_open(),
            pinned: self.pinned,
            subscribers: self.refcount,
            last_seq: self.last_seq,
            fps: self.fps.fps(),
            idle_grace_secs: self.cfg.idle_grace.as_secs(),
            camera_backend: self.cam.backend().to_string(),
            last_error: self.last_error.clone(),
            control_warnings: self.control_warnings.clone(),
        }
    }

    fn finish(&mut self) {
        self.cam.close();
        info!("capture thread stopped");
    }
}

fn now_us() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or_else(|e| {
            error!(error = %e, "system clock is before the unix epoch");
            0
        })
}

/// Capture rate over a short trailing window.
#[derive(Default)]
struct FpsTracker {
    samples: VecDeque<Instant>,
}

impl FpsTracker {
    const WINDOW: usize = 32;

    fn record(&mut self, at: Instant) {
        self.samples.push_back(at);
        while self.samples.len() > Self::WINDOW {
            self.samples.pop_front();
        }
    }

    fn fps(&self) -> f32 {
        let (Some(first), Some(last)) = (self.samples.front(), self.samples.back()) else {
            return 0.0;
        };
        let span = last.duration_since(*first).as_secs_f32();
        if span <= 0.0 {
            return 0.0;
        }
        (self.samples.len() - 1) as f32 / span
    }

    fn reset(&mut self) {
        self.samples.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fps_tracker_measures_a_steady_rate() {
        let mut t = FpsTracker::default();
        assert_eq!(t.fps(), 0.0, "no samples");

        let base = Instant::now();
        for n in 0..10 {
            t.record(base + Duration::from_millis(n * 100));
        }
        assert!((t.fps() - 10.0).abs() < 0.01, "got {}", t.fps());

        t.reset();
        assert_eq!(t.fps(), 0.0);
    }

    #[test]
    fn fps_tracker_window_is_bounded() {
        let mut t = FpsTracker::default();
        let base = Instant::now();
        for n in 0..1000 {
            t.record(base + Duration::from_millis(n));
        }
        assert_eq!(t.samples.len(), FpsTracker::WINDOW);
    }
}
