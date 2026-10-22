use gpui::{
    AnyElement, ClipboardItem, Context, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Point, Styled, Window, div, px,
};

use super::pdf_renderer;
use super::{CopyDocumentText, PdfViewer, TextPosition};

impl PdfViewer {
    pub(crate) fn extract_all_text(&mut self, cx: &mut Context<Self>) {
        let page_count = self.page_count();
        if page_count == 0 {
            log::debug!("pdf_viewer: extract_all_text called but page_count=0");
            return;
        }
        if self.text_layouts.len() >= page_count {
            log::debug!(
                "pdf_viewer: extract_all_text skipped, already have {}/{} layouts",
                self.text_layouts.len(),
                page_count
            );
            return;
        }
        log::debug!(
            "pdf_viewer: starting text extraction for {} pages (have {} so far)",
            page_count,
            self.text_layouts.len()
        );

        let pdf_bytes = self.pdf_item.read(cx).pdf_bytes().clone();

        let (sender, receiver) =
            smol::channel::unbounded::<(usize, pdf_renderer::PageTextLayout)>();

        if let Err(error) = std::thread::Builder::new()
            .name("pdf-text-extractor".into())
            .stack_size(8 * 1024 * 1024)
            .spawn(move || {
                let pdf = match pdf_renderer::open_pdf(&pdf_bytes) {
                    Ok(pdf) => pdf,
                    Err(error) => {
                        log::error!(
                            "pdf_viewer: failed to open PDF for text extraction: {error:#}"
                        );
                        return;
                    }
                };
                // `page_count` came from metadata, the bytes from the item at
                // spawn time; across a reload the two can briefly describe
                // different documents. Extract only what this document has —
                // the metadata reload re-runs extraction with matching counts.
                let actual_count = pdf.pages().len().min(page_count);
                if actual_count < page_count {
                    log::debug!(
                        "pdf_viewer: extracting {} of {} requested pages — document changed",
                        actual_count,
                        page_count
                    );
                }
                for page_index in 0..actual_count {
                    match pdf_renderer::extract_page_text(&pdf, page_index) {
                        Ok(layout) => {
                            log::debug!(
                                "pdf_viewer: extracted {} glyphs from page {}",
                                layout.glyphs.len(),
                                page_index + 1
                            );
                            // The receiver is gone because the view was
                            // released or the document was reloaded under us,
                            // so this run's layouts describe a PDF nobody is
                            // showing any more. Expected, not a failure.
                            if sender.send_blocking((page_index, layout)).is_err() {
                                log::debug!(
                                    "pdf_viewer: text extraction superseded, stopping at page {}",
                                    page_index + 1
                                );
                                break;
                            }
                        }
                        Err(error) => {
                            log::error!(
                                "pdf_viewer: failed to extract text from page {}: {error:#}",
                                page_index + 1
                            );
                        }
                    }
                }
            })
        {
            log::error!("pdf_viewer: failed to spawn text extractor thread: {error:#}");
        }

        self.text_extraction_task = cx.spawn(async move |this, cx| {
            while let Ok((page_index, layout)) = receiver.recv().await {
                let glyph_count = layout.glyphs.len();
                this.update(cx, |this, cx| {
                    log::debug!(
                        "pdf_viewer: stored text layout for page {} ({} glyphs, {}/{} pages done)",
                        page_index + 1,
                        glyph_count,
                        this.text_layouts.len() + 1,
                        this.page_count()
                    );
                    this.text_layouts.insert(page_index, layout);
                    cx.notify();
                })
                .ok();
            }
            log::debug!("pdf_viewer: text extraction channel closed, all pages done");
        });
    }

