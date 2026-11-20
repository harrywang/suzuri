//! Collaborative preview transport.
//!
//! Tinymist runs on the machine that shares the project, so its data plane
//! listens on that machine's loopback and a guest cannot connect to it. The
//! host owns the Tinymist task and relays its binary frames to each guest,
//! which merges the vector IR and renders it locally.
//!
//! The public collab server ([zed.dev]) only forwards message types it knows,
//! so the relay cannot add its own proto messages. Instead it rides on
//! `ExecuteLspCommand`, which the server already forwards to the project host:
//!
//! - `suzuri.typstPreview.start` starts (or joins) the host's Tinymist task.
//! - `suzuri.typstPreview.poll` long-polls queued frames.
//! - `suzuri.typstPreview.stop` releases one pane's subscription.
//! - `suzuri.typstPreview.resolve` resolves a page point to a source location.
//!
//! `LspStore::register_command_handler` lets the host answer those commands in
//! this app instead of passing them to Tinymist. Guests need neither Tinymist
//! nor the compiler; the host needs the Typst extension.
//!
//! [zed.dev]: https://collab.zed.dev

use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Context as _, anyhow};
use base64::Engine as _;
use gpui::{
    App, AppContext as _, AsyncApp, BorrowAppContext, Context, Entity, EntityId, Global,
    Subscription, WeakEntity,
};
use lsp::LanguageServerId;
use project::lsp_store::{LspCommandHandler, ShowDocumentOrigin};
use project::{LspStore, LspStoreEvent, Project};
use proto::PeerId;
use serde_json::{Value, json};

use crate::connection::{self, PreviewConnectionConfig, PreviewEvent};
use crate::protocol::{parse_control_frame, source_point_message};
use crate::start::{
    find_tinymist_server_for_worktree, preview_transport, start_preview_task, unique_task_id,
};
use crate::view::{OUTLINE_METHOD, is_typst_path};

/// Prefix for every command the relay owns.
pub const COMMAND_PREFIX: &str = "suzuri.typstPreview.";
/// Starts (or joins) a host-side preview stream.
pub const COMMAND_START: &str = "suzuri.typstPreview.start";
/// Releases one pane's subscription.
pub const COMMAND_STOP: &str = "suzuri.typstPreview.stop";
/// Long-polls queued frames.
pub const COMMAND_POLL: &str = "suzuri.typstPreview.poll";
/// Resolves a page point to a source location on the host.
pub const COMMAND_RESOLVE: &str = "suzuri.typstPreview.resolve";

/// How long an idle poll waits for the next frame before returning empty.
///
/// A queued frame notifies the poll, so this only bounds how often an idle
/// guest re-asks; it does not delay frames.
const POLL_WAIT: Duration = Duration::from_secs(2);
/// How long a guest waits for `start` before reporting a host that does not
/// understand the relay.
const START_TIMEOUT: Duration = Duration::from_secs(20);
/// How long a guest waits for Tinymist to resolve a click.
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(5);
/// Raw bytes per frame chunk. A whole frame travels in one long-poll when it
/// fits; large frames are split so one WebSocket message stays small.
const CHUNK_BYTES: usize = 96 * 1024;
/// Response bytes returned by one poll.
const BATCH_BYTES: usize = 1024 * 1024;
/// Frames a guest has not collected yet. Beyond this the guest is considered
/// too far behind and the stream is closed rather than growing without bound.
const MAX_QUEUED_BYTES: usize = 32 * 1024 * 1024;

/// A frame or notification delivered by [`RelayClient::next`].
#[derive(Debug)]
pub enum RelayItem {
    /// One complete data-plane frame (`new,` or `diff-v1,`).
    Frame(Vec<u8>),
    /// Tinymist's document outline.
    Outline(Value),
    /// The host stopped the stream. The reason may be empty.
    Stopped(String),
}

/// Guest-side handle for one relayed preview stream.
///
/// It is deliberately independent of the UI: the harness and the preview pane
/// both drive the same poll loop.
pub struct RelayClient {
    lsp_store: Entity<LspStore>,
    server_id: LanguageServerId,
    stream_id: String,
    after: u64,
    pending: VecDeque<Value>,
    assembly: FrameAssembly,
}

impl RelayClient {
    /// Asks the project host to start (or join) a preview for `abs_path`.
    pub async fn start(
        lsp_store: Entity<LspStore>,
        abs_path: &Path,
        cx: &mut AsyncApp,
    ) -> anyhow::Result<Self> {
        log::info!(
            "typst live preview: guest requesting relay for {}",
            abs_path.display()
        );
        let request = lsp_store.update(cx, |store, cx| {
            store.execute_lsp_command(
                LanguageServerId::from_proto(0),
                COMMAND_START.to_owned(),
                vec![json!({ "abs_path": abs_path.display().to_string() })],
                cx,
            )
        });
        let response = futures_lite::future::or(
            request,
            async {
                cx.background_executor().timer(START_TIMEOUT).await;
                Err::<Option<Value>, anyhow::Error>(anyhow!(
                    "The project host did not answer the preview request. The host needs a Suzuri build with Typst relay support."
                ))
            },
        )
        .await?;
        let response = response.context(
            "The project host did not answer the preview request. The host needs a Suzuri build with Typst relay support.",
        )?;
        let stream_id = response
            .get("stream_id")
            .and_then(Value::as_str)
            .context("the host returned no preview stream id")?
            .to_owned();
        log::info!("typst live preview: guest joined stream {stream_id}");
        let server_id = response
            .get("language_server_id")
            .and_then(Value::as_u64)
            .map(LanguageServerId::from_proto)
            .unwrap_or_else(|| LanguageServerId::from_proto(0));

        Ok(Self {
            lsp_store,
            server_id,
            stream_id,
            after: 0,
            pending: VecDeque::new(),
            assembly: FrameAssembly::default(),
        })
    }

