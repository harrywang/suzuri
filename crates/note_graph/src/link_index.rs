//! The links between a project's notes, and the part of that graph around
//! one note laid out for drawing. Nothing here touches the editor or the
//! window, so all of it runs off the main thread and under plain unit tests.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::f32::consts::{PI, TAU};
use std::path::{Component, Path, PathBuf};

use gpui::{Bounds, Point, Size, point, size};
use markdown_live_preview::WikilinkTarget;

/// A link as a note writes it, before it is known which note it means.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkReference {
    /// `[[Note]]`: a note's name or path, looked up across the project.
    Wikilink(String),
    /// `[text](folder/note.md)`: a file path, relative to the linking note
    /// unless it is absolute.
    Path(String),
}

/// Every link to another file in a note's text: wikilinks, embeds, and
/// markdown links, without their `#heading` and `|alias` parts. Links inside
/// frontmatter, code blocks, and code spans are text, not links, exactly as
/// live preview treats them. Web links are left out.
pub fn extract_links(text: &str) -> Vec<LinkReference> {
    let mut links = Vec::new();
    let mut fence: Option<(char, usize)> = None;
    for line in strip_frontmatter(text).lines() {
        let trimmed = line.trim_start();
        let marker = trimmed.chars().next().filter(|c| matches!(c, '`' | '~'));
        let run = marker.map_or(0, |marker| {
            trimmed.chars().take_while(|c| *c == marker).count()
        });
        match (fence, marker) {
            (Some((open, length)), Some(marker)) if marker == open && run >= length => {
                if trimmed[run..].trim().is_empty() {
                    fence = None;
                }
                continue;
            }
            (Some(_), _) => continue,
            (None, Some(marker)) if run >= 3 => {
                fence = Some((marker, run));
                continue;
            }
            (None, _) => {}
        }
        let line = blank_code_spans(line);
        wikilinks_in(&line, &mut links);
        markdown_links_in(&line, &mut links);
    }
    links
}

fn strip_frontmatter(text: &str) -> &str {
    let Some(rest) = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))
    else {
        return text;
    };
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        offset += line.len();
        if matches!(line.trim_end(), "---" | "...") {
            return &rest[offset..];
        }
    }
    text
}

/// The line with the contents of its code spans replaced by spaces.
fn blank_code_spans(line: &str) -> String {
    if !line.contains('`') {
        return line.to_string();
    }
    let characters: Vec<char> = line.chars().collect();
    let run_at = |start: usize| {
        characters[start..]
            .iter()
            .take_while(|character| **character == '`')
            .count()
    };
    let mut output = String::with_capacity(line.len());
    let mut index = 0;
    while index < characters.len() {
        if characters[index] != '`' {
            output.push(characters[index]);
            index += 1;
            continue;
        }
        let open = run_at(index);
        let mut close = index + open;
        let mut closed = false;
        while close < characters.len() {
            if characters[close] == '`' {
                let run = run_at(close);
                if run == open {
                    closed = true;
                    break;
                }
                close += run;
            } else {
                close += 1;
            }
        }
        let end = if closed { close + open } else { index + open };
        output.extend(std::iter::repeat_n(' ', end - index));
        index = end;
    }
    output
}

fn wikilinks_in(line: &str, links: &mut Vec<LinkReference>) {
    let mut search_from = 0;
    while let Some(open_offset) = line[search_from..].find("[[") {
        let open = search_from + open_offset;
        let Some(close_offset) = line[open + 2..].find("]]") else {
            break;
        };
        let close = open + 2 + close_offset;
        search_from = close + 2;

        let inner = &line[open + 2..close];
        if inner.is_empty() || inner.contains("[[") {
            continue;
        }
        let target = inner.split('|').next().unwrap_or(inner);
        let note = target.split('#').next().unwrap_or(target).trim();
        if !note.is_empty() {
            links.push(LinkReference::Wikilink(note.to_string()));
        }
    }
}

