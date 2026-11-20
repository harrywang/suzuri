//! Converts the reflexo vector IR into Vello scenes.
//!
//! Adapted from `tinymist-viewer` (Apache-2.0).
//!
//! Glyph outlines and images travel in the IR, so this module never needs the
//! Typst compiler or font files to render text. The only font lookup is for
//! text inside embedded SVG images.

use std::sync::Arc;

use ecow::EcoVec;
use image::codecs::gif::GifDecoder;
use image::codecs::jpeg::JpegDecoder;
use image::codecs::png::PngDecoder;
use image::codecs::webp::WebPDecoder;
use image::imageops::FilterType;
use image::metadata::Orientation;
use image::{ImageDecoder, ImageResult, Limits};
use reflexo::{
    hash::Fingerprint,
    vector::{
        ir::{
            self, Abs, Axes, ColorSpace, FontIndice, FontItem, FontRef, GradientItem, GradientKind,
            GradientStyle, ImmutStr, Module, PathStyle, PatternItem, Ratio, Scalar,
        },
        vm::{GroupContext, RenderVm, TransformContext},
    },
};
use smallvec::SmallVec;
use vello::peniko;
use vello::{
    Scene,
    kurbo::{self, Affine, Rect, Shape, Vec2},
};

use crate::{
    GroupScene, GroupSceneItem, PageAccessibility, PageTextRun, SvgResource, SvgResourceFormat,
    SvgResourceResolver, VecScene,
};

/// Largest decoded image dimension.
///
/// Vello packs images into a square atlas that grows to fit the largest one,
/// so a single 8192-wide panorama would force a 268 MB atlas even though the
/// image itself is small. 4096 keeps the worst-case atlas at a quarter of that.
const MAX_IMAGE_DIM: f64 = 4096.0;
/// Pixel budget for one decoded image.
///
/// Raster images keep their native resolution; this only shrinks photos large
/// enough that a single one would consume hundreds of megabytes of texture
/// memory.
const MAX_IMAGE_PIXELS: f64 = 4_000_000.0;
/// Decoder limits for source images.
///
/// The decoder allocates the full decoded bitmap before [`limit_raster_image`]
/// can shrink it. This bounds that transient allocation to 128 MiB and rejects
/// decompression bombs instead of letting them OOM the editor.
const MAX_SOURCE_IMAGE_DIM: u32 = 16_384;
const MAX_SOURCE_IMAGE_ALLOC: u64 = 128 * 1024 * 1024;
/// SVG figures are resolution independent, so they are rasterized at this many
/// pixels per Typst point. That keeps them sharp on a HiDPI screen and at
/// moderate zoom, within the pixel budget above.
const SVG_IMAGE_SCALE: f64 = 3.0;

pub struct Renderer<'a> {
    module: &'a Module,
    svg_resource_resolver: Option<Arc<dyn SvgResourceResolver>>,
}

impl<'a> Renderer<'a> {
    pub fn new(module: &'a Module) -> Self {
        Self {
            module,
            svg_resource_resolver: None,
        }
    }

    pub fn with_svg_resource_resolver(
        mut self,
        resolver: Option<Arc<dyn SvgResourceResolver>>,
    ) -> Self {
        self.svg_resource_resolver = resolver;
        self
    }
}

impl<'m> RenderVm<'m> for Renderer<'m> {
    type Resultant = Arc<VecScene>;
    type Group = RenderStack;

    fn get_item(&self, value: &Fingerprint) -> Option<&'m ir::VecItem> {
        self.module.get_item(value)
    }

    fn start_group(&mut self, _v: &Fingerprint) -> Self::Group {
        Self::Group {
            kind: GroupKind::General,
            ts: Affine::IDENTITY,
            clipper: None,
            glyph_style: None,
            pending_text: None,
            items: EcoVec::new(),
        }
    }

    fn start_text(&mut self, value: &Fingerprint, text: &ir::TextItem) -> Self::Group {
        let mut g = self.start_group(value);
        g.kind = GroupKind::Text;
        let stroke_scale = self
            .get_font(&text.shape.font)
            .map(|font| f64::from(font.units_per_em.0) / f64::from(text.shape.size.0))
            .filter(|scale| scale.is_finite())
            .unwrap_or(1.0);
        if let Some(font) = self.get_font(&text.shape.font) {
            g.pending_text = text_run_for_item(text, font);
        }
        g.glyph_style = Some(resolve_draw_style(self, &text.shape.styles, stroke_scale));
        g
    }
}

impl<'m> FontIndice<'m> for Renderer<'m> {
    fn get_font(&self, value: &FontRef) -> Option<&'m ir::FontItem> {
        self.module.fonts.get(value.idx as usize)
    }
}

impl<'m> GlyphFactory for Renderer<'m> {
    fn get_glyph(
        &mut self,
        font: &FontItem,
        glyph: u32,
        style: &DrawStyle,
        _pos: Vec2,
    ) -> Option<Arc<VecScene>> {
        let glyph_data = font.get_glyph(glyph)?;

        match glyph_data.as_ref() {
            ir::FlatGlyphItem::Outline(path) => {
                let path = svg_path(&path.d)?;
                let style = style.clone();
                if !style.has_draw() {
                    return None;
                }

                let mut scene = Scene::new();
                render_path_with_style(self, &mut scene, &path, &style);
                Some(Arc::new(VecScene::Scene(Box::new(scene), None)))
            }
            ir::FlatGlyphItem::Image(glyph) => {
                let (scene, image_transform) =
                    image_item_scene(&glyph.image, self.svg_resource_resolver())?;
                Some(Arc::new(VecScene::Scene(
                    Box::new(scene),
                    Some(transform_to_affine(&glyph.ts) * image_transform),
                )))
            }
            ir::FlatGlyphItem::None => None,
        }
    }

    fn resolve_paint(&self, paint: &ImmutStr) -> PaintBrush {
        resolve_paint(self.module, paint)
    }

    fn svg_resource_resolver(&self) -> Option<&dyn SvgResourceResolver> {
        self.svg_resource_resolver.as_deref()
    }
}

/// A stacked builder for [`VecScene`].
///
/// It holds state of the building process.
pub struct RenderStack {
    /// The kind of the group.
    pub kind: GroupKind,
    /// The transform matrix.
    pub ts: Affine,
    /// A unique clip path on stack
    pub clipper: Option<ir::PathItem>,
    /// The draw style for glyph outlines in text groups.
    glyph_style: Option<DrawStyle>,
    /// Text semantics emitted after the text transform is applied.
    pending_text: Option<PageTextRun>,
    /// The ordered visual and semantic items.
    pub items: EcoVec<GroupSceneItem>,
    // /// The bounding box of the group.
    // pub rect: CanvasBBox,
}

#[derive(Debug, Clone, Copy)]
pub enum GroupKind {
    General,
    Text,
}

impl From<RenderStack> for Arc<VecScene> {
    fn from(mut s: RenderStack) -> Self {
        if let Some(text_run) = s.pending_text.take() {
            s.items.push(GroupSceneItem::Text(text_run));
        }
        Arc::new(VecScene::Group(GroupScene {
            // todo: detect whether there is a failure converting paths.
            clip: s.clipper.and_then(|it| svg_path(&it.d)),
            ts: s.ts,
            items: s.items,
        }))
    }
}

fn text_run_for_item(text: &ir::TextItem, font: &FontItem) -> Option<PageTextRun> {
    let content = text.content.content.as_ref();
    if content.is_empty() || text.shape.size.0 == 0.0 || font.units_per_em.0 == 0.0 {
        return None;
    }

    let upem = font.units_per_em.0 as f64;
    let inv_ppem = upem / text.shape.size.0 as f64;
    if !inv_ppem.is_finite() {
        return None;
    }

    let ascender = font.ascender.0 as f64 * upem;
    let descender = font.descender.0 as f64 * upem;
    let y0 = descender.min(ascender);
    let y1 = descender.max(ascender);
    if y0 == y1 {
        return None;
    }

    let width = text.width().0 as f64 * inv_ppem;
    if width <= 0.0 || !width.is_finite() {
        return None;
    }

    let character_count = content.chars().count();
    if character_count == 0 {
        return None;
    }

    let mut character_bounds = text_run_character_bounds(text, font, inv_ppem, y0, y1);
    if character_bounds.len() != character_count {
        character_bounds = evenly_spaced_character_bounds(character_count, width, y0, y1);
    }

    Some(PageTextRun::new(
        content,
        Rect::new(0.0, y0, width, y1),
        character_bounds,
    ))
}