    pub(crate) fn copy_document_text(
        &mut self,
        _: &CopyDocumentText,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let page_count = self.page_count();
        if page_count == 0 {
            return;
        }

        if let (Some(start), Some(end)) = (self.selection_start, self.selection_end) {
            if start != end {
                let (start, end) = if start > end {
                    (end, start)
                } else {
                    (start, end)
                };

                let mut selected_text = String::new();
                for page_index in start.page..=end.page {
                    if let Some(layout) = self.text_layouts.get(&page_index) {
                        let page_start = if page_index == start.page {
                            start.glyph_index
                        } else {
                            0
                        };
                        let page_end = if page_index == end.page {
                            end.glyph_index
                        } else {
                            layout.glyphs.len().saturating_sub(1)
                        };
                        if !selected_text.is_empty() {
                            selected_text.push('\n');
                        }
                        selected_text.push_str(&layout.selected_text(page_start, page_end));
                    }
                }
                cx.write_to_clipboard(ClipboardItem::new_string(selected_text));
                log::debug!(
                    "pdf_viewer: copied selected text (pages {}-{}) to clipboard",
                    start.page + 1,
                    end.page + 1
                );
                return;
            }
        }

        if self.text_layouts.len() >= page_count {
            let mut full_text = String::new();
            for page_index in 0..page_count {
                if !full_text.is_empty() {
                    full_text.push_str("\n\n");
                }
                if let Some(layout) = self.text_layouts.get(&page_index) {
                    full_text.push_str(&layout.full_text());
                }
            }
            cx.write_to_clipboard(ClipboardItem::new_string(full_text));
            log::debug!(
                "pdf_viewer: copied text from {} pages to clipboard",
                page_count
            );
        } else {
            log::debug!(
                "pdf_viewer: text extraction in progress ({}/{}), starting extraction",
                self.text_layouts.len(),
                page_count
            );
            self.extract_all_text(cx);
        }
    }

    /// The page under a window position and the point on it, in PDF units.
    ///
    /// Reads the bounds each page was laid out at rather than recomputing the
    /// layout: layout rounds every page to whole pixels, so summing float page
    /// heights drifts further from the painted pages the deeper the document
    /// goes, and selection landed lines away from the cursor (#134).
    pub(crate) fn point_to_page_point(
        &self,
        window_position: Point<Pixels>,
    ) -> Option<(usize, f32, f32)> {
        let page_bounds = self.page_bounds.borrow();
        let (&page_index, bounds) = page_bounds
            .iter()
            .find(|(_, bounds)| bounds.contains(&window_position))?;
        let page_width = self.page_dimensions().get(page_index)?.width;
        if page_width <= 0.0 {
            return None;
        }
        let scale = f32::from(bounds.size.width) / page_width;
        let local = window_position - bounds.origin;
        let pdf_x = f32::from(local.x) / scale;
        let pdf_y = f32::from(local.y) / scale;

        log::debug!(
            "pdf_viewer: hit test — page {} pdf coords ({:.1}, {:.1})",
            page_index + 1,
            pdf_x,
            pdf_y
        );

        Some((page_index, pdf_x, pdf_y))
    }

    pub(crate) fn point_to_text_position(
        &self,
        window_position: Point<Pixels>,
    ) -> Option<TextPosition> {
        let (page_index, pdf_x, pdf_y) = self.point_to_page_point(window_position)?;

        let layout = match self.text_layouts.get(&page_index) {
            Some(l) => l,
            None => {
                log::debug!(
                    "pdf_viewer: hit test — no text layout for page {} (have layouts for pages: {:?})",
                    page_index + 1,
                    self.text_layouts.keys().map(|k| k + 1).collect::<Vec<_>>()
                );
                return None;
            }
        };

        log::debug!(
            "pdf_viewer: hit test — page {} has {} glyphs",
            page_index + 1,
            layout.glyphs.len()
        );

        let glyph_index = match layout.glyph_index_at_point(pdf_x, pdf_y) {
            Some(idx) => {
                if let Some(glyph) = layout.glyphs.get(idx) {
                    log::debug!(
                        "pdf_viewer: hit test — matched glyph {} '{}' at ({:.1},{:.1}) w={:.1} fs={:.1}",
                        idx,
                        glyph.character,
                        glyph.x,
                        glyph.y,
                        glyph.width,
                        glyph.font_size
                    );
                }
                idx
            }
            None => {
                log::debug!("pdf_viewer: hit test — glyph_index_at_point returned None");
                return None;
            }
        };

        Some(TextPosition {
            page: page_index,
            glyph_index,
        })
    }

    pub(crate) fn handle_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.button != MouseButton::Left {
            return;
        }
        // Clicking the document leaves the page box, as it does in a browser.
        window.focus(&self.focus_handle, cx);