fn markdown_links_in(line: &str, links: &mut Vec<LinkReference>) {
    let mut search_from = 0;
    while let Some(offset) = line[search_from..].find("](") {
        let start = search_from + offset + 2;
        let rest = &line[start..];
        let (destination, consumed) = if let Some(bracketed) = rest.strip_prefix('<') {
            match bracketed.find('>') {
                Some(end) => (&bracketed[..end], end + 2),
                None => ("", 1),
            }
        } else {
            let mut depth = 0;
            let mut end = rest.len();
            for (index, character) in rest.char_indices() {
                match character {
                    '(' => depth += 1,
                    ')' if depth == 0 => {
                        end = index;
                        break;
                    }
                    ')' => depth -= 1,
                    character if character.is_whitespace() => {
                        end = index;
                        break;
                    }
                    _ => {}
                }
            }
            (&rest[..end], end)
        };
        search_from = start + consumed;

        let destination = destination.trim();
        let has_scheme = destination.split_once(':').is_some_and(|(scheme, _)| {
            !scheme.is_empty()
                && scheme
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || "+-.".contains(character))
        });
        if has_scheme {
            continue;
        }
        let path = destination.split('#').next().unwrap_or(destination);
        let decoded = urlencoding::decode(path)
            .map(|decoded| decoded.into_owned())
            .unwrap_or_else(|_| path.to_string());
        if !decoded.is_empty() {
            links.push(LinkReference::Path(decoded));
        }
    }
}

struct CatalogNote {
    relative_path: String,
    stem: Option<String>,
    folders_deep: usize,
    absolute: PathBuf,
}

/// The project's notes, arranged so a link can be resolved without walking
/// all of them. Notes keep the order they were given in, and a note is
/// known by its position in that order.
#[derive(Default)]
pub struct NoteCatalog {
    notes: Vec<CatalogNote>,
    by_path: HashMap<String, Vec<usize>>,
    by_stem: HashMap<String, Vec<usize>>,
    by_absolute: HashMap<PathBuf, usize>,
}

impl NoteCatalog {
    /// `notes` are each note's path within its worktree, with `/`
    /// separators, and its absolute path, in the order the project lists
    /// them. That order breaks ties the way opening a wikilink does.
    pub fn new(notes: impl IntoIterator<Item = (String, PathBuf)>) -> Self {
        let mut catalog = Self::default();
        for (index, (relative_path, absolute)) in notes.into_iter().enumerate() {
            let lowercase = relative_path.to_lowercase();
            let stem = Path::new(&relative_path)
                .file_stem()
                .and_then(|stem| stem.to_str())
                .map(str::to_string);
            catalog
                .by_path
                .entry(lowercase.trim_end_matches(".md").to_string())
                .or_default()
                .push(index);
            if let Some(stem) = &stem {
                catalog
                    .by_stem
                    .entry(stem.to_lowercase())
                    .or_default()
                    .push(index);
            }
            catalog.by_absolute.entry(absolute.clone()).or_insert(index);
            catalog.notes.push(CatalogNote {
                folders_deep: relative_path.split('/').count(),
                relative_path,
                stem,
                absolute,
            });
        }
        catalog
    }

    pub fn len(&self) -> usize {
        self.notes.len()
    }

    /// The note a link written in note `source` points at, if it points at
    /// one.
    pub fn resolve(&self, source: usize, reference: &LinkReference) -> Option<usize> {
        let source_directory = self.notes.get(source)?.absolute.parent();
        match reference {
            LinkReference::Wikilink(target) => {
                let target = WikilinkTarget::new(target)?;
                let candidates: BTreeSet<usize> = [
                    self.by_path.get(target.path()),
                    self.by_stem.get(target.stem()),
                ]
                .into_iter()
                .flatten()
                .flatten()
                .copied()
                .collect();
                let mut best: Option<(u32, usize, usize)> = None;
                for index in candidates {
                    let note = &self.notes[index];
                    let Some(rank) = target.rank(&note.relative_path, note.stem.as_deref(), || {
                        source_directory.is_some() && note.absolute.parent() == source_directory
                    }) else {
                        continue;
                    };
                    if best.is_none_or(|(best_rank, best_depth, _)| {
                        (rank, note.folders_deep) < (best_rank, best_depth)
                    }) {
                        best = Some((rank, note.folders_deep, index));
                    }
                }
                best.map(|(_, _, index)| index)
            }
            LinkReference::Path(path) => {
                let path = Path::new(path);
                let absolute = if path.is_absolute() {
                    path.to_path_buf()
                } else {
                    source_directory?.join(path)
                };
                self.by_absolute.get(&without_dots(&absolute)).copied()
            }
        }
    }
}

