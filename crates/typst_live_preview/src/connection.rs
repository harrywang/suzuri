//! WebSocket connection to Tinymist's preview data plane.
//!
//! The data plane is a plain WebSocket that streams binary frame payloads (see
//! [`crate::protocol`]). It is separate from the LSP connection, so the editor
//! keeps using Tinymist for diagnostics, completion, and so on while this
//! module consumes only the preview stream.
//!
//! Tinymist validates the `Origin` header. Any `http://localhost:<port>` or
//! `http://127.0.0.1:<port>` origin is accepted, but the header must be
//! present: browsers always send one, and Tinymist plans to make its absence a
//! hard error.
//!
//! The frame size limits are raised well above the WebSocket defaults rather
//! than disabled. A full document frame embeds every image in the document, so
//! a report with a few figures easily exceeds the WebSocket default of 16 MiB
//! per frame; exceeding that limit makes the client reset the socket, and the
//! pane reconnects in a loop without ever receiving a document. The explicit
//! 256 MiB cap still keeps a runaway or corrupted server from exhausting the
//! editor's memory.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::Sender,
    },
    time::Duration,
};

use futures::{SinkExt, StreamExt};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio_tungstenite::{
    connect_async_with_config,
    tungstenite::{
        Message, client::IntoClientRequest, http::HeaderValue, protocol::WebSocketConfig,
    },
};

use crate::protocol::{DIFF_V1_PREFIX, NEW_PREFIX};

/// Tinymist's default data-plane port.
pub const DEFAULT_DATA_PLANE_PORT: u16 = 23625;

/// How long to wait before reconnecting a dropped preview stream.
pub const RECONNECT_INTERVAL: Duration = Duration::from_secs(1);

/// How often to send a WebSocket ping on an otherwise idle preview stream.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(20);

/// How often a blocked connection loop wakes up to notice [`run`]'s stop flag.
const STOP_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// How often to repeat the initial `"current"` request until a document
/// frame arrives.
///
/// Tinymist's render actor drops a full-render request when its first compile
/// has not finished yet. A large document can take longer to compile than the
/// WebSocket handshake, so sending `"current"` only once leaves the preview
/// blank until the next edit.
const CURRENT_RETRY_INTERVAL: Duration = Duration::from_millis(500);
const CURRENT_RETRIES: usize = 20;

/// How long a connected socket gets to deliver its first document frame.
///
/// A preview task can accept the WebSocket before its render actor is ready;
/// requests sent in that window are dropped. Opening a fresh connection after
/// this timeout is more reliable than reusing a socket the server never wired
/// to an actor.
const FIRST_DOCUMENT_TIMEOUT: Duration = Duration::from_secs(3);

/// Safety cap for one preview message.
///
/// A full document frame embeds every image, and the 18-page report that
/// motivated the larger limits was 17 MiB. 256 MiB leaves ample headroom while
/// keeping a runaway server from queuing gigabytes in the editor.
const MAX_PREVIEW_MESSAGE_BYTES: usize = 256 * 1024 * 1024;

/// Something observed on the preview connection.
#[derive(Debug, Clone)]
pub enum PreviewEvent {
    /// The socket connected. The client asked for the current document.
    Connected,
    /// A connection attempt failed. The client keeps retrying.
    ConnectFailed(String),
    /// A binary frame payload, still carrying its `new,`/`diff-v1,` prefix.
    Binary(Vec<u8>),
    /// A text message, such as an outline sync or source location.
    Text(String),
}

/// A connection failure. `Retry` is an internal reconnect that should not be
/// shown to the user.
#[derive(Debug)]
enum ServeError {
    Connect(String),
    Retry,
}

impl From<String> for ServeError {
    fn from(error: String) -> Self {
        Self::Connect(error)
    }
}

/// How to reach the preview data plane.
#[derive(Debug, Clone)]
pub struct PreviewConnectionConfig {
    /// The data-plane WebSocket URL, e.g. `ws://127.0.0.1:23625`.
    pub url: String,
    /// The `Origin` header to send.
    pub origin: String,
}

impl PreviewConnectionConfig {
    /// Builds a config for a data-plane port on loopback.
    pub fn loopback(port: u16) -> Self {
        Self {
            url: format!("ws://127.0.0.1:{port}"),
            origin: format!("http://localhost:{port}"),
        }
    }

    /// Overrides the URL, keeping the origin in step when it points at loopback.
    pub fn with_url(mut self, url: impl Into<String>) -> Self {
        self.url = url.into();
        self
    }
}

/// Runs the connection until it closes or `stop` is set, forwarding events to
/// `events`.
///
/// This is blocking and owns a single-threaded Tokio runtime, so it belongs on
/// a dedicated thread. The WebSocket library is Tokio-based; Suzuri's editor
/// executor is not, and mixing the two would tie the preview to an unrelated
/// runtime.
///
/// The connection reconnects on its own. Setting `stop` closes the socket and
/// returns; that is how a closed preview pane stops the stream.
///
/// `outbound` carries text messages to the server, such as a click that asks
/// Tinymist to resolve a source location. Messages sent while the connection is
/// down stay queued and are delivered after the next successful connect.
pub fn run(
    config: PreviewConnectionConfig,
    events: Sender<PreviewEvent>,
    outbound: UnboundedReceiver<String>,
    stop: Arc<AtomicBool>,
) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("failed to start the preview runtime: {error}"))?;

    runtime.block_on(async move {
        let mut outbound = outbound;
        loop {
            if stop.load(Ordering::Relaxed) {
                return Ok(());
            }

            if let Err(error) = serve(&config, &events, &mut outbound, &stop).await {
                if let ServeError::Connect(error) = error {
                    events.send(PreviewEvent::ConnectFailed(error)).ok();
                }
            }

            // Back off before reconnecting, but stay responsive to `stop`.
            let deadline = tokio::time::Instant::now() + RECONNECT_INTERVAL;
            while tokio::time::Instant::now() < deadline {
                if stop.load(Ordering::Relaxed) {
                    return Ok(());
                }
                tokio::time::sleep(STOP_POLL_INTERVAL).await;
            }
        }
    })
}

