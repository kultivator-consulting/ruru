//! The `ruru-source` Unix-socket wire protocol.
//!
//! Every message on the wire is a `u32` big-endian length prefix followed by a body of
//! exactly that many bytes. The first byte of the body is a tag:
//!
//! ```text
//! 0x01  JSON control message (the remaining bytes are UTF-8 JSON)
//! 0x02  frame: a 28-byte fixed header, then `len` payload bytes
//!
//!       seq  u64 | timestamp_us u64 | width u16 | height u16 | fourcc [u8;4] | len u32
//! ```
//!
//! A connection carries exactly one request. The server answers with a JSON [`Response`];
//! if `ok` is true and the op streams frames, frame messages follow. `snapshot` sends
//! exactly one frame then closes; `subscribe` sends frames until the client goes away or
//! the source is disabled (which sends a terminal JSON error, then closes).
//!
//! Frame payloads are passed through byte-for-byte — the source never decodes them.

mod codec;

pub use codec::{
    CodecError, MAX_MESSAGE_LEN, Message, decode_body, encode_frame_body, encode_json_body,
    read_message, write_body, write_frame, write_json,
};

use bytes::Bytes;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Protocol version carried on every request. The server rejects anything else.
pub const PROTO_VERSION: u32 = 1;

/// Body tag for a JSON control message.
pub const TAG_JSON: u8 = 0x01;
/// Body tag for a frame message.
pub const TAG_FRAME: u8 = 0x02;
/// Size of the fixed binary frame header that follows [`TAG_FRAME`].
pub const FRAME_HEADER_LEN: usize = 28;

// ---------------------------------------------------------------------------
// FourCC
// ---------------------------------------------------------------------------

/// A V4L2 pixel format code, e.g. `MJPG`.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct FourCc(pub [u8; 4]);

impl FourCc {
    /// Motion-JPEG — what the Brio emits and what the source passes straight through.
    pub const MJPG: FourCc = FourCc(*b"MJPG");

    pub const fn as_bytes(&self) -> [u8; 4] {
        self.0
    }
}

impl fmt::Display for FourCc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for b in self.0 {
            // Non-printable bytes would corrupt logs; show them escaped.
            if b.is_ascii_graphic() {
                write!(f, "{}", b as char)?;
            } else {
                write!(f, "\\x{b:02x}")?;
            }
        }
        Ok(())
    }
}

impl fmt::Debug for FourCc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "FourCc({self})")
    }
}

impl std::str::FromStr for FourCc {
    type Err = InvalidFourCc;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let b = s.as_bytes();
        if b.len() != 4 {
            return Err(InvalidFourCc(s.to_owned()));
        }
        Ok(FourCc([b[0], b[1], b[2], b[3]]))
    }
}

#[derive(Debug, thiserror::Error)]
#[error("fourcc must be exactly 4 ASCII bytes, got {0:?}")]
pub struct InvalidFourCc(pub String);

impl Serialize for FourCc {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for FourCc {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

// ---------------------------------------------------------------------------
// Frames
// ---------------------------------------------------------------------------

/// One captured frame, exactly as it came off the device.
#[derive(Clone)]
pub struct Frame {
    /// Monotonic per-source counter, assigned by the capture loop (not the device).
    pub seq: u64,
    /// Capture time in microseconds since the Unix epoch.
    pub timestamp_us: u64,
    pub width: u16,
    pub height: u16,
    pub fourcc: FourCc,
    /// Encoded payload. `Bytes` so fan-out to N subscribers is a refcount bump, not N copies.
    pub data: Bytes,
}

impl fmt::Debug for Frame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Frame")
            .field("seq", &self.seq)
            .field("timestamp_us", &self.timestamp_us)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("fourcc", &self.fourcc)
            .field("len", &self.data.len())
            .finish()
    }
}

impl Frame {
    /// True if the payload starts with a JPEG SOI marker.
    pub fn looks_like_jpeg(&self) -> bool {
        self.data.starts_with(&[0xFF, 0xD8])
    }
}