/// The path with `.` and `..` components worked out, without asking the
/// filesystem.
fn without_dots(path: &Path) -> PathBuf {
    let mut output = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                output.pop();
            }
            other => output.push(other),
        }
    }
    output
}

/// Which notes are linked, in either direction.
#[derive(Default)]
pub struct LinkGraph {
    neighbours: Vec<BTreeSet<usize>>,
}

impl LinkGraph {
    /// `links` are each note's links, in the catalog's order.
    pub fn new<'a>(
        catalog: &NoteCatalog,
        links: impl IntoIterator<Item = &'a [LinkReference]>,
    ) -> Self {
        let mut neighbours = vec![BTreeSet::new(); catalog.len()];
        for (source, links) in links.into_iter().enumerate().take(catalog.len()) {
            for reference in links {
                if let Some(target) = catalog.resolve(source, reference)
                    && target != source
                {
                    neighbours[source].insert(target);
                    neighbours[target].insert(source);
                }
            }
        }
        Self { neighbours }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlacedNote {
    /// The note's position in the catalog.
    pub note: usize,
    /// Links between this note and the center one.
    pub hops: usize,
    pub position: Point<f32>,
}

/// The notes within a few links of one note, placed for drawing with the
/// center note at the origin.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Neighbourhood {
    /// The center note first, then the rest nearest first.
    pub notes: Vec<PlacedNote>,
    /// Linked pairs, as positions in `notes`.
    pub links: Vec<(usize, usize)>,
    /// Notes in range that were left out to keep the drawing readable.
    pub omitted: usize,
}

impl Neighbourhood {
    /// The notes up to `hops` links from `center`, the nearest `limit` of
    /// them when there are more.
    pub fn around(graph: &LinkGraph, center: usize, hops: usize, limit: usize) -> Self {
        if center >= graph.neighbours.len() {
            return Self::default();
        }
        // Each found note: its catalog position, distance, and the position
        // in `found` of the note it was reached from.
        let mut found: Vec<(usize, usize, Option<usize>)> = vec![(center, 0, None)];
        let mut position_of: HashMap<usize, usize> = HashMap::from([(center, 0)]);
        let mut queue = VecDeque::from([0]);
        while let Some(from) = queue.pop_front() {
            let (note, distance, _) = found[from];
            if distance == hops {
                continue;
            }
            for &neighbour in &graph.neighbours[note] {
                if position_of.contains_key(&neighbour) {
                    continue;
                }
                position_of.insert(neighbour, found.len());
                queue.push_back(found.len());
                found.push((neighbour, distance + 1, Some(from)));
            }
        }
        let omitted = found.len().saturating_sub(limit.max(1));
        found.truncate(limit.max(1));

        let positions = radial_layout(
            &found
                .iter()
                .map(|(_, distance, parent)| (*distance, *parent))
                .collect::<Vec<_>>(),
        );
        let mut links = Vec::new();
        for (index, (note, ..)) in found.iter().enumerate() {
            for neighbour in &graph.neighbours[*note] {
                if let Some(&other) = position_of.get(neighbour)
                    && other < found.len()
                    && index < other
                {
                    links.push((index, other));
                }
            }
        }
        Self {
            notes: found
                .iter()
                .zip(positions)
                .map(|((note, hops, _), position)| PlacedNote {
                    note: *note,
                    hops: *hops,
                    position,
                })
                .collect(),
            links,
            omitted,
        }
    }

    /// The smallest box around every note's dot and label.
    pub fn bounds(&self) -> Option<Bounds<f32>> {
        self.notes
            .iter()
            .map(|note| note_bounds(note.position))
            .reduce(|a, b| a.union(&b))
    }

