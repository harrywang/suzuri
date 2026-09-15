//! Exporting a note to a document other people can open.
//!
//! Suzuri's notes are markdown with a research vault's extensions layered on
//! top, and until now nothing could turn one into a file that leaves the
//! editor. A paper could cite a hundred sources and still not reach a
//! co-author or a journal. This crate closes that, by rewriting the note into
//! the dialect Pandoc reads (see [`preprocess`]) and handing it over.
//!
//! Citations are the reason the work is shaped this way. Suzuri already writes
//! Pandoc's own `[@key]` syntax into an append-only `refs.bib`, and already
//! resolves a document's CSL style from its frontmatter, so an export can pass
//! all three to Pandoc's citeproc and get a formatted bibliography in the same
//! style the preview renders. Nothing about that had to be invented here; it
//! only had to be connected.

use std::path::{Path, PathBuf};

use editor::Editor;
use gpui::{App, AppContext as _, Context, Entity, TaskExt as _, Window, actions};
use project::Project;
use settings::{RegisterSetting, Settings as _};
use util::ResultExt as _;
use workspace::notifications::NotificationId;
use workspace::{Toast, Workspace};

mod pandoc;
mod preprocess;

pub use pandoc::Format;

use preprocess::{Preprocessed, Vault};

/// Typst rather than a LaTeX engine: the live preview already provisions it,
/// so a PDF export on a machine with no TeX costs one small download instead
/// of a TeX Live installation.
pub const DEFAULT_PDF_ENGINE: &str = "typst";

/// Settings for markdown export.
#[derive(Clone, Debug, PartialEq, RegisterSetting)]
pub struct MarkdownExportSettings {
    /// The program Pandoc typesets PDFs with.
    pub pdf_engine: String,
}

impl settings::Settings for MarkdownExportSettings {
    fn from_settings(content: &settings::SettingsContent) -> Self {
        let content = content.markdown_export.clone().unwrap_or_default();
        Self {
            pdf_engine: content
                .pdf_engine
                .filter(|engine| !engine.trim().is_empty())
                .unwrap_or_else(|| DEFAULT_PDF_ENGINE.to_string()),
        }
    }
}

actions!(
    markdown_export,
    [
        /// Exports the current markdown note to a PDF beside it.
        ExportToPdf,
        /// Exports the current markdown note to a Word document beside it.
        ExportToDocx,
        /// Exports the current markdown note to a self-contained HTML file.
        ExportToHtml,
        /// Exports the current markdown note to a LaTeX source file.
        ExportToLatex,
        /// Exports the current markdown note to an EPUB beside it.
        ExportToEpub,
    ]
);

struct ExportToast;

pub fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, _window, _cx| {
        workspace.register_action(|workspace, _: &ExportToPdf, window, cx| {
            export(workspace, Format::Pdf, window, cx);
        });
        workspace.register_action(|workspace, _: &ExportToDocx, window, cx| {
            export(workspace, Format::Docx, window, cx);
        });
        workspace.register_action(|workspace, _: &ExportToHtml, window, cx| {
            export(workspace, Format::Html, window, cx);
        });
        workspace.register_action(|workspace, _: &ExportToLatex, window, cx| {
            export(workspace, Format::Latex, window, cx);
        });
        workspace.register_action(|workspace, _: &ExportToEpub, window, cx| {
            export(workspace, Format::Epub, window, cx);
        });
    })
    .detach();
}

/// Everything one export needs, gathered while the main thread still has the
/// project to read it from.
struct Request {
    format: Format,
    source: String,
    output: PathBuf,
    resource_directory: PathBuf,
    bibliography: Option<PathBuf>,
    csl: Option<String>,
    pdf_engine: String,
    missing_embeds: Vec<String>,
}

