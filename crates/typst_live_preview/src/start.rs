//! Starting and stopping a preview through Tinymist's language server.
//!
//! The preview server belongs to the Tinymist process the editor already runs
//! through the Typst extension. Starting it is a `workspace/executeCommand`
//! request, so no second LSP process, compiler daemon, or sidecar is needed:
//!
//! 1. `tinymist.focusMain` points Tinymist at this buffer's file, so the
//!    preview compiles from the editor's in-memory source rather than whatever
//!    is on disk.
//! 2. `tinymist.doStartBrowsingPreview` starts a preview task and returns the
//!    data-plane WebSocket port. The hidden `--no-open` and port-0 arguments
//!    keep it headless on a random loopback port.
//! 3. `tinymist.doKillPreview` stops the task when the pane closes.
//!
//! If Typst support is not installed yet, asking for a preview installs the
//! `typst` extension on the spot and waits for Tinymist to come up, rather
//! than sending the user to the extension browser mid-document.
//!
//! For an SSH project Tinymist runs on the remote host, so its loopback port is
//! carried over the project's SSH connection by [`crate::tunnel`].

use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use anyhow::Context as _;
use editor::Editor;
use extension_host::{ExtensionStatus, ExtensionStore};
use gpui::{App, AsyncApp, Context, Entity, EntityId, Window};
use language::{Buffer, ToOffset as _};
use lsp::LanguageServerId;
use project::{LanguageServerStatus, LspStore, Project, WorktreeId};
use remote::RemoteClient;
use serde_json::json;
use workspace::{Pane, Toast, Workspace, notifications::NotificationId};

use crate::tunnel::PortForward;

/// How long Tinymist may take to answer the preview start command.
const START_TIMEOUT: Duration = Duration::from_secs(15);
use crate::view::{
    OpenPreview, PreviewOwner, PreviewSource, ScrollPreviewToCursor, TypstPreviewView,
    is_typst_path,
};

/// The extension id in Zed's extension registry.
const TYPST_EXTENSION_ID: &str = "typst";

/// How long to wait for Tinymist after installing Typst support.
const INSTALL_TIMEOUT: Duration = Duration::from_secs(30);
const INSTALL_POLL_INTERVAL: Duration = Duration::from_millis(500);

struct TypstPreviewToast;

/// Registers the Typst live preview action.
pub fn init(cx: &mut App) {
    crate::collab::init(cx);
    cx.observe_new(|workspace: &mut Workspace, _window, _cx| {
        workspace.register_action(|workspace, _: &OpenPreview, window, cx| {
            open_preview(workspace, window, cx);
        });
        workspace.register_action(|workspace, _: &ScrollPreviewToCursor, _window, cx| {
            scroll_preview_to_cursor(workspace, cx);
        });
    })
    .detach();
}

/// Scrolls the preview of the active editor's file to the primary cursor.
fn scroll_preview_to_cursor(workspace: &mut Workspace, cx: &mut Context<Workspace>) {
    if workspace.project().read(cx).is_via_collab() {
        show_toast(
            workspace,
            "Forward sync is off in shared projects. Scroll the preview directly.",
            cx,
        );
        return;
    }
    let Some(editor) = workspace
        .active_item(cx)
        .and_then(|item| item.downcast::<Editor>())
    else {
        return;
    };
    let Some((path, line, character)) =
        editor.read_with(cx, |editor, cx| cursor_source_point(editor, cx))
    else {
        return;
    };

    for pane in workspace.panes() {
        let views: Vec<_> = pane.read(cx).items_of_type::<TypstPreviewView>().collect();
        for view in views {
            if view.read(cx).path() == path {
                view.update(cx, |view, cx| {
                    view.scroll_to_source(&path, line, character, cx);
                });
            }
        }
    }
}

