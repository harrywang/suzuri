//! Finding, provisioning, and running Pandoc.
//!
//! A Pandoc already on the PATH is always preferred: it is the user's own, it
//! is likely newer, and on a machine that writes papers it is usually there
//! already. Only when none is found does Suzuri fetch a pinned release into
//! its data directory, the same bargain the typeset preview strikes for Typst
//! and TeX Live.
//!
//! PDF output needs a typesetting engine of its own, and the default here is
//! Typst rather than LaTeX. Suzuri already provisions Typst for the live
//! preview, so a PDF export on a machine with no TeX installation costs
//! nothing extra, where routing through LaTeX would mean a TeX Live download
//! measured in hundreds of megabytes.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use http_client::HttpClient;
use http_client::github::AssetKind;

/// The pinned Pandoc release, bumped deliberately rather than tracking latest.
const PANDOC_TAG: &str = "3.11";
const PANDOC_REPOSITORY: &str = "jgm/pandoc";

/// What an export produces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Pdf,
    Docx,
    Html,
    Latex,
    Epub,
}

impl Format {
    pub fn extension(self) -> &'static str {
        match self {
            Format::Pdf => "pdf",
            Format::Docx => "docx",
            Format::Html => "html",
            Format::Latex => "tex",
            Format::Epub => "epub",
        }
    }

    /// The Pandoc writer to ask for, or `None` when the output extension
    /// already says it. PDF is the exception: it is not a writer but a
    /// pipeline, chosen by the engine.
    fn writer(self) -> Option<&'static str> {
        match self {
            Format::Pdf => None,
            Format::Docx => Some("docx"),
            Format::Html => Some("html"),
            Format::Latex => Some("latex"),
            Format::Epub => Some("epub"),
        }
    }

    /// Whether the output needs a document preamble. Word, EPUB, and PDF are
    /// whole documents by construction; HTML and LaTeX default to a fragment.
    fn standalone(self) -> bool {
        matches!(self, Format::Html | Format::Latex)
    }

    pub fn label(self) -> &'static str {
        match self {
            Format::Pdf => "PDF",
            Format::Docx => "Word",
            Format::Html => "HTML",
            Format::Latex => "LaTeX",
            Format::Epub => "EPUB",
        }
    }
}

/// Everything one conversion needs, resolved on the main thread so the
/// conversion itself can run in the background.
pub struct Conversion {
    pub format: Format,
    /// Markdown already rewritten into Pandoc's dialect.
    pub source: String,
    pub output: PathBuf,
    /// The note's own folder: the working directory for the run, so relative
    /// image paths resolve the way they do in the preview.
    pub resource_directory: PathBuf,
    pub bibliography: Option<PathBuf>,
    /// CSL the document asked for, as XML. Written beside the input so
    /// Pandoc's citeproc formats references in the same style the preview
    /// renders them.
    pub csl: Option<String>,
    pub pdf_engine: String,
}

/// A Pandoc to run, and the Typst to hand it for PDF output.
pub struct Tools {
    pub pandoc: PathBuf,
    /// Absolute path to a PDF engine, when one had to be provisioned. Without
    /// it Pandoc looks the engine up on the PATH.
    pub pdf_engine: Option<PathBuf>,
}

/// What still has to be downloaded before an export of `format` can run, for
/// asking the user first rather than pulling tens of megabytes unannounced.
pub fn pending_download(format: Format, pdf_engine: &str) -> Option<&'static str> {
    let needs_pandoc =
        pandoc_on_path().is_none() && !provisioned_pandoc().is_some_and(|p| p.exists());
    let needs_typst = format == Format::Pdf
        && pdf_engine == "typst"
        && which::which("typst").is_err()
        && !typeset_preview::provisioned_typst_path().is_some_and(|path| path.exists());
    match (needs_pandoc, needs_typst) {
        (true, true) => Some("Pandoc and Typst"),
        (true, false) => Some("Pandoc"),
        (false, true) => Some("Typst"),
        (false, false) => None,
    }
}