fn text_run_character_bounds(
    text: &ir::TextItem,
    font: &FontItem,
    inv_ppem: f64,
    y0: f64,
    y1: f64,
) -> Vec<Rect> {
    let chars: Vec<char> = text.content.content.chars().collect();
    if chars.is_empty() {
        return Vec::new();
    }

    let glyphs: Vec<(f64, usize)> = text
        .content
        .glyphs
        .iter()
        .map(|(_, advance, glyph)| {
            (
                advance.x.0 as f64 * inv_ppem,
                glyph_ligature_len(font, *glyph),
            )
        })
        .collect();

    let Some(advances) = character_advances(&chars, &glyphs) else {
        return Vec::new();
    };

    let mut cursor = 0.0;
    advances
        .into_iter()
        .map(|advance| {
            let next = cursor + advance;
            let rect = Rect::new(cursor, y0, next, y1);
            cursor = next;
            rect
        })
        .collect()
}

/// Splits glyph advances into one advance per text character.
///
/// Tinymist normally disables the preview IR's explicit ligature lengths, so a
/// ligature such as `fi` arrives as one glyph while the text still contains two
/// characters. Without splitting that glyph, the caller falls back to dividing
/// the whole run evenly, which makes find highlights and selections drift.
///
/// Explicit cluster lengths win. Otherwise common Latin ligatures are matched
/// and that glyph's advance is split evenly across its characters. Returns
/// `None` when the glyphs cannot be mapped onto every character; the caller then
/// uses its uniform fallback.
fn character_advances(chars: &[char], glyphs: &[(f64, usize)]) -> Option<Vec<f64>> {
    let mut advances = Vec::with_capacity(chars.len());
    let mut character_index = 0;

    for (glyph_index, &(advance, explicit_len)) in glyphs.iter().enumerate() {
        if character_index >= chars.len() {
            break;
        }
        if !advance.is_finite() {
            return None;
        }

        let chars_remaining = chars.len() - character_index;
        let glyphs_remaining = glyphs.len() - glyph_index;
        let mut cluster_len = explicit_len.max(1);
        if explicit_len == 0 && chars_remaining > glyphs_remaining {
            cluster_len = common_ligature_len(&chars[character_index..]).unwrap_or(1);
        }
        cluster_len = cluster_len.clamp(1, chars_remaining);

        let character_advance = advance / cluster_len as f64;
        for _ in 0..cluster_len {
            advances.push(character_advance);
            character_index += 1;
        }
    }

    (character_index == chars.len()).then_some(advances)
}

/// The character length of a common Latin ligature at the start of `chars`.
fn common_ligature_len(chars: &[char]) -> Option<usize> {
    const LIGATURES: [&str; 5] = ["ffi", "ffl", "ff", "fi", "fl"];

    LIGATURES.iter().find_map(|ligature| {
        let length = ligature.chars().count();
        (chars.len() >= length && chars[..length].iter().copied().eq(ligature.chars()))
            .then_some(length)
    })
}

fn evenly_spaced_character_bounds(
    character_count: usize,
    width: f64,
    y0: f64,
    y1: f64,
) -> Vec<Rect> {
    let character_width = width / character_count as f64;
    (0..character_count)
        .map(|index| {
            let x0 = character_width * index as f64;
            Rect::new(x0, y0, x0 + character_width, y1)
        })
        .collect()
}

fn glyph_ligature_len(font: &FontItem, glyph: u32) -> usize {
    match font.get_glyph(glyph).map(|glyph| glyph.as_ref()) {
        Some(ir::FlatGlyphItem::Image(glyph)) => glyph.ligature_len.into(),
        Some(ir::FlatGlyphItem::Outline(glyph)) => glyph.ligature_len.into(),
        Some(ir::FlatGlyphItem::None) | None => 1,
    }
}

/// See [`TransformContext`].
impl<C> TransformContext<C> for RenderStack {
    fn transform_matrix(mut self, _ctx: &mut C, m: &ir::Transform) -> Self {
        let sub_ts = transform_to_affine(m);
        self.ts *= sub_ts;
        self
    }

    fn transform_translate(mut self, _ctx: &mut C, matrix: Axes<Abs>) -> Self {
        self.ts = self
            .ts
            .then_translate(Vec2::new(matrix.x.0 as f64, matrix.y.0 as f64));
        self
    }

    fn transform_scale(mut self, _ctx: &mut C, x: Ratio, y: Ratio) -> Self {
        self.ts = self.ts.then_scale_non_uniform(x.0 as f64, y.0 as f64);
        self
    }

    fn transform_rotate(mut self, _ctx: &mut C, angle: Scalar) -> Self {
        self.ts *= Affine::rotate(angle.0 as f64);
        self
    }

    fn transform_skew(mut self, _ctx: &mut C, matrix: (Ratio, Ratio)) -> Self {
        self.ts *= Affine::skew(matrix.0.0 as f64, matrix.1.0 as f64);
        self
    }

    fn transform_clip(mut self, _ctx: &mut C, matrix: &ir::PathItem) -> Self {
        self.clipper = Some(matrix.clone());
        self
    }
}

/// See [`GroupContext`].
impl<'m, C: RenderVm<'m, Resultant = Arc<VecScene>> + GlyphFactory> GroupContext<C>
    for RenderStack
{
    fn render_path(&mut self, ctx: &mut C, path: &ir::PathItem, _abs_ref: &Fingerprint) {
        let mut scene = Scene::new();
        let Some(path_data) = svg_path(&path.d) else {
            return;
        };
        let style = resolve_draw_style(ctx, &path.styles, 1.0);
        render_path_with_style(ctx, &mut scene, &path_data, &style);

        self.items.push(GroupSceneItem::Scene {
            pos: Vec2::new(0., 0.),
            scene: Arc::new(VecScene::Scene(Box::new(scene), None)),
        });
    }

    fn render_link(&mut self, _ctx: &mut C, link: &ir::LinkItem) {
        if link.size.x.0 <= 0.0 || link.size.y.0 <= 0.0 {
            return;
        }

        self.items
            .push(GroupSceneItem::Accessibility(PageAccessibility::link_node(
                link.href.as_ref(),
                Rect {
                    x0: 0.0,
                    y0: 0.0,
                    x1: link.size.x.0 as f64,
                    y1: link.size.y.0 as f64,
                },
            )));
    }

    fn render_image(&mut self, ctx: &mut C, image_item: &ir::ImageItem) {
        if image_item.image.width() == 0
            || image_item.image.height() == 0
            || image_item.size.x.0 < 1e-11
            || image_item.size.y.0 < 1e-11
        {
            return;
        }

        let Some((scene, transform)) = image_item_scene(image_item, ctx.svg_resource_resolver())
        else {
            return;
        };

        self.items.push(GroupSceneItem::Scene {
            pos: Vec2::new(0., 0.),
            scene: Arc::new(VecScene::Scene(Box::new(scene), Some(transform))),
        });
    }

    fn render_item_at(&mut self, ctx: &mut C, pos: ir::Point, item: &Fingerprint) {
        self.items.push(GroupSceneItem::Scene {
            pos: Vec2::new(pos.x.0 as f64, pos.y.0 as f64),
            scene: ctx.render_item(item),
        });
    }

    fn render_content_hint(&mut self, _ctx: &mut C, ch: char) {
        // Hints carry the character that separates blocks (usually `\n`). A
        // zero-width run keeps them in the text semantics without making them
        // hit-testable, so selections, copied text, and word counts see the
        // same separators as Tinymist's text export.
        self.items.push(GroupSceneItem::Text(PageTextRun::new(
            ch.to_string(),
            Rect::new(0.0, 0.0, 0.0, 0.0),
            [],
        )));
    }

    fn render_glyph(&mut self, ctx: &mut C, pos: Axes<Scalar>, font: &FontItem, glyph: u32) {
        let pos = Vec2::new(pos.x.0 as f64, pos.y.0 as f64);
        let glyph_transform = self.ts.pre_translate(pos);
        if let Some(style) = &self.glyph_style
            && let Some(glyph) = ctx.get_glyph(
                font,
                glyph,
                &style.translated_for_glyph(pos, glyph_transform),
                pos,
            )
        {
            self.items.push(GroupSceneItem::Scene { pos, scene: glyph });
        }
    }
}