/// The editor's file path and primary cursor in source coordinates (UTF-16).
pub(crate) fn cursor_source_point(editor: &Editor, cx: &App) -> Option<(PathBuf, u32, u32)> {
    let path = editor_file_path(editor, cx)?;
    let head = editor.selections.newest_anchor().head();
    let multi_buffer = editor.buffer().read(cx);
    let snapshot = multi_buffer.snapshot(cx);
    let (anchor, buffer_snapshot) = snapshot.anchor_to_buffer_anchor(head)?;
    let point = buffer_snapshot.offset_to_point_utf16(anchor.to_offset(buffer_snapshot));
    Some((path, point.row, point.column))
}

fn open_preview(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let Some(editor) = workspace
        .active_item(cx)
        .and_then(|item| item.downcast::<Editor>())
    else {
        return;
    };
    open_preview_for_editor(workspace, editor, window, cx);
}

/// Starts a live preview for the file `editor` is showing.
pub fn open_preview_for_editor(
    workspace: &mut Workspace,
    editor: Entity<Editor>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    open_preview_for_editor_impl(workspace, editor, None, window, cx);
}

/// The preview pane to replace when a temporary editor tab changes files.
pub(crate) struct PreviewPlacement {
    /// The pane that holds the preview pane item.
    pub pane: Entity<Pane>,
    /// The preview view to replace.
    pub view_id: EntityId,
}

/// Starts a preview for `editor` in place of `replace`.
///
/// This is the temporary-tab follow path: the editor that owned the old
/// preview was replaced by the pane's preview tab opening another file, so the
/// old preview view is swapped for one that renders the new file.
pub(crate) fn open_preview_for_editor_replacing(
    workspace: &mut Workspace,
    editor: Entity<Editor>,
    replace: PreviewPlacement,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    open_preview_for_editor_impl(workspace, editor, Some(replace), window, cx);
}

fn open_preview_for_editor_impl(
    workspace: &mut Workspace,
    editor: Entity<Editor>,
    replace: Option<PreviewPlacement>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(path): Option<PathBuf> =
        editor.read_with(cx, |editor, cx| editor_file_path(editor, cx))
    else {
        show_toast(
            workspace,
            "Save the file first — live preview needs a file on disk.",
            cx,
        );
        return;
    };
    if !is_typst_path(&path) {
        show_toast(workspace, "Live preview works on Typst (.typ) files.", cx);
        return;
    }
    let lsp_store = workspace.project().read(cx).lsp_store();
    if workspace.project().read(cx).is_via_collab() {
        let Some(buffer) = editor.read(cx).buffer().read(cx).as_singleton() else {
            return;
        };
        crate::view::set_primary_preview(&path);
        start_collab_preview(
            workspace,
            path,
            Some(buffer),
            Some(editor),
            lsp_store,
            replace,
            window,
            cx,
        );
        return;
    }
    // The data plane listens on the LSP host's loopback. For an SSH project
    // that is the remote machine, so the pane gets a forwarded local port.
    let transport = {
        let project = workspace.project().read(cx);
        preview_transport(project, cx)
    };
    let transport = match transport {
        Ok(transport) => transport,
        Err(message) => {
            show_toast(workspace, message, cx);
            return;
        }
    };

    let Some(buffer) = editor.read(cx).buffer().read(cx).as_singleton() else {
        return;
    };

    if let Some(server_id) = find_tinymist_server(&lsp_store, &buffer, cx) {
        start_preview(
            path,
            Some(buffer),
            Some(editor),
            transport,
            lsp_store,
            server_id,
            replace,
            window,
            cx,
        );
        return;
    }

    offer_to_install(
        workspace,
        path,
        Some(buffer),
        Some(editor),
        transport,
        lsp_store,
        replace,
        window,
        cx,
    );
}

/// How the pane reaches Tinymist's preview data plane.
#[derive(Clone)]
pub(crate) enum PreviewTransport {
    /// Tinymist's port is reachable as-is (a local project, or a remote that
    /// shares this machine's loopback).
    Direct,
    /// Tinymist runs on another machine; carry its port over the project's SSH
    /// connection.
    Forward(Entity<RemoteClient>),
}

