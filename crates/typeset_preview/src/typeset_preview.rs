//! Live typeset preview: write Typst or LaTeX in one pane and watch the
//! compiled PDF in another. HTML opens in the system browser instead —
//! GPUI has no webview, and a real browser beats any approximation.
//!
//! `typeset_preview::OpenLivePreview` compiles the active buffer's file and
//! opens the resulting PDF in a split. From then on, every save of that file
//! (which autosave issues a second after typing pauses) recompiles it; the
//! PDF item notices the changed bytes on disk and re-renders in place, so
//! the loop is: type → pause → page updates.
//!
//! Compilers are discovered on PATH: `typst` for .typ, and for .tex whichever
//! engine `latex_engine` selects — by default `latexmk`. When nothing is
//! installed, the first preview offers to download a managed compiler rather
//! than fetching one unannounced; for LaTeX that is a TeX Live installation
//! (TinyTeX), whose missing packages are then installed on demand.
//!
//! The engine is a setting because conference templates pin one: AAAI and
//! many IEEE styles call `\RequirePDFTeX` and refuse to build under XeTeX,
//! so no single default can serve every document.
//!
//! Compile errors surface as a workspace toast holding the compiler's message.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use anyhow::{Context as _, Result, bail};
use editor::{Editor, EditorEvent};
use gpui::{App, AppContext as _, Context, Entity, Global, TaskExt as _, Window, actions};
use settings::{LatexEngine, RegisterSetting, Settings as _};
use util::ResultExt as _;
use workspace::notifications::NotificationId;
use workspace::{Toast, Workspace};

/// Settings for live typeset preview.
#[derive(Clone, Debug, Default, PartialEq, RegisterSetting)]
pub struct TypesetPreviewSettings {
    pub latex_engine: LatexEngine,
}

impl settings::Settings for TypesetPreviewSettings {
    fn from_settings(content: &settings::SettingsContent) -> Self {
        let content = content.typeset_preview.clone().unwrap_or_default();
        Self {
            latex_engine: content.latex_engine.unwrap_or_default(),
        }
    }
}

actions!(
    typeset_preview,
    [
        /// Previews the current file: Typst and LaTeX compile to a PDF in a
        /// split and recompile on every save; HTML opens in the browser.
        OpenLivePreview
    ]
);

struct LiveCompileToast;

/// Files with live preview enabled, plus in-flight compile guards.
#[derive(Default)]
struct LiveCompileRegistry {
    enabled: HashSet<PathBuf>,
    in_flight: HashSet<PathBuf>,
}

impl Global for LiveCompileRegistry {}

#[derive(Clone, Copy, PartialEq)]
enum Typesetter {
    Typst,
    Latex,
    /// GPUI has no webview, so HTML opens in the system browser rather than
    /// being rendered in a pane — a real browser beats any approximation.
    Html,
}

fn typesetter_for(path: &Path) -> Option<Typesetter> {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.to_ascii_lowercase())
        .as_deref()
    {
        Some("typ") => Some(Typesetter::Typst),
        Some("tex") | Some("latex") => Some(Typesetter::Latex),
        Some("html") | Some("htm") => Some(Typesetter::Html),
        _ => None,
    }
}

pub fn init(cx: &mut App) {
    cx.set_global(LiveCompileRegistry::default());

    cx.observe_new(|workspace: &mut Workspace, _window, _cx| {
        workspace.register_action(|workspace, _: &OpenLivePreview, window, cx| {
            open_live_preview(workspace, window, cx);
        });
    })
    .detach();

    // Recompile registered files whenever their editor saves.
    cx.observe_new(|_editor: &mut Editor, window, cx: &mut Context<Editor>| {
        if window.is_none() {
            return;
        }
        cx.subscribe_self(|editor, event: &EditorEvent, cx| {
            if matches!(event, EditorEvent::Saved) {
                let Some(path) = editor_file_path(editor, cx) else {
                    return;
                };
                if cx.global::<LiveCompileRegistry>().enabled.contains(&path) {
                    let workspace = editor.workspace();
                    compile(path, workspace, cx);
                }
            }
        })
        .detach();
    })
    .detach();
}

fn editor_file_path(editor: &Editor, cx: &App) -> Option<PathBuf> {
    let buffer = editor.buffer().read(cx).as_singleton()?;
    let file = buffer.read(cx).file()?;
    Some(file.as_local()?.abs_path(cx))
}

/// Label for this path's preview affordance, or `None` when the file is not
/// previewable. Callers use it to decide whether to show a button or menu
/// entry at all, and what to call it — HTML opens in a browser, so promising
/// a PDF would be a lie.
pub fn preview_label(path: &Path) -> Option<&'static str> {
    match typesetter_for(path)? {
        Typesetter::Html => Some("Open in Browser"),
        Typesetter::Typst | Typesetter::Latex => Some("Open Live PDF Preview"),
    }
}

/// `preview_label` for the file an editor is on.
pub fn preview_label_for_editor(editor: &Entity<Editor>, cx: &App) -> Option<&'static str> {
    editor_file_path(editor.read(cx), cx)
        .as_deref()
        .and_then(preview_label)
}

fn open_live_preview(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let Some(editor) = workspace
        .active_item(cx)
        .and_then(|item| item.downcast::<Editor>())
    else {
        return;
    };
    open_live_preview_for_editor(workspace, editor, window, cx);
}