fn image_item_scene(
    image_item: &ir::ImageItem,
    svg_resource_resolver: Option<&dyn SvgResourceResolver>,
) -> Option<(vello::Scene, Affine)> {
    let (image_data, width, height, quality) = decode_image_data_for_item(
        image_item.image.format.as_ref(),
        &image_item.image.data,
        image_item.size,
        &image_item.image.attrs,
        svg_resource_resolver,
    )?;

    let mut scene = vello::Scene::new();
    let brush = peniko::ImageBrush::new(image_data).with_quality(quality);
    scene.draw_image(&brush, kurbo::Affine::IDENTITY);

    let transform = Affine::IDENTITY.pre_scale_non_uniform(
        image_item.size.x.0 as f64 / width as f64,
        image_item.size.y.0 as f64 / height as f64,
    );

    Some((scene, transform))
}

fn decode_image_data_for_item(
    format: &str,
    data: &[u8],
    target_size: Axes<Abs>,
    attrs: &[ir::ImageAttr],
    svg_resource_resolver: Option<&dyn SvgResourceResolver>,
) -> Option<(peniko::ImageData, u32, u32, peniko::ImageQuality)> {
    // Nearest-neighbor sampling is only correct when the document explicitly
    // asks for `image-rendering: pixelated`; everything else needs bilinear
    // sampling or it looks blocky as soon as the page is scaled.
    let quality = if is_pixelated(attrs) {
        peniko::ImageQuality::Low
    } else {
        peniko::ImageQuality::Medium
    };
    match format {
        "svg" | "svg+xml" => {
            let (image_data, width, height) =
                decode_svg_image(data, target_size, svg_resource_resolver)?;
            Some((image_data, width, height, quality))
        }
        _ => {
            let (image_data, width, height) =
                decode_raster_image(format, data, is_pixelated(attrs))?;
            Some((image_data, width, height, quality))
        }
    }
}

fn is_pixelated(attrs: &[ir::ImageAttr]) -> bool {
    attrs.iter().any(|attr| {
        matches!(attr, ir::ImageAttr::ImageRendering(rendering) if rendering.as_ref() == "pixelated")
    })
}

fn decode_svg_image(
    data: &[u8],
    target_size: Axes<Abs>,
    svg_resource_resolver: Option<&dyn SvgResourceResolver>,
) -> Option<(peniko::ImageData, u32, u32)> {
    let svg = std::str::from_utf8(data).ok()?;
    let options = svg_options(data, svg_resource_resolver);
    let tree = resvg::usvg::Tree::from_str(svg, &options).ok()?;
    let tree_size = tree.size();
    let width = tree_size.width();
    let height = tree_size.height();
    if width == 0.0 || height == 0.0 {
        return None;
    }

    let (width, height) =
        target_texture_size(target_size, width as f64 / height as f64, SVG_IMAGE_SCALE);
    let mut pixmap = resvg::tiny_skia::Pixmap::new(width, height)?;
    let transform = resvg::tiny_skia::Transform::from_scale(
        width as f32 / tree_size.width(),
        height as f32 / tree_size.height(),
    );
    resvg::render(&tree, transform, &mut pixmap.as_mut());

    Some((
        peniko::ImageData {
            data: peniko::Blob::new(Arc::new(pixmap.data().to_vec())),
            format: peniko::ImageFormat::Rgba8,
            alpha_type: peniko::ImageAlphaType::AlphaPremultiplied,
            width,
            height,
        },
        width,
        height,
    ))
}

fn target_texture_size(target_size: Axes<Abs>, aspect: f64, scale: f64) -> (u32, u32) {
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    let width = (target_size.x.0 as f64)
        .max(aspect * target_size.y.0 as f64)
        .max(1.0)
        * scale;
    let height = (width / aspect).max(1.0);
    let (width, height) = clamp_texture_size(width, height);
    (width.ceil().max(1.0) as u32, height.ceil().max(1.0) as u32)
}

/// Keeps a texture within the image atlas and a bounded memory budget.
///
/// Vello's image atlas tops out at 8192 x 8192. The pixel budget additionally
/// keeps a single oversized photo from consuming hundreds of megabytes of
/// texture memory.
fn clamp_texture_size(width: f64, height: f64) -> (f64, f64) {
    let fit = (MAX_IMAGE_DIM / width.max(height))
        .min((MAX_IMAGE_PIXELS / (width * height)).sqrt())
        .min(1.0);
    (width * fit, height * fit)
}

fn svg_options<'a>(
    svg_data: &'a [u8],
    svg_resource_resolver: Option<&'a dyn SvgResourceResolver>,
) -> resvg::usvg::Options<'a> {
    // Deliberately do not fall back to resvg's default string resolver: it
    // treats every href as a local path and calls `std::fs::read`, so a
    // document could pull arbitrary files from the machine into the preview
    // (and allocate their full size). Only resources supplied by the IR
    // resolver, plus `data:` URIs handled by `resolve_data`, are allowed.
    resvg::usvg::Options {
        image_href_resolver: resvg::usvg::ImageHrefResolver {
            resolve_data: resvg::usvg::ImageHrefResolver::default_data_resolver(),
            resolve_string: Box::new(move |href, _options| {
                let href = href.strip_prefix("file://").unwrap_or(href);
                svg_resource_resolver
                    .and_then(|resolver| resolver.resolve_svg_resource(svg_data, href))
                    .map(svg_resource_to_usvg_image_kind)
            }),
        },
        ..Default::default()
    }
}

fn svg_resource_to_usvg_image_kind(resource: SvgResource) -> resvg::usvg::ImageKind {
    match resource.format {
        SvgResourceFormat::Jpeg => resvg::usvg::ImageKind::JPEG(resource.data),
        SvgResourceFormat::Png => resvg::usvg::ImageKind::PNG(resource.data),
        SvgResourceFormat::Gif => resvg::usvg::ImageKind::GIF(resource.data),
        SvgResourceFormat::Webp => resvg::usvg::ImageKind::WEBP(resource.data),
    }
}

fn transform_to_affine(m: &ir::Transform) -> Affine {
    Affine::new([
        m.sx.0 as f64,
        m.ky.0 as f64,
        m.kx.0 as f64,
        m.sy.0 as f64,
        m.tx.0 as f64,
        m.ty.0 as f64,
    ])
}

fn decode_raster_image(
    format: &str,
    data: &[u8],
    pixelated: bool,
) -> Option<(peniko::ImageData, u32, u32)> {
    let png_orientation = (format == "png")
        .then(|| png_exif_orientation(data))
        .flatten();
    let data = std::io::Cursor::new(data);

    let decoded = match format {
        "jpeg" | "jpg" => decode(JpegDecoder::new(data)),
        "png" => decode_with_orientation(PngDecoder::new(data), png_orientation),
        "webp" => decode(WebPDecoder::new(data)),
        "gif" => decode(GifDecoder::new(data)),
        _ => return None,
    };
    let Ok(image_data) = decoded else {
        return None;
    };

    let width = image_data.width();
    let height = image_data.height();
    if width == 0 || height == 0 {
        return None;
    }

    let image_data = limit_raster_image(image_data, pixelated);
    let width = image_data.width();
    let height = image_data.height();

    Some((
        peniko::ImageData {
            data: peniko::Blob::new(Arc::new(image_data.to_rgba8().into_vec())),
            format: peniko::ImageFormat::Rgba8,
            alpha_type: peniko::ImageAlphaType::Alpha,
            width,
            height,
        },
        width,
        height,
    ))
}

