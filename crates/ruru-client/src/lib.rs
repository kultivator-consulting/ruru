//! Client for the `ruru-source` frame daemon.
//!
//! `ruru-source` owns the camera; a UVC device has exactly one streaming owner, so every
//! other component reaches frames through this crate rather than opening the device.
//!
//! ```no_run
//! # async fn example() -> Result<(), ruru_client::ClientError> {
//! use ruru_client::SourceClient;
//!
//! let client = SourceClient::new("/run/ruru/source.sock");
//!
//! let frame = client.snapshot().await?;
//! println!("{}x{} {} ({} bytes)", frame.width, frame.height, frame.fourcc, frame.data.len());
//!
//! let mut sub = client.subscribe().await?;
//! while let Some(frame) = sub.next().await? {
//!     println!("seq {}", frame.seq);
//! }
//! # Ok(())
//! # }
//! ```
//!
//! This crate also defines the wire protocol itself (in [`proto`]) — it is the shared
//! contract, so `ruru-source` depends on it too.

pub mod proto;

mod client;
mod error;

pub use client::{DEFAULT_SOCKET_PATH, FrameStream, SourceClient};
pub use error::{ClientError, Result};
pub use proto::{ControlDesc, ErrorCode, FourCc, Frame, SourceStatus};

pub use bytes::Bytes;