/// The outcome of [`preview_transport`], split out so the decision matrix can
/// be tested without building a project.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TransportKind {
    Direct,
    Forward,
    /// A collaborative guest must ask the host for a relayed stream instead
    /// (`start_collab_preview`); this arm only guards against a call path that
    /// forgets to branch to it.
    Collab,
    MissingRemoteClient,
}

fn transport_kind(
    is_via_collab: bool,
    is_remote: bool,
    has_remote_client: bool,
    shares_network_interface: bool,
) -> TransportKind {
    if is_via_collab {
        TransportKind::Collab
    } else if !is_remote || shares_network_interface {
        TransportKind::Direct
    } else if !has_remote_client {
        TransportKind::MissingRemoteClient
    } else {
        TransportKind::Forward
    }
}

pub(crate) fn preview_transport(
    project: &Project,
    cx: &App,
) -> Result<PreviewTransport, &'static str> {
    let remote_client = project.remote_client();
    let shares_network_interface = remote_client
        .as_ref()
        .is_some_and(|client| client.read(cx).shares_network_interface());

    match transport_kind(
        project.is_via_collab(),
        project.is_remote(),
        remote_client.is_some(),
        shares_network_interface,
    ) {
        TransportKind::Direct => Ok(PreviewTransport::Direct),
        TransportKind::Forward => match remote_client {
            Some(client) => Ok(PreviewTransport::Forward(client)),
            None => Err("Typst live preview could not reach the remote host."),
        },
        TransportKind::Collab => {
            Err("Typst live preview routes collaborative projects through the project host.")
        }
        TransportKind::MissingRemoteClient => {
            Err("Typst live preview could not reach the remote host.")
        }
    }
}

/// Explains why the preview cannot start yet, installing Typst support when
/// that is the missing piece.
fn offer_to_install(
    workspace: &mut Workspace,
    path: PathBuf,
    buffer: Option<Entity<Buffer>>,
    editor: Option<Entity<Editor>>,
    transport: PreviewTransport,
    lsp_store: Entity<LspStore>,
    replace: Option<PreviewPlacement>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let status = ExtensionStore::try_global(cx)
        .map(|store| store.read(cx).extension_status(TYPST_EXTENSION_ID));
    log::info!(
        "typst live preview: Tinymist is not ready for {} (extension status: {status:?})",
        path.display()
    );

    let message = match status {
        Some(ExtensionStatus::NotInstalled) => {
            let Some(store) = ExtensionStore::try_global(cx) else {
                return;
            };
            store.update(cx, |store, cx| {
                store.install_latest_extension(Arc::from(TYPST_EXTENSION_ID), cx);
            });
            "Installing Typst support. The preview opens as soon as Tinymist is ready…"
        }
        Some(ExtensionStatus::Installing | ExtensionStatus::Upgrading) => {
            "Typst support is still installing. The preview opens when Tinymist is ready…"
        }
        // The extension can be installed while its language server is still
        // starting, or while Tinymist has not yet registered this buffer. That
        // is not a reason to tell the user to install the extension again.
        Some(ExtensionStatus::Installed(_)) => {
            "Waiting for Tinymist to start. The preview opens when it is ready…"
        }
        Some(ExtensionStatus::Removing) => {
            "Typst support is being removed. Wait for it to finish and reopen this file."
        }
        None => {
            "Typst live preview needs Tinymist. Install the Typst extension and reopen this file."
        }
    };

    show_toast(workspace, message, cx);
    if matches!(
        status,
        Some(
            ExtensionStatus::NotInstalled
                | ExtensionStatus::Installing
                | ExtensionStatus::Upgrading
                | ExtensionStatus::Installed(_)
        )
    ) {
        wait_for_tinymist(
            path, buffer, editor, transport, lsp_store, replace, window, cx,
        );
    }
}

