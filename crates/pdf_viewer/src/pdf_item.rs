use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context as _, Result};
use gpui::{
    App, AppContext as _, BackgroundExecutor, Context, Entity, EventEmitter, Subscription, Task,
    WeakEntity,
};
use project::{Project, ProjectEntryId, ProjectItem, ProjectPath};
use util::ResultExt as _;

pub struct PdfItem {
    project: WeakEntity<Project>,
    project_path: ProjectPath,
    /// Where the document lives from the worktree's point of view. For a
    /// remote project this is a path on the host, so it is only ever shown
    /// (tab tooltip, icon lookup), never read.
    abs_path: PathBuf,
    pdf_bytes: Arc<[u8]>,
    reload_task: Task<()>,
    _project_subscription: Subscription,
}

pub enum PdfItemEvent {
    Reloaded,
}

impl EventEmitter<PdfItemEvent> for PdfItem {}

impl PdfItem {
    pub fn abs_path(&self) -> &Path {
        &self.abs_path
    }

    pub fn file_name(&self) -> &str {
        self.abs_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("document.pdf")
    }

    pub fn pdf_bytes(&self) -> &Arc<[u8]> {
        &self.pdf_bytes
    }

    pub fn project_path(&self) -> &ProjectPath {
        &self.project_path
    }

    /// Reloads the bytes from disk when the file changes underneath us —
    /// a recompiled Typst/LaTeX document, a re-exported figure, an
    /// agent-rewritten file — and announces it so views re-render.
    fn reload(&mut self, cx: &mut Context<Self>) {
        let Some(project) = self.project.upgrade() else {
            return;
        };
        let load = load_pdf(&project, &self.project_path, self.abs_path.clone(), cx);
        self.reload_task = cx.spawn(async move |this, cx| {
            let Some(bytes) = load.await.log_err() else {
                // Still incomplete after waiting the writer out. Keep the
                // document already on screen rather than replacing it with
                // bytes that cannot be parsed.
                return;
            };
            this.update(cx, |this, cx| {
                this.pdf_bytes = bytes;
                cx.emit(PdfItemEvent::Reloaded);
            })
            .ok();
        });
    }
}

pub fn is_pdf_file(path: &ProjectPath) -> bool {
    path.path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("pdf"))
}

/// How long to keep waiting for a writer to finish, as attempts spaced by
/// [`PDF_READ_RETRY_DELAY`]. A recompile rewrites the whole file, so this has
/// to cover the largest document someone might preview, not a typical one.
const PDF_READ_ATTEMPTS: usize = 20;
/// A downloaded copy is written by a message handler that runs after the
/// download request has already reported success, so the whole file may
/// still be on its way to disk. Allow for a large document on a slow disk.
const DOWNLOADED_PDF_READ_ATTEMPTS: usize = 300;
const PDF_READ_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(100);
/// `%%EOF` sits at the very end, after the cross-reference offset.
const PDF_TRAILER_WINDOW: usize = 1024;

/// Whether `bytes` is a whole PDF rather than one caught mid-write.
///
/// A partial read is the failure that matters here, and it is invisible to
/// error handling: `read` returns the truncated prefix quite successfully, so
/// the bytes reach the parser and only fail there. Every PDF opens with
/// `%PDF-` and closes with `%%EOF`, and a writer that has not finished has
/// not written the trailer yet.
fn is_complete_pdf(bytes: &[u8]) -> bool {
    if !bytes.starts_with(b"%PDF-") {
        return false;
    }
    let tail = &bytes[bytes.len().saturating_sub(PDF_TRAILER_WINDOW)..];
    tail.windows(b"%%EOF".len())
        .any(|window| window == b"%%EOF")
}