/// Downscales only raster images that exceed the texture budget.
///
/// Images keep their native resolution: decoding them at the size they occupy
/// on the page throws away the detail that the page raster needs as soon as it
/// is rendered for a HiDPI screen or zoomed in.
fn limit_raster_image(image_data: image::DynamicImage, pixelated: bool) -> image::DynamicImage {
    let (width, height) = (image_data.width() as f64, image_data.height() as f64);
    let (limited_width, limited_height) = clamp_texture_size(width, height);
    if limited_width == width && limited_height == height {
        return image_data;
    }

    let target_width = limited_width.round().max(1.0) as u32;
    let target_height = limited_height.round().max(1.0) as u32;
    let filter = if pixelated {
        FilterType::Nearest
    } else {
        FilterType::Lanczos3
    };
    image_data.resize_exact(target_width, target_height, filter)
}

fn decode<T: ImageDecoder>(decoder: ImageResult<T>) -> ImageResult<image::DynamicImage> {
    let mut decoder = decoder?;
    decoder.set_limits(decode_limits())?;
    let orientation = decoder.orientation()?;
    decode_with_loaded_orientation(decoder, orientation)
}

fn decode_with_orientation<T: ImageDecoder>(
    decoder: ImageResult<T>,
    orientation: Option<Orientation>,
) -> ImageResult<image::DynamicImage> {
    let mut decoder = decoder?;
    decoder.set_limits(decode_limits())?;
    let orientation = match orientation {
        Some(orientation) => orientation,
        None => decoder.orientation()?,
    };
    decode_with_loaded_orientation(decoder, orientation)
}

fn decode_limits() -> Limits {
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SOURCE_IMAGE_DIM);
    limits.max_image_height = Some(MAX_SOURCE_IMAGE_DIM);
    limits.max_alloc = Some(MAX_SOURCE_IMAGE_ALLOC);
    limits
}

fn decode_with_loaded_orientation<T: ImageDecoder>(
    decoder: T,
    orientation: Orientation,
) -> ImageResult<image::DynamicImage> {
    let mut dynamic = image::DynamicImage::from_decoder(decoder)?;
    dynamic.apply_orientation(orientation);
    Ok(dynamic)
}

fn png_exif_orientation(data: &[u8]) -> Option<Orientation> {
    const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
    if !data.starts_with(PNG_SIGNATURE) {
        return None;
    }

    let mut offset = PNG_SIGNATURE.len();
    while offset.checked_add(8)? <= data.len() {
        let length = u32::from_be_bytes(data[offset..offset + 4].try_into().ok()?) as usize;
        let chunk_type = &data[offset + 4..offset + 8];
        let data_start = offset + 8;
        let data_end = data_start.checked_add(length)?;
        let next = data_end.checked_add(4)?;
        if next > data.len() {
            return None;
        }

        if chunk_type == b"eXIf" {
            return Orientation::from_exif_chunk(&data[data_start..data_end]);
        }

        offset = next;
    }

    None
}

trait GlyphFactory {
    fn get_glyph(
        &mut self,
        font: &FontItem,
        glyph: u32,
        style: &DrawStyle,
        pos: Vec2,
    ) -> Option<Arc<VecScene>>;

    fn resolve_paint(&self, paint: &ImmutStr) -> PaintBrush;

    fn svg_resource_resolver(&self) -> Option<&dyn SvgResourceResolver>;
}

#[derive(Clone, Debug)]
struct DrawStyle {
    fill: Option<PaintBrush>,
    fill_rule: peniko::Fill,
    stroke: Option<StrokeStyle>,
}

impl Default for DrawStyle {
    fn default() -> Self {
        Self {
            fill: None,
            fill_rule: peniko::Fill::NonZero,
            stroke: None,
        }
    }
}

#[derive(Clone, Debug)]
struct StrokeStyle {
    brush: PaintBrush,
    stroke: kurbo::Stroke,
}

impl DrawStyle {
    fn has_draw(&self) -> bool {
        self.fill.is_some() || self.stroke.is_some()
    }

    fn translated_for_glyph(&self, pos: Vec2, glyph_transform: Affine) -> Self {
        Self {
            fill: self
                .fill
                .clone()
                .map(|paint| paint.translated_for_glyph(pos, glyph_transform)),
            fill_rule: self.fill_rule,
            stroke: self.stroke.as_ref().map(|stroke| StrokeStyle {
                brush: stroke
                    .brush
                    .clone()
                    .translated_for_glyph(pos, glyph_transform),
                stroke: stroke.stroke.clone(),
            }),
        }
    }
}

fn resolve_draw_style<'m, C>(ctx: &C, styles: &[PathStyle], stroke_scale: f64) -> DrawStyle
where
    C: RenderVm<'m, Resultant = Arc<VecScene>> + GlyphFactory,
{
    let mut draw = DrawStyle::default();
    let mut stroke_brush = PaintBrush::black();
    let mut stroke = false;
    let mut stroke_width = 0f64;
    let mut stroke_join = kurbo::Join::Miter;
    let mut stroke_cap = kurbo::Cap::Butt;
    let mut stroke_miter_limit = 4f64;
    let mut dash_pattern = SmallVec::new();
    let mut dash_offset = 0f64;

    for style in styles {
        match style {
            PathStyle::Fill(color) => {
                draw.fill = Some(ctx.resolve_paint(color));
            }
            PathStyle::Stroke(color) => {
                stroke_brush = ctx.resolve_paint(color);
                stroke = true;
            }
            PathStyle::StrokeWidth(width) => {
                stroke_width = f64::from(width.0) * stroke_scale;
            }
            PathStyle::StrokeLineCap(cap) => {
                stroke_cap = match cap.as_ref() {
                    "butt" => kurbo::Cap::Butt,
                    "round" => kurbo::Cap::Round,
                    "square" => kurbo::Cap::Square,
                    _ => kurbo::Cap::Butt,
                };
            }
            PathStyle::StrokeLineJoin(join) => {
                stroke_join = match join.as_ref() {
                    "miter" => kurbo::Join::Miter,
                    "round" => kurbo::Join::Round,
                    "bevel" => kurbo::Join::Bevel,
                    _ => kurbo::Join::Miter,
                };
            }
            PathStyle::StrokeMitterLimit(limit) => {
                stroke_miter_limit = f64::from(limit.0);
            }
            PathStyle::StrokeDashArray(array) => {
                dash_pattern = array
                    .iter()
                    .map(|dash| f64::from(dash.0) * stroke_scale)
                    .collect();
            }
            PathStyle::StrokeDashOffset(offset) => {
                dash_offset = f64::from(offset.0) * stroke_scale;
            }
            PathStyle::FillRule(rule) => {
                draw.fill_rule = match rule.as_ref() {
                    "nonzero" => peniko::Fill::NonZero,
                    "evenodd" => peniko::Fill::EvenOdd,
                    _ => peniko::Fill::NonZero,
                };
            }
        }
    }

    if stroke {
        let mut kurbo_stroke = kurbo::Stroke {
            width: stroke_width,
            join: stroke_join,
            miter_limit: stroke_miter_limit,
            start_cap: stroke_cap,
            end_cap: stroke_cap,
            ..Default::default()
        };
        if !dash_pattern.is_empty() {
            kurbo_stroke.dash_pattern = dash_pattern;
            kurbo_stroke.dash_offset = dash_offset;
        }

        draw.stroke = Some(StrokeStyle {
            brush: stroke_brush,
            stroke: kurbo_stroke,
        });
    }

    draw
}

fn render_path_with_style<'m, C>(
    ctx: &mut C,
    scene: &mut Scene,
    path: &kurbo::BezPath,
    style: &DrawStyle,
) where
    C: RenderVm<'m, Resultant = Arc<VecScene>> + GlyphFactory,
{
    if let Some(fill) = &style.fill {
        fill_path_with_paint(ctx, scene, style.fill_rule, path, fill);
    }

    if let Some(stroke) = &style.stroke {
        stroke_path_with_paint(ctx, scene, &stroke.stroke, path, &stroke.brush);
    }
}

#[derive(Clone, Debug)]
enum PaintBrush {
    Brush {
        brush: peniko::Brush,
        transform: Option<Affine>,
    },
    Pattern(PatternPaint),
}