/// Previews `editor`'s file: HTML goes to the browser, Typst and LaTeX
/// compile to a PDF in a split that recompiles on every save.
pub fn open_live_preview_for_editor(
    workspace: &mut Workspace,
    editor: Entity<Editor>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(path) = editor.read_with(cx, |editor, cx| editor_file_path(editor, cx)) else {
        show_toast(
            workspace,
            "Save the file first — live preview follows the file on disk.",
            cx,
        );
        return;
    };
    match typesetter_for(&path) {
        None => {
            show_toast(
                workspace,
                "Live preview works on Typst (.typ), LaTeX (.tex), and HTML files.",
                cx,
            );
            return;
        }
        // A browser renders HTML properly and keeps it interactive; reload
        // the tab to see saved changes.
        Some(Typesetter::Html) => {
            cx.open_url(&format!("file://{}", path.display()));
            return;
        }
        Some(_) => {}
    }

    let engine = TypesetPreviewSettings::get_global(cx).latex_engine.clone();
    // Offer the download rather than starting it. Until the user accepts,
    // the file stays unregistered, so saving it does not quietly pull tens of
    // megabytes either.
    if let Some(what) = pending_download(&path, &engine) {
        let handle = cx.entity().downgrade();
        let message = format!("Live preview needs {what}. Install it now?");
        workspace.show_toast(
            Toast::new(NotificationId::unique::<LiveCompileToast>(), message).on_click(
                "Install",
                move |window, cx| {
                    let path = path.clone();
                    handle
                        .update(cx, |workspace, cx| {
                            begin_preview(workspace, path, window, cx);
                        })
                        .ok();
                },
            ),
            cx,
        );
        return;
    }
    begin_preview(workspace, path, window, cx);
}

/// Registers `path` for live preview, compiles it, and opens the PDF.
fn begin_preview(
    workspace: &mut Workspace,
    path: PathBuf,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    cx.global_mut::<LiveCompileRegistry>()
        .enabled
        .insert(path.clone());

    let output = path.with_extension("pdf");
    let engine = TypesetPreviewSettings::get_global(cx).latex_engine.clone();
    // Downloading a toolchain takes far longer than a compile, so saying
    // "Compiling…" through it reads as a hang.
    match pending_download(&path, &engine) {
        Some(what) => show_toast(
            workspace,
            &format!("Installing {what}. This runs once and may take a minute…"),
            cx,
        ),
        None => show_toast(workspace, "Compiling…", cx),
    }
    let http = cx.http_client();
    let task = cx.background_spawn(async move { compile_document(&path, http, engine).await });
    cx.spawn_in(window, async move |workspace, cx| {
        let result = task.await;
        match result {
            Ok(()) => {
                workspace
                    .update_in(cx, |workspace, window, cx| {
                        dismiss_toast(workspace, cx);
                        open_pdf_in_split(workspace, &output, window, cx);
                    })
                    .ok();
            }
            Err(error) => {
                workspace
                    .update(cx, |workspace, cx| {
                        show_toast(workspace, &format!("{error:#}"), cx);
                    })
                    .ok();
            }
        }
    })
    .detach();
}

/// Opens (or reveals) the output PDF. If it is already open anywhere in the
/// workspace the existing tab is left alone — the PDF item reloads itself.
/// Otherwise the active pane is split right and the PDF opens there.
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
                    .is_some_and(|abs| abs == output)
            })
        })
    });
    if already_open {
        return;
    }

    let Some(project_path) = workspace.project().read(cx).find_project_path(output, cx) else {
        show_toast(
            workspace,
            "Compiled PDF is outside the project, open it manually.",
            cx,
        );
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

fn compile(path: PathBuf, workspace: Option<Entity<Workspace>>, cx: &mut Context<Editor>) {
    {
        let registry = cx.global_mut::<LiveCompileRegistry>();
        if registry.in_flight.contains(&path) {
            return;
        }
        registry.in_flight.insert(path.clone());
    }

    let http = cx.http_client();
    let engine = TypesetPreviewSettings::get_global(cx).latex_engine.clone();
    cx.spawn(async move |_, cx| {
        let result = cx
            .background_spawn({
                let path = path.clone();
                async move { compile_document(&path, http, engine).await }
            })
            .await;
        cx.update(|cx| {
            cx.global_mut::<LiveCompileRegistry>()
                .in_flight
                .remove(&path);
            if let Some(workspace) = workspace {
                workspace.update(cx, |workspace, cx| match &result {
                    Ok(()) => dismiss_toast(workspace, cx),
                    Err(error) => show_toast(workspace, &format!("{error:#}"), cx),
                });
            }
        });
        result.log_err();
    })
    .detach();
}

struct CompileCommand {
    program: PathBuf,
    arguments: Vec<String>,
    working_directory: PathBuf,
    /// Set when the program writes its output but may not exit on its own:
    /// headless Chrome finishes `--print-to-pdf` and then keeps running. Wait
    /// for the artifact instead of the exit status, then stop the process.
    artifact: Option<PathBuf>,
    /// Prepended to `PATH`. `latexmk` is a Perl script that shells out to
    /// `pdflatex` by name, so a managed TeX Live only works if its own binary
    /// directory is findable.
    path_prepend: Option<PathBuf>,
    /// The managed TeX Live's binary directory, when this compile is using
    /// one. Missing packages are installed only into an installation Suzuri
    /// owns — a system TeX belongs to the user (and usually needs root).
    managed_texlive: Option<PathBuf>,
}

/// Pinned compiler releases for auto-provisioning. PATH installs always take
/// precedence; these download on first use into the app's data directory,
/// digest-verified, so "double-click install" is the whole setup.
const TYPST_TAG: &str = "v0.15.1";
const TYPST_REPOSITORY: &str = "typst/typst";
/// TinyTeX is a prebuilt, relocatable TeX Live: the same engines Overleaf
/// runs, packaged as a versioned tarball. Releases are retained indefinitely,
/// so pinning a tag keeps provisioning reproducible.
const TINYTEX_TAG: &str = "v2026.08";
const TINYTEX_REPOSITORY: &str = "rstudio/tinytex-releases";

fn compilers_dir() -> PathBuf {
    paths::data_dir().join("typeset_compilers")
}

fn exe(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}

/// Where the provisioned typst binary lives once downloaded. Typst archives
/// contain a `typst-<triple>/` directory.
fn provisioned_typst() -> Result<PathBuf> {
    Ok(compilers_dir()
        .join(format!("typst-{TYPST_TAG}"))
        .join(typst_asset_stem()?)
        .join(exe("typst")))
}

fn tinytex_dir() -> PathBuf {
    compilers_dir().join(format!("tinytex-{TINYTEX_TAG}"))
}

/// TeX Live names its binary directory after its own platform convention
/// (`universal-darwin`, `x86_64-linux`, `windows`), so discover it rather
/// than trying to predict it.
fn tinytex_bin_dir() -> Result<PathBuf> {
    let root = tinytex_dir().join("TinyTeX").join("bin");
    let entry = std::fs::read_dir(&root)
        .with_context(|| format!("reading {root:?}"))?
        .filter_map(|entry| entry.ok())
        .find(|entry| entry.path().is_dir())
        .with_context(|| format!("no platform directory under {root:?}"))?;
    Ok(entry.path())
}

/// A managed TeX Live counts as installed once it can actually typeset.
fn tinytex_installed() -> Option<PathBuf> {
    let bin = tinytex_bin_dir().ok()?;
    bin.join(exe("pdftex")).exists().then_some(bin)
}

fn target_triple() -> Result<(&'static str, &'static str)> {
    use std::env::consts::{ARCH, OS};
    Ok(match (OS, ARCH) {
        ("macos", "aarch64") => ("aarch64", "apple-darwin"),
        ("macos", "x86_64") => ("x86_64", "apple-darwin"),
        ("linux", "aarch64") => ("aarch64", "unknown-linux-musl"),
        ("linux", "x86_64") => ("x86_64", "unknown-linux-musl"),
        ("windows", "x86_64") => ("x86_64", "pc-windows-msvc"),
        ("windows", "aarch64") => ("aarch64", "pc-windows-msvc"),
        (os, arch) => bail!("no prebuilt typesetting compiler for {os} on {arch}"),
    })
}

fn typst_asset_stem() -> Result<String> {
    let (arch, os) = target_triple()?;
    Ok(format!("typst-{arch}-{os}"))
}

/// What a first preview would have to download, if anything. Downloads are
/// offered rather than performed: they are large, and for LaTeX the choice of
/// toolchain decides whether a document compiles at all, so silently picking
/// one and failing later leaves the user with an error they cannot act on.
fn pending_download(path: &Path, engine: &LatexEngine) -> Option<&'static str> {
    match typesetter_for(path)? {
        Typesetter::Typst => (which::which("typst").is_err()
            && !provisioned_typst().ok()?.exists())
        .then_some("the Typst compiler (~30 MB)"),
        Typesetter::Latex => {
            // An external command is the user's own; never provision for it.
            let program = match engine {
                LatexEngine::External { .. } => return None,
                LatexEngine::Auto | LatexEngine::Latexmk => "latexmk",
                LatexEngine::Pdflatex => "pdflatex",
                LatexEngine::Lualatex => "lualatex",
                LatexEngine::Xelatex => "xelatex",
            };
            (which::which(program).is_err() && tinytex_installed().is_none())
                .then_some("a LaTeX toolchain (TeX Live, ~67 MB)")
        }
        // Browsers are never downloaded on the user's behalf.
        Typesetter::Html => None,
    }
}

