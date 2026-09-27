//! Crate for rendering LaTeX math strings to SVG strings.
//!
//! The entrypoint to this crate is [`render_to_svg`]. It mirrors the shape of
//! [`mermaid_render`](../../mermaid_render), the fork's other "embedded
//! technical content" renderer, so both feed the same `usvg`/`resvg`
//! rasterization path in `gpui`.
//!
//! Rendering goes through [RaTeX](https://github.com/erweixin/RaTeX), a
//! KaTeX-compatible math engine written in Rust: `ratex-parser` parses the
//! LaTeX, `ratex-layout` lays it out into a display list, and `ratex-svg`
//! exports that list to SVG.
//!
//! Two properties of the emitted SVG matter to callers:
//!
//! 1. Glyphs are emitted as `<path>` outlines rather than `<text>` elements
//!    (via [`ratex_svg::SvgOptions::embed_glyphs`] plus the `embed-fonts`
//!    feature). `usvg` therefore needs no font database to rasterize the
//!    result, and output does not shift with the user's installed fonts.
//! 2. The returned [`RenderedMath`] carries `width_em`, `height_em` and
//!    `depth_em` alongside the SVG, so a caller can size and place the element
//!    without rasterizing first. Inline math has to sit on the surrounding
//!    text's baseline, and an SVG alone cannot say where within its box that
//!    baseline falls; callers offset by `depth_em` to align it.
//! 3. Glyph outlines are stroked as well as filled, by [`GLYPH_WEIGHT_EM`], to
//!    make up the weight a coverage-only rasterizer loses against a browser's
//!    gamma-corrected text blending.
//!
//! Keeping the engine behind this crate boundary is deliberate: `math_render`
//! is the only place that names RaTeX, so swapping engines does not reach into
//! the editor.

use anyhow::{Context as _, Result};
use gpui::{Hsla, Rgba};
use ratex_layout::{LayoutOptions, layout, to_display_list};
use ratex_svg::{SvgOptions, render_to_svg as ratex_render_to_svg};
use ratex_types::math_style::MathStyle as RatexMathStyle;

/// Whether a formula was written inline in a paragraph or as its own block.
///
/// This selects TeX's math style, which is not merely cosmetic: display style
/// sets limits above and below large operators and uses full-size fractions,
/// while text style tucks limits beside the operator and shrinks fractions so
/// the formula fits within a line of prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum MathStyle {
    /// `$...$` — sized to sit within a line of prose.
    #[default]
    Inline,
    /// `$$...$$` — sized to stand alone on its own lines.
    Display,
}

impl From<MathStyle> for RatexMathStyle {
    fn from(style: MathStyle) -> Self {
        match style {
            MathStyle::Inline => RatexMathStyle::Text,
            MathStyle::Display => RatexMathStyle::Display,
        }
    }
}

/// Theme and sizing inputs for a single formula.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MathTheme {
    /// Color of the rendered glyphs, normally the editor's foreground color.
    ///
    /// A caller that paints the result through an alpha mask (as `gpui`'s
    /// `svg()` element does) never sees this, because the mask keeps only
    /// coverage.
    pub text_color: Hsla,
    /// Em size in SVG user units — the coordinate scale the document is laid
    /// out in. The outlines are vectors, so the value does not bound the
    /// quality of any later rasterization; it only sets the precision of the
    /// emitted coordinates, and the scale [`GLYPH_WEIGHT_EM`] is measured in.
    pub font_size: f32,
}

impl Default for MathTheme {
    fn default() -> Self {
        Self {
            text_color: gpui::black(),
            font_size: 40.0,
        }
    }
}

/// A rendered formula, plus the metrics needed to place it.
#[derive(Debug, Clone, PartialEq)]
pub struct RenderedMath {
    /// A standalone SVG document with glyphs as `<path>` outlines.
    pub svg: String,
    /// Width of the formula, in em. Callers size the element with this rather
    /// than rasterizing first to read the image's aspect ratio.
    pub width_em: f32,
    /// Distance from the baseline to the top of the formula, in em.
    pub height_em: f32,
    /// Distance from the baseline to the bottom of the formula, in em.
    ///
    /// Zero for a formula that sits entirely on the baseline (`E = mc^2`);
    /// positive when something descends below it (`\frac{a}{b}`).
    pub depth_em: f32,
}

impl RenderedMath {
    /// The fraction of the image's total height that sits below the baseline.
    ///
    /// Callers position inline math by shifting it down by this fraction of
    /// the image height, which puts the formula's baseline on the text's.
    pub fn baseline_fraction(&self) -> f32 {
        let total = self.height_em + self.depth_em;
        if total > 0.0 {
            self.depth_em / total
        } else {
            0.0
        }
    }
}

/// Renders a LaTeX math string to SVG.
///
/// `source` is the formula body *without* its `$` delimiters. Errors are
/// returned rather than panicking, because callers re-render on every
/// keystroke and a half-typed formula is the common case, not the exception.
pub fn render_to_svg(source: &str, style: MathStyle, theme: &MathTheme) -> Result<RenderedMath> {
    render_weighted(source, style, theme, GLYPH_WEIGHT_EM)
}