    /// The topmost note whose dot or label is under `position`.
    pub fn note_at(&self, position: Point<f32>) -> Option<usize> {
        self.notes
            .iter()
            .rposition(|note| note_bounds(note.position).contains(&position))
    }
}

/// The widest a note's label is drawn; longer names are cut short.
pub const LABEL_WIDTH: f32 = 96.0;
pub const DOT_RADIUS: f32 = 6.0;
pub const LABEL_HEIGHT: f32 = 18.0;

/// The room a note takes up around its dot: the dot, and the label under it.
pub fn note_bounds(position: Point<f32>) -> Bounds<f32> {
    Bounds::new(
        point(
            position.x - LABEL_WIDTH / 2.0,
            position.y - DOT_RADIUS - 2.0,
        ),
        size(LABEL_WIDTH, DOT_RADIUS * 2.0 + 4.0 + LABEL_HEIGHT),
    )
}

/// The least distance along a ring between two notes for their labels to
/// stay apart.
const SPACING: f32 = 112.0;
const FIRST_RING: f32 = 170.0;
const RING_GAP: f32 = 150.0;
/// Between the rings one crowded level is packed into. Two labels this far
/// apart cannot touch whichever way they lie from each other, so the rings
/// need no alignment.
const PACKED_RING_GAP: f32 = 104.0;

/// Places notes in rings around the first one: `notes` are each note's
/// distance from the center and the position of the note it hangs off,
/// nearest first. A note sits in the direction of the note it hangs off, so
/// a neighbour's own neighbours fan out behind it. A level too crowded for
/// one ring, such as the hundreds of notes that link to a hub, is packed
/// into several close rings instead of one enormous one.
pub fn radial_layout(notes: &[(usize, Option<usize>)]) -> Vec<Point<f32>> {
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); notes.len()];
    for (index, (_, parent)) in notes.iter().enumerate() {
        if let Some(parent) = parent.filter(|parent| *parent < index) {
            children[parent].push(index);
        }
    }
    // Notes are listed nearest first, so a note's children always follow it
    // and a backwards pass has every child's weight ready.
    let mut weight = vec![1.0_f32; notes.len()];
    for index in (0..notes.len()).rev() {
        if !children[index].is_empty() {
            weight[index] = children[index].iter().map(|child| weight[*child]).sum();
        }
    }
    let mut angle = vec![0.0_f32; notes.len()];
    let mut sector = vec![(0.0_f32, TAU); notes.len()];
    for index in 0..notes.len() {
        let (start, width) = sector[index];
        // A neighbour's own notes fan out behind it. Given its whole share
        // of the circle they would wrap around to sit nearer to other
        // neighbours than to it.
        let (start, width) = if index > 0 && width > PI {
            (start + (width - PI) / 2.0, PI)
        } else {
            (start, width)
        };
        let total: f32 = children[index].iter().map(|child| weight[*child]).sum();
        let mut cursor = start;
        for &child in &children[index] {
            let share = width * weight[child] / total;
            sector[child] = (cursor, share);
            angle[child] = cursor + share / 2.0;
            cursor += share;
        }
    }

    let mut positions = vec![point(0.0, 0.0); notes.len()];
    let levels = notes.iter().map(|(hops, _)| *hops).max().unwrap_or(0);
    let mut outer = 0.0;
    for level in 1..=levels {
        let mut ring: Vec<usize> = (0..notes.len())
            .filter(|index| notes[*index].0 == level)
            .collect();
        ring.sort_by(|a, b| angle[*a].total_cmp(&angle[*b]).then(a.cmp(b)));
        if ring.is_empty() {
            continue;
        }
        let base = outer + if level == 1 { FIRST_RING } else { RING_GAP };
        let narrowest = ring
            .iter()
            .zip(ring.iter().cycle().skip(1))
            .map(|(a, b)| (angle[*b] - angle[*a]).rem_euclid(TAU))
            .filter(|gap| *gap > 0.0)
            .fold(TAU, f32::min);
        let needed = if ring.len() == 1 {
            base
        } else {
            SPACING / narrowest
        };
        if needed <= base * 2.0 {
            let radius = needed.max(base);
            for &index in &ring {
                positions[index] = on_ring(radius, angle[index]);
            }
            outer = radius;
            continue;
        }

        let mut remaining = ring.as_slice();
        let mut radius = base;
        while !remaining.is_empty() {
            let capacity = ((TAU * radius / SPACING).floor() as usize).max(1);
            let (row, rest) = remaining.split_at(capacity.min(remaining.len()));
            // A row that is not full keeps the full row's spacing, so the
            // last few notes do not spread out over the whole ring.
            let step = TAU / capacity as f32;
            for (slot, &index) in row.iter().enumerate() {
                positions[index] = on_ring(radius, step * slot as f32);
            }
            remaining = rest;
            outer = radius;
            radius += PACKED_RING_GAP;
        }
    }
    positions
}