/// The engine a `.tex` document should be compiled with, and the arguments it
/// takes. `file` is appended by the caller.
fn latex_invocation(engine: &LatexEngine) -> (&str, Vec<String>) {
    // `-interaction=nonstopmode` without `-halt-on-error`: a run that keeps
    // going reports every missing file at once, so the package resolver
    // converges in far fewer rounds.
    match engine {
        LatexEngine::Auto | LatexEngine::Latexmk => (
            "latexmk",
            vec!["-pdf".into(), "-interaction=nonstopmode".into()],
        ),
        LatexEngine::Pdflatex => ("pdflatex", vec!["-interaction=nonstopmode".into()]),
        LatexEngine::Lualatex => ("lualatex", vec!["-interaction=nonstopmode".into()]),
        LatexEngine::Xelatex => ("xelatex", vec!["-interaction=nonstopmode".into()]),
        LatexEngine::External { command, arguments } => {
            (command.as_str(), arguments.clone().unwrap_or_default())
        }
    }
}

async fn compile_command(
    path: &Path,
    http: std::sync::Arc<dyn http_client::HttpClient>,
    engine: LatexEngine,
) -> Result<CompileCommand> {
    let working_directory = path
        .parent()
        .context("file has no parent directory")?
        .to_path_buf();
    let file = path
        .file_name()
        .context("file has no name")?
        .to_string_lossy()
        .into_owned();
    match typesetter_for(path).context("not a previewable document")? {
        Typesetter::Typst => {
            let program = match which::which("typst") {
                Ok(program) => program,
                Err(_) => ensure_typst(http).await?,
            };
            Ok(CompileCommand {
                program,
                arguments: vec!["compile".into(), file],
                working_directory,
                artifact: None,
                path_prepend: None,
                managed_texlive: None,
            })
        }
        Typesetter::Latex => {
            let (name, mut arguments) = latex_invocation(&engine);
            arguments.push(file);
            // An external command is the user's own toolchain: run it as
            // given, and never provision or mutate anything on its behalf.
            if let LatexEngine::External { .. } = engine {
                let program =
                    which::which(name).with_context(|| format!("{name} not found on PATH"))?;
                return Ok(CompileCommand {
                    program,
                    arguments,
                    working_directory,
                    artifact: None,
                    path_prepend: None,
                    managed_texlive: None,
                });
            }
            // A TeX already on PATH is the user's, and takes precedence over
            // anything Suzuri would install.
            if let Ok(program) = which::which(name) {
                return Ok(CompileCommand {
                    program,
                    arguments,
                    working_directory,
                    artifact: None,
                    path_prepend: None,
                    managed_texlive: None,
                });
            }
            let bin = ensure_tinytex(http).await?;
            Ok(CompileCommand {
                program: bin.join(exe(name)),
                arguments,
                working_directory,
                artifact: None,
                path_prepend: Some(bin.clone()),
                managed_texlive: Some(bin),
            })
        }
        // HTML never reaches here: `open_live_preview_for_editor` hands it
        // to the browser instead of a compiler.
        Typesetter::Html => bail!("HTML previews open in a browser"),
    }
}