    pub fn stream_id(&self) -> &str {
        &self.stream_id
    }

    pub fn server_id(&self) -> LanguageServerId {
        self.server_id
    }

    /// Returns the next frame, outline, or stop notice.
    ///
    /// `Ok(None)` means the poll window elapsed with no news. Errors are
    /// transport failures (the project disconnected, or the host no longer
    /// understands the stream).
    pub async fn next(&mut self, cx: &mut AsyncApp) -> anyhow::Result<Option<RelayItem>> {
        loop {
            if let Some(item) = self.pending.pop_front() {
                self.after = self
                    .after
                    .max(item.get("seq").and_then(Value::as_u64).unwrap_or(0));
                if let Some(item) = self.handle_item(&item)? {
                    return Ok(Some(item));
                }
                continue;
            }

            let response = self
                .request(
                    COMMAND_POLL,
                    json!({ "stream_id": self.stream_id, "after": self.after }),
                    cx,
                )
                .await?;
            let Some(response) = response else {
                return Ok(None);
            };
            let items = response
                .get("items")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            if items.is_empty() {
                return Ok(None);
            }
            self.pending.extend(items);
        }
    }

    /// Resolves a click on `page` at Typst points `(x, y)` on the host.
    ///
    /// Only the requesting guest receives the location; the host's editor
    /// does not jump.
    pub async fn resolve_at(
        lsp_store: &Entity<LspStore>,
        server_id: LanguageServerId,
        stream_id: &str,
        page: usize,
        x: f64,
        y: f64,
        cx: &mut AsyncApp,
    ) -> anyhow::Result<Option<(lsp::Uri, Option<lsp::Range>)>> {
        let response = lsp_store
            .update(cx, |store, cx| {
                store.execute_lsp_command(
                    server_id,
                    COMMAND_RESOLVE.to_owned(),
                    vec![json!({
                        "stream_id": stream_id,
                        "page": page,
                        "x": x,
                        "y": y,
                    })],
                    cx,
                )
            })
            .await?;
        let Some(response) = response else {
            return Ok(None);
        };
        let Some(uri) = response.get("uri").and_then(Value::as_str) else {
            return Ok(None);
        };
        let uri = uri.parse::<lsp::Uri>()?;
        let start = point_from_json(response.get("selection_start"));
        let end = point_from_json(response.get("selection_end"));
        let selection = start.zip(end).map(|(start, end)| lsp::Range { start, end });
        Ok(Some((uri, selection)))
    }

    /// Releases the stream. Best effort: the pame is closing anyway.
    pub fn stop(&self, cx: &mut App) {
        self.lsp_store
            .update(cx, |store, cx| {
                store.execute_lsp_command(
                    self.server_id,
                    COMMAND_STOP.to_owned(),
                    vec![json!({ "stream_id": self.stream_id })],
                    cx,
                )
            })
            .detach();
    }

    async fn request(
        &self,
        command: &str,
        argument: Value,
        cx: &mut AsyncApp,
    ) -> anyhow::Result<Option<Value>> {
        self.lsp_store
            .update(cx, |store, cx| {
                store.execute_lsp_command(self.server_id, command.to_owned(), vec![argument], cx)
            })
            .await
    }

    fn handle_item(&mut self, item: &Value) -> anyhow::Result<Option<RelayItem>> {
        match item.get("kind").and_then(Value::as_str).unwrap_or("") {
            "frame" => Ok(self.assembly.accept(item)?.map(RelayItem::Frame)),
            "outline" => Ok(Some(RelayItem::Outline(
                item.get("outline").cloned().unwrap_or(Value::Null),
            ))),
            "stopped" => Ok(Some(RelayItem::Stopped(
                item.get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            ))),
            "closed" => Ok(Some(RelayItem::Stopped(
                "The project host stopped the preview.".to_owned(),
            ))),
            _ => Ok(None),
        }
    }
}

fn point_from_json(value: Option<&Value>) -> Option<lsp::Position> {
    let value = value?;
    let row = u32::try_from(value.get("row")?.as_u64()?).ok()?;
    let column = u32::try_from(value.get("column")?.as_u64()?).ok()?;
    Some(lsp::Position::new(row, column))
}

/// Reassembles one frame's base64 chunks.
#[derive(Default)]
struct FrameAssembly {
    frame: Option<u64>,
    next_index: u32,
    data: String,
}

impl FrameAssembly {
    fn accept(&mut self, item: &Value) -> anyhow::Result<Option<Vec<u8>>> {
        let frame = item.get("frame").and_then(Value::as_u64).unwrap_or(0);
        let index = u32::try_from(item.get("index").and_then(Value::as_u64).unwrap_or(0))
            .context("invalid frame chunk index")?;
        let count = u32::try_from(item.get("count").and_then(Value::as_u64).unwrap_or(1))
            .context("invalid frame chunk count")?;
        let data = item.get("data").and_then(Value::as_str).unwrap_or("");

        if self.frame != Some(frame) || index != self.next_index {
            *self = Self {
                frame: Some(frame),
                next_index: 0,
                data: String::new(),
            };
        }
        if index != self.next_index {
            return Ok(None);
        }

        self.data.push_str(data);
        self.next_index += 1;
        if self.next_index < count {
            return Ok(None);
        }

        let data = std::mem::take(&mut self.data);
        *self = Self::default();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data)
            .context("decoding a relayed frame")?;
        Ok(Some(bytes))
    }
}

/// One host-side queue per preview stream.
///
/// The WebSocket thread and the outline handler push into it; each guest poll
/// reads a batch and advances its cursor. Items are kept until the guest
/// acknowledges them, so a lost response is retried instead of leaving a hole.
#[derive(Default)]
struct FrameQueue {
    state: Mutex<QueueState>,
    notify: Arc<tokio::sync::Notify>,
}

#[derive(Default)]
struct QueueState {
    items: VecDeque<Value>,
    bytes: usize,
    next_seq: u64,
    next_frame: u64,
    closed: bool,
}

