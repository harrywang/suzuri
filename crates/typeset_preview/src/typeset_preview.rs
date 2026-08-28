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
//! Compilers are discovered on PATH: `typst` for .typ; `tectonic`, then
//! `latexmk`, for .tex. Compile errors surface as a workspace toast holding
//! the compiler's message.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use editor::{Editor, EditorEvent};
use gpui::{App, AppContext as _, Context, Entity, Global, TaskExt as _, Window, actions};
use util::ResultExt as _;
use workspace::notifications::NotificationId;
use workspace::{Toast, Workspace};

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

    cx.global_mut::<LiveCompileRegistry>()
        .enabled
        .insert(path.clone());

    let output = path.with_extension("pdf");
    if let Some(compiler_name) = pending_download_name(&path) {
        show_toast(
            workspace,
            &format!("Downloading {compiler_name} (first use)…"),
            cx,
        );
    }
    let http = cx.http_client();
    let task = {
        let path = path.clone();
        cx.background_spawn(async move {
            let command = compile_command(&path, http).await?;
            run_compile(command).await
        })
    };
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
    cx.spawn(async move |_, cx| {
        let result = cx
            .background_spawn({
                let path = path.clone();
                async move {
                    let command = compile_command(&path, http).await?;
                    run_compile(command).await
                }
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
}

/// Pinned compiler releases for auto-provisioning. PATH installs always take
/// precedence; these download on first use into the app's data directory,
/// digest-verified, so "double-click install" is the whole setup.
const TYPST_TAG: &str = "v0.15.1";
const TYPST_REPOSITORY: &str = "typst/typst";
const TECTONIC_TAG: &str = "tectonic@0.17.0";
const TECTONIC_REPOSITORY: &str = "tectonic-typesetting/tectonic";

fn compilers_dir() -> PathBuf {
    paths::data_dir().join("typeset_compilers")
}

fn exe(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}

/// Where a provisioned compiler's binary lives once downloaded.
fn provisioned_binary(name: &str) -> Result<PathBuf> {
    let dir = compilers_dir().join(format!(
        "{name}-{}",
        match name {
            "typst" => TYPST_TAG,
            _ => TECTONIC_TAG.rsplit('@').next().unwrap_or(TECTONIC_TAG),
        }
    ));
    Ok(match name {
        // Typst archives contain a `typst-<triple>/` directory.
        "typst" => dir.join(typst_asset_stem()?).join(exe("typst")),
        // Tectonic archives contain the bare binary.
        _ => dir.join(exe("tectonic")),
    })
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

/// The name a first-run download would fetch, if any — used to toast
/// "Downloading …" before the user waits on it.
fn pending_download_name(path: &Path) -> Option<&'static str> {
    match typesetter_for(path)? {
        Typesetter::Typst => (which::which("typst").is_err()
            && !provisioned_binary("typst").ok()?.exists())
        .then_some("the Typst compiler"),
        Typesetter::Latex => (which::which("tectonic").is_err()
            && which::which("latexmk").is_err()
            && !provisioned_binary("tectonic").ok()?.exists())
        .then_some("the Tectonic LaTeX compiler"),
        // Browsers are never downloaded on the user's behalf.
        Typesetter::Html => None,
    }
}

async fn compile_command(
    path: &Path,
    http: std::sync::Arc<dyn http_client::HttpClient>,
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
            })
        }
        Typesetter::Latex => {
            if let Ok(program) = which::which("tectonic") {
                Ok(CompileCommand {
                    program,
                    arguments: vec![file],
                    working_directory,
                    artifact: None,
                })
            } else if let Ok(program) = which::which("latexmk") {
                Ok(CompileCommand {
                    program,
                    arguments: vec![
                        "-pdf".into(),
                        "-interaction=nonstopmode".into(),
                        "-halt-on-error".into(),
                        file,
                    ],
                    working_directory,
                    artifact: None,
                })
            } else {
                let program = ensure_tectonic(http).await?;
                Ok(CompileCommand {
                    program,
                    arguments: vec![file],
                    working_directory,
                    artifact: None,
                })
            }
        }
        // HTML never reaches here: `open_live_preview_for_editor` hands it
        // to the browser instead of a compiler.
        Typesetter::Html => bail!("HTML previews open in a browser"),
    }
}