async fn ensure_typst(http: std::sync::Arc<dyn http_client::HttpClient>) -> Result<PathBuf> {
    let binary = provisioned_typst()?;
    if binary.exists() {
        return Ok(binary);
    }
    let destination = binary
        .ancestors()
        .nth(2)
        .context("unexpected typst layout")?
        .to_path_buf();
    let (arch, os) = target_triple()?;
    if os.contains("windows") {
        let asset_name = format!("typst-{arch}-{os}.zip");
        download_release_archive(
            &http,
            TYPST_REPOSITORY,
            TYPST_TAG,
            &asset_name,
            &destination,
            http_client::github::AssetKind::Zip,
        )
        .await?;
    } else {
        // Typst publishes .tar.xz on unix, which the shared downloader
        // doesn't handle; fetch and hand it to the system tar, which
        // decompresses xz natively on macOS and Linux.
        let asset_name = format!("typst-{arch}-{os}.tar.xz");
        let (url, digest) = release_asset(&http, TYPST_REPOSITORY, TYPST_TAG, &asset_name).await?;
        std::fs::create_dir_all(&destination)
            .with_context(|| format!("creating {destination:?}"))?;
        let archive_directory = destination.join("archive");
        let archive_path = fetch_to_file(
            &http,
            &url,
            digest.as_deref(),
            &archive_directory,
            &asset_name,
        )
        .await?;
        let mut tar = util::command::new_command("tar");
        tar.args(["-xf"])
            .arg(&archive_path)
            .arg("-C")
            .arg(&destination);
        let extracted = run_extraction(&mut tar, &asset_name).await;
        std::fs::remove_dir_all(&archive_directory).ok();
        extracted?;
    }
    anyhow::ensure!(
        binary.exists(),
        "downloaded typst archive did not contain {binary:?}"
    );
    Ok(binary)
}

/// Unpacks a downloaded archive, reporting the extractor's own diagnostics
/// rather than a bare exit status.
async fn run_extraction(command: &mut util::command::Command, asset_name: &str) -> Result<()> {
    let output = command
        .output()
        .await
        .with_context(|| format!("extracting {asset_name}"))?;
    anyhow::ensure!(
        output.status.success(),
        "extracting {asset_name} failed: {}",
        String::from_utf8_lossy(&output.stderr)
            .trim()
            .chars()
            .take(300)
            .collect::<String>()
    );
    Ok(())
}

/// The TinyTeX asset for this platform. The bundles are per-platform, and
/// Windows ships a 7-Zip self-extracting `.exe` rather than an archive.
fn tinytex_asset_name() -> Result<String> {
    use std::env::consts::{ARCH, OS};
    let platform = match (OS, ARCH) {
        ("macos", _) => "darwin".to_string(),
        ("linux", "aarch64") => "linux-arm64".to_string(),
        ("linux", "x86_64") => "linux-x86_64".to_string(),
        ("windows", _) => {
            return Ok(format!("TinyTeX-1-windows-{TINYTEX_TAG}.exe"));
        }
        (os, arch) => bail!("no prebuilt TeX Live for {os} on {arch}"),
    };
    Ok(format!("TinyTeX-1-{platform}-{TINYTEX_TAG}.tar.xz"))
}

/// Downloads and prepares a managed TeX Live, returning its binary directory.
async fn ensure_tinytex(http: std::sync::Arc<dyn http_client::HttpClient>) -> Result<PathBuf> {
    if let Some(bin) = tinytex_installed() {
        return Ok(bin);
    }
    let destination = tinytex_dir();
    let asset_name = tinytex_asset_name()?;
    let (url, digest) = release_asset(&http, TINYTEX_REPOSITORY, TINYTEX_TAG, &asset_name).await?;
    std::fs::create_dir_all(&destination).with_context(|| format!("creating {destination:?}"))?;
    let archive_directory = destination.join("archive");
    let archive_path = fetch_to_file(
        &http,
        &url,
        digest.as_deref(),
        &archive_directory,
        &asset_name,
    )
    .await?;

    let mut extractor = if asset_name.ends_with(".exe") {
        // A 7-Zip SFX: extracting it means running it. `-y` accepts the
        // prompts, `-o` sets the output directory.
        let mut installer = util::command::new_command(&archive_path);
        installer
            .arg("-y")
            .arg(format!("-o{}", destination.display()));
        installer
    } else {
        // `.tar.xz`, which the shared downloader cannot handle; the system
        // tar decompresses xz natively on macOS and Linux.
        let mut tar = util::command::new_command("tar");
        tar.arg("-xf")
            .arg(&archive_path)
            .arg("-C")
            .arg(&destination);
        tar
    };
    let extracted = run_extraction(&mut extractor, &asset_name).await;
    std::fs::remove_dir_all(&archive_directory).ok();
    extracted?;

    let bin = tinytex_installed()
        .with_context(|| format!("downloaded TeX Live archive did not populate {destination:?}"))?;

    // The bundled `tlmgr` is older than the package repository it talks to,
    // and refuses every install until it updates itself. Failing here is not
    // fatal: the toolchain still typesets whatever it already ships.
    run_texlive_tool(&bin, "tlmgr", &["update".into(), "--self".into()])
        .await
        .context("updating tlmgr")
        .log_err();
    Ok(bin)
}

/// Runs one of the managed TeX Live's own tools with its binary directory on
/// `PATH`, returning combined output.
async fn run_texlive_tool(bin: &Path, tool: &str, arguments: &[String]) -> Result<String> {
    let mut command = util::command::new_command(bin.join(exe(tool)));
    command.args(arguments).env("PATH", prepended_path(bin));
    let output = command
        .output()
        .await
        .with_context(|| format!("running {tool}"))?;
    let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
    combined.push_str(&String::from_utf8_lossy(&output.stderr));
    anyhow::ensure!(
        output.status.success(),
        "{tool} failed: {}",
        combined.trim().chars().take(300).collect::<String>()
    );
    Ok(combined)
}

fn prepended_path(bin: &Path) -> std::ffi::OsString {
    let existing = std::env::var_os("PATH").unwrap_or_default();
    let mut entries = vec![bin.to_path_buf()];
    entries.extend(std::env::split_paths(&existing));
    std::env::join_paths(entries).unwrap_or(existing)
}