impl FrameQueue {
    fn new() -> Self {
        Self {
            state: Mutex::new(QueueState {
                // Cursor 0 means "nothing consumed yet", so sequence numbers
                // start at 1.
                next_seq: 1,
                ..QueueState::default()
            }),
            notify: Arc::new(tokio::sync::Notify::new()),
        }
    }

    fn notify(&self) -> Arc<tokio::sync::Notify> {
        self.notify.clone()
    }

    fn push(&self, mut item: Value) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let seq = state.next_seq;
        state.next_seq += 1;
        item["seq"] = json!(seq);
        let size = item.to_string().len();
        state.bytes += size;
        state.items.push_back(item);
        if state.bytes > MAX_QUEUED_BYTES {
            let reason = "The preview fell behind and was reset.";
            state.items.clear();
            state.bytes = 0;
            let seq = state.next_seq;
            state.next_seq += 1;
            state
                .items
                .push_back(json!({ "kind": "stopped", "seq": seq, "reason": reason }));
            state.closed = true;
        }
        drop(state);
        self.notify.notify_one();
    }

    fn push_frame(&self, bytes: &[u8]) {
        let frame = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            let frame = state.next_frame;
            state.next_frame += 1;
            frame
        };
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        let chunks = encoded.as_bytes().chunks(CHUNK_BYTES).collect::<Vec<_>>();
        let count = chunks.len().max(1);
        for (index, chunk) in chunks.into_iter().enumerate() {
            self.push(json!({
                "kind": "frame",
                "frame": frame,
                "index": index,
                "count": count,
                "data": std::str::from_utf8(chunk).unwrap_or_default(),
            }));
        }
    }

    fn push_outline(&self, outline: Value) {
        self.push(json!({ "kind": "outline", "outline": outline }));
    }

    fn close(&self, reason: &str) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.closed {
            return;
        }
        state.closed = true;
        let seq = state.next_seq;
        state.next_seq += 1;
        state
            .items
            .push_back(json!({ "kind": "stopped", "seq": seq, "reason": reason }));
        drop(state);
        self.notify.notify_one();
    }

    /// Returns the items after `after`, up to one response batch.
    ///
    /// Acknowledged items are dropped from the front.
    fn take(&self, after: u64) -> Option<Vec<Value>> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        while state
            .items
            .front()
            .and_then(|item| item.get("seq").and_then(Value::as_u64))
            .is_some_and(|seq| seq <= after)
        {
            if let Some(item) = state.items.pop_front() {
                state.bytes = state.bytes.saturating_sub(item.to_string().len());
            }
        }
        if state.items.is_empty() {
            return None;
        }

        let mut batch = Vec::new();
        let mut bytes = 0;
        for item in &state.items {
            let size = item.to_string().len();
            if !batch.is_empty() && bytes + size > BATCH_BYTES {
                break;
            }
            bytes += size;
            batch.push(item.clone());
            if bytes >= BATCH_BYTES {
                break;
            }
        }
        Some(batch)
    }

    fn is_closed(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .closed
    }
}

/// Reference-counted subscriptions to one host-side stream.
///
/// A peer may open the same file in several panes; the Tinymist task lives
/// until the last pane closes (or the peer disconnects).
#[derive(Default)]
struct Subscribers(HashMap<PeerId, usize>);

impl Subscribers {
    fn subscribe(&mut self, peer: PeerId) {
        *self.0.entry(peer).or_default() += 1;
    }

    /// Drops one subscription. Returns whether the stream is now unused.
    fn unsubscribe(&mut self, peer: PeerId) -> bool {
        if let Some(count) = self.0.get_mut(&peer) {
            *count -= 1;
            if *count == 0 {
                self.0.remove(&peer);
            }
        }
        self.0.is_empty()
    }

    /// Drops every subscription held by a peer that left the project. Returns
    /// whether the stream is now unused.
    fn remove_peer(&mut self, peer: PeerId) -> bool {
        self.0.remove(&peer);
        self.0.is_empty()
    }
}

/// A host-side preview task and its subscribers.
struct Stream {
    abs_path: PathBuf,
    server_id: LanguageServerId,
    task_id: String,
    subscribers: Subscribers,
    local_port: u16,
    outbound: tokio::sync::mpsc::UnboundedSender<String>,
    outbound_rx: Option<tokio::sync::mpsc::UnboundedReceiver<String>>,
    forward: Option<Arc<crate::tunnel::PortForward>>,
    stop: Arc<AtomicBool>,
    ready: bool,
    queue: Arc<FrameQueue>,
}

/// A source location resolved by Tinymist for a guest click.
struct ResolvedLocation {
    uri: String,
    start: Option<(u32, u32)>,
    end: Option<(u32, u32)>,
}

struct PendingResolve {
    sender: smol::channel::Sender<Option<ResolvedLocation>>,
}

struct PreviewRelay {
    lsp_store: Entity<LspStore>,
    streams: HashMap<String, Stream>,
    /// Serializes preview startup so two guests opening the same file at once
    /// join one Tinymist task instead of starting two.
    start_lock: Arc<smol::lock::Mutex<()>>,
    pending_resolve: Option<PendingResolve>,
    _project_subscription: Subscription,
    _lsp_subscription: Subscription,
}

impl PreviewRelay {
    fn new(
        project: WeakEntity<Project>,
        lsp_store: Entity<LspStore>,
        cx: &mut Context<Self>,
    ) -> Self {
        let project_entity = project.upgrade().expect("a project entity is alive here");
        let project_subscription = cx.subscribe(&project_entity, |relay, project, event, cx| {
            relay.on_project_event(project, event, cx)
        });
        let lsp_subscription = cx.subscribe(&lsp_store, |relay, lsp_store, event, cx| {
            relay.on_lsp_event(lsp_store, event, cx)
        });
        Self {
            lsp_store,
            streams: HashMap::new(),
            start_lock: Arc::new(smol::lock::Mutex::new(())),
            pending_resolve: None,
            _project_subscription: project_subscription,
            _lsp_subscription: lsp_subscription,
        }
    }