async fn ensure_typst(http: std::sync::Arc<dyn http_client::HttpClient>) -> Result<PathBuf> {
    let binary = provisioned_binary("typst")?;
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
        let bytes = fetch_verified(&http, &url, digest.as_deref()).await?;
        std::fs::create_dir_all(&destination)
            .with_context(|| format!("creating {destination:?}"))?;
        let archive_path = destination.join(&asset_name);
        std::fs::write(&archive_path, &bytes)
            .with_context(|| format!("writing {archive_path:?}"))?;
        let status = util::command::new_std_command("tar")
            .args(["-xf"])
            .arg(&archive_path)
            .arg("-C")
            .arg(&destination)
            .status()
            .context("running tar")?;
        std::fs::remove_file(&archive_path).ok();
        anyhow::ensure!(status.success(), "extracting {asset_name} failed");
    }
    anyhow::ensure!(
        binary.exists(),
        "downloaded typst archive did not contain {binary:?}"
    );
    Ok(binary)
}

async fn ensure_tectonic(http: std::sync::Arc<dyn http_client::HttpClient>) -> Result<PathBuf> {
    let binary = provisioned_binary("tectonic")?;
    if binary.exists() {
        return Ok(binary);
    }
    let destination = binary.parent().context("no parent")?.to_path_buf();
    let version = TECTONIC_TAG.rsplit('@').next().unwrap_or(TECTONIC_TAG);
    let (arch, os) = target_triple()?;
    let (asset_name, kind) = if os.contains("windows") {
        (
            format!("tectonic-{version}-{arch}-{os}.zip"),
            http_client::github::AssetKind::Zip,
        )
    } else {
        (
            format!("tectonic-{version}-{arch}-{os}.tar.gz"),
            http_client::github::AssetKind::TarGz,
        )
    };
    download_release_archive(
        &http,
        TECTONIC_REPOSITORY,
        TECTONIC_TAG,
        &asset_name,
        &destination,
        kind,
    )
    .await?;
    anyhow::ensure!(
        binary.exists(),
        "downloaded tectonic archive did not contain {binary:?}"
    );
    Ok(binary)
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

async fn fetch_verified(
    http: &std::sync::Arc<dyn http_client::HttpClient>,
    url: &str,
    digest: Option<&str>,
) -> Result<Vec<u8>> {
    use futures::AsyncReadExt as _;
    let mut response = http
        .get(url, Default::default(), true)
        .await
        .with_context(|| format!("downloading {url}"))?;
    let mut bytes = Vec::new();
    response
        .body_mut()
        .read_to_end(&mut bytes)
        .await
        .with_context(|| format!("reading {url}"))?;
    anyhow::ensure!(
        response.status().is_success(),
        "download of {url} failed with {}",
        response.status()
    );
    if let Some(expected) = digest {
        use sha2::Digest as _;
        let actual = format!("{:x}", sha2::Sha256::digest(&bytes));
        anyhow::ensure!(
            actual.eq_ignore_ascii_case(expected),
            "sha256 mismatch for {url}: expected {expected}, got {actual}"
        );
    }
    Ok(bytes)
}

async fn run_compile(command: CompileCommand) -> Result<()> {
    if let Some(artifact) = command.artifact.clone() {
        return run_until_artifact(command, artifact);
    }
    let output = util::command::new_std_command(&command.program)
        .args(&command.arguments)
        .current_dir(&command.working_directory)
        .output()
        .with_context(|| format!("running {}", command.program.display()))?;
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let mut message: String = stderr.trim().chars().take(600).collect();
        if message.is_empty() {
            message = format!("compiler exited with {}", output.status);
        }
        bail!("{message}")
    }
}

/// Runs a program that writes `artifact` and may never exit, giving up after
/// a minute. The artifact counts as finished once it is newer than the run
/// and its size has stopped changing.
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