/// Resolves a release asset to its download URL and normalized sha256.
async fn release_asset(
    http: &std::sync::Arc<dyn http_client::HttpClient>,
    repository: &str,
    tag: &str,
    asset_name: &str,
) -> Result<(String, Option<String>)> {
    let release =
        http_client::github::get_release_by_tag_name(repository, tag, http.clone()).await?;
    let asset = release
        .assets
        .iter()
        .find(|asset| asset.name == asset_name)
        .with_context(|| format!("no release asset named {asset_name:?}"))?;
    // The GitHub API returns digests as `sha256:<hex>`; the comparison side
    // wants bare hex.
    let digest = asset
        .digest
        .as_deref()
        .map(|digest| digest.strip_prefix("sha256:").unwrap_or(digest).to_string());
    Ok((asset.browser_download_url.clone(), digest))
}

async fn download_release_archive(
    http: &std::sync::Arc<dyn http_client::HttpClient>,
    repository: &str,
    tag: &str,
    asset_name: &str,
    destination: &Path,
    kind: http_client::github::AssetKind,
) -> Result<()> {
    let (url, digest) = release_asset(http, repository, tag, asset_name).await?;
    // The downloader stages its temp files in the destination's parent and
    // assumes it exists.
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {parent:?}"))?;
    }
    http_client::github_download::download_server_binary(
        http.as_ref(),
        &url,
        digest.as_deref(),
        destination,
        kind,
    )
    .await
}

/// Downloads `url` into `directory/file_name`, returning the file's path.
///
/// This streams through the shared downloader rather than reading the body
/// into memory: `read_to_end` on an `AsyncBody` stalls indefinitely on a
/// large asset, which left the first preview sitting on "Compiling…" forever
/// while a 67 MB archive never arrived. It also verifies the digest and
/// stages the file, so a failed download cannot leave a partial archive
/// behind for the extractor to choke on.
async fn fetch_to_file(
    http: &std::sync::Arc<dyn http_client::HttpClient>,
    url: &str,
    digest: Option<&str>,
    directory: &Path,
    file_name: &str,
) -> Result<PathBuf> {
    if let Some(parent) = directory.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {parent:?}"))?;
    }
    http_client::github_download::download_server_raw_binary(
        http.as_ref(),
        url,
        digest,
        directory,
        file_name,
    )
    .await
    .with_context(|| format!("downloading {url}"))?;
    let path = directory.join(file_name);
    anyhow::ensure!(path.exists(), "download of {url} produced no {path:?}");
    Ok(path)
}

struct CompileRun {
    success: bool,
    output: String,
}

impl CompileRun {
    /// TeX prefixes real errors with `! `, and buries them in hundreds of
    /// lines of routine chatter; that one line is what belongs in a toast.
    fn message(&self) -> String {
        if let Some(error) = self.output.lines().find(|line| line.starts_with("! ")) {
            return error.trim().to_string();
        }
        let trimmed = self.output.trim();
        if trimmed.is_empty() {
            return "compiler reported no output".to_string();
        }
        trimmed.chars().take(600).collect()
    }
}

async fn run_once(command: &CompileCommand) -> Result<CompileRun> {
    let mut process = util::command::new_command(&command.program);
    process
        .args(&command.arguments)
        .current_dir(&command.working_directory);
    if let Some(bin) = &command.path_prepend {
        process.env("PATH", prepended_path(bin));
    }
    // TeX appends to this, so a stale copy would keep reporting fonts that
    // have since been installed.
    std::fs::remove_file(command.working_directory.join(MISSFONT_LOG)).ok();
    let output = process
        .output()
        .await
        .with_context(|| format!("running {}", command.program.display()))?;
    // TeX reports its errors on stdout, not stderr, so both are needed --
    // both to show the user and to find missing packages in.
    let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
    combined.push_str(&String::from_utf8_lossy(&output.stderr));
    Ok(CompileRun {
        success: output.status.success(),
        output: combined,
    })
}

/// How many install-and-retry rounds a single compile may take. Each round
/// resolves every file the previous run named, so this is generous.
const MAX_RESOLVE_ROUNDS: usize = 12;

async fn compile_document(
    path: &Path,
    http: std::sync::Arc<dyn http_client::HttpClient>,
    engine: LatexEngine,
) -> Result<()> {
    let command = compile_command(path, http, engine).await?;
    if let Some(artifact) = command.artifact.clone() {
        return run_until_artifact(command, artifact);
    }
    // Only a TeX Live that Suzuri installed may be modified. A system one is
    // the user's, and installing into it would generally need root anyway.
    let managed = command.managed_texlive.clone();
    let mut run = run_once(&command).await?;
    for _ in 0..MAX_RESOLVE_ROUNDS {
        let mut missing = missing_files(&run.output);
        missing.extend(missing_fonts(&command.working_directory));
        missing.sort();
        missing.dedup();
        // A run that succeeded but is still missing a font produced a PDF
        // typeset in the wrong one, so success alone is not a stopping
        // condition -- only having nothing left to resolve is.
        if missing.is_empty() {
            break;
        }
        let Some(bin) = &managed else { break };
        if !install_missing_packages(bin, &missing).await {
            break;
        }
        clear_latexmk_state(&command);
        run = run_once(&command).await?;
    }
    if run.success {
        Ok(())
    } else {
        bail!("{}", run.message())
    }
}

/// Fonts TeX could not find. When `mktextfm` is on the PATH, a missing metric
/// is not an error in the log at all: TeX defers to it, records the request in
/// `missfont.log`, and exits successfully having typeset the document in a
/// substituted font. That silent path is the one that actually happens, so it
/// has to be read from the file rather than the compiler's output.
fn missing_fonts(working_directory: &Path) -> Vec<String> {
    let Ok(contents) = std::fs::read_to_string(working_directory.join(MISSFONT_LOG)) else {
        return Vec::new();
    };
    contents
        .lines()
        .filter_map(|line| {
            // `mktextfm pcrr8t`, or `mktexpk --mfmode ... --dpi 600 pcrr8t`.
            let name = line.split_whitespace().last()?;
            (!name.is_empty() && !name.starts_with('-')).then(|| format!("{name}.tfm"))
        })
        .collect()
}

