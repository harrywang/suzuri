//! Headless end-to-end check: a recorded Tinymist frame becomes a page image.
//!
//! This is the milestone that validates the whole client half of the live
//! preview without any UI: protocol parsing, the rkyv IR, incremental merge,
//! the Vello scene conversion, and GPU rasterization with readback.
//!
//! The fixtures are real frames recorded from Tinymist 0.15.10 running on
//! `preview.typ` (see `examples/record_frames.rs`). `preview-new.bin` is the
//! full `new,` document sent on connect; `preview-diff.bin` is the
//! `diff-v1,` frame produced by appending a paragraph to the second page.
//!
//! The rendered page is written to
//! `target/typst-live-preview/page-0.png` for eyeballing.

use std::{fs, path::PathBuf};

use anyhow::{Context as _, Result};
use typst_live_preview::{
    PageTextPosition, PageTextSelection, PreviewSession, SceneRasterizer, count_words,
    protocol::preview_update_from_bytes,
};
use vello::peniko::Color;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn output_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/typst-live-preview")
}

#[test]
fn a_recorded_new_frame_renders_page_zero() -> Result<()> {
    let frame = fs::read(fixture("preview-new.bin")).context("reading the recorded frame")?;
    let update = preview_update_from_bytes(&frame).context("not a preview data frame")?;
    assert!(
        update.reset_before_merge,
        "the first frame on connect is a full `new,` document"
    );

    let mut session = PreviewSession::new();
    session.merge(update);

    // The pane gets its layout from the cheap metadata pass; scene building is
    // deferred until it wants pixels.
    let metas = session.page_metas();
    assert_eq!(metas.len(), 2, "the fixture has two pages");
    assert!(
        (metas[0].size.width - 566.929).abs() < 0.5 && (metas[0].size.height - 283.465).abs() < 0.5,
        "unexpected page size: {:?}",
        metas[0].size
    );

    let pages = session.render_pages().context("rendering the pages")?;
    assert_eq!(pages.len(), 2, "the fixture has two pages");
    assert_eq!(
        metas
            .iter()
            .map(|meta| meta.content_hash)
            .collect::<Vec<_>>(),
        pages
            .iter()
            .map(|page| page.content_hash)
            .collect::<Vec<_>>(),
        "metadata and rendered pages must describe the same document"
    );

    // 200mm x 100mm in Typst points.
    let expected = (566.929, 283.465);
    let page = &pages[0];
    assert!(
        (page.size.width - expected.0).abs() < 0.5 && (page.size.height - expected.1).abs() < 0.5,
        "unexpected page size: {:?}",
        page.size
    );

    // Text and links come out of the same IR, so a page with body text must
    // produce selectable runs. Tinymist also emits the `#link` as a link node.
    assert!(
        !page.accessibility.is_empty(),
        "expected text runs or links on page 0"
    );
    let has_link = page
        .accessibility
        .links()
        .iter()
        .any(|link| link.href() == "https://typst.app");
    assert!(
        has_link,
        "expected the fixture's link in page accessibility"
    );
    assert!(
        page.accessibility
            .text_runs()
            .iter()
            .any(|run| run.text().contains("Tinymist")),
        "expected the fixture's text in page accessibility: {:?}",
        page.accessibility
            .text_runs()
            .iter()
            .map(|run| run.text())
            .collect::<Vec<_>>()
    );

    let mut rasterizer = SceneRasterizer::new();
    let rendered = rasterizer
        .render_page(&page.scene, page.size, 2.0, Color::WHITE)
        .context("rasterizing page 0")?;

    // 200mm at 2 px/pt, rounded.
    assert_eq!((rendered.width, rendered.height), (1134, 567));

    let distinct = rendered
        .image
        .pixels()
        .map(|pixel| pixel.0)
        .collect::<std::collections::HashSet<_>>();
    assert!(
        distinct.len() > 8,
        "the page looks blank: only {} distinct colors",
        distinct.len()
    );

    let out = output_dir();
    fs::create_dir_all(&out).context("creating the output directory")?;
    let png = out.join("page-0.png");
    rendered
        .image
        .save(&png)
        .with_context(|| format!("writing {png:?}"))?;
    eprintln!("wrote {}", png.display());

    Ok(())
}

#[test]
fn a_diff_updates_only_the_changed_page() -> Result<()> {
    let new_frame = fs::read(fixture("preview-new.bin")).context("reading the new frame")?;
    let diff_frame = fs::read(fixture("preview-diff.bin")).context("reading the diff frame")?;

    let mut session = PreviewSession::new();
    session.merge(preview_update_from_bytes(&new_frame).context("not a preview data frame")?);
    session
        .render_pages()
        .context("rendering the initial pages")?;
    let before = session
        .pages()
        .iter()
        .map(|page| page.content)
        .collect::<Vec<_>>();

    let update = preview_update_from_bytes(&diff_frame).context("not a preview data frame")?;
    assert!(
        !update.reset_before_merge,
        "an edit arrives as a `diff-v1,` frame"
    );
    session.merge(update);
    session.render_pages().context("rendering after the diff")?;
    let after = session
        .pages()
        .iter()
        .map(|page| page.content)
        .collect::<Vec<_>>();

    assert_eq!(before.len(), 2);
    assert_eq!(after.len(), 2, "the diff must not drop pages");
    assert_eq!(
        before[0], after[0],
        "the edit touched page 2, so page 1's scene must be reused"
    );
    assert_ne!(
        before[1], after[1],
        "the edit should change the second page's content hash"
    );

    Ok(())
}