/// The read limits used for the preview data plane's WebSocket.
///
/// Tinymist streams the whole document in one binary message, and that message
/// embeds every raster image. A report with a handful of figures already
/// exceeds the WebSocket defaults, so the limits are raised to a generous
/// safety cap. The server is our own Tinymist process on loopback, but a bad
/// frame must still not be able to exhaust memory.
fn socket_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .max_message_size(Some(MAX_PREVIEW_MESSAGE_BYTES))
        .max_frame_size(Some(MAX_PREVIEW_MESSAGE_BYTES))
}

/// Connects once and pumps messages until the socket closes or `stop` is set.
async fn serve(
    config: &PreviewConnectionConfig,
    events: &Sender<PreviewEvent>,
    outbound: &mut UnboundedReceiver<String>,
    stop: &AtomicBool,
) -> Result<(), ServeError> {
    let mut request = config
        .url
        .as_str()
        .into_client_request()
        .map_err(|error| format!("invalid preview URL {:?}: {error}", config.url))?;
    request.headers_mut().insert(
        "Origin",
        HeaderValue::from_str(&config.origin)
            .map_err(|error| format!("invalid preview origin {:?}: {error}", config.origin))?,
    );

    // A document frame embeds every image, so it can be far larger than the
    // WebSocket defaults (16 MiB per frame, 64 MiB per message). The explicit
    // cap is high enough for real documents but still bounds memory.
    let socket_config = socket_config();

    let (mut socket, _response) = connect_async_with_config(request, Some(socket_config), false)
        .await
        .map_err(|error| format!("opening the preview WebSocket failed: {error}"))?;

    events.send(PreviewEvent::Connected).ok();
    socket
        .send(Message::Text("current".into()))
        .await
        .map_err(|error| format!("requesting the current preview document failed: {error}"))?;

    let mut stop_tick = tokio::time::interval(STOP_POLL_INTERVAL);
    stop_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut heartbeat = tokio::time::interval(HEARTBEAT_INTERVAL);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    heartbeat.tick().await;
    let mut current_tick = tokio::time::interval(CURRENT_RETRY_INTERVAL);
    current_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    current_tick.tick().await;
    let mut current_retries = 0;
    let mut received_document = false;
    let document_deadline = tokio::time::Instant::now() + FIRST_DOCUMENT_TIMEOUT;

    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(document_deadline), if !received_document => {
                return Err(ServeError::Retry);
            }
            _ = stop_tick.tick() => {
                if stop.load(Ordering::Relaxed) {
                    return Ok(());
                }
            }
            _ = heartbeat.tick() => {
                socket
                    .send(Message::Ping(Vec::new().into()))
                    .await
                    .map_err(|error| format!("preview keep-alive failed: {error}"))?;
            }
            _ = current_tick.tick(), if !received_document && current_retries < CURRENT_RETRIES => {
                current_retries += 1;
                socket
                    .send(Message::Text("current".into()))
                    .await
                    .map_err(|error| format!("requesting the current preview document failed: {error}"))?;
            }
            message = socket.next() => {
                match message {
                    Some(Ok(Message::Binary(bytes))) => {
                        if bytes.starts_with(NEW_PREFIX) || bytes.starts_with(DIFF_V1_PREFIX) {
                            received_document = true;
                        }
                        events.send(PreviewEvent::Binary(bytes.to_vec())).ok();
                    }
                    Some(Ok(Message::Text(text))) => {
                        events.send(PreviewEvent::Text(text.to_string())).ok();
                    }
                    Some(Ok(Message::Ping(payload))) => {
                        socket
                            .send(Message::Pong(payload))
                            .await
                            .map_err(|error| format!("answering the preview ping failed: {error}"))?;
                    }
                    Some(Ok(Message::Pong(_) | Message::Frame(_))) => {}
                    Some(Ok(Message::Close(_))) | None => return Ok(()),
                    Some(Err(error)) => {
                        return Err(format!("preview WebSocket failed: {error}").into());
                    }
                }
            }
            command = outbound.recv() => {
                match command {
                    Some(text) => {
                        socket
                            .send(Message::Text(text.into()))
                            .await
                            .map_err(|error| format!("sending a preview message failed: {error}"))?;
                    }
                    None => return Ok(()),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_frames_have_a_bounded_size_limit() {
        let config = socket_config();

        assert_eq!(config.max_frame_size, Some(MAX_PREVIEW_MESSAGE_BYTES));
        assert_eq!(config.max_message_size, Some(MAX_PREVIEW_MESSAGE_BYTES));
        assert!(MAX_PREVIEW_MESSAGE_BYTES > 17 * 1024 * 1024);
    }
}
