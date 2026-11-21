//! Real V4L2 backend (developed against the Logitech MX Brio), behind the [`Camera`] trait.
//!
//! Everything V4L2-specific is confined here. The design was validated against the real
//! device before it was written:
//!
//! * **Open by the `/dev/v4l/by-id/…` symlink every time.** Opening it follows the link to
//!   the current `/dev/videoN`, so a replug (which re-points the symlink) is recovered
//!   simply by reopening — no separate re-resolve step.
//! * **Persistent mmap stream.** The Brio emits a few incomplete JPEGs at stream start, so
//!   warmup (discard N frames) is mandatory, and warmup needs several frames from *one*
//!   `VIDIOC_STREAMON` — a per-grab stream would restart the garbage each time. The stream
//!   is created on open and held; `grab_frame` dequeues from it. The capture loop drives
//!   warmup by calling `grab_frame` and discarding.
//! * **Controls resolved by NAME → id at runtime.** The crate reports human names
//!   ("Focus, Absolute"); [`normalize`] maps them to the snake_case names config uses
//!   ("focus_absolute"), so no kernel control ids are hardcoded. `CtrlClass` category
//!   markers are skipped.
//!
//! `Stream` owns its own `Arc<Handle>` (cloned from the device), so it does not borrow the
//! `Device` — the two live side by side here, and controls can be set while streaming.

use super::{Camera, CameraError, CapturedFrame};
use crate::config::CameraConfig;
use bytes::Bytes;
use ruru_client::proto::{ControlDesc, FourCc as ProtoFourCc};
use std::collections::BTreeMap;
use std::path::PathBuf;
use tracing::{info, warn};
use v4l::buffer::Type as BufType;
use v4l::control::{Control, Type as CtrlType, Value};
use v4l::io::mmap::Stream as MmapStream;
use v4l::io::traits::CaptureStream;
use v4l::video::Capture;
use v4l::video::capture::Parameters;
use v4l::{Device, Format, FourCC};

/// Number of mmap buffers in the capture queue. Four is the conventional depth: enough to
/// absorb scheduling jitter without holding many multi-MB 4K buffers.
const BUFFER_COUNT: u32 = 4;

/// Cached per-control metadata, so `set`/`get` resolve name→id and range-check without
/// re-enumerating the device each call.
#[derive(Clone)]
struct ControlMeta {
    id: u32,
    typ: CtrlType,
    min: i64,
    max: i64,
    step: i64,
    default: i64,
}

pub struct V4l2Camera {
    device_path: PathBuf,
    fourcc: [u8; 4],
    req_width: u16,
    req_height: u16,
    fps: u32,

    dev: Option<Device>,
    stream: Option<MmapStream<'static>>,
    /// Actual negotiated dimensions (the device may snap to a supported size).
    active_width: u16,
    active_height: u16,
    /// Normalized control name → metadata, rebuilt on each open.
    controls: BTreeMap<String, ControlMeta>,
}

impl V4l2Camera {
    pub fn new(cfg: &CameraConfig) -> Result<Self, CameraError> {
        let device_path = cfg.device.clone().ok_or_else(|| {
            CameraError::Other("camera.device is required for the v4l2 backend".into())
        })?;
        let fourcc = fourcc_bytes(&cfg.fourcc)?;
        Ok(Self {
            device_path,
            fourcc,
            req_width: cfg.width,
            req_height: cfg.height,
            fps: cfg.fps,
            dev: None,
            stream: None,
            active_width: cfg.width,
            active_height: cfg.height,
            controls: BTreeMap::new(),
        })
    }

    fn dev(&self) -> Result<&Device, CameraError> {
        self.dev
            .as_ref()
            .ok_or_else(|| CameraError::Capture("device is not open".into()))
    }
}

