//! Incremental data transfer from the Tinymist preview server.
//!
//! Adapted from `tinymist-viewer` (Apache-2.0). The merge order is
//! load-bearing: a `new,` frame resets [`IncrVelloDocClient`] *before* the
//! payload is merged, while a `diff-v1,` frame merges into the existing state.
//!
//! A page's Vello scene is rebuilt only when its IR content hash changes, and
//! flushed scenes are reused across frames (including when pages move because
//! one was deleted).

use std::{collections::HashMap, sync::Arc};

use reflexo::{
    error::prelude::*,
    hash::Fingerprint,
    vector::{
        incr::IncrDocClient,
        ir::{ImmutStr, LayoutRegion, LayoutRegionNode, Module, Page},
        vm::RenderVm,
    },
};
use vello::{
    Scene,
    kurbo::{Size, Vec2},
    peniko::{Color, color::parse_color},
};

use crate::{PageAccessibility, SvgResourceResolver, VecPage};

/// A rendered page with its page metadata.
#[derive(Clone)]
pub struct RenderedPage {
    /// The flushed Vello scene.
    pub scene: Arc<Scene>,
    /// The page size in Typst page coordinates.
    pub size: Size,
    /// The IR content hash this scene was built from.
    ///
    /// The UI uses it to decide whether a rasterized page is still current.
    pub content_hash: Fingerprint,
    /// The page's links and selectable text.
    pub accessibility: PageAccessibility,
}

/// Incremental pass from vector to vello scene.
pub struct IncrVelloPass {
    /// Fills background color with a css color string
    /// Default is white.
    ///
    /// Note: If the string is empty, the background is transparent.
    pub fill: ImmutStr,
    /// Holds a sequence of vello pages that are rendered
    pub pages: Vec<VecPage>,
    /// Holds flushed vello scenes for pages that are rendered.
    flushed_pages: Vec<FlushedPage>,
    /// Resolves image resources linked from inside SVG images.
    pub svg_resource_resolver: Option<Arc<dyn SvgResourceResolver>>,
}

impl Default for IncrVelloPass {
    fn default() -> Self {
        Self {
            fill: "#ffffff".into(),
            pages: vec![],
            flushed_pages: vec![],
            svg_resource_resolver: None,
        }
    }
}

#[derive(Clone)]
struct FlushedPage {
    content_hash: Fingerprint,
    page: RenderedPage,
}

impl FlushedPage {
    fn matches(&self, page: &VecPage) -> bool {
        self.content_hash == page.content_hash && self.page.size == vec2_to_size(page.size)
    }
}

impl IncrVelloPass {
    /// The number of cached vector pages and flushed Vello scenes.
    pub fn cached_page_counts(&self) -> (usize, usize) {
        (self.pages.len(), self.flushed_pages.len())
    }

    /// Interprets the changes in the given module and pages.
    pub fn interpret_changes(&mut self, module: &Module, pages: &[Page]) {
        let mut ct = crate::render::Renderer::new(module)
            .with_svg_resource_resolver(self.svg_resource_resolver.clone());

        let pages: Vec<VecPage> = pages
            .iter()
            .enumerate()
            .map(|(idx, Page { content, size })| {
                if idx < self.pages.len() && self.pages[idx].content_hash == *content {
                    return self.pages[idx].clone();
                }

                let size = Vec2::new(size.x.0 as f64, size.y.0 as f64);
                let elem = ct.render_item(content);
                let accessibility = elem.page_accessibility();
                VecPage {
                    size,
                    elem,
                    accessibility,
                    content_hash: *content,
                }
            })
            .collect();

        self.pages = pages;
    }

    /// Flushes a page to the canvas with the given transform.
    pub fn flush_page(&mut self, idx: usize) -> (Arc<Scene>, Vec2) {
        let page = self.flush_page_with_accessibility(idx);
        (page.scene, size_to_vec2(page.size))
    }

    fn flush_page_with_accessibility(&mut self, idx: usize) -> RenderedPage {
        if idx >= self.pages.len() {
            log::warn!("Index out of bounds: {idx}");
            return RenderedPage {
                scene: Arc::new(vello::Scene::new()),
                size: Size::ZERO,
                content_hash: Fingerprint::from_pair(0, 0),
                accessibility: PageAccessibility::default(),
            };
        }

        let page = &self.pages[idx];
        let rendered_page = Self::flush_page_uncached(page);

        let flushed = FlushedPage {
            content_hash: page.content_hash,
            page: rendered_page.clone(),
        };
        if idx < self.flushed_pages.len() {
            self.flushed_pages[idx] = flushed;
        } else if idx == self.flushed_pages.len() {
            self.flushed_pages.push(flushed);
        }

        rendered_page
    }