/// Loads the document's bytes from wherever the project keeps them.
///
/// A local worktree is read straight from disk. A remote worktree cannot be
/// read at all from this side of the connection (`Worktree::load_binary_file`
/// refuses for remote worktrees), so the bytes are fetched through the same
/// download channel the project panel's "Download" action uses, which the
/// remote server already serves for any path in the worktree.
fn load_pdf(
    project: &Entity<Project>,
    project_path: &ProjectPath,
    abs_path: PathBuf,
    cx: &mut App,
) -> Task<Result<Arc<[u8]>>> {
    let is_local = project
        .read(cx)
        .worktree_for_id(project_path.worktree_id, cx)
        .is_some_and(|worktree| worktree.read(cx).is_local());
    if is_local {
        load_pdf_bytes(
            abs_path,
            cx.background_executor().clone(),
            PDF_READ_ATTEMPTS,
        )
    } else {
        download_remote_pdf(project, project_path.clone(), &abs_path, cx)
    }
}

static NEXT_DOWNLOAD_ID: AtomicU64 = AtomicU64::new(1);

/// Copies a remote PDF to a scratch file, reads it, and removes the copy.
///
/// The bytes are held in memory by [`PdfItem`], so nothing on disk needs to
/// outlive this call; a stale copy would only go out of date on the next
/// remote recompile anyway.
fn download_remote_pdf(
    project: &Entity<Project>,
    project_path: ProjectPath,
    abs_path: &Path,
    cx: &mut App,
) -> Task<Result<Arc<[u8]>>> {
    let file_name = abs_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("document.pdf")
        .to_owned();
    let download_id = NEXT_DOWNLOAD_ID.fetch_add(1, Ordering::SeqCst);
    let destination =
        remote_pdf_scratch_dir().join(format!("{}-{download_id}-{file_name}", std::process::id()));
    let background = cx.background_executor().clone();
    let project = project.clone();
    let abs_path = abs_path.to_owned();

    cx.spawn(async move |cx| {
        background
            .spawn(async { std::fs::create_dir_all(remote_pdf_scratch_dir()) })
            .await
            .context("Failed to create the scratch directory for remote PDFs")?;

        let download = project.update(cx, |project, cx| {
            project.download_file(
                project_path.worktree_id,
                project_path.path.clone(),
                destination.clone(),
                cx,
            )
        });
        let result = match download.await {
            Ok(()) => {
                // Success here means the server has sent every chunk, not
                // that the client has finished writing them, so keep
                // reading until the trailer shows up.
                load_pdf_bytes(
                    destination.clone(),
                    background.clone(),
                    DOWNLOADED_PDF_READ_ATTEMPTS,
                )
                .await
            }
            Err(error) => Err(error),
        }
        .with_context(|| format!("Failed to download remote PDF: {}", abs_path.display()));

        background
            .spawn(async move {
                if let Err(error) = std::fs::remove_file(&destination)
                    && error.kind() != std::io::ErrorKind::NotFound
                {
                    log::warn!(
                        "failed to remove downloaded PDF copy {}: {error}",
                        destination.display()
                    );
                }
            })
            .await;
        result
    })
}

fn remote_pdf_scratch_dir() -> PathBuf {
    paths::temp_dir().join("remote-pdfs")
}

fn load_pdf_bytes(
    abs_path: PathBuf,
    background: BackgroundExecutor,
    attempts: usize,
) -> Task<Result<Arc<[u8]>>> {
    let timers = background.clone();
    background.spawn(async move {
        let mut failure = None;
        for attempt in 0..attempts {
            if attempt > 0 {
                timers.timer(PDF_READ_RETRY_DELAY).await;
            }
            match std::fs::read(&abs_path) {
                Ok(bytes) if is_complete_pdf(&bytes) => return Ok(Arc::from(bytes)),
                // Truncated: the compiler is still writing. Leave the
                // previously loaded document on screen and look again.
                Ok(bytes) => {
                    failure = Some(anyhow::anyhow!(
                        "PDF at {} was incomplete after {} bytes",
                        abs_path.display(),
                        bytes.len()
                    ));
                }
                Err(error) => {
                    failure = Some(anyhow::Error::new(error));
                }
            }
        }
        Err(failure.unwrap_or_else(|| anyhow::anyhow!("no read attempted")))
            .with_context(|| format!("Failed to read PDF: {}", abs_path.display()))
    })
}