impl Camera for V4l2Camera {
    fn backend(&self) -> &'static str {
        "v4l2"
    }

    fn ensure_open(&mut self) -> Result<(), CameraError> {
        if self.is_open() {
            return Ok(());
        }
        // Reopen from scratch — this is also the replug path (the by-id symlink now points
        // at the current node).
        let device = self.device_path.display().to_string();
        let open_err = |message: String| CameraError::Open {
            device: device.clone(),
            message,
        };

        let dev = Device::with_path(&self.device_path).map_err(|e| open_err(e.to_string()))?;

        // Negotiate MJPG at the requested size.
        let requested = Format::new(
            self.req_width as u32,
            self.req_height as u32,
            FourCC::new(&self.fourcc),
        );
        let applied = Capture::set_format(&dev, &requested)
            .map_err(|e| open_err(format!("set_format: {e}")))?;

        let want = String::from_utf8_lossy(&self.fourcc).into_owned();
        if applied.fourcc.to_string() != want {
            return Err(open_err(format!(
                "device negotiated {} instead of {want}",
                applied.fourcc
            )));
        }
        self.active_width = applied.width as u16;
        self.active_height = applied.height as u16;
        if (applied.width, applied.height) != (self.req_width as u32, self.req_height as u32) {
            warn!(
                requested = %format!("{}x{}", self.req_width, self.req_height),
                got = %format!("{}x{}", applied.width, applied.height),
                "device snapped to a different resolution"
            );
        }

        // Frame rate is best-effort — some devices ignore it, and it must not fail the open.
        match Capture::set_params(&dev, &Parameters::with_fps(self.fps)) {
            Ok(p) => info!(fps = self.fps, interval = ?p.interval, "capture params set"),
            Err(e) => warn!(error = %e, "could not set fps; using the device default"),
        }

        self.build_control_cache(&dev)?;

        let stream = MmapStream::with_buffers(&dev, BufType::VideoCapture, BUFFER_COUNT)
            .map_err(|e| open_err(format!("starting the mmap stream: {e}")))?;

        info!(
            device = %device,
            resolution = %format!("{}x{}", self.active_width, self.active_height),
            fourcc = %want,
            controls = self.controls.len(),
            "v4l2 device opened"
        );
        self.dev = Some(dev);
        self.stream = Some(stream);
        Ok(())
    }

    fn close(&mut self) {
        // Order matters: the stream's Drop issues STREAMOFF and unmaps before the fd closes.
        self.stream = None;
        self.dev = None;
        self.controls.clear();
    }

    fn is_open(&self) -> bool {
        self.dev.is_some() && self.stream.is_some()
    }

    fn grab_frame(&mut self) -> Result<CapturedFrame, CameraError> {
        let (width, height) = (self.active_width, self.active_height);
        let fourcc = ProtoFourCc(self.fourcc);

        let stream = self
            .stream
            .as_mut()
            .ok_or_else(|| CameraError::Capture("device is not open".into()))?;

        let (buf, meta) = stream
            .next()
            .map_err(|e| CameraError::Capture(format!("dequeue: {e}")))?;

        // MJPG buffers are max-sized; the real JPEG is the first `bytesused` bytes.
        let len = (meta.bytesused as usize).min(buf.len());
        let data = Bytes::copy_from_slice(&buf[..len]);

        Ok(CapturedFrame {
            width,
            height,
            fourcc,
            data,
            // V4L2 timestamps are monotonic, not epoch — let the capture loop stamp
            // wall-clock so the wire `timestamp_us` means "when captured" like the mock.
            timestamp_us: None,
        })
    }

    fn query_controls(&mut self) -> Result<Vec<ControlDesc>, CameraError> {
        let dev = self.dev()?;
        self.controls
            .iter()
            .map(|(name, meta)| control_desc(dev, name, meta))
            .collect()
    }

    fn set_control(&mut self, name: &str, value: i64) -> Result<ControlDesc, CameraError> {
        let dev = self.dev()?;
        // Accept either the snake_case config name or the device's human name.
        let norm = normalize(name);
        let meta = self
            .controls
            .get(&norm)
            .ok_or_else(|| CameraError::UnknownControl(name.to_owned()))?;

        if value < meta.min || value > meta.max {
            return Err(CameraError::ControlRange {
                name: norm.clone(),
                value,
                min: meta.min,
                max: meta.max,
            });
        }

        // Match the control's declared type: booleans take Value::Boolean, everything else
        // (integers and menus) takes Value::Integer.
        let wire = match meta.typ {
            CtrlType::Boolean => Value::Boolean(value != 0),
            _ => Value::Integer(value),
        };
        dev.set_control(Control {
            id: meta.id,
            value: wire,
        })
        .map_err(|e| CameraError::Other(format!("set_control `{norm}`: {e}")))?;

        // Re-read: the device snaps to its step grid and gates inactive controls, so the
        // truth is what it now reports, not what we asked for.
        control_desc(dev, &norm, meta)
    }
}