const MISSFONT_LOG: &str = "missfont.log";

/// `latexmk` remembers a failed run and then reports "Nothing to do" on every
/// retry, so its state has to go before recompiling.
fn clear_latexmk_state(command: &CompileCommand) {
    let Some(stem) = command
        .arguments
        .last()
        .map(Path::new)
        .and_then(|file| file.file_stem())
        .map(|stem| stem.to_string_lossy().into_owned())
    else {
        return;
    };
    std::fs::remove_file(
        command
            .working_directory
            .join(format!("{stem}.fdb_latexmk")),
    )
    .ok();
}

/// Files a failed run reported missing, as names `tlmgr` can search for.
fn missing_files(output: &str) -> Vec<String> {
    let mut files = Vec::new();
    for line in output.lines() {
        // `! LaTeX Error: File `newtx.sty' not found.`
        if let Some((_, rest)) = line.split_once("File `")
            && let Some((name, tail)) = rest.split_once('\'')
            && tail.trim_start().starts_with("not found")
        {
            files.push(name.to_string());
        }
        // latexmk's own summary: `Missing input file 'newfloat.sty'`
        if let Some((_, rest)) = line.split_once("Missing input file '")
            && let Some((name, _)) = rest.split_once('\'')
        {
            files.push(name.to_string());
        }
        // `! Font TS1/ntxtlf/m/n/10=ts1-qtmr at 10.0pt not loadable: Metric
        // (TFM) file not found.` -- the metric is named after `=`. This case
        // matters more than a missing .sty: TeX carries on and still writes a
        // PDF, so an unhandled font failure is silent visual corruption
        // rather than a build error.
        if line.contains("not loadable: Metric (TFM) file not found")
            && let Some((_, rest)) = line.split_once('=')
            && let Some((name, _)) = rest.split_once(" at ")
        {
            files.push(format!("{}.tfm", name.trim()));
        }
    }
    files.sort();
    files.dedup();
    files
}

/// Lookups that came back empty. Without this, a document naming a package
/// that does not exist would spend a network round trip on every save.
static UNRESOLVABLE: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(Mutex::default);
/// `tlmgr` locks the whole installation, so concurrent previews of two
/// documents would otherwise race and one would fail. The guard is held
/// across awaits, so it has to be a futures-aware lock.
static TLMGR_LOCK: LazyLock<futures::lock::Mutex<()>> =
    LazyLock::new(futures::lock::Mutex::default);

/// Installs the packages providing `files`, returning whether anything new
/// arrived — a `false` means retrying the compile is pointless.
async fn install_missing_packages(bin: &Path, files: &[String]) -> bool {
    let _guard = TLMGR_LOCK.lock().await;
    let mut installed_any = false;
    for file in files {
        if unresolvable_contains(file) {
            continue;
        }
        let Some(package) = texlive_package_for(bin, file).await else {
            remember_unresolvable(file);
            continue;
        };
        if run_texlive_tool(bin, "tlmgr", &["install".to_string(), package])
            .await
            .log_err()
            .is_some()
        {
            installed_any = true;
        } else {
            remember_unresolvable(file);
        }
    }
    installed_any
}

fn unresolvable_contains(file: &str) -> bool {
    UNRESOLVABLE
        .lock()
        .map(|set| set.contains(file))
        .unwrap_or(false)
}

fn remember_unresolvable(file: &str) {
    if let Ok(mut set) = UNRESOLVABLE.lock() {
        set.insert(file.to_string());
    }
}

/// Asks `tlmgr` which package ships `file`.
async fn texlive_package_for(bin: &Path, file: &str) -> Option<String> {
    let output = run_texlive_tool(
        bin,
        "tlmgr",
        &[
            "search".to_string(),
            "--global".to_string(),
            "--file".to_string(),
            format!("/{file}"),
        ],
    )
    .await
    .log_err()?;
    output.lines().find_map(parse_package_line)
}

/// A search result heads its block with `name:` at column zero. The
/// `tlmgr: package repository ...` banner has the same shape, so it has to be
/// excluded by name.
fn parse_package_line(line: &str) -> Option<String> {
    let name = line.strip_suffix(':')?;
    if name.is_empty() || name == "tlmgr" || name.contains(char::is_whitespace) {
        return None;
    }
    Some(name.to_string())
}

/// Runs a program that writes `artifact` and may never exit, giving up after
/// a minute. The artifact counts as finished once it is newer than the run
/// and its size has stopped changing.
#[allow(
    clippy::disallowed_methods,
    reason = "the loop below polls the child synchronously; converting it needs an async timer"
)]
fn run_until_artifact(command: CompileCommand, artifact: PathBuf) -> Result<()> {
    use std::time::{Duration, Instant};

    let previously_modified = std::fs::metadata(&artifact)
        .and_then(|metadata| metadata.modified())
        .ok();
    let started = Instant::now();
    let mut child = util::command::new_std_command(&command.program)
        .args(&command.arguments)
        .current_dir(&command.working_directory)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .with_context(|| format!("running {}", command.program.display()))?;

    let mut last_size = None;
    let result = loop {
        let exited = child.try_wait().ok().flatten().is_some();
        let fresh = std::fs::metadata(&artifact).ok().and_then(|metadata| {
            let modified = metadata.modified().ok()?;
            let is_new = previously_modified.is_none_or(|previous| modified > previous);
            is_new.then_some(metadata.len())
        });
        match fresh {
            // Two equal readings mean the write has settled.
            Some(size) if size > 0 && last_size == Some(size) => break Ok(()),
            Some(size) => last_size = Some(size),
            None if exited => {
                break Err(anyhow::anyhow!(
                    "{} wrote no output",
                    command.program.display()
                ));
            }
            None => {}
        }
        if started.elapsed() > Duration::from_secs(60) {
            break Err(anyhow::anyhow!(
                "{} did not finish within 60s",
                command.program.display()
            ));
        }
        std::thread::sleep(Duration::from_millis(150));
    };
    child.kill().ok();
    child.wait().ok();
    result
}