impl ProjectItem for PdfItem {
    fn try_open(
        project: &Entity<Project>,
        path: &ProjectPath,
        cx: &mut App,
    ) -> Option<Task<Result<Entity<Self>>>> {
        if !is_pdf_file(path) {
            return None;
        }

        let worktree = project.read(cx).worktree_for_id(path.worktree_id, cx)?;
        let abs_path = worktree.read(cx).abs_path().join(path.path.as_std_path());
        let project_path = path.clone();
        let load = load_pdf(project, &project_path, abs_path.clone(), cx);
        let project = project.clone();

        Some(cx.spawn(async move |cx| {
            let pdf_bytes = load.await?;

            let entity = cx.update(|cx| {
                cx.new(|cx| {
                    let subscription = cx.subscribe(
                        &project,
                        |this: &mut PdfItem, _project, event: &project::Event, cx| {
                            if let project::Event::WorktreeUpdatedEntries(
                                worktree_id,
                                updated_entries,
                            ) = event
                            {
                                if *worktree_id == this.project_path.worktree_id
                                    && updated_entries
                                        .iter()
                                        .any(|(path, _, _)| *path == this.project_path.path)
                                {
                                    this.reload(cx);
                                }
                            }
                        },
                    );
                    PdfItem {
                        project: project.downgrade(),
                        project_path,
                        abs_path,
                        pdf_bytes,
                        reload_task: Task::ready(()),
                        _project_subscription: subscription,
                    }
                })
            });
            Ok(entity)
        }))
    }

    fn entry_id(&self, _cx: &App) -> Option<ProjectEntryId> {
        None
    }

    fn project_path(&self, _cx: &App) -> Option<ProjectPath> {
        Some(self.project_path.clone())
    }

    fn is_dirty(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape of a real file: header, body, cross-reference offset, and
    /// the trailer that only exists once the writer is done.
    fn whole_pdf() -> Vec<u8> {
        let mut bytes = b"%PDF-1.7\n".to_vec();
        bytes.extend(std::iter::repeat_n(b'x', 4096));
        bytes.extend(b"\nstartxref\n1234\n%%EOF\n");
        bytes
    }

    #[test]
    fn a_whole_pdf_is_accepted() {
        assert!(is_complete_pdf(&whole_pdf()));
    }

    /// The case that reached the parser: a compile still writing the file
    /// returns a readable prefix with no trailer. Every truncation of a real
    /// document has to be rejected, not just an obvious one.
    #[test]
    fn every_truncation_is_rejected() {
        let whole = whole_pdf();
        // Anything short of the trailer, up to cutting into `%%EOF` itself.
        // Dropping only the final newline is *not* truncation: a PDF ending
        // exactly at `%%EOF` is complete, and must still be accepted.
        for length in [0, 1, 8, 9, 100, 2048, whole.len() - 3] {
            assert!(
                !is_complete_pdf(&whole[..length]),
                "{length} bytes should not look complete"
            );
        }
        assert!(
            is_complete_pdf(&whole[..whole.len() - 1]),
            "a PDF ending at %%EOF with no trailing newline is complete"
        );
    }

    #[test]
    fn a_file_that_is_not_a_pdf_is_rejected() {
        assert!(!is_complete_pdf(b"not a pdf at all, but it does say %%EOF"));
    }

    /// `%%EOF` is found by scanning a window at the end, so a document larger
    /// than that window must still be accepted.
    #[test]
    fn a_document_larger_than_the_trailer_window_is_accepted() {
        let mut bytes = b"%PDF-1.7\n".to_vec();
        bytes.extend(std::iter::repeat_n(b'x', PDF_TRAILER_WINDOW * 4));
        bytes.extend(b"\n%%EOF\n");
        assert!(is_complete_pdf(&bytes));
        // ...and the same document truncated inside that tail is not.
        let truncated = &bytes[..bytes.len() - 3];
        assert!(!is_complete_pdf(truncated));
    }
}

/// Opening a PDF that lives on a remote host. Runs a real headless server
/// over the fake transport, so the download RPC, the chunk reassembly, and
/// the scratch-file cleanup are all exercised, not mocked.
///
/// The project writes the downloaded file with `smol::fs::write`, which
/// completes on a blocking thread outside the deterministic scheduler, so
/// these tests allow parking and let real time pass.
#[cfg(test)]
mod remote_tests {
    use super::*;
    use fs::FakeFs;
    use gpui::TestAppContext;
    use project::Project;
    use remote::RemoteClient;
    use remote_server::{HeadlessAppState, HeadlessProject};
    use util::{path, rel_path::rel_path};