/// Angles start at the top and run clockwise.
fn on_ring(radius: f32, angle: f32) -> Point<f32> {
    point(radius * angle.sin(), -radius * angle.cos())
}

pub const MIN_ZOOM: f32 = 0.1;
pub const MAX_ZOOM: f32 = 4.0;

/// The zoom and offset that center `content` in `viewport` with `margin`
/// around it. The offset is where the graph's origin lands in the viewport.
/// A fit never zooms in past 100%.
pub fn fit(content: Bounds<f32>, viewport: Size<f32>, margin: f32) -> (f32, Point<f32>) {
    let available = size(
        (viewport.width - 2.0 * margin).max(1.0),
        (viewport.height - 2.0 * margin).max(1.0),
    );
    let zoom = (available.width / content.size.width.max(1.0))
        .min(available.height / content.size.height.max(1.0))
        .clamp(MIN_ZOOM, 1.0);
    let center = content.center();
    let offset = point(
        viewport.width / 2.0 - center.x * zoom,
        viewport.height / 2.0 - center.y * zoom,
    );
    (zoom, offset)
}

/// Changes the zoom while keeping the graph point under `anchor` (a viewport
/// position) where it is.
pub fn zoom_about(
    zoom: f32,
    offset: Point<f32>,
    new_zoom: f32,
    anchor: Point<f32>,
) -> (f32, Point<f32>) {
    let new_zoom = new_zoom.clamp(MIN_ZOOM, MAX_ZOOM);
    let graph_anchor = point((anchor.x - offset.x) / zoom, (anchor.y - offset.y) / zoom);
    let new_offset = point(
        anchor.x - graph_anchor.x * new_zoom,
        anchor.y - graph_anchor.y * new_zoom,
    );
    (new_zoom, new_offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wikilink(target: &str) -> LinkReference {
        LinkReference::Wikilink(target.to_string())
    }

    fn path(target: &str) -> LinkReference {
        LinkReference::Path(target.to_string())
    }

    fn catalog(paths: &[&str]) -> NoteCatalog {
        NoteCatalog::new(
            paths
                .iter()
                .map(|path| (path.to_string(), Path::new("/vault").join(path))),
        )
    }

    #[test]
    fn links_are_read_without_their_headings_and_aliases() {
        assert_eq!(
            extract_links(
                "See [[Alpha]], [[Beta#Method|the method]] and ![[Gamma]].\n\
                 Also [a file](notes/Delta%20One.md#results) and [another](<Epsilon Two.md>).\n"
            ),
            vec![
                wikilink("Alpha"),
                wikilink("Beta"),
                wikilink("Gamma"),
                path("notes/Delta One.md"),
                path("Epsilon Two.md"),
            ]
        );
    }

    #[test]
    fn links_to_the_web_or_to_the_same_note_are_not_note_links() {
        assert_eq!(
            extract_links(
                "[site](https://example.com/a.md) [mail](mailto:a@b.c) [[#Heading]] \
                 [here](#heading) [titled](Note.md \"A title\")"
            ),
            vec![path("Note.md")]
        );
    }

    #[test]
    fn text_in_frontmatter_and_code_is_not_a_link() {
        assert_eq!(
            extract_links(
                "---\nrelated: \"[[Frontmatter]]\"\n---\n\
                 `[[Span]]` then [[Real]] and ``a ` [[Double]]``\n\
                 ```md\n[[Fenced]]\n```\n\
                 ~~~~\n[[Tilde]]\n~~~\n[[Still fenced]]\n~~~~\n\
                 [[After]]\n"
            ),
            vec![wikilink("Real"), wikilink("After")]
        );
    }

    #[test]
    fn a_wikilink_prefers_a_full_path_then_a_neighbour_then_the_shallowest_note() {
        let catalog = catalog(&[
            "deep/folder/Method.md",
            "papers/Method.md",
            "papers/Chen.md",
            "deep/folder/Other.md",
            "a/b/Solo.md",
            "a/Solo.md",
        ]);
        assert_eq!(catalog.resolve(2, &wikilink("Method")), Some(1));
        assert_eq!(catalog.resolve(3, &wikilink("method")), Some(0));
        assert_eq!(catalog.resolve(0, &wikilink("Chen")), Some(2));
        assert_eq!(
            catalog.resolve(2, &wikilink("deep/folder/Method.md")),
            Some(0)
        );
        assert_eq!(catalog.resolve(2, &wikilink("Solo")), Some(5));
        assert_eq!(catalog.resolve(2, &wikilink("Missing")), None);

        // A bare name is also the whole path of a note at the top of the
        // project, which therefore wins over one beside the linking note.
        let catalog = self::catalog(&["papers/Method.md", "Method.md", "papers/Chen.md"]);
        assert_eq!(catalog.resolve(2, &wikilink("Method")), Some(1));
    }

    #[test]
    fn a_markdown_link_is_a_path_from_the_linking_note() {
        let catalog = catalog(&["papers/Chen.md", "notes/Idea.md", "Idea.md"]);
        assert_eq!(catalog.resolve(0, &path("../notes/Idea.md")), Some(1));
        assert_eq!(catalog.resolve(1, &path("./Idea.md")), Some(1));
        assert_eq!(catalog.resolve(0, &path("/vault/Idea.md")), Some(2));
        assert_eq!(catalog.resolve(0, &path("Idea.md")), None);
        assert_eq!(catalog.resolve(0, &path("figure.png")), None);
    }

    fn graph(paths: &[&str], links: &[&[LinkReference]]) -> LinkGraph {
        LinkGraph::new(&catalog(paths), links.iter().copied())
    }

    #[test]
    fn the_neighbourhood_follows_links_both_ways_up_to_the_depth() {
        // A -> B -> C -> D, and E -> A.
        let graph = graph(
            &["A.md", "B.md", "C.md", "D.md", "E.md", "Alone.md"],
            &[
                &[wikilink("B"), wikilink("A")],
                &[wikilink("C")],
                &[wikilink("D")],
                &[],
                &[wikilink("A")],
                &[],
            ],
        );
        let shown = |hops| {
            Neighbourhood::around(&graph, 0, hops, 100)
                .notes
                .iter()
                .map(|note| (note.note, note.hops))
                .collect::<Vec<_>>()
        };
        assert_eq!(shown(1), vec![(0, 0), (1, 1), (4, 1)]);
        assert_eq!(shown(2), vec![(0, 0), (1, 1), (4, 1), (2, 2)]);
        assert_eq!(shown(3), vec![(0, 0), (1, 1), (4, 1), (2, 2), (3, 3)]);

        let around_c = Neighbourhood::around(&graph, 2, 1, 100);
        assert_eq!(around_c.links, vec![(0, 1), (0, 2)]);
        assert_eq!(around_c.notes[0].position, point(0.0, 0.0));
    }

    #[test]
    fn links_between_neighbours_are_drawn_too() {
        let graph = graph(
            &["A.md", "B.md", "C.md"],
            &[&[wikilink("B"), wikilink("C")], &[wikilink("C")], &[]],
        );
        let shown = Neighbourhood::around(&graph, 0, 1, 100);
        assert_eq!(shown.links, vec![(0, 1), (0, 2), (1, 2)]);
    }

    #[test]
    fn a_crowded_neighbourhood_keeps_the_nearest_notes() {
        let names: Vec<String> = (0..50).map(|index| format!("N{index}.md")).collect();
        let paths: Vec<&str> = names.iter().map(String::as_str).collect();
        let hub: Vec<LinkReference> = (1..50)
            .map(|index| wikilink(&format!("N{index}")))
            .collect();
        let mut links: Vec<&[LinkReference]> = vec![&[]; 50];
        links[0] = &hub;
        let shown = Neighbourhood::around(&graph(&paths, &links), 0, 1, 20);
        assert_eq!(shown.notes.len(), 20);
        assert_eq!(shown.omitted, 30);
        assert!(shown.links.iter().all(|(a, b)| *a < 20 && *b < 20));
    }

    fn closest_pair(positions: &[Point<f32>]) -> f32 {
        let mut closest = f32::MAX;
        for (index, a) in positions.iter().enumerate() {
            for b in &positions[index + 1..] {
                closest = closest.min((a.x - b.x).hypot(a.y - b.y));
            }
        }
        closest
    }

    fn labels_overlap(positions: &[Point<f32>]) -> usize {
        let mut overlaps = 0;
        for (index, a) in positions.iter().enumerate() {
            for b in &positions[index + 1..] {
                if note_bounds(*a).intersects(&note_bounds(*b)) {
                    overlaps += 1;
                }
            }
        }
        overlaps
    }

    #[test]
    fn a_few_neighbours_share_one_ring_evenly() {
        let positions = radial_layout(&[
            (0, None),
            (1, Some(0)),
            (1, Some(0)),
            (1, Some(0)),
            (1, Some(0)),
        ]);
        assert_eq!(positions[0], point(0.0, 0.0));
        for position in &positions[1..] {
            assert!((position.x.hypot(position.y) - FIRST_RING).abs() < 0.01);
        }
        assert_eq!(labels_overlap(&positions), 0);
    }

    #[test]
    fn a_hub_with_hundreds_of_neighbours_stays_compact_and_legible() {
        let mut notes = vec![(0, None)];
        notes.extend((0..300).map(|_| (1, Some(0))));
        let positions = radial_layout(&notes);
        let farthest = positions
            .iter()
            .map(|position| position.x.hypot(position.y))
            .fold(0.0, f32::max);
        assert!(
            farthest < 1200.0,
            "300 notes on one ring would need a radius over 5000, got {farthest}"
        );
        assert!(closest_pair(&positions) > 60.0);
        assert_eq!(labels_overlap(&positions), 0);
        assert_eq!(positions, radial_layout(&notes), "the layout is stable");
    }

    #[test]
    fn second_hop_notes_sit_behind_the_neighbour_they_hang_off() {
        // Two neighbours, the second with three notes of its own.
        let notes = [
            (0, None),
            (1, Some(0)),
            (1, Some(0)),
            (2, Some(2)),
            (2, Some(2)),
            (2, Some(2)),
        ];
        let positions = radial_layout(&notes);
        let direction = |position: Point<f32>| position.x.atan2(-position.y).rem_euclid(TAU);
        let parent = direction(positions[2]);
        let other = direction(positions[1]);
        for child in &positions[3..] {
            let to_parent = (direction(*child) - parent).abs();
            let to_other = (direction(*child) - other).abs();
            assert!(to_parent < to_other);
            assert!(child.x.hypot(child.y) > positions[2].x.hypot(positions[2].y));
        }
        assert_eq!(labels_overlap(&positions), 0);
    }

    #[test]
    fn fitting_centers_the_graph_and_zooming_keeps_the_pointer_still() {
        let content = Bounds::new(point(-100.0, -50.0), size(200.0, 100.0));
        let (zoom, offset) = fit(content, size(800.0, 600.0), 40.0);
        assert_eq!(zoom, 1.0);
        assert_eq!(offset, point(400.0, 300.0));

        let anchor = point(250.0, 120.0);
        let (new_zoom, new_offset) = zoom_about(zoom, offset, 2.0, anchor);
        let before = point((anchor.x - offset.x) / zoom, (anchor.y - offset.y) / zoom);
        let after = point(
            (anchor.x - new_offset.x) / new_zoom,
            (anchor.y - new_offset.y) / new_zoom,
        );
        assert!((before.x - after.x).abs() < 0.001 && (before.y - after.y).abs() < 0.001);
    }
}
