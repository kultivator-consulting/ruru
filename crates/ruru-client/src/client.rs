use crate::error::{ClientError, Result};
use crate::proto::{
    self, ControlDesc, Frame, Message, Op, Request, Response, SourceStatus, WireError,
};
use futures_core::Stream;
use std::path::{Path, PathBuf};
use tokio::net::UnixStream;

/// Where the source listens in production. Dev configs override it.
pub const DEFAULT_SOCKET_PATH: &str = "/run/ruru/source.sock";

/// The only supported way to talk to `ruru-source`.
///
/// Each call opens its own short-lived connection — the protocol is one request per
/// connection, so there is no shared state to synchronise and no reconnect logic to get
/// wrong. [`SourceClient::subscribe`] hands its connection to the returned stream.
#[derive(Debug, Clone)]
pub struct SourceClient {
    path: PathBuf,
}

impl SourceClient {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Client for [`DEFAULT_SOCKET_PATH`].
    pub fn default_path() -> Self {
        Self::new(DEFAULT_SOCKET_PATH)
    }

    pub fn socket_path(&self) -> &Path {
        &self.path
    }

    /// Connect, send one request, and read the acknowledgement.
    ///
    /// Returns the still-open connection so frame-streaming ops can keep reading from it.
    async fn request(&self, op: Op) -> Result<(UnixStream, Response)> {
        let mut stream =
            UnixStream::connect(&self.path)
                .await
                .map_err(|source| ClientError::Connect {
                    path: self.path.clone(),
                    source,
                })?;

        proto::write_json(&mut stream, &Request::new(op)).await?;

        let msg = proto::read_message(&mut stream)
            .await?
            .ok_or(ClientError::NoResponse)?;
        let response: Response = msg
            .json_as()
            .ok_or(ClientError::UnexpectedFrame)?
            .map_err(ClientError::Codec)?;

        if !response.ok {
            let WireError { code, message } = response.error.unwrap_or(WireError {
                code: proto::ErrorCode::Internal,
                message: "source reported failure without a reason".into(),
            });
            return Err(ClientError::Source { code, message });
        }

        Ok((stream, response))
    }

    /// One frame. Attaches for the duration, so the camera opens on demand if it was idle.
    pub async fn snapshot(&self) -> Result<Frame> {
        let (mut stream, _ack) = self.request(Op::Snapshot).await?;
        match proto::read_message(&mut stream).await? {
            Some(Message::Frame(frame)) => Ok(frame),
            Some(Message::Json(bytes)) => Err(terminal_error(&bytes)),
            None => Err(ClientError::NoResponse),
        }
    }

    /// A continuous stream of frames. The camera stays open until the stream is dropped.
    pub async fn subscribe(&self) -> Result<FrameStream> {
        let (stream, _ack) = self.request(Op::Subscribe).await?;
        Ok(FrameStream { stream })
    }

    pub async fn status(&self) -> Result<SourceStatus> {
        let (_, response) = self.request(Op::Status).await?;
        response.status.ok_or(ClientError::MissingField("status"))
    }

    /// Master gate. `false` closes the camera and terminates live subscriptions.
    pub async fn set_enabled(&self, enabled: bool) -> Result<SourceStatus> {
        let (_, response) = self.request(Op::Enable { enabled }).await?;
        response.status.ok_or(ClientError::MissingField("status"))
    }

    pub async fn list_controls(&self) -> Result<Vec<ControlDesc>> {
        let (_, response) = self.request(Op::ListControls).await?;
        response
            .controls
            .ok_or(ClientError::MissingField("controls"))
    }

    pub async fn get_control(&self, name: &str) -> Result<ControlDesc> {
        let (_, response) = self
            .request(Op::GetControl {
                name: name.to_owned(),
            })
            .await?;
        response.control.ok_or(ClientError::MissingField("control"))
    }

    /// Set a control; the returned descriptor carries the value as re-read from the device.
    /// When `persist` is true the source also records it as a runtime override that survives
    /// a restart (see [`Op::SetControl`]).
    pub async fn set_control(&self, name: &str, value: i64, persist: bool) -> Result<ControlDesc> {
        let (_, response) = self
            .request(Op::SetControl {
                name: name.to_owned(),
                value,
                persist,
            })
            .await?;
        response.control.ok_or(ClientError::MissingField("control"))
    }
}

/// A live subscription. Dropping it detaches, which lets the source idle-close the camera.
#[derive(Debug)]
pub struct FrameStream {
    stream: UnixStream,
}

impl FrameStream {
    /// Next frame, or `None` once the source closes the stream cleanly.
    ///
    /// A mid-stream control message is terminal: the source sends one when it shuts the
    /// subscription down deliberately (being disabled, most importantly), and it is
    /// surfaced here as an error rather than a silent end-of-stream.
    pub async fn next(&mut self) -> Result<Option<Frame>> {
        match proto::read_message(&mut self.stream).await? {
            Some(Message::Frame(frame)) => Ok(Some(frame)),
            Some(Message::Json(bytes)) => Err(terminal_error(&bytes)),
            None => Ok(None),
        }
    }

    /// Adapt to a [`Stream`] for consumers that want combinators.
    pub fn into_stream(mut self) -> impl Stream<Item = Result<Frame>> {
        async_stream::stream! {
            loop {
                match self.next().await {
                    Ok(Some(frame)) => yield Ok(frame),
                    Ok(None) => break,
                    Err(e) => {
                        yield Err(e);
                        break;
                    }
                }
            }
        }
    }
}

/// Interpret a mid-stream control message as the reason the stream is ending.
fn terminal_error(bytes: &[u8]) -> ClientError {
    match serde_json::from_slice::<Response>(bytes) {
        Ok(Response {
            error: Some(WireError { code, message }),
            ..
        }) => ClientError::Source { code, message },
        Ok(_) => ClientError::UnexpectedControlMessage,
        Err(e) => ClientError::Codec(e.into()),
    }
}
