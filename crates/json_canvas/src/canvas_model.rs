//! The JSON Canvas 1.0 format (<https://jsoncanvas.org/spec/1.0/>) and the
//! geometry the viewer needs from it, kept free of GPUI views so it can be
//! tested directly.

use gpui::{Bounds, Point, Size, point, size};
use serde::Deserialize;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Canvas {
    /// In file order, which the spec defines as ascending z-index.
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub id: String,
    pub bounds: Bounds<f32>,
    pub color: Option<CanvasColor>,
    pub kind: NodeKind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum NodeKind {
    Text {
        text: String,
    },
    File {
        /// Relative to the vault root.
        path: String,
        subpath: Option<String>,
    },
    Link {
        url: String,
    },
    Group {
        label: Option<String>,
    },
    /// A node type this viewer does not know, drawn as an empty card so the
    /// edges that reach it still have somewhere to land.
    Unknown {
        kind: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CanvasColor {
    /// `"1"` to `"6"`: red, orange, yellow, green, cyan, purple. Applications
    /// choose the exact shades.
    Preset(u8),
    Rgb(u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Top,
    Right,
    Bottom,
    Left,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Edge {
    pub id: String,
    pub from_node: String,
    pub from_side: Option<Side>,
    pub from_arrow: bool,
    pub to_node: String,
    pub to_side: Option<Side>,
    pub to_arrow: bool,
    pub color: Option<CanvasColor>,
    pub label: Option<String>,
}

#[derive(Deserialize)]
struct RawCanvas {
    #[serde(default)]
    nodes: Vec<serde_json::Value>,
    #[serde(default)]
    edges: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawNode {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    color: Option<String>,
    text: Option<String>,
    file: Option<String>,
    subpath: Option<String>,
    url: Option<String>,
    label: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawEdge {
    id: String,
    from_node: String,
    from_side: Option<String>,
    from_end: Option<String>,
    to_node: String,
    to_side: Option<String>,
    to_end: Option<String>,
    color: Option<String>,
    label: Option<String>,
}

/// Parses a canvas, skipping any node or edge that does not fit the spec
/// rather than rejecting the file: a canvas written by another tool, or by an
/// agent, should show everything that can be shown.
pub fn parse_canvas(source: &str) -> anyhow::Result<Canvas> {
    // An empty file is what a new canvas looks like before anything is
    // written into it.
    if source.trim().is_empty() {
        return Ok(Canvas::default());
    }
    let raw: RawCanvas = serde_json::from_str(source)?;

    let nodes: Vec<Node> = raw
        .nodes
        .into_iter()
        .filter_map(|value| match serde_json::from_value::<RawNode>(value) {
            Ok(node) => Some(node_from_raw(node)),
            Err(error) => {
                log::warn!("json_canvas: skipping a node: {error}");
                None
            }
        })
        .collect();

    let edges = raw
        .edges
        .into_iter()
        .filter_map(|value| match serde_json::from_value::<RawEdge>(value) {
            Ok(edge) => Some(edge_from_raw(edge)),
            Err(error) => {
                log::warn!("json_canvas: skipping an edge: {error}");
                None
            }
        })
        .filter(|edge| {
            let connects = |id: &str| nodes.iter().any(|node| node.id == id);
            let keep = connects(&edge.from_node) && connects(&edge.to_node);
            if !keep {
                log::warn!(
                    "json_canvas: skipping edge {:?}, it names a node that does not exist",
                    edge.id
                );
            }
            keep
        })
        .collect();

    Ok(Canvas { nodes, edges })
}

fn node_from_raw(raw: RawNode) -> Node {
    let kind = match raw.kind.as_str() {
        "text" => NodeKind::Text {
            text: raw.text.unwrap_or_default(),
        },
        "file" => NodeKind::File {
            path: raw.file.unwrap_or_default(),
            subpath: raw.subpath,
        },
        "link" => NodeKind::Link {
            url: raw.url.unwrap_or_default(),
        },
        "group" => NodeKind::Group { label: raw.label },
        _ => NodeKind::Unknown { kind: raw.kind },
    };
    Node {
        id: raw.id,
        bounds: Bounds::new(
            point(raw.x as f32, raw.y as f32),
            size(raw.width.max(0.0) as f32, raw.height.max(0.0) as f32),
        ),
        color: raw.color.as_deref().and_then(parse_color),
        kind,
    }
}

fn edge_from_raw(raw: RawEdge) -> Edge {
    Edge {
        id: raw.id,
        from_node: raw.from_node,
        from_side: raw.from_side.as_deref().and_then(parse_side),
        from_arrow: raw.from_end.as_deref() == Some("arrow"),
        to_node: raw.to_node,
        to_side: raw.to_side.as_deref().and_then(parse_side),
        // The spec's default for the destination end is an arrow.
        to_arrow: raw.to_end.as_deref() != Some("none"),
        color: raw.color.as_deref().and_then(parse_color),
        label: raw.label.filter(|label| !label.is_empty()),
    }
}

pub fn parse_color(color: &str) -> Option<CanvasColor> {
    if let Some(hex) = color.strip_prefix('#') {
        if hex.len() == 6 {
            return u32::from_str_radix(hex, 16).ok().map(CanvasColor::Rgb);
        }
        return None;
    }
    match color.parse::<u8>() {
        Ok(preset @ 1..=6) => Some(CanvasColor::Preset(preset)),
        _ => None,
    }
}

fn parse_side(side: &str) -> Option<Side> {
    match side {
        "top" => Some(Side::Top),
        "right" => Some(Side::Right),
        "bottom" => Some(Side::Bottom),
        "left" => Some(Side::Left),
        _ => None,
    }
}

impl Canvas {
    pub fn node(&self, id: &str) -> Option<&Node> {
        self.nodes.iter().find(|node| node.id == id)
    }

    /// The smallest box around every node, or `None` for an empty canvas.
    pub fn content_bounds(&self) -> Option<Bounds<f32>> {
        self.nodes
            .iter()
            .map(|node| node.bounds)
            .reduce(|a, b| a.union(&b))
    }

    /// The topmost card under a canvas position. Groups are skipped: they
    /// are backgrounds, and a click inside one is meant for whatever it
    /// holds, or for nothing.
    pub fn card_at(&self, position: Point<f32>) -> Option<&Node> {
        self.nodes
            .iter()
            .rev()
            .filter(|node| !matches!(node.kind, NodeKind::Group { .. }))
            .find(|node| node.bounds.contains(&position))
    }
}

/// The zoom and offset that fit `content` inside `viewport` with `margin` on
/// every side, centered. The offset is where canvas `(0, 0)` lands in the
/// viewport. A fit never zooms in past 100%, so a small canvas opens at its
/// natural size instead of blown up.
pub fn fit(content: Bounds<f32>, viewport: Size<f32>, margin: f32) -> (f32, Point<f32>) {
    let available = size(
        (viewport.width - 2.0 * margin).max(1.0),
        (viewport.height - 2.0 * margin).max(1.0),
    );
    let zoom = (available.width / content.size.width.max(1.0))
        .min(available.height / content.size.height.max(1.0))
        .clamp(MIN_ZOOM, 1.0);
    let content_center = content.center();
    let offset = point(
        viewport.width / 2.0 - content_center.x * zoom,
        viewport.height / 2.0 - content_center.y * zoom,
    );
    (zoom, offset)
}

pub const MIN_ZOOM: f32 = 0.1;
pub const MAX_ZOOM: f32 = 4.0;

/// Changes the zoom while keeping the canvas point under `anchor` (a
/// viewport position) where it is, as pinching and ctrl-scrolling expect.
pub fn zoom_about(
    zoom: f32,
    offset: Point<f32>,
    new_zoom: f32,
    anchor: Point<f32>,
) -> (f32, Point<f32>) {
    let new_zoom = new_zoom.clamp(MIN_ZOOM, MAX_ZOOM);
    let canvas_anchor = point((anchor.x - offset.x) / zoom, (anchor.y - offset.y) / zoom);
    let new_offset = point(
        anchor.x - canvas_anchor.x * new_zoom,
        anchor.y - canvas_anchor.y * new_zoom,
    );
    (new_zoom, new_offset)
}

/// Where an edge leaves or enters a node. Without a side in the file, the
/// edge uses the side that faces the other node.
pub fn anchor(bounds: Bounds<f32>, side: Option<Side>, toward: Point<f32>) -> (Point<f32>, Side) {
    let center = bounds.center();
    let side = side.unwrap_or_else(|| {
        let dx = toward.x - center.x;
        let dy = toward.y - center.y;
        if dx.abs() >= dy.abs() {
            if dx >= 0.0 { Side::Right } else { Side::Left }
        } else if dy >= 0.0 {
            Side::Bottom
        } else {
            Side::Top
        }
    });
    let position = match side {
        Side::Top => point(center.x, bounds.top()),
        Side::Bottom => point(center.x, bounds.bottom()),
        Side::Left => point(bounds.left(), center.y),
        Side::Right => point(bounds.right(), center.y),
    };
    (position, side)
}

pub fn side_direction(side: Side) -> Point<f32> {
    match side {
        Side::Top => point(0.0, -1.0),
        Side::Bottom => point(0.0, 1.0),
        Side::Left => point(-1.0, 0.0),
        Side::Right => point(1.0, 0.0),
    }
}

/// An edge as a cubic Bézier in canvas coordinates: each end leaves its
/// node straight out of the side it is attached to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EdgeCurve {
    pub start: Point<f32>,
    pub control_start: Point<f32>,
    pub control_end: Point<f32>,
    pub end: Point<f32>,
    pub start_side: Side,
    pub end_side: Side,
}

impl EdgeCurve {
    pub fn between(from: &Node, from_side: Option<Side>, to: &Node, to_side: Option<Side>) -> Self {
        let (start, start_side) = anchor(from.bounds, from_side, to.bounds.center());
        let (end, end_side) = anchor(to.bounds, to_side, from.bounds.center());
        let distance = ((end.x - start.x).powi(2) + (end.y - start.y).powi(2)).sqrt();
        let reach = (distance / 2.0).clamp(30.0, 150.0);
        let push = |at: Point<f32>, side: Side| {
            let direction = side_direction(side);
            point(at.x + direction.x * reach, at.y + direction.y * reach)
        };
        Self {
            start,
            control_start: push(start, start_side),
            control_end: push(end, end_side),
            end,
            start_side,
            end_side,
        }
    }

    pub fn midpoint(&self) -> Point<f32> {
        point(
            (self.start.x + 3.0 * self.control_start.x + 3.0 * self.control_end.x + self.end.x)
                / 8.0,
            (self.start.y + 3.0 * self.control_start.y + 3.0 * self.control_end.y + self.end.y)
                / 8.0,
        )
    }
}

/// The part of a note a file node shows: frontmatter dropped, and narrowed
/// to one section when the node names a `#heading` subpath.
pub fn note_excerpt(source: &str, subpath: Option<&str>) -> String {
    let body = strip_frontmatter(source);
    let Some(heading) = subpath
        .and_then(|subpath| subpath.strip_prefix('#'))
        .filter(|heading| !heading.starts_with('^'))
    else {
        return body.to_string();
    };

    let heading_level = |line: &str| {
        let level = line
            .chars()
            .take_while(|character| *character == '#')
            .count();
        (level > 0 && line[level..].starts_with(' ')).then_some(level)
    };
    let mut lines = body.lines();
    let mut section = Vec::new();
    let mut section_level = None;
    for line in lines.by_ref() {
        if let Some(level) = heading_level(line)
            && line[level..].trim() == heading.trim()
        {
            section_level = Some(level);
            section.push(line);
            break;
        }
    }
    let Some(section_level) = section_level else {
        return body.to_string();
    };
    for line in lines {
        if heading_level(line).is_some_and(|level| level <= section_level) {
            break;
        }
        section.push(line);
    }
    section.join("\n")
}

fn strip_frontmatter(source: &str) -> &str {
    let Some(rest) = source
        .strip_prefix("---\n")
        .or_else(|| source.strip_prefix("---\r\n"))
    else {
        return source;
    };
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        offset += line.len();
        if line.trim_end() == "---" {
            return rest[offset..].trim_start_matches(['\r', '\n']);
        }
    }
    source
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_node_type_and_edge_defaults() {
        let canvas = parse_canvas(
            r##"{
              "nodes": [
                {"id":"g","type":"group","x":-10,"y":-10,"width":500,"height":300,"label":"Papers","color":"4"},
                {"id":"t","type":"text","x":0,"y":0,"width":200,"height":100,"text":"**Claim**"},
                {"id":"f","type":"file","x":250,"y":0,"width":200,"height":100,"file":"papers/A.md","subpath":"#Method","color":"#ff8800"},
                {"id":"l","type":"link","x":0,"y":150,"width":200,"height":100,"url":"https://jsoncanvas.org"}
              ],
              "edges": [
                {"id":"e1","fromNode":"t","toNode":"f","fromSide":"right","toSide":"left","label":"supports"},
                {"id":"e2","fromNode":"f","toNode":"l","fromEnd":"arrow","toEnd":"none","color":"1"}
              ]
            }"##,
        )
        .expect("a valid canvas");

        let kinds: Vec<_> = canvas.nodes.iter().map(|node| &node.kind).collect();
        assert_eq!(
            kinds,
            [
                &NodeKind::Group {
                    label: Some("Papers".into())
                },
                &NodeKind::Text {
                    text: "**Claim**".into()
                },
                &NodeKind::File {
                    path: "papers/A.md".into(),
                    subpath: Some("#Method".into())
                },
                &NodeKind::Link {
                    url: "https://jsoncanvas.org".into()
                },
            ]
        );
        assert_eq!(canvas.nodes[0].color, Some(CanvasColor::Preset(4)));
        assert_eq!(canvas.nodes[2].color, Some(CanvasColor::Rgb(0xff8800)));

        let [first, second] = canvas.edges.as_slice() else {
            panic!("expected two edges, got {:?}", canvas.edges);
        };
        assert_eq!(
            (
                first.from_arrow,
                first.to_arrow,
                first.from_side,
                first.to_side
            ),
            (false, true, Some(Side::Right), Some(Side::Left))
        );
        assert_eq!(first.label.as_deref(), Some("supports"));
        assert_eq!((second.from_arrow, second.to_arrow), (true, false));
        assert_eq!(second.color, Some(CanvasColor::Preset(1)));
    }

    /// A canvas from another tool or an agent can carry things this viewer
    /// does not know. Each bad piece is dropped on its own; the rest shows.
    #[test]
    fn parsing_tolerates_what_it_does_not_understand() {
        let canvas = parse_canvas(
            r#"{
              "nodes": [
                {"id":"a","type":"text","x":0,"y":0,"width":10,"height":10,"text":"kept","extra":{"by":"plugin"}},
                {"id":"b","type":"sticker","x":20,"y":0,"width":10,"height":10},
                {"id":"c","type":"text","text":"no position"},
                {"id":"d","type":"text","x":40,"y":0,"width":10,"height":10,"color":"9"}
              ],
              "edges": [
                {"id":"ok","fromNode":"a","toNode":"b"},
                {"id":"dangling","fromNode":"a","toNode":"c"},
                {"id":"broken","fromNode":"a"}
              ],
              "metadata": {"version": "1.0"}
            }"#,
        )
        .expect("the file as a whole still parses");

        let ids: Vec<_> = canvas.nodes.iter().map(|node| node.id.as_str()).collect();
        assert_eq!(ids, ["a", "b", "d"]);
        assert_eq!(
            canvas.nodes[1].kind,
            NodeKind::Unknown {
                kind: "sticker".into()
            }
        );
        assert_eq!(canvas.nodes[2].color, None, "\"9\" is not a preset");
        let edges: Vec<_> = canvas.edges.iter().map(|edge| edge.id.as_str()).collect();
        assert_eq!(edges, ["ok"]);
    }

    #[test]
    fn an_empty_file_is_an_empty_canvas_and_garbage_is_an_error() {
        assert_eq!(parse_canvas("  \n").expect("empty"), Canvas::default());
        assert_eq!(parse_canvas("{}").expect("no keys"), Canvas::default());
        assert!(parse_canvas("{\"nodes\": [").is_err());
    }

    #[test]
    fn fitting_centers_the_content_and_never_zooms_past_actual_size() {
        let content = Bounds::new(point(-100.0, -50.0), size(2000.0, 1000.0));
        let (zoom, offset) = fit(content, size(1040.0, 1040.0), 20.0);
        assert_eq!(zoom, 0.5);
        // Content center (900, 450) lands on the viewport center (520, 520).
        assert_eq!(offset, point(520.0 - 450.0, 520.0 - 225.0));

        let small = Bounds::new(point(0.0, 0.0), size(100.0, 100.0));
        let (zoom, offset) = fit(small, size(1000.0, 800.0), 20.0);
        assert_eq!(zoom, 1.0);
        assert_eq!(offset, point(450.0, 350.0));
    }

    #[test]
    fn zooming_keeps_the_point_under_the_pointer_still() {
        let (zoom, offset) = (0.5, point(100.0, 40.0));
        let anchor = point(300.0, 200.0);
        let canvas_point = point((anchor.x - offset.x) / zoom, (anchor.y - offset.y) / zoom);
        let (new_zoom, new_offset) = zoom_about(zoom, offset, 2.0, anchor);
        assert_eq!(new_zoom, 2.0);
        assert_eq!(
            point(
                canvas_point.x * new_zoom + new_offset.x,
                canvas_point.y * new_zoom + new_offset.y
            ),
            anchor
        );
        assert_eq!(zoom_about(zoom, offset, 100.0, anchor).0, MAX_ZOOM);
    }

    #[test]
    fn edges_without_sides_use_the_facing_sides() {
        let node = |id: &str, x: f32, y: f32| Node {
            id: id.into(),
            bounds: Bounds::new(point(x, y), size(100.0, 50.0)),
            color: None,
            kind: NodeKind::Text {
                text: String::new(),
            },
        };
        let left = node("a", 0.0, 0.0);
        let right = node("b", 400.0, 20.0);
        let below = node("c", 10.0, 400.0);

        let curve = EdgeCurve::between(&left, None, &right, None);
        assert_eq!(
            (curve.start_side, curve.end_side),
            (Side::Right, Side::Left)
        );
        assert_eq!(curve.start, point(100.0, 25.0));
        assert_eq!(curve.end, point(400.0, 45.0));
        assert_eq!(curve.control_start, point(250.0, 25.0));

        let curve = EdgeCurve::between(&left, None, &below, Some(Side::Left));
        assert_eq!(
            (curve.start_side, curve.end_side),
            (Side::Bottom, Side::Left)
        );
        assert_eq!(curve.end, point(10.0, 425.0));
    }

    #[test]
    fn the_topmost_card_wins_and_groups_never_do() {
        let canvas = parse_canvas(
            r#"{"nodes": [
              {"id":"group","type":"group","x":0,"y":0,"width":500,"height":500},
              {"id":"under","type":"text","x":10,"y":10,"width":100,"height":100},
              {"id":"over","type":"text","x":50,"y":50,"width":100,"height":100}
            ]}"#,
        )
        .expect("valid");
        let id_at = |x, y| canvas.card_at(point(x, y)).map(|node| node.id.as_str());
        assert_eq!(id_at(60.0, 60.0), Some("over"));
        assert_eq!(id_at(20.0, 20.0), Some("under"));
        assert_eq!(id_at(400.0, 400.0), None);
    }

    #[test]
    fn note_excerpts_drop_frontmatter_and_narrow_to_a_heading() {
        let note = "---\ntags: [paper]\n---\n# Title\nIntro\n## Method\nSteps\n### Detail\nMore\n## Results\nNumbers\n";
        assert_eq!(
            note_excerpt(note, None),
            "# Title\nIntro\n## Method\nSteps\n### Detail\nMore\n## Results\nNumbers\n"
        );
        assert_eq!(
            note_excerpt(note, Some("#Method")),
            "## Method\nSteps\n### Detail\nMore"
        );
        assert_eq!(
            note_excerpt(note, Some("#Missing")),
            note_excerpt(note, None),
            "an unknown heading shows the whole note"
        );
        assert_eq!(note_excerpt("---\nunclosed", None), "---\nunclosed");
    }
}