fn pandoc_on_path() -> Option<PathBuf> {
    which::which("pandoc").ok()
}

/// Resolves the tools an export needs, downloading what is missing.
pub async fn ensure_tools(
    format: Format,
    pdf_engine: &str,
    http: std::sync::Arc<dyn HttpClient>,
) -> Result<Tools> {
    let pandoc = match pandoc_on_path() {
        Some(found) => found,
        None => ensure_pandoc(http.clone()).await?,
    };

    // Only Typst is provisioned. A user who names a LaTeX engine is naming a
    // TeX installation they already have, and silently substituting a
    // different engine would change how their document is typeset.
    let pdf_engine = if format == Format::Pdf && pdf_engine == "typst" {
        match which::which("typst") {
            Ok(found) => Some(found),
            Err(_) => Some(typeset_preview::ensure_typst_binary(http).await?),
        }
    } else {
        None
    };

    Ok(Tools { pandoc, pdf_engine })
}

fn tools_dir() -> PathBuf {
    paths::data_dir().join("export_tools")
}

fn pandoc_dir() -> PathBuf {
    tools_dir().join(format!("pandoc-{PANDOC_TAG}"))
}

/// The provisioned Pandoc, if one has been unpacked.
///
/// The archive's internal layout differs by platform and has changed between
/// releases, so the binary is discovered in the unpacked tree rather than
/// predicted from a path that a version bump would quietly invalidate.
fn provisioned_pandoc() -> Option<PathBuf> {
    find_binary(&pandoc_dir(), &executable_name("pandoc"), 4)
}

fn executable_name(stem: &str) -> String {
    format!("{stem}{}", std::env::consts::EXE_SUFFIX)
}

fn find_binary(directory: &Path, name: &str, depth: usize) -> Option<PathBuf> {
    let entries = std::fs::read_dir(directory).ok()?;
    let mut directories = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let is_directory = entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false);
        if is_directory {
            directories.push(path);
        } else if path.file_name().and_then(|found| found.to_str()) == Some(name) {
            return Some(path);
        }
    }
    if depth == 0 {
        return None;
    }
    directories
        .into_iter()
        .find_map(|child| find_binary(&child, name, depth - 1))
}

async fn ensure_pandoc(http: std::sync::Arc<dyn HttpClient>) -> Result<PathBuf> {
    if let Some(existing) = provisioned_pandoc() {
        return Ok(existing);
    }
    let (asset_name, kind) = pandoc_asset()?;
    let destination = pandoc_dir();
    std::fs::create_dir_all(&destination).with_context(|| format!("creating {destination:?}"))?;

    let release =
        http_client::github::get_release_by_tag_name(PANDOC_REPOSITORY, PANDOC_TAG, http.clone())
            .await?;
    let asset = release
        .assets
        .iter()
        .find(|asset| asset.name == asset_name)
        .with_context(|| format!("no Pandoc release asset named {asset_name:?}"))?;

    http_client::github_download::download_server_binary(
        http.as_ref(),
        &asset.browser_download_url,
        None,
        &destination.join("unpacked"),
        kind,
    )
    .await
    .context("downloading Pandoc")?;

    provisioned_pandoc().with_context(|| {
        format!(
            "the downloaded Pandoc archive did not contain a {} binary",
            executable_name("pandoc")
        )
    })
}

/// The release asset for this platform. Pandoc names its architectures
/// differently per operating system, so this cannot be built from one triple.
fn pandoc_asset() -> Result<(String, AssetKind)> {
    use std::env::consts::{ARCH, OS};
    Ok(match (OS, ARCH) {
        ("macos", "aarch64") => (
            format!("pandoc-{PANDOC_TAG}-arm64-macOS.zip"),
            AssetKind::Zip,
        ),
        ("macos", "x86_64") => (
            format!("pandoc-{PANDOC_TAG}-x86_64-macOS.zip"),
            AssetKind::Zip,
        ),
        ("linux", "aarch64") => (
            format!("pandoc-{PANDOC_TAG}-linux-arm64.tar.gz"),
            AssetKind::TarGz,
        ),
        ("linux", "x86_64") => (
            format!("pandoc-{PANDOC_TAG}-linux-amd64.tar.gz"),
            AssetKind::TarGz,
        ),
        ("windows", _) => (
            format!("pandoc-{PANDOC_TAG}-windows-x86_64.zip"),
            AssetKind::Zip,
        ),
        (os, arch) => bail!("no prebuilt Pandoc for {os} on {arch}"),
    })
}