#[derive(Clone, Debug)]
struct PatternPaint {
    pattern: Arc<PatternItem>,
    transform: Affine,
}

impl PaintBrush {
    fn black() -> Self {
        Self::solid(peniko::Color::BLACK)
    }

    fn solid(color: peniko::Color) -> Self {
        Self::Brush {
            brush: peniko::Brush::Solid(color),
            transform: None,
        }
    }

    fn translated_for_glyph(self, pos: Vec2, glyph_transform: Affine) -> Self {
        match self {
            Self::Brush {
                brush,
                mut transform,
            } => {
                if matches!(brush, peniko::Brush::Gradient(_)) {
                    let matrix = transform.unwrap_or(Affine::IDENTITY);
                    transform = Some(glyph_transform.inverse() * matrix);
                }

                Self::Brush { brush, transform }
            }
            Self::Pattern(mut pattern) => {
                pattern.transform = pattern.transform.then_translate(-pos);
                Self::Pattern(pattern)
            }
        }
    }
}

fn resolve_paint(module: &Module, paint: &ImmutStr) -> PaintBrush {
    if paint.starts_with("@g") {
        resolve_gradient(module, paint.as_ref()).unwrap_or_else(PaintBrush::black)
    } else if paint.starts_with("@p") {
        resolve_pattern(module, paint.as_ref()).unwrap_or_else(PaintBrush::black)
    } else if paint.starts_with('@') {
        PaintBrush::black()
    } else {
        PaintBrush::solid(
            peniko::color::parse_color(paint.as_ref())
                .map(|it| it.to_alpha_color())
                .unwrap_or(peniko::Color::BLACK),
        )
    }
}

fn fill_path_with_paint<'m, C>(
    ctx: &mut C,
    scene: &mut Scene,
    fill_rule: peniko::Fill,
    path: &kurbo::BezPath,
    paint: &PaintBrush,
) where
    C: RenderVm<'m, Resultant = Arc<VecScene>> + GlyphFactory,
{
    match paint {
        PaintBrush::Brush { brush, transform } => {
            scene.fill(fill_rule, Affine::IDENTITY, brush, *transform, path);
        }
        PaintBrush::Pattern(pattern) => {
            scene.push_clip_layer(fill_rule, Affine::IDENTITY, path);
            render_pattern_tiles(ctx, scene, pattern, path.bounding_box());
            scene.pop_layer();
        }
    }
}

fn stroke_path_with_paint<'m, C>(
    ctx: &mut C,
    scene: &mut Scene,
    stroke: &kurbo::Stroke,
    path: &kurbo::BezPath,
    paint: &PaintBrush,
) where
    C: RenderVm<'m, Resultant = Arc<VecScene>> + GlyphFactory,
{
    match paint {
        PaintBrush::Brush { brush, transform } => {
            scene.stroke(stroke, Affine::IDENTITY, brush, *transform, path);
        }
        PaintBrush::Pattern(pattern) => {
            if stroke.width == 0. {
                return;
            }

            scene.push_clip_layer(stroke, Affine::IDENTITY, path);
            render_pattern_tiles(ctx, scene, pattern, stroke_pattern_bounds(path, stroke));
            scene.pop_layer();
        }
    }
}

fn stroke_pattern_bounds(path: &kurbo::BezPath, stroke: &kurbo::Stroke) -> Rect {
    let padding = stroke.width * stroke.miter_limit.max(1.);
    path.bounding_box().inset(padding)
}

fn render_pattern_tiles<'m, C>(ctx: &mut C, scene: &mut Scene, pattern: &PatternPaint, bounds: Rect)
where
    C: RenderVm<'m, Resultant = Arc<VecScene>> + GlyphFactory,
{
    if !is_valid_rect(bounds) {
        return;
    }

    let tile_width = f64::from(pattern.pattern.size.x.0 + pattern.pattern.spacing.x.0);
    let tile_height = f64::from(pattern.pattern.size.y.0 + pattern.pattern.spacing.y.0);
    if tile_width <= f64::EPSILON || tile_height <= f64::EPSILON {
        return;
    }

    let pattern_bounds = pattern.transform.inverse().transform_rect_bbox(bounds);
    if !is_valid_rect(pattern_bounds) {
        return;
    }

    let tile = ctx.render_item(&pattern.pattern.frame);
    let mut tile_scene = Scene::new();
    let tile_clip = Rect::new(0., 0., tile_width, tile_height);
    tile_scene.push_clip_layer(peniko::Fill::NonZero, Affine::IDENTITY, &tile_clip);
    tile.render(&mut tile_scene);
    tile_scene.pop_layer();

    let start_x = tile_start(pattern_bounds.min_x(), tile_width);
    let end_x = tile_end(pattern_bounds.max_x(), tile_width);
    let start_y = tile_start(pattern_bounds.min_y(), tile_height);
    let end_y = tile_end(pattern_bounds.max_y(), tile_height);

    for y in start_y..=end_y {
        for x in start_x..=end_x {
            let tile_origin = Vec2::new(
                f64::from(x) * tile_width + f64::from(pattern.pattern.offset.x.0),
                f64::from(y) * tile_height + f64::from(pattern.pattern.offset.y.0),
            );
            scene.append(
                &tile_scene,
                Some(pattern.transform.pre_translate(tile_origin)),
            );
        }
    }
}

fn is_valid_rect(rect: Rect) -> bool {
    rect.x0.is_finite()
        && rect.y0.is_finite()
        && rect.x1.is_finite()
        && rect.y1.is_finite()
        && !rect.is_zero_area()
}

fn tile_start(value: f64, step: f64) -> i32 {
    tile_index((value / step).floor() - 1.)
}

fn tile_end(value: f64, step: f64) -> i32 {
    tile_index((value / step).ceil() + 1.)
}

fn tile_index(value: f64) -> i32 {
    value.clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
}

fn resolve_gradient(module: &Module, paint: &str) -> Option<PaintBrush> {
    let id = paint.strip_prefix("@g")?;
    let mut fingerprint = parse_fingerprint(id)?;
    let mut transform = None;
    let mut gradient_geometry = GradientGeometry::default();

    if let Some(ir::VecItem::ColorTransform(color_transform)) = module.get_item(&fingerprint) {
        fingerprint = color_transform.item;
        let converted = convert_transform(&color_transform.transform);
        transform = Some(converted);
        gradient_geometry = GradientGeometry::from_paint_transform(converted);
    }

    let gradient = match module.get_item(&fingerprint) {
        Some(ir::VecItem::Gradient(gradient)) => gradient.as_ref(),
        _ => return None,
    };

    let converted = convert_gradient(gradient, gradient_geometry)?;
    if converted.uses_paint_transform {
        transform = converted.transform;
    } else if let Some(gradient_transform) = converted.transform {
        transform = Some(match transform {
            Some(transform) => transform * gradient_transform,
            None => gradient_transform,
        });
    }

    Some(PaintBrush::Brush {
        brush: peniko::Brush::Gradient(converted.gradient),
        transform,
    })
}

fn resolve_pattern(module: &Module, paint: &str) -> Option<PaintBrush> {
    let id = paint.strip_prefix("@p")?;
    let mut fingerprint = parse_fingerprint(id)?;
    let mut transform = Affine::IDENTITY;

    if let Some(ir::VecItem::ColorTransform(color_transform)) = module.get_item(&fingerprint) {
        fingerprint = color_transform.item;
        transform = convert_transform(&color_transform.transform);
    }

    let pattern = match module.get_item(&fingerprint) {
        Some(ir::VecItem::Pattern(pattern)) => pattern.clone(),
        _ => return None,
    };

    Some(PaintBrush::Pattern(PatternPaint { pattern, transform }))
}

fn parse_fingerprint(id: &str) -> Option<Fingerprint> {
    if id.len() < 11 {
        return None;
    }

    Fingerprint::try_from_str(id).ok()
}

fn convert_transform(m: &ir::Transform) -> Affine {
    Affine::new([
        m.sx.0 as f64,
        m.ky.0 as f64,
        m.kx.0 as f64,
        m.sy.0 as f64,
        m.tx.0 as f64,
        m.ty.0 as f64,
    ])
}

