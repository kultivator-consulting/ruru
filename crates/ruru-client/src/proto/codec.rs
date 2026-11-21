//! Framing for the source socket.
//!
//! The encode/decode half is deliberately synchronous and allocation-explicit so it can be
//! unit-tested without a runtime; the async helpers on top are thin length-prefix wrappers.

use super::{FRAME_HEADER_LEN, FourCc, Frame, TAG_FRAME, TAG_JSON};
use bytes::Bytes;
use serde::Serialize;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Hard cap on a single message. A 4K MJPEG frame is 1-3 MB, so this is generous —
/// it exists so a malformed or hostile length prefix can't drive an unbounded allocation.
pub const MAX_MESSAGE_LEN: u32 = 32 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("message of {len} bytes exceeds the {cap} byte cap")]
    TooLarge { len: u32, cap: u32 },

    #[error("truncated message: need {need} bytes, have {have}")]
    Truncated { need: usize, have: usize },

    #[error("unknown message tag {0:#04x}")]
    UnknownTag(u8),

    #[error("empty message body")]
    Empty,

    #[error("declared payload length {declared} does not match body remainder {actual}")]
    LengthMismatch { declared: u32, actual: usize },

    #[error("malformed json: {0}")]
    Json(#[from] serde_json::Error),
}

/// A decoded message body.
#[derive(Debug)]
pub enum Message {
    /// Raw JSON bytes — the caller deserializes into whatever it expects.
    Json(Bytes),
    Frame(Frame),
}