    fn pdf_with_body(body: &str) -> Vec<u8> {
        format!("%PDF-1.7\n{body}\nstartxref\n1234\n%%EOF\n").into_bytes()
    }

    async fn remote_project(
        server_fs: &Arc<FakeFs>,
        cx: &mut TestAppContext,
        server_cx: &mut TestAppContext,
    ) -> (Entity<Project>, Entity<HeadlessProject>) {
        cx.executor().allow_parking();
        cx.update(|cx| release_channel::init(semver::Version::new(0, 0, 0), cx));
        server_cx.update(|cx| release_channel::init(semver::Version::new(0, 0, 0), cx));

        let (opts, server_session, _) = RemoteClient::fake_server(cx, server_cx);
        server_cx.update(HeadlessProject::init);
        let headless = server_cx.new(|cx| {
            HeadlessProject::new(
                HeadlessAppState {
                    session: server_session,
                    fs: server_fs.clone(),
                    http_client: Arc::new(http_client::BlockedHttpClient),
                    node_runtime: node_runtime::NodeRuntime::unavailable(),
                    languages: Arc::new(language::LanguageRegistry::new(
                        cx.background_executor().clone(),
                    )),
                    extension_host_proxy: Arc::new(extension::ExtensionHostProxy::new()),
                    startup_time: std::time::Instant::now(),
                },
                false,
                cx,
            )
        });

        let remote_client = RemoteClient::connect_mock(opts, cx).await;
        cx.update(|cx| {
            if !cx.has_global::<settings::SettingsStore>() {
                let settings_store = settings::SettingsStore::test(cx);
                cx.set_global(settings_store);
            }
        });
        let client = cx.update(|cx| {
            client::Client::new(
                Arc::new(clock::FakeSystemClock::new()),
                http_client::FakeHttpClient::with_404_response(),
                cx,
            )
        });
        let user_store = cx.new(|cx| client::UserStore::new(client.clone(), cx));
        let languages = Arc::new(language::LanguageRegistry::test(cx.executor()));
        let client_fs = FakeFs::new(cx.executor());
        cx.update(|cx| Project::init(&client, cx));
        let project = cx.update(|cx| {
            Project::remote(
                remote_client,
                client,
                node_runtime::NodeRuntime::unavailable(),
                user_store,
                languages,
                client_fs,
                false,
                cx,
            )
        });
        (project, headless)
    }

    async fn open_remote_worktree(
        project: &Entity<Project>,
        cx: &mut TestAppContext,
    ) -> project::WorktreeId {
        let (worktree, _) = project
            .update(cx, |project, cx| {
                project.find_or_create_worktree(path!("/code/project"), true, cx)
            })
            .await
            .expect("remote worktree should open");
        cx.run_until_parked();
        worktree.read_with(cx, |worktree, _| worktree.id())
    }

    fn open_pdf(
        project: &Entity<Project>,
        project_path: &ProjectPath,
        cx: &mut TestAppContext,
    ) -> Task<Result<Entity<PdfItem>>> {
        cx.update(|cx| PdfItem::try_open(project, project_path, cx))
            .expect("a .pdf path must be claimed by the PDF item")
    }