fn export(
    workspace: &mut Workspace,
    format: Format,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let request = match build_request(workspace, format, cx) {
        Ok(request) => request,
        Err(message) => {
            show_toast(workspace, &message, cx);
            return;
        }
    };

    // Offer the download rather than starting it: an export should not pull
    // tens of megabytes because someone opened a menu.
    if let Some(what) = pandoc::pending_download(format, &request.pdf_engine) {
        let handle = cx.entity().downgrade();
        let message = format!(
            "Exporting to {} needs {what}. Install it now?",
            format.label()
        );
        workspace.show_toast(
            Toast::new(NotificationId::unique::<ExportToast>(), message).on_click(
                "Install",
                move |window, cx| {
                    let request = request.clone_for_retry();
                    handle
                        .update(cx, |workspace, cx| {
                            run(workspace, request, window, cx);
                        })
                        .ok();
                },
            ),
            cx,
        );
        return;
    }
    run(workspace, request, window, cx);
}

impl Request {
    /// The toast's Install button fires after the borrow that built this is
    /// gone, so the request has to be owned by the callback.
    fn clone_for_retry(&self) -> Self {
        Self {
            format: self.format,
            source: self.source.clone(),
            output: self.output.clone(),
            resource_directory: self.resource_directory.clone(),
            bibliography: self.bibliography.clone(),
            csl: self.csl.clone(),
            pdf_engine: self.pdf_engine.clone(),
            missing_embeds: self.missing_embeds.clone(),
        }
    }
}

fn build_request(
    workspace: &Workspace,
    format: Format,
    cx: &mut Context<Workspace>,
) -> Result<Request, String> {
    let Some(editor) = workspace.active_item_as::<Editor>(cx) else {
        return Err("Open a markdown note to export it.".to_string());
    };
    let project = workspace.project().clone();

    let Some((path, text, worktree_root)) = read_editor(&editor, &project, cx) else {
        return Err("Save the note first — export works on the file on disk.".to_string());
    };
    if !is_markdown(&path) {
        return Err("Export works on markdown notes.".to_string());
    }

    let source_directory = path.parent().map(Path::to_path_buf);
    let vault = ProjectVault::from_project(&project, cx);
    let Preprocessed {
        text: source,
        missing_embeds,
    } = preprocess::preprocess(&text, source_directory.as_deref(), &vault);

    let bibliography = worktree_root.as_ref().and_then(|root| {
        let library = root.join(&citations::CitationsSettings::get_global(cx).library);
        library.exists().then_some(library)
    });
    // Only worth resolving when there is a bibliography to format against.
    let csl = bibliography.as_ref().and_then(|_| {
        let name = citations::document_style(&text)
            .unwrap_or_else(|| citations::DEFAULT_STYLE.to_string());
        citations::style_xml(&name)
    });

    Ok(Request {
        format,
        source,
        output: path.with_extension(format.extension()),
        resource_directory: source_directory.unwrap_or_else(|| PathBuf::from(".")),
        bibliography,
        csl,
        pdf_engine: MarkdownExportSettings::get_global(cx).pdf_engine.clone(),
        missing_embeds,
    })
}

fn read_editor(
    editor: &Entity<Editor>,
    project: &Entity<Project>,
    cx: &App,
) -> Option<(PathBuf, String, Option<PathBuf>)> {
    let editor = editor.read(cx);
    let buffer = editor.buffer().read(cx).as_singleton()?;
    let buffer = buffer.read(cx);
    let file = buffer.file()?;
    let path = file.as_local()?.abs_path(cx);
    let worktree_root = project
        .read(cx)
        .worktree_for_id(file.worktree_id(cx), cx)
        .map(|worktree| worktree.read(cx).abs_path().to_path_buf());
    Some((path, buffer.text(), worktree_root))
}

fn is_markdown(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .is_some_and(|extension| matches!(extension.as_str(), "md" | "markdown" | "mdx"))
}

fn run(
    workspace: &mut Workspace,
    request: Request,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let format = request.format;
    let output = request.output.clone();
    let missing_embeds = request.missing_embeds.clone();
    let pdf_engine = request.pdf_engine.clone();

    match pandoc::pending_download(format, &pdf_engine) {
        Some(what) => show_toast(
            workspace,
            &format!("Installing {what}. This runs once and may take a minute…"),
            cx,
        ),
        None => show_toast(workspace, &format!("Exporting to {}…", format.label()), cx),
    }

    let http = cx.http_client();
    let conversion = pandoc::Conversion {
        format,
        source: request.source,
        output: request.output,
        resource_directory: request.resource_directory,
        bibliography: request.bibliography,
        csl: request.csl,
        pdf_engine: request.pdf_engine,
    };
    let task = cx.background_spawn(async move {
        let tools = pandoc::ensure_tools(format, &pdf_engine, http).await?;
        pandoc::convert(conversion, tools).await
    });

    cx.spawn_in(window, async move |workspace, cx| match task.await {
        Ok(()) => {
            workspace
                .update_in(cx, |workspace, window, cx| {
                    dismiss_toast(workspace, cx);
                    announce_success(workspace, format, &output, &missing_embeds, window, cx);
                })
                .ok();
        }
        Err(error) => {
            workspace
                .update(cx, |workspace, cx| {
                    show_toast(workspace, &format!("Export failed. {error:#}"), cx);
                })
                .ok();
        }
    })
    .detach();
}

