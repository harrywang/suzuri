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
//! 2. The returned [`RenderedMath`] carries `height_em`/`depth_em` alongside
//!    the SVG. Inline math has to sit on the surrounding text's baseline, and
//!    an SVG alone cannot say where within its box that baseline falls.
//!    Callers offset by `depth_em` to align it.
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
    pub text_color: Hsla,
    /// Em size in SVG user units. The rasterized image is scaled to the
    /// editor's font size, so this only sets the resolution of the outlines.
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
        svg,
        height_em: display_list.height as f32,
        depth_em: display_list.depth as f32,
    })
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