    fn on_project_event(
        &mut self,
        _project: Entity<Project>,
        event: &project::Event,
        cx: &mut Context<Self>,
    ) {
        match event {
            project::Event::RemoteIdChanged(None) | project::Event::Closed => {
                self.stop_all(cx);
            }
            project::Event::CollaboratorLeft(peer_id) => {
                let empty = self
                    .streams
                    .iter_mut()
                    .filter_map(|(stream_id, stream)| {
                        stream
                            .subscribers
                            .remove_peer(*peer_id)
                            .then(|| stream_id.clone())
                    })
                    .collect::<Vec<_>>();
                for stream_id in empty {
                    self.stop_stream(&stream_id, cx);
                }
            }
            _ => {}
        }
    }

    fn on_lsp_event(
        &mut self,
        _lsp_store: Entity<LspStore>,
        event: &LspStoreEvent,
        _cx: &mut Context<Self>,
    ) {
        match event {
            LspStoreEvent::LanguageServerShowDocument(request)
                if request.origin == ShowDocumentOrigin::Preview =>
            {
                let location = ResolvedLocation {
                    uri: request.uri.as_str().to_owned(),
                    start: request
                        .selection
                        .map(|range| (range.start.line, range.start.character)),
                    end: request
                        .selection
                        .map(|range| (range.end.line, range.end.character)),
                };
                request.clone().respond(true);
                if let Some(pending) = self.pending_resolve.take() {
                    pending.sender.try_send(Some(location)).ok();
                }
            }
            LspStoreEvent::CustomNotification {
                server_id,
                method,
                params,
            } if *method == OUTLINE_METHOD => {
                let mut delivered = false;
                for stream in self.streams.values() {
                    if stream.server_id == *server_id {
                        stream.queue.push_outline(params.clone());
                        delivered = true;
                    }
                }
                if delivered {
                    log::debug!("typst live preview: relaying the document outline");
                }
            }
            _ => {}
        }
    }

    /// Starts the WebSocket relay that fills this stream's queue.
    fn start_stream(&mut self, stream_id: &str, _cx: &mut Context<Self>) {
        let Some(stream) = self.streams.get_mut(stream_id) else {
            return;
        };
        if stream.ready {
            return;
        }
        let Some(outbound_rx) = stream.outbound_rx.take() else {
            return;
        };
        stream.ready = true;

        let config = PreviewConnectionConfig::loopback(stream.local_port);
        let stop = stream.stop.clone();
        let queue = stream.queue.clone();
        std::thread::Builder::new()
            .name("typst-preview-relay".into())
            .spawn(move || {
                let (net_tx, net_rx) = std::sync::mpsc::channel();
                let connection_stop = stop.clone();
                std::thread::Builder::new()
                    .name("typst-preview-relay-ws".into())
                    .spawn(move || {
                        if let Err(error) =
                            connection::run(config, net_tx, outbound_rx, connection_stop)
                        {
                            log::warn!("typst preview relay connection stopped: {error}");
                        }
                    })
                    .ok();

                for event in net_rx {
                    match event {
                        PreviewEvent::Binary(bytes) => {
                            // Viewport jumps are host-local. Forwarding them
                            // would make one user's cursor scroll everyone's
                            // preview, so collaborative panes never follow.
                            if is_viewport_frame(&bytes) {
                                continue;
                            }
                            queue.push_frame(&bytes);
                        }
                        PreviewEvent::ConnectFailed(reason) => {
                            queue.close(&reason);
                            return;
                        }
                        PreviewEvent::Connected | PreviewEvent::Text(_) => {}
                    }
                }
                queue.close("");
            })
            .ok();
    }

    fn remove_subscriber(&mut self, stream_id: &str, peer_id: PeerId, cx: &mut Context<Self>) {
        let Some(stream) = self.streams.get_mut(stream_id) else {
            return;
        };
        if stream.subscribers.unsubscribe(peer_id) {
            self.stop_stream(stream_id, cx);
        }
    }

    fn stop_stream(&mut self, stream_id: &str, cx: &mut Context<Self>) {
        let Some(stream) = self.streams.remove(stream_id) else {
            return;
        };
        stream.stop.store(true, Ordering::Relaxed);
        stream.queue.close("");
        if let Some(forward) = stream.forward {
            forward.stop();
        }
        drop(stream.outbound);
        drop(stream.outbound_rx);

        let server_id = stream.server_id;
        let task_id = stream.task_id;
        self.lsp_store
            .update(cx, |store, cx| {
                store.execute_lsp_command(
                    server_id,
                    "tinymist.doKillPreview".to_owned(),
                    vec![json!(task_id)],
                    cx,
                )
            })
            .detach();
    }

    fn stop_all(&mut self, cx: &mut Context<Self>) {
        let stream_ids = self.streams.keys().cloned().collect::<Vec<_>>();
        for stream_id in stream_ids {
            self.stop_stream(&stream_id, cx);
        }
    }
}

impl Drop for PreviewRelay {
    fn drop(&mut self) {
        for stream in self.streams.values() {
            stream.stop.store(true, Ordering::Relaxed);
            if let Some(forward) = &stream.forward {
                forward.stop();
            }
        }
    }
}

/// Keeps the host relay for each project alive for as long as the project is.
#[derive(Default)]
struct RelayRegistry(HashMap<EntityId, Entity<PreviewRelay>>);

impl Global for RelayRegistry {}

