use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result};
use gpui::{
    App, AppContext as _, BackgroundExecutor, Context, Entity, EventEmitter, Subscription, Task,
};
use project::{Project, ProjectEntryId, ProjectItem, ProjectPath};
use util::ResultExt as _;

pub struct PdfItem {
    project_path: ProjectPath,
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
    fn reload_from_disk(&mut self, cx: &mut Context<Self>) {
        let abs_path = self.abs_path.clone();
        let background = cx.background_executor().clone();
        self.reload_task = cx.spawn(async move |this, cx| {
            let Some(bytes) = load_pdf_bytes(abs_path, background).await.log_err() else {
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

fn load_pdf_bytes(abs_path: PathBuf, background: BackgroundExecutor) -> Task<Result<Arc<[u8]>>> {
    let timers = background.clone();
    background.spawn(async move {
        let mut failure = None;
        for attempt in 0..PDF_READ_ATTEMPTS {
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
        let project = project.clone();
        let background = cx.background_executor().clone();

        Some(cx.spawn(async move |cx| {
            let pdf_bytes = load_pdf_bytes(abs_path.clone(), background).await?;

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
                                    this.reload_from_disk(cx);
                                }
                            }
                        },
                    );
                    PdfItem {
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