/// Polls for the language server the extension is installing.
fn wait_for_tinymist(
    path: PathBuf,
    buffer: Option<Entity<Buffer>>,
    editor: Option<Entity<Editor>>,
    transport: PreviewTransport,
    lsp_store: Entity<LspStore>,
    replace: Option<PreviewPlacement>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    cx.spawn_in(window, async move |workspace, cx| {
        let attempts = INSTALL_TIMEOUT.as_millis() / INSTALL_POLL_INTERVAL.as_millis();
        for _ in 0..attempts {
            cx.background_executor().timer(INSTALL_POLL_INTERVAL).await;

            let found = cx.update(|_, cx| {
                buffer
                    .as_ref()
                    .and_then(|buffer| find_tinymist_server(&lsp_store, buffer, cx))
            });
            if let Ok(Some(server_id)) = found {
                workspace
                    .update_in(cx, |_, window, cx| {
                        start_preview(
                            path,
                            buffer,
                            editor,
                            transport,
                            lsp_store,
                            server_id,
                            replace,
                            window,
                            cx,
                        );
                    })
                    .ok();
                return;
            }
        }

        workspace
            .update(cx, |workspace, cx| {
                show_toast(
                    workspace,
                    "Typst support installed, but Tinymist did not start. Reopen the .typ file and try again.",
                    cx,
                )
            })
            .ok();
    })
    .detach();
}

fn start_preview(
    path: PathBuf,
    buffer: Option<Entity<Buffer>>,
    editor: Option<Entity<Editor>>,
    transport: PreviewTransport,
    lsp_store: Entity<LspStore>,
    server_id: LanguageServerId,
    replace: Option<PreviewPlacement>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let task_id = unique_task_id();
    let owner = PreviewOwner::new(lsp_store.downgrade(), server_id, task_id.clone());
    crate::view::set_primary_preview(&path);

    cx.spawn_in(window, async move |workspace, cx| {
        let started =
            match start_preview_task(&lsp_store, server_id, &path, &task_id, transport, cx).await {
                Ok(started) => started,
                Err(error) => {
                    let message = format!("Tinymist could not start the preview: {error:#}");
                    workspace
                        .update(cx, |workspace, cx| show_toast(workspace, &message, cx))
                        .ok();
                    return;
                }
            };
        log::info!(
            "typst live preview: data plane on 127.0.0.1:{} for {}",
            started.port,
            path.display()
        );

        workspace
            .update_in(cx, |workspace, window, cx| {
                TypstPreviewView::open(
                    workspace,
                    path,
                    buffer,
                    editor,
                    owner.with_forward(started.forward),
                    PreviewSource::WebSocket { port: started.port },
                    replace,
                    window,
                    cx,
                );
            })
            .ok();
    })
    .detach();
}

/// Starts a preview whose compiler runs on the collab host.
///
/// The guest cannot reach Tinymist's loopback port, so it asks the host for a
/// relayed stream and renders the frames itself.
fn start_collab_preview(
    workspace: &mut Workspace,
    path: PathBuf,
    buffer: Option<Entity<Buffer>>,
    editor: Option<Entity<Editor>>,
    lsp_store: Entity<LspStore>,
    replace: Option<PreviewPlacement>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let project = workspace.project().clone();
    if project.read(cx).remote_id().is_none() {
        show_toast(
            workspace,
            "The shared project has no id yet. Try again in a moment.",
            cx,
        );
        return;
    }

    cx.spawn_in(window, async move |workspace, mut cx| {
        let relay = match crate::collab::RelayClient::start(lsp_store.clone(), &path, &mut cx).await
        {
            Ok(relay) => relay,
            Err(error) => {
                let message = format!(
                    "The project host could not start the Typst preview: {error:#}. The host needs the Typst extension, Tinymist, and a Suzuri build with relay support."
                );
                workspace
                    .update(cx, |workspace, cx| show_toast(workspace, &message, cx))
                    .ok();
                return;
            }
        };

        let owner = PreviewOwner::new_collab(
            lsp_store.downgrade(),
            relay.stream_id().to_owned(),
            relay.server_id(),
        );
        workspace
            .update_in(cx, |workspace, window, cx| {
                TypstPreviewView::open(
                    workspace,
                    path,
                    buffer,
                    editor,
                    owner,
                    PreviewSource::Collab(relay),
                    replace,
                    window,
                    cx,
                );
            })
            .ok();
    })
    .detach();
}