/// Runs one conversion. The rewritten markdown and the CSL style are staged in
/// a temporary directory rather than beside the note, so an export never adds
/// files to the user's vault, not even briefly.
pub async fn convert(conversion: Conversion, tools: Tools) -> Result<()> {
    let staging = tempfile::tempdir().context("creating a temporary directory for the export")?;
    let input = staging.path().join("document.md");
    std::fs::write(&input, &conversion.source).context("writing the document to convert")?;

    let csl_path = match &conversion.csl {
        Some(xml) => {
            let path = staging.path().join("style.csl");
            std::fs::write(&path, xml).context("writing the citation style")?;
            Some(path)
        }
        None => None,
    };

    let mut command = util::command::new_command(&tools.pandoc);
    command
        .arg("--from")
        // `mark` carries `==highlight==` through; the rest of Suzuri's
        // extensions are rewritten before Pandoc sees them.
        .arg("markdown+mark")
        .arg("--output")
        .arg(&conversion.output)
        .arg("--resource-path")
        .arg(&conversion.resource_directory)
        .current_dir(&conversion.resource_directory);

    if let Some(writer) = conversion.format.writer() {
        command.arg("--to").arg(writer);
    }
    if conversion.format.standalone() {
        command.arg("--standalone");
    }
    if conversion.format == Format::Html {
        // A single file that still shows its figures is the shareable
        // artifact; a folder of loose assets is not.
        command.arg("--embed-resources");
    }
    if conversion.format == Format::Pdf {
        match &tools.pdf_engine {
            Some(engine) => command.arg("--pdf-engine").arg(engine),
            None => command.arg("--pdf-engine").arg(&conversion.pdf_engine),
        };
    }
    if let Some(bibliography) = &conversion.bibliography {
        command
            .arg("--citeproc")
            .arg("--bibliography")
            .arg(bibliography);
        if let Some(csl_path) = &csl_path {
            command.arg("--csl").arg(csl_path);
        }
    }
    command.arg(&input);

    let output = command
        .output()
        .await
        .with_context(|| format!("running {}", tools.pandoc.display()))?;
    if output.status.success() {
        return Ok(());
    }
    bail!("{}", failure_message(&output.stderr, &output.stdout));
}