/// [`render_to_svg`] with the outline thickening left open, so a test can
/// measure what the compensation is worth against the raw engine output.
fn render_weighted(
    source: &str,
    style: MathStyle,
    theme: &MathTheme,
    weight_em: f64,
) -> Result<RenderedMath> {
    let nodes = ratex_parser::parse(source)
        .map_err(|error| anyhow::anyhow!("{}", error.message))
        .context("parsing LaTeX math")?;

    let options = LayoutOptions {
        style: style.into(),
        color: hsla_to_ratex_color(theme.text_color),
        ..Default::default()
    };

    let display_list = to_display_list(&layout(&nodes, &options));

    let svg = ratex_render_to_svg(
        &display_list,
        &SvgOptions {
            font_size: theme.font_size as f64,
            // Padding would be baked into the raster and offset the baseline;
            // callers add spacing with layout instead.
            padding: 0.0,
            embed_glyphs: true,
            ..Default::default()
        },
    );

    Ok(RenderedMath {
        svg: thicken_outlines(&svg, weight_em * theme.font_size as f64),
        width_em: display_list.width as f32,
        height_em: display_list.height as f32,
        depth_em: display_list.depth as f32,
    })
}

/// Extra outline width, as a fraction of an em, added to every glyph.
///
/// `resvg` antialiases by coverage alone, while a browser's text rasterizer
/// blends in a gamma-corrected space that darkens partial coverage. Rendering
/// the same formula both ways at the same size and measuring ink per unit
/// area puts KaTeX-in-Chrome 19% heavier than the plain coverage raster, which
/// is why formulas read as washed out beside the prose around them. Stroking
/// each outline by this much restores the missing weight (measured at +20%);
/// `outline_thickening_restores_browser_glyph_weight` pins the ratio.
const GLYPH_WEIGHT_EM: f64 = 0.012;

/// Strokes every filled glyph outline in its own fill colour, widening it by
/// `stroke_width` user units.
///
/// Each outline carries its colour in its own `fill`, so the stroke is taken
/// from the element rather than re-derived from the theme — the two must match
/// exactly or the stroke shows up as a halo.
fn thicken_outlines(svg: &str, stroke_width: f64) -> String {
    if stroke_width <= 0.0 {
        return svg.to_string();
    }
    const STROKE_NONE: &str = r#"stroke="none""#;
    let mut out = String::with_capacity(svg.len() + svg.len() / 4);
    let mut rest = svg;
    while let Some(element_start) = rest.find("<path ") {
        let after_tag = element_start + "<path ".len();
        let element_end = rest[after_tag..]
            .find('>')
            .map_or(rest.len(), |offset| after_tag + offset);
        let element = &rest[after_tag..element_end];

        match fill_paint(element).filter(|_| element.contains(STROKE_NONE)) {
            Some(paint) => {
                out.push_str(&rest[..after_tag]);
                out.push_str(&element.replace(
                    STROKE_NONE,
                    &format!(
                        r#"stroke="{paint}" stroke-width="{stroke_width}" stroke-linejoin="round""#
                    ),
                ));
            }
            None => out.push_str(&rest[..element_end]),
        }
        rest = &rest[element_end..];
    }
    out.push_str(rest);
    out
}

