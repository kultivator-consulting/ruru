//! The device seam.
//!
//! Everything above this module is device-agnostic; everything device-specific lives
//! behind [`Camera`]. `MockCamera` and the real V4L2 backend both implement it, so the
//! capture loop, the socket server and the client never know which one is running.
//!
//! Nothing here knows about whiteboards. Dewarp, crop and calibration are `ruru-mcp`'s
//! job, by design — this component only knows cameras and frames.

mod font;
pub mod mock;
pub mod v4l2;

use bytes::Bytes;
use ruru_client::proto::{ControlDesc, FourCc};

pub use mock::{MockCamera, MockStats};

/// A frame as it came off the device, before the capture loop stamps it.
#[derive(Debug, Clone)]
pub struct CapturedFrame {
    pub width: u16,
    pub height: u16,
    pub fourcc: FourCc,
    /// Encoded bytes, passed through untouched — no decode in the hot loop.
    pub data: Bytes,
    /// Device-supplied capture time when it has one; the loop fills in wall-clock if not.
    pub timestamp_us: Option<u64>,
}

#[derive(Debug, thiserror::Error)]
pub enum CameraError {
    #[error("opening {device}: {message}")]
    Open { device: String, message: String },

    #[error("capturing a frame: {0}")]
    Capture(String),

    #[error("no control named `{0}`")]
    UnknownControl(String),

    #[error("control `{name}` accepts {min}..={max}, got {value}")]
    ControlRange {
        name: String,
        value: i64,
        min: i64,
        max: i64,
    },

    #[error("device layer: {0}")]
    Other(String),
}

/// One owner, one device handle.
///
/// Implementations are driven exclusively by the capture thread, so they need only be
/// `Send`, never `Sync`, and never need internal locking.
pub trait Camera: Send {
    /// Human-readable backend name, surfaced in `status`.
    fn backend(&self) -> &'static str;

    /// Acquire the device. Idempotent — a no-op when already open.
    fn ensure_open(&mut self) -> Result<(), CameraError>;

    /// Release the device. Idempotent.
    fn close(&mut self);

    fn is_open(&self) -> bool;

    /// Block until the next frame is available and return it.
    fn grab_frame(&mut self) -> Result<CapturedFrame, CameraError>;

    fn query_controls(&mut self) -> Result<Vec<ControlDesc>, CameraError>;

    fn get_control(&mut self, name: &str) -> Result<ControlDesc, CameraError> {
        self.query_controls()?
            .into_iter()
            .find(|c| c.name == name)
            .ok_or_else(|| CameraError::UnknownControl(name.to_owned()))
    }

    /// Set a control and return it as re-read from the device.
    fn set_control(&mut self, name: &str, value: i64) -> Result<ControlDesc, CameraError>;
}