fn show_toast(workspace: &mut Workspace, message: &str, cx: &mut Context<Workspace>) {
    workspace.show_toast(
        Toast::new(
            NotificationId::unique::<LiveCompileToast>(),
            message.to_string(),
        ),
        cx,
    );
}

fn dismiss_toast(workspace: &mut Workspace, cx: &mut Context<Workspace>) {
    workspace.dismiss_toast(&NotificationId::unique::<LiveCompileToast>(), cx);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real pdflatex output: the error lands on stdout, and names the file
    /// between a backtick and an apostrophe.
    #[test]
    fn missing_style_file_is_extracted() {
        let output = "(./AnonymousSubmission2027.tex\n\
                      LaTeX2e <2026-06-01>\n\
                      ! LaTeX Error: File `newtxtext.sty' not found.\n\
                      \n\
                      Type X to quit or <RETURN> to proceed,\n";
        assert_eq!(missing_files(output), vec!["newtxtext.sty".to_string()]);
    }

    /// latexmk restates the same failure in its own summary format.
    #[test]
    fn latexmk_missing_input_summary_is_extracted() {
        let output = "Latexmk: Examining 'paper.log'\n\
                      Latexmk: Missing input file 'algorithm.sty' message in .log file:\n";
        assert_eq!(missing_files(output), vec!["algorithm.sty".to_string()]);
    }

    /// A missing font metric is the dangerous case: TeX carries on and still
    /// writes a PDF, so failing to resolve one silently typesets the document
    /// in the wrong font instead of reporting an error.
    #[test]
    fn missing_font_metric_is_extracted() {
        let output = "! Font TS1/ntxtlf/m/n/10=ts1-qtmr at 10.0pt not loadable: \
                      Metric (TFM) file not found.\n\
                      ! Font T1/pcr/m/n/9=pcrr8t at 9.0pt not loadable: \
                      Metric (TFM) file not found.\n";
        assert_eq!(
            missing_files(output),
            vec!["pcrr8t.tfm".to_string(), "ts1-qtmr.tfm".to_string()]
        );
    }

    /// The failure that actually happens in the field: with `mktextfm`
    /// available, TeX does not report a missing metric as an error at all. It
    /// writes the request here, exits 0, and hands back a PDF typeset in a
    /// substituted font -- so this file is the only evidence.
    #[test]
    fn missing_fonts_are_read_from_missfont_log() {
        let directory =
            std::env::temp_dir().join(format!("suzuri-missfont-{}", std::process::id()));
        std::fs::create_dir_all(&directory).expect("temp dir");
        std::fs::write(
            directory.join(MISSFONT_LOG),
            "mktextfm pcrr8t\nmktexpk --mfmode ljfour --dpi 600 ptmr8t\n",
        )
        .expect("writing missfont.log");

        assert_eq!(
            missing_fonts(&directory),
            vec!["pcrr8t.tfm".to_string(), "ptmr8t.tfm".to_string()]
        );
        std::fs::remove_dir_all(&directory).ok();
    }

    #[test]
    fn missing_fonts_is_empty_without_a_missfont_log() {
        let directory = std::env::temp_dir().join(format!("suzuri-nomiss-{}", std::process::id()));
        std::fs::create_dir_all(&directory).expect("temp dir");
        assert!(missing_fonts(&directory).is_empty());
        std::fs::remove_dir_all(&directory).ok();
    }

    #[test]
    fn missing_files_are_deduplicated_and_clean_output_yields_none() {
        let repeated = "! LaTeX Error: File `caption.sty' not found.\n\
                        ! LaTeX Error: File `caption.sty' not found.\n";
        assert_eq!(missing_files(repeated), vec!["caption.sty".to_string()]);
        assert!(missing_files("Output written on paper.pdf (8 pages).").is_empty());
    }

    /// `tlmgr search` heads each hit with `name:` at column zero -- and so
    /// does its own repository banner, which is not a result.
    #[test]
    fn package_lookup_skips_the_tlmgr_banner() {
        let output = "tlmgr: package repository https://tlnet.yihui.org (verified)\n\
                      newtx:\n\
                      \ttexmf-dist/tex/latex/newtx/newtxtext.sty\n";
        assert_eq!(
            output.lines().find_map(parse_package_line),
            Some("newtx".to_string())
        );
        assert_eq!(parse_package_line("tlmgr:"), None);
        assert_eq!(
            parse_package_line("\ttexmf-dist/tex/latex/newtx/x.sty"),
            None
        );
        assert_eq!(parse_package_line("some words here:"), None);
    }

    /// The engine is a setting because templates pin one; each value has to
    /// reach a different program.
    #[test]
    fn each_engine_selects_its_own_program() {
        assert_eq!(latex_invocation(&LatexEngine::Auto).0, "latexmk");
        assert_eq!(latex_invocation(&LatexEngine::Latexmk).0, "latexmk");
        assert_eq!(latex_invocation(&LatexEngine::Pdflatex).0, "pdflatex");
        assert_eq!(latex_invocation(&LatexEngine::Lualatex).0, "lualatex");
        assert_eq!(latex_invocation(&LatexEngine::Xelatex).0, "xelatex");

        let external = LatexEngine::External {
            command: "tectonic".to_string(),
            arguments: Some(vec!["--keep-logs".to_string()]),
        };
        let (program, arguments) = latex_invocation(&external);
        assert_eq!(program, "tectonic");
        assert_eq!(arguments, vec!["--keep-logs".to_string()]);
    }

    /// Every engine must keep running past the first error, or the resolver
    /// only learns about one missing file per compile.
    #[test]
    fn engines_do_not_halt_on_first_error() {
        for engine in [
            LatexEngine::Auto,
            LatexEngine::Latexmk,
            LatexEngine::Pdflatex,
            LatexEngine::Lualatex,
            LatexEngine::Xelatex,
        ] {
            let (_, arguments) = latex_invocation(&engine);
            assert!(
                arguments.iter().any(|a| a == "-interaction=nonstopmode"),
                "{engine:?} should run nonstop"
            );
            assert!(
                !arguments.iter().any(|a| a == "-halt-on-error"),
                "{engine:?} should not halt on the first error"
            );
        }
    }

    /// An external command is the user's own toolchain and must be passed
    /// through untouched -- no injected flags.
    #[test]
    fn external_engine_arguments_are_not_augmented() {
        let (_, arguments) = latex_invocation(&LatexEngine::External {
            command: "arara".to_string(),
            arguments: None,
        });
        assert!(arguments.is_empty());
    }

    #[test]
    fn compile_message_prefers_the_tex_error_line() {
        let run = CompileRun {
            success: false,
            output: "This is pdfTeX, Version 3.141592653\n\
                     entering extended mode\n\
                     ! LaTeX Error: File `newtx.sty' not found.\n\
                     Type X to quit\n"
                .to_string(),
        };
        assert_eq!(run.message(), "! LaTeX Error: File `newtx.sty' not found.");
    }

    #[test]
    fn compile_message_falls_back_when_tex_reported_nothing() {
        let empty = CompileRun {
            success: false,
            output: "   \n".to_string(),
        };
        assert_eq!(empty.message(), "compiler reported no output");
    }

    /// Windows ships a self-extracting executable rather than an archive, so
    /// provisioning has to branch on the asset's shape.
    #[test]
    fn tinytex_asset_matches_this_platform() {
        let asset = tinytex_asset_name().expect("this platform has a TeX Live build");
        assert!(asset.contains(TINYTEX_TAG));
        if cfg!(windows) {
            assert!(asset.ends_with(".exe"));
        } else {
            assert!(asset.ends_with(".tar.xz"));
        }
    }

    /// An external command must never trigger provisioning: it is the user's
    /// own toolchain, wherever it lives.
    #[test]
    fn external_engine_never_requests_a_download() {
        let external = LatexEngine::External {
            command: "tectonic".to_string(),
            arguments: None,
        };
        assert_eq!(pending_download(Path::new("paper.tex"), &external), None);
    }

    /// Settings reach the store through `#[derive(RegisterSetting)]`, not an
    /// explicit call at init, and `SettingsStore::get` *panics* on a type it
    /// does not know. A missing derive therefore compiles cleanly and then
    /// takes the app down the first time someone previews a document, so the
    /// registration is pinned here rather than discovered at runtime.
    ///
    /// This also pins that `default.json` carries the key: `from_settings`
    /// falls back to `Auto` silently, so a typo there would go unnoticed.
    #[gpui::test]
    fn settings_are_registered_and_default_to_auto(cx: &mut gpui::App) {
        let store = settings::SettingsStore::new(cx, &settings::default_settings());
        cx.set_global(store);
        assert_eq!(
            TypesetPreviewSettings::get_global(cx).latex_engine,
            LatexEngine::Auto
        );
    }

    /// Writes a document carrying the constraint that started all this:
    /// AAAI's style opens with `\RequirePDFTeX`, so the document compiles
    /// under pdfTeX and aborts under anything else. Reproducing just that
    /// line keeps the fixture free of AAAI's own style files.
    fn write_pdftex_only_document(directory: &Path) -> Result<String> {
        std::fs::create_dir_all(directory)?;
        let name = "pdftex-only.tex";
        std::fs::write(
            directory.join(name),
            "\\documentclass{article}\n\
             \\usepackage{iftex}\n\
             \\RequirePDFTeX\n\
             \\begin{document}\n\
             Ink density saturates.\n\
             \\end{document}\n",
        )?;
        Ok(name.to_string())
    }

    fn command_for(engine: &str, directory: &Path, file: String) -> Option<CompileCommand> {
        let program = which::which(engine).ok()?;
        let (_, mut arguments) = latex_invocation(&match engine {
            "pdflatex" => LatexEngine::Pdflatex,
            _ => LatexEngine::Xelatex,
        });
        arguments.push(file);
        Some(CompileCommand {
            program,
            arguments,
            working_directory: directory.to_path_buf(),
            artifact: None,
            path_prepend: None,
            managed_texlive: None,
        })
    }

    /// End-to-end against a real TeX installation, when one is present. This
    /// is the regression that motivated making the engine configurable: the
    /// same document must build under pdfTeX and fail under XeTeX, so an
    /// engine chosen for the user silently decides whether their paper
    /// previews at all.
    #[test]
    fn pdftex_only_document_builds_under_pdftex_and_fails_under_xetex() {
        if which::which("pdflatex").is_err() || which::which("xelatex").is_err() {
            // No TeX on this machine (CI); the unit tests above still pin the
            // parsing and engine selection.
            return;
        }
        let directory = std::env::temp_dir().join(format!(
            "suzuri-typeset-{}-{}",
            std::process::id(),
            "pdftex-check"
        ));
        let file = write_pdftex_only_document(&directory).expect("writing the fixture");

        let pdflatex = command_for("pdflatex", &directory, file.clone()).expect("pdflatex");
        let run = futures::executor::block_on(run_once(&pdflatex)).expect("running pdflatex");
        assert!(
            run.success,
            "pdfTeX should build a \\RequirePDFTeX document, got: {}",
            run.message()
        );

        let xelatex = command_for("xelatex", &directory, file).expect("xelatex");
        let run = futures::executor::block_on(run_once(&xelatex)).expect("running xelatex");
        assert!(
            !run.success,
            "XeTeX must not build a \\RequirePDFTeX document"
        );
        assert!(
            run.output.contains("pdfTeX is required"),
            "the failure should name the cause, got: {}",
            run.message()
        );

        std::fs::remove_dir_all(&directory).ok();
    }

    #[test]
    fn only_previewable_files_can_request_a_download() {
        assert_eq!(
            pending_download(Path::new("notes.md"), &LatexEngine::Auto),
            None
        );
        // HTML opens in a browser, which is never downloaded for the user.
        assert_eq!(
            pending_download(Path::new("page.html"), &LatexEngine::Auto),
            None
        );
    }
}