/// Registers the per-project relays and the host command handler.
///
/// `Project::client()` is the app-wide collab client; every project shares it,
/// so nothing here may register message handlers per project. The relay rides
/// on `ExecuteLspCommand`, whose handlers live on each project's `LspStore`.
pub fn init(cx: &mut App) {
    cx.observe_new::<Project>(|project, _, cx| {
        // `observe_new` runs inside the project's update lease, so reading the
        // project entity here would panic. Take the stores from the borrowed
        // project instead.
        let lsp_store = project.lsp_store();
        let project_entity = cx.entity();
        let project_id = project_entity.entity_id();
        let is_via_collab = project.is_via_collab();
        let relay =
            cx.new(|cx| PreviewRelay::new(project_entity.downgrade(), lsp_store.clone(), cx));
        let relay_weak = relay.downgrade();

        cx.update_default_global(|registry: &mut RelayRegistry, _| {
            registry.0.insert(project_id, relay);
        });

        // A guest only sends relay commands; the host answers them. Registering
        // on a guest would intercept its own outgoing commands.
        if !is_via_collab {
            let project = project_entity.downgrade();
            let handler: LspCommandHandler =
                Arc::new(move |_lsp_store, command, arguments, sender, cx| {
                    relay_command(
                        relay_weak.clone(),
                        project.clone(),
                        command,
                        arguments,
                        sender,
                        cx,
                    )
                });
            lsp_store.update(cx, |store, _| {
                store.register_command_handler(COMMAND_PREFIX, handler);
            });
        }

        cx.on_release(move |_, cx| {
            let relay =
                cx.update_global::<RelayRegistry, _>(|registry, _| registry.0.remove(&project_id));
            if let Some(relay) = relay {
                relay.update(cx, |relay, cx| relay.stop_all(cx));
            }
        })
        .detach();
    })
    .detach();
}

/// Dispatches one relay command on the project host.
fn relay_command(
    relay: WeakEntity<PreviewRelay>,
    project: WeakEntity<Project>,
    command: String,
    arguments: Vec<Value>,
    sender: Option<PeerId>,
    mut cx: AsyncApp,
) -> futures::future::LocalBoxFuture<'static, anyhow::Result<Option<Value>>> {
    Box::pin(async move {
        if command == COMMAND_POLL {
            // The guest polls continuously; keep idle polls out of the normal
            // log even though they are the majority of relay commands.
            log::debug!("typst live preview: host handling {command} from {sender:?}");
        } else {
            log::info!("typst live preview: host handling {command} from {sender:?}");
        }
        let relay = relay.upgrade().context("the preview relay is gone")?;
        let result = match command.as_str() {
            COMMAND_START => start_guest_stream(relay, project, arguments, sender, &mut cx)
                .await
                .map(Some),
            COMMAND_STOP => {
                stop_guest_stream(relay, arguments, sender, &mut cx)?;
                Ok(Some(json!({})))
            }
            COMMAND_POLL => poll_guest_stream(relay, arguments, &mut cx).await,
            COMMAND_RESOLVE => resolve_guest_click(relay, arguments, &mut cx).await,
            _ => Ok(None),
        };
        if let Err(error) = &result {
            log::warn!("typst live preview: relay command {command} failed: {error:#}");
        }
        result
    })
}