/// The value of an element's `fill` attribute, unless it paints nothing.
fn fill_paint(element: &str) -> Option<&str> {
    let after = element.split_once(r#"fill=""#)?.1;
    let paint = after.split_once('"')?.0;
    (paint != "none").then_some(paint)
}

fn hsla_to_ratex_color(color: Hsla) -> ratex_types::color::Color {
    let Rgba { r, g, b, a } = Rgba::from(color);
    ratex_types::color::Color { r, g, b, a }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rasterizer is given no font database, so any `<text>` element would
    /// silently render as nothing. Everything must be outlines.
    #[test]
    fn glyphs_are_outlines_not_text_elements() {
        for source in [
            r"E = mc^2",
            r"\int_0^\infty e^{-x^2}\,dx",
            r"\begin{pmatrix} a & b \\ c & d \end{pmatrix}",
            r"\sqrt{\frac{1}{1 + \sqrt{x}}}",
        ] {
            let rendered = render_to_svg(source, MathStyle::Display, &MathTheme::default())
                .expect("formula should render");
            assert!(
                !rendered.svg.contains("<text"),
                "{source:?} emitted a <text> element, which needs font resolution"
            );
            assert!(
                rendered.svg.contains("<path"),
                "{source:?} emitted no glyph outlines"
            );
        }
    }

    /// Pins the property the whole pipeline depends on: `usvg` can parse the
    /// output with a default (empty) font database.
    #[test]
    fn output_parses_with_usvg_and_no_fontdb() {
        let rendered = render_to_svg(
            r"\sum_{i=1}^{n} i = \frac{n(n+1)}{2}",
            MathStyle::Display,
            &MathTheme::default(),
        )
        .expect("formula should render");

        let tree = usvg::Tree::from_str(&rendered.svg, &usvg::Options::default())
            .expect("usvg should parse the SVG without a font database");
        assert!(tree.size().width() > 0.0 && tree.size().height() > 0.0);
    }

    /// Rasterizes at `scale` and returns mean coverage per pixel — the "ink"
    /// measure that distinguishes heavier strokes from a bigger formula.
    fn ink_density(svg: &str, scale: f32) -> f32 {
        let tree = usvg::Tree::from_str(svg, &usvg::Options::default())
            .expect("usvg should parse the SVG");
        let size = tree.size();
        let (width, height) = (
            (size.width() * scale).ceil() as u32,
            (size.height() * scale).ceil() as u32,
        );
        let mut pixmap = resvg::tiny_skia::Pixmap::new(width, height).expect("non-empty pixmap");
        resvg::render(
            &tree,
            resvg::tiny_skia::Transform::from_scale(
                width as f32 / size.width(),
                height as f32 / size.height(),
            ),
            &mut pixmap.as_mut(),
        );
        let ink: f32 = pixmap
            .pixels()
            .iter()
            .map(|pixel| pixel.alpha() as f32 / 255.0)
            .sum();
        ink / (width * height) as f32
    }

    /// The reason [`GLYPH_WEIGHT_EM`] exists. Measured against KaTeX rendered
    /// in Chrome at the same size, a plain coverage raster is 19% light; this
    /// pins the compensation to that neighbourhood, so a future engine change
    /// that already emits heavier outlines shows up as a failure here rather
    /// than as visibly bold math.
    #[test]
    fn outline_thickening_restores_browser_glyph_weight() {
        let source = r"E(S) = \sum_{i=1}^{c} -p_i \log_2 p_i";
        let theme = MathTheme::default();
        let thickened = render_weighted(source, MathStyle::Display, &theme, GLYPH_WEIGHT_EM)
            .expect("formula should render")
            .svg;
        let plain = render_weighted(source, MathStyle::Display, &theme, 0.0)
            .expect("formula should render")
            .svg;
        assert!(
            plain.contains(r#"stroke="none""#) && !thickened.contains(r#"stroke="none""#),
            "thickening should replace the engine's own `stroke=\"none\"`"
        );

        let ratio = ink_density(&thickened, 1.0) / ink_density(&plain, 1.0);
        assert!(
            (1.1..=1.3).contains(&ratio),
            "thickening changed ink by {ratio:.3}x, expected roughly 1.2x"
        );
    }

    /// The element is sized from `width_em` without rasterizing, so it has to
    /// agree with the SVG the same call returns, or formulas get letterboxed.
    #[test]
    fn width_em_matches_the_svg_aspect_ratio() {
        for source in [
            r"E = mc^2",
            r"\sum_{i=1}^{n} i = \frac{n(n+1)}{2}",
            r"\begin{pmatrix} a & b \\ c & d \end{pmatrix}",
        ] {
            let rendered = render_to_svg(source, MathStyle::Display, &MathTheme::default())
                .expect("formula should render");
            let tree = usvg::Tree::from_str(&rendered.svg, &usvg::Options::default())
                .expect("usvg should parse the SVG");
            let svg_aspect = tree.size().width() / tree.size().height();
            let metric_aspect = rendered.width_em / (rendered.height_em + rendered.depth_em);
            assert!(
                (svg_aspect - metric_aspect).abs() / svg_aspect < 0.02,
                "{source:?}: metrics say {metric_aspect:.4}, SVG says {svg_aspect:.4}"
            );
        }
    }

    #[test]
    fn depth_distinguishes_descending_formulas() {
        let flat = render_to_svg("E = mc^2", MathStyle::Inline, &MathTheme::default())
            .expect("formula should render");
        let descending = render_to_svg(r"\frac{a}{b}", MathStyle::Inline, &MathTheme::default())
            .expect("formula should render");

        assert_eq!(flat.depth_em, 0.0);
        assert!(descending.depth_em > 0.0);
        assert!(descending.baseline_fraction() > flat.baseline_fraction());
    }

    /// Inline style must be visibly more compact than display style, otherwise
    /// formulas in prose blow up the line height.
    #[test]
    fn inline_style_is_more_compact_than_display() {
        let source = r"\sum_{i=1}^{n} i";
        let inline = render_to_svg(source, MathStyle::Inline, &MathTheme::default())
            .expect("formula should render");
        let display = render_to_svg(source, MathStyle::Display, &MathTheme::default())
            .expect("formula should render");

        let inline_total = inline.height_em + inline.depth_em;
        let display_total = display.height_em + display.depth_em;
        assert!(
            inline_total < display_total,
            "inline {inline_total} should be shorter than display {display_total}"
        );
    }

    /// Live preview re-renders on every keystroke, so partial input arrives
    /// constantly. It must produce an error, never a panic.
    #[test]
    fn malformed_input_errors_without_panicking() {
        for source in [
            r"\frac{a}{",
            r"\begin{pmatrix} a",
            r"\nosuchcommand{x}",
            "",
            "^",
        ] {
            let result = render_to_svg(source, MathStyle::Inline, &MathTheme::default());
            // Either outcome is fine; not unwinding is the point.
            drop(result);
        }
    }
}