// ---------------------------------------------------------------------------
// Requests
// ---------------------------------------------------------------------------

/// What a client is asking the source to do. One per connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    /// One frame, then the connection closes.
    Snapshot,
    /// Frames until the client disconnects or the source is disabled.
    Subscribe,
    /// Every control the device exposes.
    ListControls,
    /// One control by name.
    GetControl { name: String },
    /// Set one control; the response carries the re-read value. When `persist` is true the
    /// source records the value in its runtime control overrides (a writable state file) and
    /// re-applies it on every subsequent open — surviving a restart. `persist` defaults to
    /// false so older clients (and direct runtime tuning) keep the transient behaviour.
    SetControl {
        name: String,
        value: i64,
        #[serde(default)]
        persist: bool,
    },
    /// Current source state.
    Status,
    /// Master gate. `false` closes the camera and refuses capture until re-enabled.
    Enable { enabled: bool },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    /// Must equal [`PROTO_VERSION`].
    pub v: u32,
    #[serde(flatten)]
    pub op: Op,
}

impl Request {
    pub fn new(op: Op) -> Self {
        Self {
            v: PROTO_VERSION,
            op,
        }
    }
}

// ---------------------------------------------------------------------------
// Responses
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// The source is disabled; capture is refused until it is re-enabled.
    Disabled,
    /// Request carried an unsupported `v`.
    ProtoVersion,
    /// Malformed or unparseable request.
    BadRequest,
    /// No control by that name.
    UnknownControl,
    /// Value outside the control's `min..=max`.
    ControlRange,
    /// The device failed (open, capture, or ioctl).
    CameraError,
    /// Anything unexpected on the server side.
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireError {
    pub code: ErrorCode,
    pub message: String,
}

/// A control as the device describes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlDesc {
    pub name: String,
    pub value: i64,
    pub min: i64,
    pub max: i64,
    pub step: i64,
    pub default: i64,
}

/// A point-in-time view of the source, as the capture thread sees it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceStatus {
    /// Master gate.
    pub enabled: bool,
    /// Whether the device handle is currently held.
    pub open: bool,
    /// Always-on pin — holds the device open with zero subscribers.
    pub pinned: bool,
    /// Live attachment count (snapshots in flight + subscribers).
    pub subscribers: usize,
    /// Sequence number of the most recent broadcast frame.
    pub last_seq: u64,
    /// Measured capture rate over a short trailing window.
    pub fps: f32,
    pub idle_grace_secs: u64,
    pub camera_backend: String,
    /// Most recent *transient* device-layer failure (open/capture/ioctl), if any. Sticky
    /// until the next successful capture clears it.
    pub last_error: Option<String>,
    /// Locked controls that did not apply on the last open — a *persistent* configuration
    /// problem (a wrong name, or a gate ordered after the control it gates), distinct from
    /// a transient `last_error`. Empty when every locked control took. Not cleared by a
    /// successful capture, because the config is still wrong until it is fixed and the
    /// device reopened.
    #[serde(default)]
    pub control_warnings: Vec<String>,
}

/// The server's answer. Exactly one is sent per connection, before any frames.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<WireError>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<SourceStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control: Option<ControlDesc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controls: Option<Vec<ControlDesc>>,
}

impl Response {
    pub fn ok() -> Self {
        Self {
            ok: true,
            ..Default::default()
        }
    }

    pub fn err(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: Some(WireError {
                code,
                message: message.into(),
            }),
            ..Default::default()
        }
    }

    pub fn with_status(mut self, status: SourceStatus) -> Self {
        self.status = Some(status);
        self
    }

    pub fn with_control(mut self, control: ControlDesc) -> Self {
        self.control = Some(control);
        self
    }

    pub fn with_controls(mut self, controls: Vec<ControlDesc>) -> Self {
        self.controls = Some(controls);
        self
    }
}