/// A preview task started through Tinymist's language server.
pub(crate) struct StartedPreview {
    /// The local data-plane port, after any SSH forwarding.
    pub port: u16,
    /// The tunnel carrying a remote data plane, if one was needed.
    pub forward: Option<Arc<PortForward>>,
}

/// Starts a Tinymist preview task and returns its reachable data-plane port.
///
/// Shared by the local pane and the collaborative host relay: both need the
/// same `focusMain` + `doStartBrowsingPreview` sequence and transport handling.
pub(crate) async fn start_preview_task(
    lsp_store: &Entity<LspStore>,
    server_id: LanguageServerId,
    path: &Path,
    task_id: &str,
    transport: PreviewTransport,
    cx: &mut AsyncApp,
) -> anyhow::Result<StartedPreview> {
    let focus = lsp_store.update(cx, |store, cx| {
        store.execute_lsp_command(
            server_id,
            "tinymist.focusMain".to_owned(),
            vec![json!(path.display().to_string())],
            cx,
        )
    });
    if let Err(error) = focus.await {
        // Focus is a best-effort improvement: a preview started without it
        // still renders, just from the file Tinymist last focused.
        log::warn!("tinymist.focusMain failed: {error:#}");
    }

    let arguments = vec![json!([
        path.display().to_string(),
        "--data-plane-host=127.0.0.1:0",
        "--control-plane-host=127.0.0.1:0",
        "--no-open",
        format!("--task-id={task_id}"),
    ])];
    let start = lsp_store.update(cx, |store, cx| {
        store.execute_lsp_command(
            server_id,
            "tinymist.doStartBrowsingPreview".to_owned(),
            arguments,
            cx,
        )
    });
    let response = futures_lite::future::or(async { start.await }, async {
        cx.background_executor().timer(START_TIMEOUT).await;
        Err(anyhow::Error::msg(
            "Tinymist did not answer the preview start request. Restart Tinymist on the host.",
        ))
    })
    .await?;
    let port = response
        .as_ref()
        .and_then(|value| value.get("dataPlanePort"))
        .and_then(|value| value.as_u64())
        .and_then(|port| u16::try_from(port).ok())
        .context("Tinymist did not return a preview data-plane port")?;

    let (port, forward) = match transport {
        PreviewTransport::Direct => (port, None),
        PreviewTransport::Forward(remote_client) => {
            let (local_port, forward) = PortForward::open_in_app(&remote_client, port, cx)
                .await
                .context("Tinymist started on the remote host, but its preview port could not be forwarded")?;
            (local_port, Some(Arc::new(forward)))
        }
    };

    Ok(StartedPreview { port, forward })
}