    fn flush_pages_with_accessibility(&mut self) -> Vec<RenderedPage> {
        let mut pages = Vec::with_capacity(self.pages.len());
        let mut flushed_pages = Vec::with_capacity(self.pages.len());
        let mut reusable_flushed_pages =
            Self::collect_reusable_flushed_pages(&mut self.flushed_pages);

        for page in &self.pages {
            if let Some(flushed) =
                Self::take_matching_flushed_page(&mut reusable_flushed_pages, page)
            {
                pages.push(flushed.page.clone());
                flushed_pages.push(flushed);
                continue;
            }

            let rendered_page = Self::flush_page_uncached(page);
            flushed_pages.push(FlushedPage {
                content_hash: page.content_hash,
                page: rendered_page.clone(),
            });
            pages.push(rendered_page);
        }

        self.flushed_pages = flushed_pages;
        pages
    }

    fn collect_reusable_flushed_pages(
        flushed_pages: &mut Vec<FlushedPage>,
    ) -> HashMap<Fingerprint, Vec<FlushedPage>> {
        let mut reusable: HashMap<Fingerprint, Vec<FlushedPage>> =
            HashMap::with_capacity(flushed_pages.len());

        for flushed in flushed_pages.drain(..) {
            reusable
                .entry(flushed.content_hash)
                .or_default()
                .push(flushed);
        }

        reusable
    }

    fn take_matching_flushed_page(
        reusable: &mut HashMap<Fingerprint, Vec<FlushedPage>>,
        page: &VecPage,
    ) -> Option<FlushedPage> {
        let content_hash = page.content_hash;
        let (flushed, remove_entry) = {
            let candidates = reusable.get_mut(&content_hash)?;
            let position = candidates
                .iter()
                .position(|flushed| flushed.matches(page))?;
            let flushed = candidates.swap_remove(position);
            (flushed, candidates.is_empty())
        };

        if remove_entry {
            reusable.remove(&content_hash);
        }

        Some(flushed)
    }

    fn flush_page_uncached(page: &VecPage) -> RenderedPage {
        let VecPage { size, elem, .. } = page;
        let mut elem_scene = vello::Scene::new();
        elem.render(&mut elem_scene);

        RenderedPage {
            scene: Arc::new(elem_scene),
            size: vec2_to_size(*size),
            content_hash: page.content_hash,
            accessibility: page.accessibility.clone(),
        }
    }
}

fn vec2_to_size(size: Vec2) -> Size {
    Size::new(size.x, size.y)
}

fn size_to_vec2(size: Size) -> Vec2 {
    Vec2::new(size.width, size.height)
}

/// Maintains the state of the incremental rendering a canvas at client side
#[derive(Default)]
pub struct IncrVelloDocClient {
    /// State of converting vector to canvas
    pub vec2vello: IncrVelloPass,

    /// Expected exact state of the current DOM.
    /// Initially it is None meaning no any page is rendered.
    pub doc_view: Option<Vec<Page>>,
}

impl IncrVelloDocClient {
    /// Resets the state of the incremental rendering.
    pub fn reset(&mut self) {
        let fill = self.vec2vello.fill.clone();
        self.vec2vello = IncrVelloPass {
            fill,
            pages: vec![],
            flushed_pages: vec![],
            svg_resource_resolver: self.vec2vello.svg_resource_resolver.clone(),
        };
        self.doc_view = None;
    }

    /// Sets the resolver for image resources linked from SVG images.
    pub fn set_svg_resource_resolver(&mut self, resolver: Option<Arc<dyn SvgResourceResolver>>) {
        self.vec2vello.svg_resource_resolver = resolver;
        self.vec2vello.pages.clear();
        self.vec2vello.flushed_pages.clear();
    }

    /// Sets canvas's background color
    pub fn set_fill(&mut self, fill: ImmutStr) {
        self.vec2vello.fill = fill;
    }

    /// Returns the configured default page background color.
    pub fn background_color(&self) -> Option<Color> {
        let fill = self.vec2vello.fill.as_ref();
        if fill.is_empty() {
            return None;
        }

        match parse_color(fill) {
            Ok(color) => Some(color.to_alpha_color()),
            Err(err) => {
                log::warn!("Invalid page background color {fill:?}: {err}");
                None
            }
        }
    }