impl V4l2Camera {
    fn build_control_cache(&mut self, dev: &Device) -> Result<(), CameraError> {
        self.controls.clear();
        let descs = dev
            .query_controls()
            .map_err(|e| CameraError::Other(format!("query_controls: {e}")))?;
        for d in descs {
            if matches!(d.typ, CtrlType::CtrlClass) {
                continue;
            }
            self.controls.insert(
                normalize(&d.name),
                ControlMeta {
                    id: d.id,
                    typ: d.typ,
                    min: d.minimum,
                    max: d.maximum,
                    step: d.step as i64,
                    default: d.default,
                },
            );
        }
        Ok(())
    }
}

fn read_value(dev: &Device, id: u32) -> Result<i64, CameraError> {
    let ctrl = dev
        .control(id)
        .map_err(|e| CameraError::Other(format!("reading control {id:#010x}: {e}")))?;
    Ok(match ctrl.value {
        Value::Integer(v) => v,
        Value::Boolean(b) => b as i64,
        _ => 0,
    })
}

fn control_desc(dev: &Device, name: &str, meta: &ControlMeta) -> Result<ControlDesc, CameraError> {
    Ok(ControlDesc {
        name: name.to_owned(),
        value: read_value(dev, meta.id)?,
        min: meta.min,
        max: meta.max,
        step: meta.step,
        default: meta.default,
    })
}

fn fourcc_bytes(s: &str) -> Result<[u8; 4], CameraError> {
    let b = s.as_bytes();
    if b.len() != 4 {
        return Err(CameraError::Other(format!(
            "fourcc must be exactly 4 bytes, got {s:?}"
        )));
    }
    Ok([b[0], b[1], b[2], b[3]])
}

/// Map a V4L2 human control name to the snake_case name config uses.
/// "Focus, Absolute" → "focus_absolute"; "White Balance, Automatic" → "white_balance_automatic".
fn normalize(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut prev_sep = true; // trims leading separators
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            prev_sep = false;
        } else if !prev_sep {
            out.push('_');
            prev_sep = true;
        }
    }
    while out.ends_with('_') {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::normalize;

    #[test]
    fn normalizes_brio_control_names_to_config_names() {
        // Exactly the mapping verified against the real device during refine.
        let cases = [
            ("Brightness", "brightness"),
            ("White Balance, Automatic", "white_balance_automatic"),
            ("White Balance Temperature", "white_balance_temperature"),
            ("Power Line Frequency", "power_line_frequency"),
            ("Auto Exposure", "auto_exposure"),
            ("Exposure Time, Absolute", "exposure_time_absolute"),
            ("Exposure, Dynamic Framerate", "exposure_dynamic_framerate"),
            ("Focus, Absolute", "focus_absolute"),
            ("Focus, Automatic Continuous", "focus_automatic_continuous"),
            ("Pan, Absolute", "pan_absolute"),
            ("Backlight Compensation", "backlight_compensation"),
        ];
        for (human, expected) in cases {
            assert_eq!(normalize(human), expected, "for {human:?}");
        }
    }

    #[test]
    fn normalize_is_idempotent_on_already_snake_case() {
        assert_eq!(normalize("focus_absolute"), "focus_absolute");
        assert_eq!(normalize("power_line_frequency"), "power_line_frequency");
    }
}