struct ConvertedGradient {
    gradient: peniko::Gradient,
    transform: Option<Affine>,
    uses_paint_transform: bool,
}

#[derive(Clone, Copy, Debug, Default)]
struct GradientGeometry {
    bake_transform: Option<Affine>,
}

impl GradientGeometry {
    fn from_paint_transform(transform: Affine) -> Self {
        Self {
            bake_transform: can_bake_gradient_transform(transform).then_some(transform),
        }
    }
}

fn convert_gradient(
    gradient: &GradientItem,
    geometry: GradientGeometry,
) -> Option<ConvertedGradient> {
    if gradient.stops.is_empty() {
        return None;
    }

    let mut transform = None;
    let (stops, interpolation_cs) = convert_gradient_stops(gradient);

    let mut peniko_gradient = match &gradient.kind {
        GradientKind::Linear(angle) => {
            let (start, end) = if let Some(paint_transform) = geometry.bake_transform {
                linear_gradient_points_for_vello(angle.0, paint_transform)
            } else {
                linear_gradient_points(angle.0)
            };
            peniko::Gradient::new_linear(start, end)
        }
        GradientKind::Radial(radius) => {
            let mut center = Axes::new(Scalar(0.5), Scalar(0.5));
            let mut focal_center = Axes::new(Scalar(0.5), Scalar(0.5));
            let mut focal_radius = Scalar(0.);

            for style in &gradient.styles {
                match style {
                    GradientStyle::Center(value) => center = *value,
                    GradientStyle::FocalCenter(value) => focal_center = *value,
                    GradientStyle::FocalRadius(value) => focal_radius = *value,
                }
            }

            peniko::Gradient::new_two_point_radial(
                (focal_center.x.0 as f64, focal_center.y.0 as f64),
                focal_radius.0,
                (center.x.0 as f64, center.y.0 as f64),
                radius.0,
            )
        }
        GradientKind::Conic(angle) => {
            let mut center = Axes::new(Scalar(0.5), Scalar(0.5));
            for style in &gradient.styles {
                if let GradientStyle::Center(value) = style {
                    center = *value;
                }
            }

            if let Some(paint_transform) = geometry.bake_transform {
                center = transform_point(paint_transform, center);
                transform = Some(conic_gradient_transform(center, *angle));
            } else {
                transform = Some(conic_gradient_transform(center, *angle));
            }

            peniko::Gradient::new_sweep(
                (center.x.0 as f64, center.y.0 as f64),
                0.,
                std::f32::consts::TAU,
            )
        }
    };

    peniko_gradient.interpolation_cs = interpolation_cs;
    peniko_gradient.stops = stops;

    Some(ConvertedGradient {
        gradient: peniko_gradient,
        transform,
        uses_paint_transform: matches!(
            gradient.kind,
            GradientKind::Linear(_) | GradientKind::Conic(_)
        ) && geometry.bake_transform.is_some(),
    })
}

fn convert_gradient_stops(
    gradient: &GradientItem,
) -> (peniko::ColorStops, peniko::color::ColorSpaceTag) {
    // Vello 0.7 resolves gradient ramps in sRGB regardless of
    // `Gradient::interpolation_cs`. Tinymist's viewer pre-samples non-sRGB
    // mixing spaces through Typst's color mixer; that mixer lives in the
    // `typst` crate, which we deliberately do not link into the GUI. Until it
    // is ported, the raw stops go to Vello and a non-sRGB space renders as
    // sRGB.
    //
    // TODO: port Typst's color mixing so Oklab/Oklch/Hsl/Hsv/Cmyk/LinearRgb/
    // D65Gray gradients interpolate in their own space.
    (
        raw_gradient_stops(gradient),
        color_space_tag(gradient.space),
    )
}

fn raw_gradient_stops(gradient: &GradientItem) -> peniko::ColorStops {
    let mut stops = peniko::ColorStops::new();
    for (color, offset) in &gradient.stops {
        stops.push(peniko::ColorStop {
            offset: offset.0,
            color: dynamic_color_from_rgba8(*color),
        });
    }

    stops
}

fn dynamic_color_from_rgba8(color: ir::Rgba8Item) -> peniko::color::DynamicColor {
    peniko::color::DynamicColor::from_alpha_color(peniko::Color::from_rgba8(
        color.r, color.g, color.b, color.a,
    ))
}

fn conic_gradient_transform(center: Axes<Scalar>, angle: Scalar) -> Affine {
    let center = Vec2::new(center.x.0 as f64, center.y.0 as f64);
    Affine::rotate_about(angle.0 as f64 + std::f64::consts::PI, (center.x, center.y))
}

fn can_bake_gradient_transform(transform: Affine) -> bool {
    let [xx, yx, xy, yy, dx, dy] = transform.as_coeffs();
    xx > f64::EPSILON
        && yy > f64::EPSILON
        && yx.abs() <= f64::EPSILON
        && xy.abs() <= f64::EPSILON
        && dx.is_finite()
        && dy.is_finite()
}

fn corrected_linear_angle(angle: f32, transform: Affine) -> f32 {
    let [xx, _, _, yy, _, _] = transform.as_coeffs();
    let aspect = (xx / yy).abs() as f32;
    let angle = angle.rem_euclid(std::f32::consts::TAU);
    (angle.sin() / aspect)
        .atan2(angle.cos())
        .rem_euclid(std::f32::consts::TAU)
}

fn linear_gradient_points_for_vello(angle: f32, transform: Affine) -> ((f64, f64), (f64, f64)) {
    let angle = corrected_linear_angle(angle, transform);
    let (sin, cos) = angle.sin_cos();
    let factor = sin.abs() + cos.abs();
    let start = linear_gradient_start(angle);
    let start = transform_tuple(transform, start);

    let [xx, _, _, yy, _, _] = transform.as_coeffs();
    let line_x = f64::from(cos / factor) / xx;
    let line_y = f64::from(sin / factor) / yy;
    let scale = line_x * line_x + line_y * line_y;
    if scale <= f64::EPSILON {
        return (start, start);
    }

    let end = (start.0 + line_x / scale, start.1 + line_y / scale);
    (start, end)
}

fn linear_gradient_start(angle: f32) -> (f64, f64) {
    match angle.rem_euclid(std::f32::consts::TAU) {
        angle if angle < std::f32::consts::FRAC_PI_2 => (0., 0.),
        angle if angle < std::f32::consts::PI => (1., 0.),
        angle if angle < 3. * std::f32::consts::FRAC_PI_2 => (1., 1.),
        _ => (0., 1.),
    }
}

fn transform_tuple(transform: Affine, point: (f64, f64)) -> (f64, f64) {
    let [xx, yx, xy, yy, dx, dy] = transform.as_coeffs();
    let (x, y) = point;
    (xx * x + xy * y + dx, yx * x + yy * y + dy)
}

fn transform_point(transform: Affine, point: Axes<Scalar>) -> Axes<Scalar> {
    let [xx, yx, xy, yy, dx, dy] = transform.as_coeffs();
    let x = point.x.0 as f64;
    let y = point.y.0 as f64;
    Axes::new(
        Scalar((xx * x + xy * y + dx) as f32),
        Scalar((yx * x + yy * y + dy) as f32),
    )
}

fn linear_gradient_points(angle: f32) -> ((f64, f64), (f64, f64)) {
    let angle = angle.rem_euclid(std::f32::consts::TAU);
    let (sin, cos) = angle.sin_cos();
    let length = sin.abs() + cos.abs();

    match angle {
        angle if angle < std::f32::consts::FRAC_PI_2 => {
            ((0., 0.), ((cos * length) as f64, (sin * length) as f64))
        }
        angle if angle < std::f32::consts::PI => (
            (1., 0.),
            ((cos * length + 1.) as f64, (sin * length) as f64),
        ),
        angle if angle < 3. * std::f32::consts::FRAC_PI_2 => (
            (1., 1.),
            ((cos * length + 1.) as f64, (sin * length + 1.) as f64),
        ),
        _ => (
            (0., 1.),
            ((cos * length) as f64, (sin * length + 1.) as f64),
        ),
    }
}