impl Message {
    /// Deserialize a JSON message into `T`. Returns `None` for frame messages.
    pub fn json_as<T: serde::de::DeserializeOwned>(&self) -> Option<Result<T, CodecError>> {
        match self {
            Message::Json(bytes) => Some(serde_json::from_slice(bytes).map_err(CodecError::from)),
            Message::Frame(_) => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Encode
// ---------------------------------------------------------------------------

/// Tag byte + UTF-8 JSON. Does not include the outer length prefix.
pub fn encode_json_body(value: &impl Serialize) -> Result<Vec<u8>, CodecError> {
    let json = serde_json::to_vec(value)?;
    let mut body = Vec::with_capacity(1 + json.len());
    body.push(TAG_JSON);
    body.extend_from_slice(&json);
    Ok(body)
}

/// Tag byte + fixed header + payload. Does not include the outer length prefix.
pub fn encode_frame_body(frame: &Frame) -> Vec<u8> {
    let mut body = Vec::with_capacity(1 + FRAME_HEADER_LEN + frame.data.len());
    body.push(TAG_FRAME);
    body.extend_from_slice(&frame.seq.to_be_bytes());
    body.extend_from_slice(&frame.timestamp_us.to_be_bytes());
    body.extend_from_slice(&frame.width.to_be_bytes());
    body.extend_from_slice(&frame.height.to_be_bytes());
    body.extend_from_slice(&frame.fourcc.as_bytes());
    body.extend_from_slice(&(frame.data.len() as u32).to_be_bytes());
    body.extend_from_slice(&frame.data);
    body
}

// ---------------------------------------------------------------------------
// Decode
// ---------------------------------------------------------------------------

/// Decode one message body (tag byte onward, no length prefix).
pub fn decode_body(body: Bytes) -> Result<Message, CodecError> {
    let tag = *body.first().ok_or(CodecError::Empty)?;
    match tag {
        TAG_JSON => Ok(Message::Json(body.slice(1..))),
        TAG_FRAME => {
            let rest = body.slice(1..);
            if rest.len() < FRAME_HEADER_LEN {
                return Err(CodecError::Truncated {
                    need: FRAME_HEADER_LEN,
                    have: rest.len(),
                });
            }
            let h = &rest[..FRAME_HEADER_LEN];
            let seq = u64::from_be_bytes(h[0..8].try_into().expect("8 bytes"));
            let timestamp_us = u64::from_be_bytes(h[8..16].try_into().expect("8 bytes"));
            let width = u16::from_be_bytes(h[16..18].try_into().expect("2 bytes"));
            let height = u16::from_be_bytes(h[18..20].try_into().expect("2 bytes"));
            let fourcc = FourCc(h[20..24].try_into().expect("4 bytes"));
            let declared = u32::from_be_bytes(h[24..28].try_into().expect("4 bytes"));

            let data = rest.slice(FRAME_HEADER_LEN..);
            // The header's `len` is redundant with the outer prefix by construction; a
            // mismatch means the two disagree, so trust neither.
            if declared as usize != data.len() {
                return Err(CodecError::LengthMismatch {
                    declared,
                    actual: data.len(),
                });
            }

            Ok(Message::Frame(Frame {
                seq,
                timestamp_us,
                width,
                height,
                fourcc,
                data,
            }))
        }
        other => Err(CodecError::UnknownTag(other)),
    }
}

// ---------------------------------------------------------------------------
// Async transport
// ---------------------------------------------------------------------------

/// Write a pre-encoded body behind its length prefix.
pub async fn write_body<W: AsyncWrite + Unpin>(w: &mut W, body: &[u8]) -> Result<(), CodecError> {
    let len = u32::try_from(body.len()).map_err(|_| CodecError::TooLarge {
        len: u32::MAX,
        cap: MAX_MESSAGE_LEN,
    })?;
    if len > MAX_MESSAGE_LEN {
        return Err(CodecError::TooLarge {
            len,
            cap: MAX_MESSAGE_LEN,
        });
    }
    w.write_all(&len.to_be_bytes()).await?;
    w.write_all(body).await?;
    w.flush().await?;
    Ok(())
}

pub async fn write_json<W: AsyncWrite + Unpin>(
    w: &mut W,
    value: &impl Serialize,
) -> Result<(), CodecError> {
    let body = encode_json_body(value)?;
    write_body(w, &body).await
}

pub async fn write_frame<W: AsyncWrite + Unpin>(
    w: &mut W,
    frame: &Frame,
) -> Result<(), CodecError> {
    let body = encode_frame_body(frame);
    write_body(w, &body).await
}

/// Read one whole message. `Ok(None)` means the peer closed cleanly at a message boundary.
pub async fn read_message<R: AsyncRead + Unpin>(r: &mut R) -> Result<Option<Message>, CodecError> {
    let mut len_buf = [0u8; 4];
    match r.read_exact(&mut len_buf).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let len = u32::from_be_bytes(len_buf);
    if len > MAX_MESSAGE_LEN {
        return Err(CodecError::TooLarge {
            len,
            cap: MAX_MESSAGE_LEN,
        });
    }
    if len == 0 {
        return Err(CodecError::Empty);
    }
    let mut body = vec![0u8; len as usize];
    r.read_exact(&mut body).await?;
    decode_body(Bytes::from(body)).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::{Op, Request, Response};

    fn sample_frame(payload: &[u8]) -> Frame {
        Frame {
            seq: 42,
            timestamp_us: 1_753_160_400_123_456,
            width: 1920,
            height: 1080,
            fourcc: FourCc::MJPG,
            data: Bytes::copy_from_slice(payload),
        }
    }

    #[test]
    fn frame_body_round_trips() {
        let original = sample_frame(&[0xFF, 0xD8, 1, 2, 3, 0xFF, 0xD9]);
        let body = encode_frame_body(&original);
        assert_eq!(body.len(), 1 + FRAME_HEADER_LEN + original.data.len());

        let Message::Frame(decoded) = decode_body(Bytes::from(body)).unwrap() else {
            panic!("expected a frame message");
        };
        assert_eq!(decoded.seq, original.seq);
        assert_eq!(decoded.timestamp_us, original.timestamp_us);
        assert_eq!(decoded.width, original.width);
        assert_eq!(decoded.height, original.height);
        assert_eq!(decoded.fourcc, FourCc::MJPG);
        assert_eq!(decoded.data, original.data);
    }

    #[test]
    fn empty_payload_frame_round_trips() {
        let body = encode_frame_body(&sample_frame(&[]));
        let Message::Frame(decoded) = decode_body(Bytes::from(body)).unwrap() else {
            panic!("expected a frame message");
        };
        assert!(decoded.data.is_empty());
    }

    #[test]
    fn json_body_round_trips() {
        let req = Request::new(Op::SetControl {
            name: "focus_absolute".into(),
            value: 40,
            persist: true,
        });
        let body = encode_json_body(&req).unwrap();
        let msg = decode_body(Bytes::from(body)).unwrap();
        let decoded: Request = msg.json_as().unwrap().unwrap();
        assert_eq!(decoded, req);
    }

    #[test]
    fn request_wire_shape_is_flat() {
        let json = serde_json::to_string(&Request::new(Op::Snapshot)).unwrap();
        assert_eq!(json, r#"{"v":1,"op":"snapshot"}"#);
    }

    #[test]
    fn error_response_omits_empty_fields() {
        let json = serde_json::to_string(&Response::err(super::super::ErrorCode::Disabled, "off"))
            .unwrap();
        assert_eq!(
            json,
            r#"{"ok":false,"error":{"code":"disabled","message":"off"}}"#
        );
    }

    #[test]
    fn empty_body_is_rejected() {
        assert!(matches!(decode_body(Bytes::new()), Err(CodecError::Empty)));
    }

    #[test]
    fn unknown_tag_is_rejected() {
        assert!(matches!(
            decode_body(Bytes::from_static(&[0x7F, 1, 2, 3])),
            Err(CodecError::UnknownTag(0x7F))
        ));
    }

    #[test]
    fn truncated_frame_header_is_rejected() {
        let mut body = encode_frame_body(&sample_frame(&[1, 2, 3]));
        body.truncate(1 + FRAME_HEADER_LEN - 4);
        assert!(matches!(
            decode_body(Bytes::from(body)),
            Err(CodecError::Truncated { .. })
        ));
    }

    #[test]
    fn payload_shorter_than_declared_is_rejected() {
        let mut body = encode_frame_body(&sample_frame(&[1, 2, 3, 4, 5, 6, 7, 8]));
        body.truncate(body.len() - 3);
        assert!(matches!(
            decode_body(Bytes::from(body)),
            Err(CodecError::LengthMismatch {
                declared: 8,
                actual: 5
            })
        ));
    }

    #[tokio::test]
    async fn async_round_trip_over_a_pipe() {
        let (mut client, mut server) = tokio::io::duplex(1 << 20);
        let frame = sample_frame(&[0xFF, 0xD8, 9, 9, 9]);

        write_json(&mut client, &Response::ok()).await.unwrap();
        write_frame(&mut client, &frame).await.unwrap();
        drop(client);

        let first: Response = read_message(&mut server)
            .await
            .unwrap()
            .unwrap()
            .json_as()
            .unwrap()
            .unwrap();
        assert!(first.ok);

        let Some(Message::Frame(got)) = read_message(&mut server).await.unwrap() else {
            panic!("expected a frame message");
        };
        assert_eq!(got.seq, frame.seq);
        assert_eq!(got.data, frame.data);

        // Clean EOF at a message boundary.
        assert!(read_message(&mut server).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn oversized_length_prefix_is_rejected_before_allocating() {
        let (mut client, mut server) = tokio::io::duplex(64);
        tokio::spawn(async move {
            let _ = client.write_all(&(MAX_MESSAGE_LEN + 1).to_be_bytes()).await;
            // Deliberately never send a body — the reader must bail on the prefix alone.
            std::future::pending::<()>().await;
        });

        assert!(matches!(
            read_message(&mut server).await,
            Err(CodecError::TooLarge { .. })
        ));
    }

    #[test]
    fn fourcc_display_and_parse() {
        assert_eq!(FourCc::MJPG.to_string(), "MJPG");
        assert_eq!("MJPG".parse::<FourCc>().unwrap(), FourCc::MJPG);
        assert!("MJP".parse::<FourCc>().is_err());
        assert_eq!(FourCc([0x00, b'A', b'B', b'C']).to_string(), "\\x00ABC");
    }
}