fn find_tinymist_server(
    lsp_store: &Entity<LspStore>,
    buffer: &Entity<Buffer>,
    cx: &mut App,
) -> Option<LanguageServerId> {
    let (buffer_id, worktree_id) = buffer.read_with(cx, |buffer, cx| {
        (
            buffer.remote_id(),
            buffer.file().map(|file| file.worktree_id(cx)),
        )
    });

    lsp_store.update(cx, |store, _cx| {
        // Remote projects run language servers on the host, so this store has no
        // live `LanguageServer` handles. Their status map is still kept in sync
        // by `StartLanguageServer` / `UpdateLanguageServer` messages, and that
        // is enough to route `executeCommand` to the right server.
        let is_local = store.as_local().is_some();

        if is_local {
            let opened_in = store
                .language_server_ids_for_opened_buffer(buffer_id)
                .map(|ids| ids.iter().copied().collect::<Vec<_>>())
                .unwrap_or_default();
            if let Some(server_id) = opened_in.into_iter().find(|server_id| {
                store
                    .language_server_for_id(*server_id)
                    .is_some_and(|server| is_tinymist_name(server.name().0.as_ref()))
            }) {
                return Some(server_id);
            }
        }

        // A running Tinymist is enough to start a preview: `focusMain` carries
        // the path and `doStartBrowsingPreview` starts the data plane. This
        // covers the short window where the server is up but has not sent its
        // `didOpen` registration for this buffer yet, and reopened buffers
        // whose registration was released while the process stayed alive.
        find_tinymist_server_for_worktree_in_store(store, worktree_id)
    })
}

/// Finds Tinymist from the status map, without needing an open buffer.
///
/// The collaborative relay only has the requested path, not the guest's
/// buffer, so it resolves the worktree id and uses this instead.
pub(crate) fn find_tinymist_server_for_worktree(
    lsp_store: &Entity<LspStore>,
    worktree_id: Option<WorktreeId>,
    cx: &mut App,
) -> Option<LanguageServerId> {
    lsp_store.update(cx, |store, _cx| {
        find_tinymist_server_for_worktree_in_store(store, worktree_id)
    })
}

fn find_tinymist_server_for_worktree_in_store(
    store: &LspStore,
    worktree_id: Option<WorktreeId>,
) -> Option<LanguageServerId> {
    let is_local = store.as_local().is_some();
    let from_statuses = tinymist_server_in_statuses(store.language_server_statuses(), worktree_id);

    if is_local {
        // Locally, statuses can outlive a stopped server. Keep confirming
        // that the entry still has a live process behind it.
        from_statuses.filter(|server_id| {
            store
                .language_server_for_id(*server_id)
                .is_some_and(|server| is_tinymist_name(server.name().0.as_ref()))
        })
    } else {
        from_statuses
    }
}

fn is_tinymist_name(name: &str) -> bool {
    name.eq_ignore_ascii_case("tinymist")
}

/// Picks Tinymist out of the language-server status map.
///
/// This works for both local and remote projects: the status map is populated
/// from the host even when the server itself is not running in this process.
fn tinymist_server_in_statuses<'a>(
    statuses: impl Iterator<Item = (LanguageServerId, &'a LanguageServerStatus)>,
    worktree_id: Option<WorktreeId>,
) -> Option<LanguageServerId> {
    statuses
        .filter(|(_, status)| {
            is_tinymist_name(status.name.0.as_ref())
                && (status.worktree.is_none() || status.worktree == worktree_id)
        })
        .map(|(server_id, _)| server_id)
        .next()
}

fn editor_file_path(editor: &Editor, cx: &App) -> Option<PathBuf> {
    let buffer = editor.buffer().read(cx).as_singleton()?;
    let file = buffer.read(cx).file()?;
    // For remote files this is the path on the remote host, which is exactly
    // what Tinymist (running there) expects.
    file.file_system_abs_path(cx)
}

/// Whether the editor is showing a Typst document.
pub fn is_typst_editor(editor: &Entity<Editor>, cx: &App) -> bool {
    editor
        .read_with(cx, |editor, cx| editor_file_path(editor, cx))
        .is_some_and(|path| is_typst_path(&path))
}

static NEXT_TASK_ID: AtomicU64 = AtomicU64::new(1);

pub(crate) fn unique_task_id() -> String {
    format!(
        "typst-live-preview-{}",
        NEXT_TASK_ID.fetch_add(1, Ordering::Relaxed)
    )
}

fn show_toast(workspace: &mut Workspace, message: &str, cx: &mut Context<Workspace>) {
    workspace.show_toast(
        Toast::new(
            NotificationId::unique::<TypstPreviewToast>(),
            message.to_string(),
        ),
        cx,
    );
}

