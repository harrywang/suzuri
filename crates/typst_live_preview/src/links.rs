//! Parsing and resolution of links found in the preview's vector IR.
//!
//! Typst emits three kinds of link targets:
//!
//! - External URLs (`https://…`, `mailto:…`) that belong to the system browser.
//! - Local files, either absolute or relative to the source document.
//! - Internal document locations, which typst.ts encodes as
//!   `@typst:handleTypstLocation(this, <page>, <x>, <y>)` (page is one-based).
//!
//! The preview must distinguish them itself: the webview's JavaScript used to
//! do it with a global function, but a native pane has to resolve local paths
//! and turn internal targets into a scroll.

use std::path::{Component, Path, PathBuf};

/// What a page link points at.
#[derive(Debug, Clone, PartialEq)]
pub enum LinkTarget {
    /// A location inside the document. `page` is zero-based; `x` and `y` are
    /// Typst points from the page's top-left.
    Document { page: usize, x: f32, y: f32 },
    /// A URL the platform should open externally.
    External(String),
    /// A file on this machine. Relative destinations are resolved against the
    /// directory of the document being previewed.
    LocalFile(PathBuf),
}

const INTERNAL_PREFIX: &str = "@typst:handleTypstLocation(";

/// Classifies a link `href` found on a page.
///
/// `base_dir` is the directory containing the previewed document, used to
/// resolve relative local paths.
pub fn classify_link(href: &str, base_dir: Option<&Path>) -> Option<LinkTarget> {
    let href = href.trim();
    if href.is_empty() {
        return None;
    }

    if let Some(target) = parse_internal(href) {
        return Some(target);
    }

    if let Some(path) = parse_file_url(href) {
        return Some(LinkTarget::LocalFile(path));
    }

    if is_external(href) {
        return Some(LinkTarget::External(href.to_owned()));
    }

    // Fragments alone (`#section`) cannot be resolved without the destination
    // table; treating them as files would be worse than ignoring them.
    if href.starts_with('#') {
        return None;
    }

    let decoded = percent_decode(href);
    let path = Path::new(&decoded);
    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base_dir?.join(path)
    };
    Some(LinkTarget::LocalFile(clean_path(&resolved)))
}

/// Parses `@typst:handleTypstLocation(this, 2, 12.5, 34.25)`.
fn parse_internal(href: &str) -> Option<LinkTarget> {
    let arguments = href.strip_prefix(INTERNAL_PREFIX)?.strip_suffix(')')?;
    let mut parts = arguments.split(',');
    let _element = parts.next()?.trim();
    let page = parts.next()?.trim().parse::<usize>().ok()?;
    let x = parts.next()?.trim().parse::<f32>().ok()?;
    let y = parts.next()?.trim().parse::<f32>().ok()?;
    if parts.next().is_some() || page == 0 {
        return None;
    }
    Some(LinkTarget::Document {
        page: page - 1,
        x,
        y,
    })
}

/// Parses `file://` URLs, including percent-encoded and `localhost` forms.
fn parse_file_url(href: &str) -> Option<PathBuf> {
    let rest = href.strip_prefix("file://")?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    let decoded = percent_decode(rest);
    Some(clean_path(Path::new(&decoded)))
}

/// Whether the link should be opened by the platform rather than the editor.
fn is_external(href: &str) -> bool {
    let lower = href.to_ascii_lowercase();
    lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("mailto:")
        || lower.starts_with("tel:")
        || lower.starts_with("ftp://")
        || lower.starts_with("//")
}

/// Decodes `%XX` escapes, leaving malformed escapes untouched.
fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let high = (bytes[index + 1] as char).to_digit(16);
            let low = (bytes[index + 2] as char).to_digit(16);
            if let (Some(high), Some(low)) = (high, low) {
                decoded.push((high * 16 + low) as u8);
                index += 3;
                continue;
            }
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

/// Resolves `.` and `..` without touching the filesystem.
fn clean_path(path: &Path) -> PathBuf {
    let mut clean = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !clean.pop() {
                    clean.push(Component::ParentDir);
                }
            }
            other => clean.push(other.as_os_str()),
        }
    }
    clean
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{LinkTarget, classify_link};

    #[test]
    fn internal_locations_become_document_points() {
        assert_eq!(
            classify_link(
                "@typst:handleTypstLocation(this, 3, 12.5, 34.25)",
                Some(Path::new("/project"))
            ),
            Some(LinkTarget::Document {
                page: 2,
                x: 12.5,
                y: 34.25,
            })
        );
    }

    #[test]
    fn malformed_internal_locations_are_ignored() {
        assert_eq!(
            classify_link("@typst:handleTypstLocation(this, 0, 1, 2)", None),
            None
        );
        assert_eq!(
            classify_link("@typst:handleTypstLocation(this, 1, x, 2)", None),
            None
        );
        assert_eq!(classify_link("@typst:handleTypstLocation", None), None);
    }

    #[test]
    fn external_urls_stay_external() {
        for href in [
            "https://typst.app/",
            "http://example.com",
            "mailto:hi@example.com",
            "tel:+123",
            "//example.com/image.png",
        ] {
            assert_eq!(
                classify_link(href, Some(Path::new("/project"))),
                Some(LinkTarget::External(href.to_owned())),
                "{href}"
            );
        }
    }

    #[test]
    fn relative_targets_resolve_next_to_the_document() {
        assert_eq!(
            classify_link("images/fig.png", Some(Path::new("/project/notes"))),
            Some(LinkTarget::LocalFile(PathBuf::from(
                "/project/notes/images/fig.png"
            )))
        );
        assert_eq!(
            classify_link("../shared/chapter.typ", Some(Path::new("/project/notes"))),
            Some(LinkTarget::LocalFile(PathBuf::from(
                "/project/shared/chapter.typ"
            )))
        );
    }

    #[test]
    fn file_urls_decode_and_resolve() {
        assert_eq!(
            classify_link(
                "file:///home/me/my%20notes/chapter.typ",
                Some(Path::new("/project"))
            ),
            Some(LinkTarget::LocalFile(PathBuf::from(
                "/home/me/my notes/chapter.typ"
            )))
        );
        assert_eq!(
            classify_link(
                "file://localhost/tmp/a%2Fb.typ",
                Some(Path::new("/project"))
            ),
            Some(LinkTarget::LocalFile(PathBuf::from("/tmp/a/b.typ")))
        );
    }

    #[test]
    fn absolute_paths_are_local_files() {
        assert_eq!(
            classify_link("/tmp/figure.pdf", Some(Path::new("/project"))),
            Some(LinkTarget::LocalFile(PathBuf::from("/tmp/figure.pdf")))
        );
    }

    #[test]
    fn relative_links_without_a_base_directory_are_ignored() {
        assert_eq!(classify_link("chapter.typ", None), None);
        assert_eq!(classify_link("#anchor", Some(Path::new("/project"))), None);
    }
}