/// Starts (or joins) the Tinymist task for one guest pane.
async fn start_guest_stream(
    relay: Entity<PreviewRelay>,
    project: WeakEntity<Project>,
    arguments: Vec<Value>,
    sender: Option<PeerId>,
    cx: &mut AsyncApp,
) -> anyhow::Result<Value> {
    let abs_path = PathBuf::from(
        arguments
            .first()
            .and_then(|argument| argument.get("abs_path"))
            .and_then(Value::as_str)
            .context("the preview request is missing a path")?,
    );
    let peer_id = sender.context("the preview request has no sender")?;
    let project = project.upgrade().context("the project is gone")?;

    anyhow::ensure!(
        !cx.update(|cx| project.read(cx).is_via_collab()),
        "a collaborative guest cannot host a preview"
    );
    anyhow::ensure!(
        is_typst_path(&abs_path),
        "only Typst documents can be previewed"
    );
    anyhow::ensure!(
        cx.update(|cx| project.read(cx).remote_id()).is_some(),
        "the project is not shared"
    );
    let project_path = cx
        .update(|cx| {
            project
                .read(cx)
                .project_path_for_absolute_path(&abs_path, cx)
        })
        .context("the requested preview file is not part of this project")?;

    // Another guest may be starting the same file right now. Wait for it, then
    // take the join path below instead of starting a second Tinymist task.
    let start_lock = relay.update(cx, |relay, _| relay.start_lock.clone());
    let _start_guard = start_lock.lock().await;

    if let Some((stream_id, server_id)) = relay.update(cx, |relay, _| {
        relay
            .streams
            .iter_mut()
            .find(|(_, stream)| stream.abs_path == abs_path)
            .map(|(stream_id, stream)| {
                stream.subscribers.subscribe(peer_id);
                (stream_id.clone(), stream.server_id)
            })
    }) {
        return Ok(json!({
            "stream_id": stream_id,
            "language_server_id": server_id.to_proto(),
        }));
    }

    #[cfg(any(test, feature = "test-support"))]
    if let Some(fake) = test_support::fake_preview(&abs_path) {
        let stream_id = unique_task_id();
        let queue = Arc::new(FrameQueue::new());
        let (outbound, mut outbound_rx) = tokio::sync::mpsc::unbounded_channel();
        let stop = Arc::new(AtomicBool::new(false));
        let subscribers = {
            let mut subscribers = Subscribers::default();
            subscribers.subscribe(peer_id);
            subscribers
        };
        relay.update(cx, |relay, _| {
            relay.streams.insert(
                stream_id.clone(),
                Stream {
                    abs_path: abs_path.clone(),
                    server_id: fake.language_server_id,
                    task_id: stream_id.clone(),
                    subscribers,
                    local_port: 0,
                    outbound,
                    outbound_rx: None,
                    forward: None,
                    stop,
                    ready: true,
                    queue: queue.clone(),
                },
            );
        });
        let src_points = fake.src_points.clone();
        cx.spawn(async move |_| {
            while let Some(text) = outbound_rx.recv().await {
                src_points.send(text).await.ok();
            }
        })
        .detach();
        relay.update(cx, |_, cx| {
            let relay = relay.clone();
            let stream_id = stream_id.clone();
            let queue = queue.clone();
            cx.spawn(async move |_, cx| {
                while let Ok(command) = fake.commands.recv().await {
                    match command {
                        test_support::HostCommand::Frame(frame) => {
                            if !is_viewport_frame(&frame) {
                                queue.push_frame(&frame);
                            }
                        }
                        test_support::HostCommand::Outline(outline) => queue.push_outline(outline),
                        test_support::HostCommand::Close(reason) => {
                            queue.close(&reason);
                            relay.update(cx, |relay, cx| relay.stop_stream(&stream_id, cx));
                            break;
                        }
                    }
                }
            })
            .detach();
        });
        return Ok(json!({
            "stream_id": stream_id,
            "language_server_id": fake.language_server_id.to_proto(),
        }));
    }

    let transport = cx
        .update(|cx| preview_transport(project.read(cx), cx))
        .map_err(|message| anyhow!(message))?;
    let lsp_store = cx.update(|cx| project.read(cx).lsp_store());
    let server_id = cx
        .update(|cx| {
            find_tinymist_server_for_worktree(&lsp_store, Some(project_path.worktree_id), cx)
        })
        .context(
            "The project host is not running Tinymist. Install the Typst extension on the host.",
        )?;
    log::info!(
        "typst live preview: host starting Tinymist task {} with server {server_id:?}",
        abs_path.display()
    );

    let task_id = unique_task_id();
    let started =
        start_preview_task(&lsp_store, server_id, &abs_path, &task_id, transport, cx).await?;
    log::info!(
        "typst live preview: relaying {} (port {})",
        abs_path.display(),
        started.port
    );

    let (outbound, outbound_rx) = tokio::sync::mpsc::unbounded_channel();
    let stop = Arc::new(AtomicBool::new(false));
    let stream_id = task_id.clone();
    let queue = Arc::new(FrameQueue::new());
    relay.update(cx, |relay, cx| {
        relay.lsp_store.update(cx, |store, cx| {
            store.watch_custom_notification(server_id, OUTLINE_METHOD, cx);
        });
        relay.streams.insert(
            stream_id.clone(),
            Stream {
                abs_path,
                server_id,
                task_id,
                subscribers: {
                    let mut subscribers = Subscribers::default();
                    subscribers.subscribe(peer_id);
                    subscribers
                },
                local_port: started.port,
                outbound,
                outbound_rx: Some(outbound_rx),
                forward: started.forward,
                stop,
                ready: false,
                queue,
            },
        );
        relay.start_stream(&stream_id, cx);
    });

    Ok(json!({
        "stream_id": stream_id,
        "language_server_id": server_id.to_proto(),
    }))
}

/// Releases one pane's subscription.
fn stop_guest_stream(
    relay: Entity<PreviewRelay>,
    arguments: Vec<Value>,
    sender: Option<PeerId>,
    cx: &mut AsyncApp,
) -> anyhow::Result<()> {
    let stream_id = arguments
        .first()
        .and_then(|argument| argument.get("stream_id"))
        .and_then(Value::as_str)
        .context("the preview request is missing a stream id")?;
    let peer_id = sender.context("the preview request has no sender")?;
    relay.update(cx, |relay, cx| {
        relay.remove_subscriber(stream_id, peer_id, cx);
    });
    Ok(())
}

/// Long-polls queued frames for one stream.
async fn poll_guest_stream(
    relay: Entity<PreviewRelay>,
    arguments: Vec<Value>,
    cx: &mut AsyncApp,
) -> anyhow::Result<Option<Value>> {
    let stream_id = arguments
        .first()
        .and_then(|argument| argument.get("stream_id"))
        .and_then(Value::as_str)
        .context("the preview request is missing a stream id")?
        .to_owned();
    let after = arguments
        .first()
        .and_then(|argument| argument.get("after"))
        .and_then(Value::as_u64)
        .unwrap_or(0);

    let deadline = Instant::now() + POLL_WAIT;
    loop {
        let queue = relay.update(cx, |relay, _| {
            relay
                .streams
                .get(&stream_id)
                .map(|stream| stream.queue.clone())
        });
        let Some(queue) = queue else {
            return Ok(Some(json!({ "items": [{ "kind": "closed" }] })));
        };
        if let Some(items) = queue.take(after) {
            return Ok(Some(json!({ "items": items })));
        }
        if queue.is_closed() {
            return Ok(Some(json!({ "items": [{ "kind": "closed" }] })));
        }

        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(None);
        }
        let timer = cx.background_executor().timer(remaining);
        futures_lite::future::or(queue.notify().notified_owned(), timer).await;
    }
}