/// Whether the workspace's Tinymist server is available for this buffer.
///
/// Exposed for callers that want to hide the preview affordance when Typst
/// support is not installed.
pub fn tinymist_server_for(editor: &Editor, cx: &mut App) -> Option<LanguageServerId> {
    let buffer = editor.buffer().read(cx).as_singleton()?;
    let lsp_store = editor.project()?.read(cx).lsp_store();
    find_tinymist_server(&lsp_store, &buffer, cx)
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use lsp::{LanguageServerId, LanguageServerName};
    use project::{LanguageServerStatus, WorktreeId};

    use super::{TransportKind, tinymist_server_in_statuses, transport_kind};

    fn status(name: &str, worktree: Option<WorktreeId>) -> LanguageServerStatus {
        LanguageServerStatus {
            name: LanguageServerName(name.into()),
            language_name: None,
            server_version: None,
            server_readable_version: None,
            pending_work: BTreeMap::new(),
            has_pending_diagnostic_updates: false,
            progress_tokens: Default::default(),
            worktree,
            binary: None,
            configuration: None,
            workspace_folders: BTreeSet::new(),
            process_id: None,
        }
    }

    fn find(
        statuses: &[(LanguageServerId, LanguageServerStatus)],
        worktree_id: Option<WorktreeId>,
    ) -> Option<LanguageServerId> {
        tinymist_server_in_statuses(
            statuses.iter().map(|(id, status)| (*id, status)),
            worktree_id,
        )
    }

    #[test]
    fn status_lookup_finds_tinymist_without_a_local_server_handle() {
        let worktree = WorktreeId::from_usize(7);
        let statuses = [
            (LanguageServerId(1), status("rust-analyzer", Some(worktree))),
            (LanguageServerId(2), status("Tinymist", Some(worktree))),
        ];

        assert_eq!(find(&statuses, Some(worktree)), Some(LanguageServerId(2)));
    }

    #[test]
    fn status_lookup_skips_servers_from_other_worktrees() {
        let worktree = WorktreeId::from_usize(7);
        let statuses = [
            (
                LanguageServerId(1),
                status("tinymist", Some(WorktreeId::from_usize(8))),
            ),
            (LanguageServerId(2), status("tinymist", Some(worktree))),
        ];

        assert_eq!(find(&statuses, Some(worktree)), Some(LanguageServerId(2)));
    }

    #[test]
    fn status_lookup_accepts_project_wide_servers() {
        let worktree = WorktreeId::from_usize(7);
        let statuses = [(LanguageServerId(1), status("tinymist", None))];

        assert_eq!(find(&statuses, Some(worktree)), Some(LanguageServerId(1)));
    }

    #[test]
    fn status_lookup_returns_none_without_tinymist() {
        let worktree = WorktreeId::from_usize(7);
        let statuses = [(LanguageServerId(1), status("rust-analyzer", Some(worktree)))];

        assert_eq!(find(&statuses, Some(worktree)), None);
    }

    #[test]
    fn local_projects_use_the_port_directly() {
        assert_eq!(
            transport_kind(false, false, false, false),
            TransportKind::Direct
        );
    }

    #[test]
    fn remotes_that_share_the_network_use_the_port_directly() {
        assert_eq!(
            transport_kind(false, true, true, true),
            TransportKind::Direct
        );
    }

    #[test]
    fn ssh_remotes_forward_the_port() {
        assert_eq!(
            transport_kind(false, true, true, false),
            TransportKind::Forward
        );
    }

    #[test]
    fn collab_projects_never_resolve_to_a_local_transport() {
        assert_eq!(
            transport_kind(true, true, true, false),
            TransportKind::Collab
        );
    }

    #[test]
    fn missing_remote_clients_are_rejected() {
        assert_eq!(
            transport_kind(false, true, false, false),
            TransportKind::MissingRemoteClient
        );
    }
}