/// Pandoc's own diagnostic, trimmed to something a toast can hold.
///
/// The last lines carry the actual complaint: a failing PDF engine prints its
/// whole log first, and leading with that buries the reason.
fn failure_message(stderr: &[u8], stdout: &[u8]) -> String {
    let mut combined = String::from_utf8_lossy(stderr).into_owned();
    if combined.trim().is_empty() {
        combined = String::from_utf8_lossy(stdout).into_owned();
    }
    let lines: Vec<&str> = combined
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.trim().is_empty())
        .collect();
    let tail = lines
        .iter()
        .rev()
        .take(6)
        .rev()
        .copied()
        .collect::<Vec<_>>()
        .join("\n");
    if tail.trim().is_empty() {
        "Pandoc failed without reporting a reason.".to_string()
    } else {
        tail.chars().take(600).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_format_has_a_distinct_extension() {
        let formats = [
            Format::Pdf,
            Format::Docx,
            Format::Html,
            Format::Latex,
            Format::Epub,
        ];
        let mut extensions: Vec<&str> = formats.iter().map(|format| format.extension()).collect();
        extensions.sort_unstable();
        let count = extensions.len();
        extensions.dedup();
        assert_eq!(extensions.len(), count);
    }

    #[test]
    fn pdf_has_no_writer_because_the_engine_chooses_it() {
        assert_eq!(Format::Pdf.writer(), None);
        assert_eq!(Format::Docx.writer(), Some("docx"));
    }

    #[test]
    fn fragment_formats_are_the_ones_asking_for_standalone() {
        assert!(Format::Html.standalone());
        assert!(Format::Latex.standalone());
        assert!(!Format::Docx.standalone());
        assert!(!Format::Pdf.standalone());
    }

    #[test]
    fn this_platform_has_a_pandoc_asset() {
        let (name, _) = pandoc_asset().expect("a prebuilt Pandoc for the test platform");
        assert!(name.starts_with("pandoc-"));
        assert!(name.ends_with(".zip") || name.ends_with(".tar.gz"));
    }

    #[test]
    fn failure_message_keeps_the_last_lines() {
        let stderr = b"line one\nline two\nthe actual error";
        let message = failure_message(stderr, b"");
        assert!(message.contains("the actual error"));
    }

    #[test]
    fn failure_message_falls_back_to_stdout() {
        let message = failure_message(b"", b"said on stdout");
        assert_eq!(message, "said on stdout");
    }

    #[test]
    fn failure_message_never_returns_nothing() {
        assert!(!failure_message(b"", b"").is_empty());
    }

    /// The one assumption in this crate that cannot be checked by reading
    /// code: that a style re-serialized out of hayagriva's archive is CSL a
    /// real citeproc accepts. If it is not, exports silently fall back to
    /// Pandoc's default style and every reference comes out in the wrong
    /// format, which is the kind of thing nobody notices until a submission.
    ///
    /// Skipped where Pandoc is not installed, so this does not turn CI red on
    /// a machine that was never going to run an export.
    #[test]
    fn pandoc_accepts_the_style_the_preview_renders() {
        let Ok(pandoc) = which::which("pandoc") else {
            return;
        };
        let directory = tempfile::tempdir().expect("a temporary directory");
        let bibliography = directory.path().join("refs.bib");
        std::fs::write(
            &bibliography,
            "@article{vaswani2017attention,\n  title={Attention is all you need},\n  \
             author={Vaswani, Ashish and Shazeer, Noam},\n  year={2017},\n  \
             journal={Advances in Neural Information Processing Systems}\n}\n",
        )
        .expect("writing the bibliography");
        let output = directory.path().join("out.html");

        let csl = citations::style_xml("ieee").expect("IEEE is a bundled style");
        let conversion = Conversion {
            format: Format::Html,
            source: "Shown here [@vaswani2017attention].\n\n## References\n".to_string(),
            output: output.clone(),
            resource_directory: directory.path().to_path_buf(),
            bibliography: Some(bibliography),
            csl: Some(csl),
            pdf_engine: "typst".to_string(),
        };
        let tools = Tools {
            pandoc,
            pdf_engine: None,
        };

        smol::block_on(convert(conversion, tools)).expect("Pandoc converts the document");
        let rendered = std::fs::read_to_string(&output).expect("reading the exported document");
        assert!(
            rendered.contains("Vaswani"),
            "the bibliography was not rendered:\n{rendered}"
        );
        assert!(
            rendered.contains("[1]"),
            "IEEE numbers its citations, so the archived style did not survive \
             the round trip into CSL:\n{rendered}"
        );
    }

    /// A note using every extension the fork adds, carried all the way to a
    /// finished document. The unit tests pin each rewrite in isolation; this
    /// pins that the rewritten whole is still markdown Pandoc will accept,
    /// which is the failure the individual tests cannot see.
    #[test]
    fn a_note_using_every_extension_survives_the_whole_pipeline() {
        let Ok(pandoc) = which::which("pandoc") else {
            return;
        };
        let directory = tempfile::tempdir().expect("a temporary directory");
        std::fs::write(
            directory.path().join("Method.md"),
            "## Method\n\nWe measured ==twice== and cut once.\n",
        )
        .expect("writing the embedded note");
        let bibliography = directory.path().join("refs.bib");
        std::fs::write(
            &bibliography,
            "@book{knuth1984,\n  title={The TeXbook},\n  author={Knuth, Donald},\n  \
             year={1984},\n  publisher={Addison-Wesley}\n}\n",
        )
        .expect("writing the bibliography");

        let note = concat!(
            "---\ntitle: A Complete Note\ncsl: ieee\n---\n\n",
            "# Findings\n\n",
            "Typesetting is ==solved== [@knuth1984], filed under #method/qualitative.\n\n",
            "See [[Other Note|the companion]] for context.\n\n",
            "> [!warning] Mind the gap\n> Results are preliminary.\n\n",
            "![[Method]]\n\n",
            "Inline math $E = mc^2$ and a fence:\n\n",
            "```rust\nlet unchanged = \"[[not a link]]\";\n```\n",
        );

        let vault = crate::ProjectVault::for_directory(directory.path());
        let processed = crate::preprocess::preprocess(note, Some(directory.path()), &vault);
        assert!(
            processed.missing_embeds.is_empty(),
            "Method.md should have resolved: {:?}",
            processed.missing_embeds
        );

        let output = directory.path().join("out.html");
        let conversion = Conversion {
            format: Format::Html,
            source: processed.text,
            output: output.clone(),
            resource_directory: directory.path().to_path_buf(),
            bibliography: Some(bibliography),
            csl: citations::style_xml("ieee"),
            pdf_engine: "typst".to_string(),
        };
        smol::block_on(convert(
            conversion,
            Tools {
                pandoc,
                pdf_engine: None,
            },
        ))
        .expect("Pandoc converts the rewritten note");

        let rendered = std::fs::read_to_string(&output).expect("reading the exported document");
        assert!(rendered.contains("<mark>solved</mark>"), "highlight: {rendered}");
        assert!(rendered.contains("the companion"), "wikilink text: {rendered}");
        assert!(
            rendered.contains("#method/qualitative"),
            "the tag should survive as text: {rendered}"
        );
        assert!(
            !rendered.contains("<h1 id=\"methodqualitative\""),
            "the tag was read as a heading: {rendered}"
        );
        assert!(rendered.contains("Mind the gap"), "callout title: {rendered}");
        assert!(
            rendered.contains("cut once"),
            "the transclusion was not spliced: {rendered}"
        );
        assert!(rendered.contains("Knuth"), "bibliography: {rendered}");
        assert!(
            rendered.contains("[[not a link]]"),
            "the fenced code was rewritten: {rendered}"
        );
    }

    /// PDF is the format the whole feature exists for, and it is the only one
    /// that runs a second program. Typesetting a real document is the only way
    /// to know the engine is wired up correctly.
    #[test]
    fn a_pdf_is_typeset_through_typst() {
        let (Ok(pandoc), Ok(typst)) = (which::which("pandoc"), which::which("typst")) else {
            return;
        };
        let directory = tempfile::tempdir().expect("a temporary directory");
        let output = directory.path().join("paper.pdf");
        let conversion = Conversion {
            format: Format::Pdf,
            source: "# A Paper\n\nWith a sentence and $x^2$ of math.\n".to_string(),
            output: output.clone(),
            resource_directory: directory.path().to_path_buf(),
            bibliography: None,
            csl: None,
            pdf_engine: "typst".to_string(),
        };
        smol::block_on(convert(
            conversion,
            Tools {
                pandoc,
                pdf_engine: Some(typst),
            },
        ))
        .expect("Pandoc typesets the PDF");

        let bytes = std::fs::read(&output).expect("reading the exported PDF");
        assert!(
            bytes.starts_with(b"%PDF"),
            "the output is not a PDF: {:?}",
            &bytes[..bytes.len().min(16)]
        );
    }
}