    /// Patches the delta of the incremental rendering.
    fn patch_delta(&mut self, kern: &IncrDocClient) {
        if let Some(layout) = &kern.layout {
            let pages = layout.pages(&kern.doc.module);
            if let Some(pages) = pages {
                self.vec2vello
                    .interpret_changes(pages.module(), pages.pages());
            }
        }
    }

    fn select_page_layout(kern: &IncrDocClient) -> Option<LayoutRegionNode> {
        for (idx, region) in kern.doc.layouts.iter().enumerate() {
            let mut visited = vec![idx];
            if let Some(layout) = Self::select_page_layout_from_region(
                region,
                &kern.doc.layouts,
                &kern.doc.module,
                &mut visited,
            ) {
                return Some(layout);
            }
        }

        None
    }

    fn select_page_layout_from_region(
        region: &LayoutRegion,
        all_layouts: &[LayoutRegion],
        module: &Module,
        visited: &mut Vec<usize>,
    ) -> Option<LayoutRegionNode> {
        match region {
            LayoutRegion::ByScalar(region) => {
                for (_, layout) in &region.layouts {
                    if let Some(layout) =
                        Self::select_page_layout_from_node(layout, all_layouts, module, visited)
                    {
                        return Some(layout);
                    }
                }
            }
            LayoutRegion::ByStr(region) => {
                for (_, layout) in &region.layouts {
                    if let Some(layout) =
                        Self::select_page_layout_from_node(layout, all_layouts, module, visited)
                    {
                        return Some(layout);
                    }
                }
            }
        }

        None
    }

    fn select_page_layout_from_node(
        layout: &LayoutRegionNode,
        all_layouts: &[LayoutRegion],
        module: &Module,
        visited: &mut Vec<usize>,
    ) -> Option<LayoutRegionNode> {
        if layout.pages(module).is_some() {
            return Some(layout.clone());
        }

        let LayoutRegionNode::Indirect(idx) = layout else {
            return None;
        };

        if visited.contains(idx) {
            log::warn!("Ignoring cyclic indirect layout reference: {idx}");
            return None;
        }

        let region = all_layouts.get(*idx)?;
        visited.push(*idx);
        let selected = Self::select_page_layout_from_region(region, all_layouts, module, visited);
        visited.pop();
        selected
    }

    /// Selects the current page layout and returns its sizes and content
    /// hashes, without building any Vello scenes.
    ///
    /// This is the cheap half of [`Self::render_pages_with_accessibility`]:
    /// the editor needs the page list to lay the pane out, but pixels are only
    /// wanted when the viewport settles. Building scenes for frames that the
    /// user is still typing over is pure waste.
    pub fn page_metas(&mut self, kern: &mut IncrDocClient) -> Vec<(Size, Fingerprint)> {
        let Some(layout) = Self::select_page_layout(kern) else {
            kern.layout = None;
            self.reset();
            return Vec::new();
        };
        kern.set_layout(layout);

        let Some(pages) = kern
            .layout
            .as_ref()
            .and_then(|layout| layout.pages(&kern.doc.module))
        else {
            return Vec::new();
        };

        pages
            .pages()
            .iter()
            .map(|Page { content, size }| (Size::new(size.x.0 as f64, size.y.0 as f64), *content))
            .collect()
    }

    /// Renders a specific page of the document in the given window.
    pub fn render_pages(&mut self, kern: &mut IncrDocClient) -> Result<Vec<(Arc<Scene>, Size)>> {
        Ok(self
            .render_pages_with_accessibility(kern)?
            .into_iter()
            .map(|page| (page.scene, page.size))
            .collect())
    }

    /// Renders pages with their accessibility metadata.
    pub fn render_pages_with_accessibility(
        &mut self,
        kern: &mut IncrDocClient,
    ) -> Result<Vec<RenderedPage>> {
        let Some(layout) = Self::select_page_layout(kern) else {
            kern.layout = None;
            self.reset();
            return Ok(vec![]);
        };
        kern.set_layout(layout);

        self.patch_delta(kern);

        // todo: subpixel: pixel_per_pt
        // let ts = sk::Transform::from_scale(s, s);
        // let ts = Affine::scale(s as f64);

        let res = self.vec2vello.flush_pages_with_accessibility();
        Ok(res)
    }
}