fn color_space_tag(space: ColorSpace) -> peniko::color::ColorSpaceTag {
    match space {
        ColorSpace::Oklab => peniko::color::ColorSpaceTag::Oklab,
        ColorSpace::Oklch => peniko::color::ColorSpaceTag::Oklch,
        ColorSpace::Srgb => peniko::color::ColorSpaceTag::Srgb,
        ColorSpace::LinearRgb => peniko::color::ColorSpaceTag::LinearSrgb,
        ColorSpace::Hsl => peniko::color::ColorSpaceTag::Hsl,
        ColorSpace::Luma | ColorSpace::D65Gray | ColorSpace::Hsv | ColorSpace::Cmyk => {
            peniko::color::ColorSpaceTag::Srgb
        }
    }
}

/// Converts an SVG path to a [`kurbo::BezPath`].
fn svg_path(path_data: &str) -> Option<kurbo::BezPath> {
    let mut builder = GlyphPathBuilder::default();
    for segment in svgtypes::SimplifyingPathParser::from(path_data) {
        let segment = match segment {
            Ok(v) => v,
            Err(_) => break,
        };

        match segment {
            svgtypes::SimplePathSegment::MoveTo { x, y } => {
                builder.move_to(x as f32, y as f32);
            }
            svgtypes::SimplePathSegment::LineTo { x, y } => {
                builder.line_to(x as f32, y as f32);
            }
            svgtypes::SimplePathSegment::Quadratic { x1, y1, x, y } => {
                builder.quad_to(x1 as f32, y1 as f32, x as f32, y as f32);
            }
            svgtypes::SimplePathSegment::CurveTo {
                x1,
                y1,
                x2,
                y2,
                x,
                y,
            } => {
                builder.curve_to(
                    x1 as f32, y1 as f32, x2 as f32, y2 as f32, x as f32, y as f32,
                );
            }
            svgtypes::SimplePathSegment::ClosePath => {
                builder.close();
            }
        }
    }

    Some(builder.0)
}

#[derive(Default)]
pub struct GlyphPathBuilder(kurbo::BezPath);