    fn scratch_copies() -> Vec<PathBuf> {
        std::fs::read_dir(remote_pdf_scratch_dir())
            .map(|entries| {
                entries
                    .filter_map(|entry| entry.ok())
                    .map(|entry| entry.path())
                    .filter(|path| {
                        path.file_name()
                            .and_then(|name| name.to_str())
                            .is_some_and(|name| name.starts_with(&std::process::id().to_string()))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    #[gpui::test]
    async fn a_remote_pdf_is_downloaded_and_opened(
        cx: &mut TestAppContext,
        server_cx: &mut TestAppContext,
    ) {
        let server_fs = FakeFs::new(server_cx.executor());
        server_fs
            .insert_tree(path!("/code/project"), serde_json::json!({ "docs": {} }))
            .await;
        server_fs
            .insert_file(path!("/code/project/docs/report.pdf"), pdf_with_body("v1"))
            .await;

        let (project, _headless) = remote_project(&server_fs, cx, server_cx).await;
        let worktree_id = open_remote_worktree(&project, cx).await;
        let project_path = ProjectPath {
            worktree_id,
            path: rel_path("docs/report.pdf").into(),
        };

        let item = open_pdf(&project, &project_path, cx)
            .await
            .expect("remote PDF should open");
        cx.run_until_parked();

        item.read_with(cx, |item, _| {
            assert_eq!(item.pdf_bytes().as_ref(), pdf_with_body("v1").as_slice());
            assert_eq!(item.file_name(), "report.pdf");
        });
        assert!(
            scratch_copies().is_empty(),
            "the downloaded copy must be removed once the bytes are in memory: {:?}",
            scratch_copies()
        );
    }

    /// The recompile flow over SSH: the host rewrites the PDF, the remote
    /// worktree reports the change, and the item fetches the new bytes.
    #[gpui::test]
    async fn a_remote_pdf_reloads_when_the_host_rewrites_it(
        cx: &mut TestAppContext,
        server_cx: &mut TestAppContext,
    ) {
        let server_fs = FakeFs::new(server_cx.executor());
        server_fs
            .insert_tree(path!("/code/project"), serde_json::json!({ "docs": {} }))
            .await;
        server_fs
            .insert_file(path!("/code/project/docs/report.pdf"), pdf_with_body("v1"))
            .await;

        let (project, _headless) = remote_project(&server_fs, cx, server_cx).await;
        let worktree_id = open_remote_worktree(&project, cx).await;
        let project_path = ProjectPath {
            worktree_id,
            path: rel_path("docs/report.pdf").into(),
        };
        let item = open_pdf(&project, &project_path, cx)
            .await
            .expect("remote PDF should open");
        cx.run_until_parked();

        let reloads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        cx.update({
            let reloads = reloads.clone();
            |cx| {
                cx.subscribe(&item, move |_, event: &PdfItemEvent, _| {
                    let PdfItemEvent::Reloaded = event;
                    reloads.fetch_add(1, Ordering::SeqCst);
                })
                .detach();
            }
        });

        server_fs
            .insert_file(path!("/code/project/docs/report.pdf"), pdf_with_body("v2"))
            .await;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(40);
        // Awaiting a timer parks the scheduler, which is what lets real time
        // (and with it the blocking file write) pass under `allow_parking`.
        while reloads.load(Ordering::SeqCst) == 0 && std::time::Instant::now() < deadline {
            cx.executor()
                .timer(std::time::Duration::from_millis(50))
                .await;
        }

        item.read_with(cx, |item, _| {
            assert_eq!(item.pdf_bytes().as_ref(), pdf_with_body("v2").as_slice());
        });
        assert_eq!(reloads.load(Ordering::SeqCst), 1);
        assert!(scratch_copies().is_empty());
    }

    #[gpui::test]
    async fn a_missing_remote_pdf_fails_instead_of_hanging(
        cx: &mut TestAppContext,
        server_cx: &mut TestAppContext,
    ) {
        let server_fs = FakeFs::new(server_cx.executor());
        server_fs
            .insert_tree(path!("/code/project"), serde_json::json!({ "docs": {} }))
            .await;

        let (project, _headless) = remote_project(&server_fs, cx, server_cx).await;
        let worktree_id = open_remote_worktree(&project, cx).await;
        let project_path = ProjectPath {
            worktree_id,
            path: rel_path("docs/missing.pdf").into(),
        };

        let error = open_pdf(&project, &project_path, cx)
            .await
            .expect_err("a file the host does not have cannot open");
        assert!(
            format!("{error:#}").contains("missing.pdf"),
            "the error should name the file: {error:#}"
        );
        assert!(scratch_copies().is_empty());
    }
}