        log::debug!(
            "pdf_viewer: mouse_down at ({:.0},{:.0})",
            f32::from(event.position.x),
            f32::from(event.position.y),
        );

        if let Some(position) = self.point_to_text_position(event.position) {
            log::debug!(
                "pdf_viewer: selection start — page {} glyph {}",
                position.page + 1,
                position.glyph_index
            );
            self.selection_start = Some(position);
            self.selection_end = Some(position);
            self.is_selecting = true;
        } else {
            log::debug!("pdf_viewer: mouse_down — no text position found, clearing selection");
            self.selection_start = None;
            self.selection_end = None;
            self.is_selecting = false;
        }
        cx.notify();
    }

    pub(crate) fn handle_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.is_selecting {
            return;
        }

        if event.pressed_button != Some(MouseButton::Left) {
            log::debug!("pdf_viewer: mouse_move — left button released, stopping selection");
            self.is_selecting = false;
            return;
        }

        if let Some(position) = self.point_to_text_position(event.position) {
            log::debug!(
                "pdf_viewer: selection drag — page {} glyph {}",
                position.page + 1,
                position.glyph_index
            );
            self.selection_end = Some(position);
        }
        cx.notify();
    }

    pub(crate) fn handle_mouse_up(
        &mut self,
        _event: &MouseUpEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.is_selecting = false;

        if self.selection_start == self.selection_end {
            log::debug!("pdf_viewer: mouse_up — click without drag, clearing selection");
            self.selection_start = None;
            self.selection_end = None;
        } else {
            log::debug!(
                "pdf_viewer: mouse_up — selection: {:?} to {:?}",
                self.selection_start,
                self.selection_end
            );
        }
        cx.notify();
    }

    pub(crate) fn selection_highlights_for_page(
        &self,
        page_index: usize,
        fit_scale: f32,
    ) -> Vec<AnyElement> {
        let (start, end) = match (self.selection_start, self.selection_end) {
            (Some(start), Some(end)) if start != end => {
                if start > end {
                    (end, start)
                } else {
                    (start, end)
                }
            }
            _ => return Vec::new(),
        };

        let layout = match self.text_layouts.get(&page_index) {
            Some(layout) => layout,
            None => return Vec::new(),
        };

        if page_index < start.page || page_index > end.page {
            return Vec::new();
        }

        let glyph_start = if page_index == start.page {
            start.glyph_index
        } else {
            0
        };
        let glyph_end = if page_index == end.page {
            end.glyph_index
        } else {
            layout.glyphs.len().saturating_sub(1)
        };

        let scale = fit_scale * self.zoom_level;
        let mut highlights = Vec::new();

        if glyph_start <= glyph_end {
            log::debug!(
                "pdf_viewer: rendering {} highlights for page {} (glyphs {}..={}, scale={:.3})",
                glyph_end - glyph_start + 1,
                page_index + 1,
                glyph_start,
                glyph_end,
                scale,
            );
        }

        for glyph_idx in glyph_start..=glyph_end {
            if let Some(glyph) = layout.glyphs.get(glyph_idx) {
                let display_x = glyph.x * scale;
                let display_width = glyph.width * scale;
                let display_height = glyph.font_size * scale;
                // glyph.y is the baseline; text ascenders extend above it
                // (smaller Y in top-down coords). Shift up by ~80% of
                // font_size to cover the main glyph body.
                let display_y = (glyph.y - glyph.font_size * 0.8) * scale;

                if glyph_idx == glyph_start {
                    log::debug!(
                        "pdf_viewer: first highlight glyph '{}' — pdf({:.1},{:.1}) display({:.0},{:.0}) size({:.0}x{:.0})",
                        glyph.character,
                        glyph.x,
                        glyph.y,
                        display_x,
                        display_y,
                        display_width,
                        display_height,
                    );
                }

                highlights.push(
                    div()
                        .absolute()
                        .left(px(display_x))
                        .top(px(display_y))
                        .w(px(display_width))
                        .h(px(display_height))
                        .bg(gpui::rgba(0x3584e430))
                        .into_any_element(),
                );
            }
        }

        highlights
    }
}