/// Resolves a guest click on the host and returns the source location.
async fn resolve_guest_click(
    relay: Entity<PreviewRelay>,
    arguments: Vec<Value>,
    cx: &mut AsyncApp,
) -> anyhow::Result<Option<Value>> {
    let argument = arguments.first().context("missing resolve arguments")?;
    let stream_id = argument
        .get("stream_id")
        .and_then(Value::as_str)
        .context("the resolve request is missing a stream id")?;
    let page = argument.get("page").and_then(Value::as_u64).unwrap_or(0) as usize;
    let x = argument.get("x").and_then(Value::as_f64).unwrap_or(0.0);
    let y = argument.get("y").and_then(Value::as_f64).unwrap_or(0.0);

    let (sender, receiver) = smol::channel::bounded(1);
    let outbound = relay.update(cx, |relay, _| {
        anyhow::ensure!(
            relay.pending_resolve.is_none(),
            "another preview click is already being resolved"
        );
        let stream = relay
            .streams
            .get(stream_id)
            .context("the preview stream is not running")?;
        relay.pending_resolve = Some(PendingResolve { sender });
        Ok(stream.outbound.clone())
    })?;

    let lsp_store = relay.update(cx, |relay, _| relay.lsp_store.clone());
    lsp_store.update(cx, |store, _cx| {
        store.route_next_show_document_to_preview();
    });
    outbound
        .send(source_point_message(page, x, y))
        .context("the preview connection is not running")?;

    let location =
        futures_lite::future::or(async { receiver.recv().await.ok().flatten() }, async {
            cx.background_executor().timer(RESOLVE_TIMEOUT).await;
            None
        })
        .await;

    relay.update(cx, |relay, _| {
        relay.pending_resolve = None;
    });
    if location.is_none() {
        lsp_store.update(cx, |store, _cx| store.clear_show_document_origin());
    }

    Ok(location.map(|location| {
        json!({
            "uri": location.uri,
            "selection_start": location.start.map(|(row, column)| json!({"row": row, "column": column})),
            "selection_end": location.end.map(|(row, column)| json!({"row": row, "column": column})),
        })
    }))
}

/// Whether a frame only moves the host's viewport.
///
/// `viewport` and `jump` frames are answers to *one* user's cursor or a source
/// click. The relay drops them so a guest's navigation never scrolls everyone
/// else's preview.
fn is_viewport_frame(bytes: &[u8]) -> bool {
    parse_control_frame(bytes).is_some()
}

#[cfg(any(test, feature = "test-support"))]
pub mod test_support {
    use std::{
        path::{Path, PathBuf},
        sync::{Arc, Mutex, PoisonError},
    };

    use serde_json::Value;

    /// A command the fake host feeds into its stream queue.
    pub enum HostCommand {
        Frame(Vec<u8>),
        Outline(Value),
        Close(String),
    }

    /// The fake host's view of one started preview.
    pub struct FakePreview {
        pub language_server_id: lsp::LanguageServerId,
        pub commands: smol::channel::Receiver<HostCommand>,
        pub src_points: smol::channel::Sender<String>,
    }

    type Factory = Arc<dyn Fn(&Path) -> Option<FakePreview> + Send + Sync>;

    static FACTORY: Mutex<Option<Factory>> = Mutex::new(None);