impl GlyphPathBuilder {
    // Y axis is inverted.
    fn move_to(&mut self, x: f32, y: f32) {
        self.0.move_to((x as f64, y as f64));
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.0.line_to((x as f64, y as f64));
    }

    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.0.quad_to((x1 as f64, y1 as f64), (x as f64, y as f64));
    }

    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.0.curve_to(
            (x1 as f64, y1 as f64),
            (x2 as f64, y2 as f64),
            (x as f64, y as f64),
        );
    }

    fn close(&mut self) {
        self.0.close_path();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn svg_targets_scale_with_the_page() {
        let target = Axes::new(Scalar(100.0), Scalar(50.0));

        assert_eq!(
            target_texture_size(target, 2.0, SVG_IMAGE_SCALE),
            (300, 150)
        );
    }

    #[test]
    fn large_images_are_capped_without_distortion() {
        let (width, height) = clamp_texture_size(20_000.0, 1_000.0);

        assert!(width <= MAX_IMAGE_DIM && height <= MAX_IMAGE_DIM);
        assert!(width * height <= MAX_IMAGE_PIXELS);
        assert!((width / height - 20.0).abs() < 0.01);
    }

    #[test]
    fn raster_images_keep_their_native_size_below_the_budget() {
        let image = image::DynamicImage::ImageRgba8(image::RgbaImage::new(8, 4));

        let limited = limit_raster_image(image, false);

        assert_eq!((limited.width(), limited.height()), (8, 4));
    }

    #[test]
    fn raster_images_decode_at_native_resolution() {
        let image = image::RgbaImage::from_pixel(8, 6, image::Rgba([1, 2, 3, 255]));
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut png, image::ImageFormat::Png)
            .expect("encode the test image");

        let (_, width, height) =
            decode_raster_image("png", png.get_ref(), false).expect("decode the test image");

        assert_eq!((width, height), (8, 6));
    }

    #[test]
    fn splits_a_common_ligature_across_its_characters() {
        let chars: Vec<char> = "fieri.".chars().collect();
        let glyphs = [(10.0, 0), (6.0, 0), (6.0, 0), (6.0, 0), (3.0, 0)];

        assert_eq!(
            character_advances(&chars, &glyphs),
            Some(vec![5.0, 5.0, 6.0, 6.0, 6.0, 3.0])
        );
    }

    #[test]
    fn splits_three_character_ligatures() {
        let chars: Vec<char> = "office".chars().collect();
        let glyphs = [(5.0, 0), (18.0, 0), (5.0, 0), (5.0, 0)];

        assert_eq!(
            character_advances(&chars, &glyphs),
            Some(vec![5.0, 6.0, 6.0, 6.0, 5.0, 5.0])
        );
    }

    #[test]
    fn explicit_cluster_lengths_win_over_ligature_detection() {
        let chars: Vec<char> = "ffi".chars().collect();
        let glyphs = [(15.0, 3)];

        assert_eq!(
            character_advances(&chars, &glyphs),
            Some(vec![5.0, 5.0, 5.0])
        );
    }

    #[test]
    fn unknown_clusters_are_left_for_the_uniform_fallback() {
        let chars: Vec<char> = "aé".chars().collect();
        let glyphs = [(10.0, 0)];

        assert_eq!(character_advances(&chars, &glyphs), None);
    }

    fn sample_gradient(kind: GradientKind) -> ir::VecItem {
        ir::VecItem::Gradient(Arc::new(GradientItem {
            stops: vec![
                (
                    ir::Rgba8Item {
                        r: 255,
                        g: 0,
                        b: 0,
                        a: 255,
                    },
                    Scalar(0.),
                ),
                (
                    ir::Rgba8Item {
                        r: 0,
                        g: 0,
                        b: 255,
                        a: 255,
                    },
                    Scalar(1.),
                ),
            ],
            anti_alias: true,
            space: ColorSpace::Oklab,
            kind,
            styles: vec![],
        }))
    }

    fn sample_pattern(frame: Fingerprint) -> ir::VecItem {
        ir::VecItem::Pattern(Arc::new(PatternItem {
            frame,
            size: Axes::new(Scalar(4.), Scalar(5.)),
            spacing: Axes::new(Scalar(1.), Scalar(2.)),
            offset: Axes::new(Scalar(0.), Scalar(0.)),
        }))
    }

    #[test]
    fn resolves_solid_paint() {
        let module = Module::default();

        let paint = resolve_paint(&module, &"#f00".into());

        let PaintBrush::Brush { brush, transform } = paint else {
            panic!("expected brush paint");
        };

        assert!(transform.is_none());
        assert_eq!(
            brush,
            peniko::Brush::Solid(peniko::Color::from_rgb8(255, 0, 0))
        );
    }

    #[test]
    fn resolves_direct_linear_gradient_paint() {
        let mut module = Module::default();
        let gradient_id = Fingerprint::from_pair(1, 0);
        module.items.insert(
            gradient_id,
            sample_gradient(GradientKind::Linear(Scalar(0.))),
        );

        let paint = resolve_paint(&module, &format!("@{}", gradient_id.as_svg_id("g")).into());

        let PaintBrush::Brush { brush, transform } = paint else {
            panic!("expected brush paint");
        };

        assert!(transform.is_none());
        let peniko::Brush::Gradient(gradient) = brush else {
            panic!("expected gradient brush");
        };

        // The raw stops go to Vello unchanged (plus their mixing space tag);
        // Vello 0.7 resolves them in sRGB. Tinymist pre-samples non-sRGB
        // spaces, which needs Typst's color mixer -- not yet ported.
        assert_eq!(
            gradient.interpolation_cs,
            peniko::color::ColorSpaceTag::Oklab
        );
        assert_eq!(gradient.stops.len(), 2);
        assert_eq!(gradient.stops[0].offset, 0.);
        assert_eq!(gradient.stops[1].offset, 1.);
        assert!(matches!(gradient.kind, peniko::GradientKind::Linear(_)));
    }

    #[test]
    fn resolves_gradient_paint_with_color_transform() {
        let mut module = Module::default();
        let gradient_id = Fingerprint::from_pair(1, 0);
        let transform_id = Fingerprint::from_pair(2, 0);
        let transform = ir::Transform {
            sx: Scalar(2.),
            ky: Scalar(0.),
            kx: Scalar(0.),
            sy: Scalar(3.),
            tx: Scalar(4.),
            ty: Scalar(5.),
        };

        module.items.insert(
            gradient_id,
            sample_gradient(GradientKind::Radial(Scalar(0.75))),
        );
        module.items.insert(
            transform_id,
            ir::VecItem::ColorTransform(Arc::new(ir::ColorTransform {
                transform,
                item: gradient_id,
            })),
        );

        let paint = resolve_paint(&module, &format!("@{}", transform_id.as_svg_id("g")).into());

        let PaintBrush::Brush {
            brush,
            transform: paint_transform,
        } = paint
        else {
            panic!("expected brush paint");
        };

        assert_eq!(paint_transform, Some(convert_transform(&transform)));
        assert!(matches!(brush, peniko::Brush::Gradient(_)));
    }

    #[test]
    fn resolves_conic_gradient_with_typst_orientation() {
        let gradient = sample_gradient(GradientKind::Conic(Scalar(0.)));
        let ir::VecItem::Gradient(gradient) = gradient else {
            panic!("expected gradient item");
        };

        let converted = convert_gradient(&gradient, GradientGeometry::default())
            .expect("gradient should convert");
        assert_affine_approx_eq(
            converted.transform.expect("expected conic transform"),
            Affine::new([-1., 0., 0., -1., 1., 1.]),
        );

        let peniko_gradient = converted.gradient;
        let peniko::GradientKind::Sweep(sweep) = peniko_gradient.kind else {
            panic!("expected sweep gradient");
        };

        assert_eq!(sweep.start_angle, 0.);
        assert_eq!(sweep.end_angle, std::f32::consts::TAU);
    }

    #[test]
    fn resolves_conic_gradient_without_aspect_ratio_distortion() {
        let mut module = Module::default();
        let gradient_id = Fingerprint::from_pair(1, 0);
        let transform_id = Fingerprint::from_pair(2, 0);
        let transform = ir::Transform::from_scale(Scalar(70.), Scalar(25.));

        let mut gradient = sample_gradient(GradientKind::Conic(Scalar(135f32.to_radians())));
        let ir::VecItem::Gradient(gradient_item) = &mut gradient else {
            panic!("expected gradient item");
        };
        Arc::make_mut(gradient_item)
            .styles
            .push(GradientStyle::Center(Axes::new(Scalar(0.7), Scalar(0.3))));

        module.items.insert(gradient_id, gradient);
        module.items.insert(
            transform_id,
            ir::VecItem::ColorTransform(Arc::new(ir::ColorTransform {
                transform,
                item: gradient_id,
            })),
        );

        let paint = resolve_paint(&module, &format!("@{}", transform_id.as_svg_id("g")).into());

        let PaintBrush::Brush {
            brush,
            transform: paint_transform,
        } = paint
        else {
            panic!("expected brush paint");
        };

        assert_affine_approx_eq(
            paint_transform.expect("expected conic transform"),
            conic_gradient_transform(
                Axes::new(Scalar(49.), Scalar(7.5)),
                Scalar(135f32.to_radians()),
            ),
        );

        let peniko::Brush::Gradient(gradient) = brush else {
            panic!("expected gradient brush");
        };
        let peniko::GradientKind::Sweep(sweep) = gradient.kind else {
            panic!("expected sweep gradient");
        };

        assert!((sweep.center.x - 49.).abs() <= 1e-6);
        assert!((sweep.center.y - 7.5).abs() <= 1e-6);
    }

    #[test]
    fn resolves_translated_conic_gradient_without_aspect_ratio_distortion() {
        let mut module = Module::default();
        let gradient_id = Fingerprint::from_pair(1, 0);
        let transform_id = Fingerprint::from_pair(2, 0);
        let transform = ir::Transform {
            sx: Scalar(200.),
            ky: Scalar(0.),
            kx: Scalar(0.),
            sy: Scalar(80.),
            tx: Scalar(-10.),
            ty: Scalar(-20.),
        };

        let mut gradient = sample_gradient(GradientKind::Conic(Scalar(45f32.to_radians())));
        let ir::VecItem::Gradient(gradient_item) = &mut gradient else {
            panic!("expected gradient item");
        };
        Arc::make_mut(gradient_item)
            .styles
            .push(GradientStyle::Center(Axes::new(Scalar(0.5), Scalar(0.5))));

        module.items.insert(gradient_id, gradient);
        module.items.insert(
            transform_id,
            ir::VecItem::ColorTransform(Arc::new(ir::ColorTransform {
                transform,
                item: gradient_id,
            })),
        );

        let paint = resolve_paint(&module, &format!("@{}", transform_id.as_svg_id("g")).into());

        let PaintBrush::Brush {
            brush,
            transform: paint_transform,
        } = paint
        else {
            panic!("expected brush paint");
        };

        assert_affine_approx_eq(
            paint_transform.expect("expected conic transform"),
            conic_gradient_transform(
                Axes::new(Scalar(90.), Scalar(20.)),
                Scalar(45f32.to_radians()),
            ),
        );

        let peniko::Brush::Gradient(gradient) = brush else {
            panic!("expected gradient brush");
        };
        let peniko::GradientKind::Sweep(sweep) = gradient.kind else {
            panic!("expected sweep gradient");
        };

        assert!((sweep.center.x - 90.).abs() <= 1e-6);
        assert!((sweep.center.y - 20.).abs() <= 1e-6);
    }

    fn assert_affine_approx_eq(actual: Affine, expected: Affine) {
        for (actual, expected) in actual.as_coeffs().into_iter().zip(expected.as_coeffs()) {
            assert!(
                (actual - expected).abs() <= 1e-6,
                "expected affine coefficient {expected}, got {actual}"
            );
        }
    }

    #[test]
    fn resolves_direct_pattern_paint() {
        let mut module = Module::default();
        let frame_id = Fingerprint::from_pair(1, 0);
        let pattern_id = Fingerprint::from_pair(2, 0);
        module.items.insert(pattern_id, sample_pattern(frame_id));

        let paint = resolve_paint(&module, &format!("@{}", pattern_id.as_svg_id("p")).into());

        let PaintBrush::Pattern(pattern) = paint else {
            panic!("expected pattern paint");
        };

        assert_eq!(pattern.pattern.frame, frame_id);
        assert_eq!(pattern.pattern.size, Axes::new(Scalar(4.), Scalar(5.)));
        assert_eq!(pattern.pattern.spacing, Axes::new(Scalar(1.), Scalar(2.)));
        assert_eq!(pattern.transform, Affine::IDENTITY);
    }

    #[test]
    fn resolves_pattern_paint_with_color_transform() {
        let mut module = Module::default();
        let frame_id = Fingerprint::from_pair(1, 0);
        let pattern_id = Fingerprint::from_pair(2, 0);
        let transform_id = Fingerprint::from_pair(3, 0);
        let transform = ir::Transform {
            sx: Scalar(2.),
            ky: Scalar(0.),
            kx: Scalar(0.),
            sy: Scalar(3.),
            tx: Scalar(4.),
            ty: Scalar(5.),
        };

        module.items.insert(pattern_id, sample_pattern(frame_id));
        module.items.insert(
            transform_id,
            ir::VecItem::ColorTransform(Arc::new(ir::ColorTransform {
                transform,
                item: pattern_id,
            })),
        );

        let paint = resolve_paint(&module, &format!("@{}", transform_id.as_svg_id("p")).into());

        let PaintBrush::Pattern(pattern) = paint else {
            panic!("expected pattern paint");
        };

        assert_eq!(pattern.pattern.frame, frame_id);
        assert_eq!(pattern.transform, convert_transform(&transform));
    }

    #[test]
    fn keeps_unknown_reference_as_black_fallback() {
        let module = Module::default();

        let paint = resolve_paint(&module, &"@pAQAAAAAAAAA".into());

        let PaintBrush::Brush { brush, transform } = paint else {
            panic!("expected brush paint");
        };

        assert!(transform.is_none());
        assert_eq!(brush, peniko::Brush::Solid(peniko::Color::BLACK));
    }
}