#[test]
fn replaying_diffs_keeps_session_state_bounded() -> Result<()> {
    let new_frame = fs::read(fixture("preview-new.bin")).context("reading the new frame")?;
    let diff_frame = fs::read(fixture("preview-diff.bin")).context("reading the diff frame")?;

    let mut session = PreviewSession::new();
    let update = preview_update_from_bytes(&new_frame).context("new frame")?;
    session.merge(update);
    session.render_pages().context("initial render")?;
    let baseline = session.stats();

    for _ in 0..200 {
        let update = preview_update_from_bytes(&diff_frame).context("diff frame")?;
        session.merge(update);
        session.page_metas();
        session.render_pages().context("render")?;
    }

    let after = session.stats();
    assert_eq!(after.pending_glyphs, 0);
    assert_eq!(after.module_glyphs, baseline.module_glyphs);
    assert!(
        after.module_items <= baseline.module_items + 3,
        "the incremental IR must garbage-collect old items: {baseline:?} -> {after:?}"
    );
    assert_eq!(after.cached_pages, baseline.cached_pages);
    assert_eq!(after.flushed_pages, baseline.flushed_pages);

    Ok(())
}

#[test]
fn counts_words_in_a_recorded_document() -> Result<()> {
    let frame = fs::read(fixture("preview-new.bin")).context("reading the frame")?;
    let update = preview_update_from_bytes(&frame).context("not a preview data frame")?;
    let mut session = PreviewSession::new();
    session.merge(update);
    session.render_pages().context("rendering the pages")?;

    let count = session.words_count();
    assert_eq!(
        count.words, 40,
        "the recorded fixture should keep its word count: {count:?}"
    );
    assert!(count.chars >= count.words);
    Ok(())
}

#[test]
fn selecting_the_whole_document_matches_the_document_word_count() -> Result<()> {
    let frame = fs::read(fixture("preview-new.bin")).context("reading the frame")?;
    let update = preview_update_from_bytes(&frame).context("not a preview data frame")?;
    let mut session = PreviewSession::new();
    session.merge(update);
    let pages = session.render_pages().context("rendering the pages")?;

    // Mirror what the preview pane does for a document-wide selection.
    let mut selected = String::new();
    for page in &pages {
        let runs = page.accessibility.text_runs();
        let Some(last) = runs.last() else {
            continue;
        };
        let selection = PageTextSelection {
            anchor: PageTextPosition {
                run_index: 0,
                character_index: 0,
            },
            focus: PageTextPosition {
                run_index: runs.len() - 1,
                character_index: last.character_count(),
            },
        };
        let page_text = page.accessibility.selected_text(selection);
        if page_text.is_empty() {
            continue;
        }
        if !selected.is_empty() {
            selected.push('\n');
        }
        selected.push_str(&page_text);
    }

    assert!(
        selected.contains('\n'),
        "content hints should separate blocks in selection text: {selected:?}"
    );
    assert_eq!(
        count_words(&selected).words,
        session.words_count().words,
        "selection text and the IR word count disagree"
    );
    assert_eq!(session.words_count().words, 40);
    Ok(())
}

#[test]
fn ligature_text_runs_keep_per_character_widths() -> Result<()> {
    let frame = fs::read(fixture("preview-new.bin")).context("reading the frame")?;
    let update = preview_update_from_bytes(&frame).context("not a preview data frame")?;
    let mut session = PreviewSession::new();
    session.merge(update);
    let pages = session.render_pages().context("rendering the pages")?;

    let run = pages
        .iter()
        .flat_map(|page| page.accessibility.text_runs())
        .find(|run| run.text().starts_with("fieri."))
        .context("the fixture should contain the lorem run starting with `fieri.`")?;
    let widths = run
        .character_bounds()
        .iter()
        .map(|rect| rect.width())
        .collect::<Vec<_>>();

    assert_eq!(widths.len(), run.character_count());
    assert!(
        (widths[0] - widths[1]).abs() < 1e-6,
        "the `fi` ligature advance should be split across `f` and `i`: {widths:?}"
    );
    assert!(
        widths
            .windows(2)
            .any(|pair| (pair[0] - pair[1]).abs() > 1e-6),
        "character widths must not all be uniform: {widths:?}"
    );
    assert!(
        (widths.iter().sum::<f64>() - run.bounds().width()).abs() < 1e-3,
        "the character bounds must cover the whole run: {widths:?}, run={:?}",
        run.bounds()
    );

    Ok(())
}