    /// Installs the preview backend used instead of Tinymist in tests.
    pub fn set_fake_preview(
        factory: impl Fn(&Path) -> Option<FakePreview> + Send + Sync + 'static,
    ) {
        *FACTORY.lock().unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(factory));
    }

    pub fn clear_fake_preview() {
        *FACTORY.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }

    pub(crate) fn fake_preview(path: &Path) -> Option<FakePreview> {
        let factory = FACTORY
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()?;
        factory(path)
    }

    /// A fake host that serves exactly one previewed path.
    ///
    /// `senders()` grows by one for every *started* stream, which lets a test
    /// assert that two panes joined one host task instead of starting two.
    pub struct FakeHost {
        expected: PathBuf,
        senders: Arc<Mutex<Vec<smol::channel::Sender<HostCommand>>>>,
        src_points: smol::channel::Sender<String>,
        src_points_rx: smol::channel::Receiver<String>,
    }

    impl FakeHost {
        pub fn new(expected: impl Into<PathBuf>) -> Self {
            let (src_points, src_points_rx) = smol::channel::unbounded();
            Self {
                expected: expected.into(),
                senders: Arc::new(Mutex::new(Vec::new())),
                src_points,
                src_points_rx,
            }
        }

        /// Installs this host as the process-wide fake preview backend.
        pub fn install(&self) {
            let expected = self.expected.clone();
            let senders = self.senders.clone();
            let src_points = self.src_points.clone();
            set_fake_preview(move |path| {
                if path != expected {
                    return None;
                }
                let (commands, receiver) = smol::channel::unbounded();
                senders
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(commands);
                Some(FakePreview {
                    language_server_id: lsp::LanguageServerId::from_proto(1),
                    commands: receiver,
                    src_points: src_points.clone(),
                })
            });
        }

        /// One sender per started stream. Waiting on a new entry proves the
        /// host handled a start request.
        pub fn senders(&self) -> Vec<smol::channel::Sender<HostCommand>> {
            self.senders
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
        }

        pub fn src_points(&self) -> &smol::channel::Receiver<String> {
            &self.src_points_rx
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, atomic::AtomicBool};

    use client::UserStore;
    use clock::FakeSystemClock;
    use http_client::FakeHttpClient;
    use language::LanguageRegistry;
    use project::{FakeFs, Fs, LocalProjectFlags, Project};
    use settings::SettingsStore;

    use super::*;

    fn peer(id: u64) -> PeerId {
        PeerId::from_u64(id)
    }

    #[test]
    fn subscribers_are_reference_counted_per_peer() {
        let mut subscribers = Subscribers::default();
        let alice = peer(1);

        subscribers.subscribe(alice);
        subscribers.subscribe(alice);

        assert!(!subscribers.unsubscribe(alice), "one pane is still open");
        assert!(
            subscribers.unsubscribe(alice),
            "the last pane releases the stream"
        );
    }

    #[test]
    fn a_leaving_peer_releases_only_its_own_subscriptions() {
        let mut subscribers = Subscribers::default();
        let (alice, bob) = (peer(1), peer(2));
        subscribers.subscribe(alice);
        subscribers.subscribe(alice);
        subscribers.subscribe(bob);

        assert!(!subscribers.remove_peer(alice), "bob is still watching");
        assert!(
            subscribers.unsubscribe(bob),
            "one unsubscribe releases bob; alice's counters are gone"
        );
    }

    #[test]
    fn an_unknown_peer_does_not_disturb_live_subscriptions() {
        let mut subscribers = Subscribers::default();
        let (alice, stranger) = (peer(1), peer(9));
        subscribers.subscribe(alice);

        assert!(!subscribers.unsubscribe(stranger));
        assert!(!subscribers.remove_peer(stranger));
        assert!(subscribers.unsubscribe(alice));
    }

    #[test]
    fn the_relay_drops_only_viewport_frames() {
        assert!(is_viewport_frame(b"viewport,2 12.5 34.25"));
        assert!(is_viewport_frame(b"jump,1 2 3,4 5 6"));
        assert!(!is_viewport_frame(b"diff-v1,payload"));
        assert!(!is_viewport_frame(b"new,payload"));
    }

    #[test]
    fn a_queue_keeps_items_until_the_guest_acknowledges_them() {
        let queue = FrameQueue::new();
        queue.push_outline(json!({ "items": [] }));
        queue.push_outline(json!({ "items": [{ "title": "One" }] }));

        let first = queue.take(0).expect("first batch");
        assert_eq!(first.len(), 2);
        let seq = first[1]["seq"].as_u64().unwrap();

        // Re-reading with the same cursor replays the same window, so a lost
        // response cannot lose a frame.
        assert_eq!(queue.take(0).expect("replayed batch").len(), 2);
        // Acknowledging drops the delivered items.
        assert!(queue.take(seq).is_none());
    }

    #[test]
    fn large_frames_are_chunked_and_reassemble_exactly() {
        let bytes = vec![0x5a; CHUNK_BYTES * 2 + 17];
        let queue = FrameQueue::new();
        queue.push_frame(&bytes);

        let batch = queue.take(0).expect("a batch");
        let mut assembly = FrameAssembly::default();
        let mut frame = None;
        for item in batch {
            if let Some(decoded) = assembly.accept(&item).unwrap() {
                frame = Some(decoded);
            }
        }

        assert_eq!(frame.expect("the whole frame"), bytes);
    }

    #[test]
    fn a_partial_frame_waits_for_its_remaining_chunks() {
        let bytes = vec![0x11; CHUNK_BYTES + 1];
        let queue = FrameQueue::new();
        queue.push_frame(&bytes);
        let batch = queue.take(0).expect("a batch");
        assert!(batch.len() > 1);

        let mut assembly = FrameAssembly::default();
        assert!(assembly.accept(&batch[0]).unwrap().is_none());
        let frame = batch[1..]
            .iter()
            .find_map(|item| assembly.accept(item).unwrap())
            .expect("the assembled frame");
        assert_eq!(frame, bytes);
    }

    #[test]
    fn closing_a_queue_delivers_a_stop_notice() {
        let queue = FrameQueue::new();
        queue.close("Tinymist exited");
        let batch = queue.take(0).expect("a stop notice");
        assert_eq!(batch[0]["kind"], "stopped");
        assert_eq!(batch[0]["reason"], "Tinymist exited");
        assert!(queue.is_closed());
    }

    fn test_project_with_client(
        client: Arc<client::Client>,
        user_store: Entity<UserStore>,
        fs: Arc<dyn Fs>,
        cx: &mut gpui::TestAppContext,
    ) -> Entity<Project> {
        let languages = LanguageRegistry::test(cx.executor());
        cx.update(|cx| {
            Project::local(
                client,
                node_runtime::NodeRuntime::unavailable(),
                user_store,
                Arc::new(languages),
                fs,
                None,
                LocalProjectFlags::default(),
                cx,
            )
        })
    }

    /// The app-wide collab client is shared by every project. Creating a
    /// second project must neither re-register message handlers nor let one
    /// project's relay answer for another's messages.
    #[gpui::test]
    async fn projects_sharing_one_collab_client_get_separate_relays(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            let mut settings_store = SettingsStore::test(cx);
            settings_store.register_setting::<feature_flags::FeatureFlagsSettings>();
            cx.set_global(settings_store);
        });

        let client = cx.update(|cx| {
            client::Client::new(
                Arc::new(FakeSystemClock::new()),
                FakeHttpClient::with_404_response(),
                cx,
            )
        });
        cx.update(|cx| client::Client::set_global(client.clone(), cx));

        // Calling `init` twice must be harmless.
        cx.update(|cx| crate::init(cx));
        cx.update(|cx| crate::init(cx));

        let fs = FakeFs::new(cx.executor());
        // Production has exactly one user store; it registers client handlers.
        let user_store = cx.new(|cx| UserStore::new(client.clone(), cx));
        let first = test_project_with_client(client.clone(), user_store.clone(), fs.clone(), cx);
        let second = test_project_with_client(client, user_store, fs, cx);

        let (first_relay, second_relay) = cx.update(|cx| {
            let registry = cx.try_global::<RelayRegistry>().expect("relay registry");
            (
                registry.0.get(&first.entity_id()).cloned(),
                registry.0.get(&second.entity_id()).cloned(),
            )
        });
        let first_relay = first_relay.expect("the first project has a relay");
        let second_relay = second_relay.expect("the second project has a relay");
        assert_ne!(first_relay.entity_id(), second_relay.entity_id());

        let mut async_cx = cx.to_async();
        first_relay.update(&mut async_cx, |relay, _| {
            relay.streams.insert(
                "only-in-first".to_owned(),
                Stream {
                    abs_path: PathBuf::from("/one/test.typ"),
                    server_id: LanguageServerId::from_proto(1),
                    task_id: "only-in-first".to_owned(),
                    subscribers: Subscribers::default(),
                    local_port: 0,
                    outbound: tokio::sync::mpsc::unbounded_channel().0,
                    outbound_rx: None,
                    forward: None,
                    stop: Arc::new(AtomicBool::new(false)),
                    ready: true,
                    queue: Arc::new(FrameQueue::new()),
                },
            )
        });
        assert!(first_relay.update(&mut async_cx, |relay, _| {
            relay.streams.contains_key("only-in-first")
        }));
        assert!(!second_relay.update(&mut async_cx, |relay, _| {
            relay.streams.contains_key("only-in-first")
        }));
    }
}