fn announce_success(
    workspace: &mut Workspace,
    format: Format,
    output: &Path,
    missing_embeds: &[String],
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    // A PDF has a viewer right here, so show it rather than describing it.
    if format == Format::Pdf {
        open_pdf_in_split(workspace, output, window, cx);
    }

    let name = output
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("the document");
    let message = if missing_embeds.is_empty() {
        format!("Exported {name}.")
    } else {
        // Silence here would mean a section is simply absent from the paper
        // with nothing to indicate it ever existed.
        format!(
            "Exported {name}, without {}: {}.",
            plural(
                missing_embeds.len(),
                "one embedded note",
                "some embedded notes"
            ),
            summarize(missing_embeds)
        )
    };
    let reveal = output.to_path_buf();
    workspace.show_toast(
        Toast::new(NotificationId::unique::<ExportToast>(), message)
            .on_click("Show in Folder", move |_window, cx| cx.reveal_path(&reveal)),
        cx,
    );
}

fn plural(count: usize, one: &str, many: &str) -> String {
    if count == 1 { one } else { many }.to_string()
}

/// Names the first few missing embeds; a document that lost twenty sections
/// has a different problem than a toast can help with.
fn summarize(missing: &[String]) -> String {
    const SHOWN: usize = 3;
    let listed = missing
        .iter()
        .take(SHOWN)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if missing.len() > SHOWN {
        format!("{listed} and {} more", missing.len() - SHOWN)
    } else {
        listed
    }
}

fn open_pdf_in_split(
    workspace: &mut Workspace,
    output: &Path,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let already_open = workspace.panes().iter().any(|pane| {
        pane.read(cx).items().any(|item| {
            item.project_path(cx).is_some_and(|project_path| {
                workspace
                    .project()
                    .read(cx)
                    .absolute_path(&project_path, cx)
                    .is_some_and(|absolute| absolute == output)
            })
        })
    });
    if already_open {
        return;
    }
    let Some(project_path) = workspace.project().read(cx).find_project_path(output, cx) else {
        return;
    };
    let pane = workspace.split_pane(
        workspace.active_pane().clone(),
        workspace::SplitDirection::Right,
        window,
        cx,
    );
    workspace
        .open_path(project_path, Some(pane.downgrade()), false, window, cx)
        .detach_and_log_err(cx);
}

fn show_toast(workspace: &mut Workspace, message: &str, cx: &mut Context<Workspace>) {
    workspace.show_toast(
        Toast::new(NotificationId::unique::<ExportToast>(), message.to_string()),
        cx,
    );
}

fn dismiss_toast(workspace: &mut Workspace, cx: &mut Context<Workspace>) {
    workspace.dismiss_toast(&NotificationId::unique::<ExportToast>(), cx);
}

/// The project's markdown notes, snapshotted so the rewrite can run off the
/// main thread.
struct ProjectVault {
    notes: Vec<VaultNote>,
}

struct VaultNote {
    /// Worktree-relative path, lowercased, without its extension.
    relative: String,
    /// File stem, lowercased.
    stem: String,
    absolute: PathBuf,
    depth: usize,
}

impl ProjectVault {
    fn from_project(project: &Entity<Project>, cx: &App) -> Self {
        let mut notes = Vec::new();
        for worktree in project.read(cx).worktrees(cx) {
            let worktree = worktree.read(cx);
            for entry in worktree.entries(false, 0) {
                if !entry.is_file() || entry.path.extension() != Some("md") {
                    continue;
                }
                let Some(stem) = entry.path.file_stem() else {
                    continue;
                };
                notes.push(VaultNote {
                    relative: entry
                        .path
                        .as_unix_str()
                        .to_lowercase()
                        .trim_end_matches(".md")
                        .to_string(),
                    stem: stem.to_lowercase(),
                    absolute: worktree.absolutize(&entry.path),
                    depth: entry.path.components().count(),
                });
            }
        }
        Self { notes }
    }
}

#[cfg(test)]
impl ProjectVault {
    /// The notes in one directory, for tests that need the real resolution
    /// rules without standing up a project.
    fn for_directory(directory: &Path) -> Self {
        let mut notes = Vec::new();
        let Ok(entries) = std::fs::read_dir(directory) else {
            return Self { notes };
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("md") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            notes.push(VaultNote {
                relative: stem.to_lowercase(),
                stem: stem.to_lowercase(),
                absolute: path.clone(),
                depth: 1,
            });
        }
        Self { notes }
    }
}

impl Vault for ProjectVault {
    /// Ranked the way the preview resolves a wikilink, so a note that opens on
    /// click is the note that gets embedded: an exact relative path first,
    /// then a matching file name beside the linking note, then any other
    /// match, shallowest first.
    fn resolve(&self, target: &str, source_directory: Option<&Path>) -> Option<PathBuf> {
        let target = target.trim().trim_end_matches(".md");
        if target.is_empty() {
            return None;
        }
        let wanted_path = target.to_lowercase();
        let wanted_stem = Path::new(target).file_stem()?.to_str()?.to_lowercase();

        let mut best: Option<(u32, usize, &Path)> = None;
        for note in &self.notes {
            let rank = if note.relative == wanted_path {
                0
            } else if note.stem == wanted_stem {
                let beside = source_directory
                    .is_some_and(|directory| note.absolute.parent() == Some(directory));
                if beside { 1 } else { 2 }
            } else {
                continue;
            };
            let better = best.as_ref().is_none_or(|(best_rank, best_depth, _)| {
                (rank, note.depth) < (*best_rank, *best_depth)
            });
            if better {
                best = Some((rank, note.depth, note.absolute.as_path()));
            }
        }
        best.map(|(_, _, path)| path.to_path_buf())
    }

    fn read(&self, path: &Path) -> Option<String> {
        std::fs::read_to_string(path).log_err()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_files_are_the_ones_exported() {
        assert!(is_markdown(Path::new("/notes/paper.md")));
        assert!(is_markdown(Path::new("/notes/paper.MARKDOWN")));
        assert!(!is_markdown(Path::new("/notes/paper.txt")));
        assert!(!is_markdown(Path::new("/notes/paper.typ")));
        assert!(!is_markdown(Path::new("/notes/paper")));
    }

    #[test]
    fn output_sits_beside_the_note() {
        let note = Path::new("/vault/papers/draft.md");
        assert_eq!(
            note.with_extension(Format::Docx.extension()),
            Path::new("/vault/papers/draft.docx")
        );
    }

    #[test]
    fn summary_lists_a_few_and_counts_the_rest() {
        let missing: Vec<String> = ["a", "b", "c", "d", "e"]
            .iter()
            .map(|name| (*name).to_string())
            .collect();
        assert_eq!(summarize(&missing), "a, b, c and 2 more");
        assert_eq!(summarize(&missing[..2]), "a, b");
    }

    #[test]
    fn plural_agrees_with_its_count() {
        assert_eq!(plural(1, "one note", "some notes"), "one note");
        assert_eq!(plural(3, "one note", "some notes"), "some notes");
    }

    /// The settings chain runs through four files and the compiler checks only
    /// part of it: `from_settings` falls back silently, so a section missing
    /// from `default.json` or from the deserializer's list would leave the
    /// default in place and look entirely correct until someone tried to
    /// change it.
    #[gpui::test]
    fn settings_are_registered_and_default_to_typst(cx: &mut gpui::App) {
        let store = settings::SettingsStore::new(cx, &settings::default_settings());
        cx.set_global(store);
        assert_eq!(
            MarkdownExportSettings::get_global(cx).pdf_engine,
            DEFAULT_PDF_ENGINE
        );
    }
}
