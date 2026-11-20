//! Preview data-plane protocol handling.
//!
//! Tinymist's preview server sends binary WebSocket frames prefixed with the
//! kind of update they carry. The payload after the prefix is an rkyv-encoded
//! [`BytesModuleStream`](reflexo::vector::stream::BytesModuleStream); see
//! [`crate::incr`] for merging it.

/// Prefix for incremental vector document updates.
pub const DIFF_V1_PREFIX: &[u8] = b"diff-v1,";

/// Prefix for a full current vector document update.
pub const NEW_PREFIX: &[u8] = b"new,";

/// A preview data-plane frame with its prefix stripped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewDataFrame<'a> {
    /// An incremental update.
    DiffV1(&'a [u8]),
    /// A full current update. Clients must reset their render state before
    /// merging the payload.
    FullCurrent(&'a [u8]),
}

/// Splits a preview data-plane binary frame into its event kind and payload.
pub fn split_preview_data_frame(bytes: &[u8]) -> Option<PreviewDataFrame<'_>> {
    if let Some(payload) = bytes.strip_prefix(DIFF_V1_PREFIX) {
        Some(PreviewDataFrame::DiffV1(payload))
    } else {
        bytes
            .strip_prefix(NEW_PREFIX)
            .map(PreviewDataFrame::FullCurrent)
    }
}

/// A preview vector document update ready to merge into the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreviewUpdate<'a> {
    /// Whether the viewer must reset client-side state before merging.
    pub reset_before_merge: bool,
    /// The serialized vector document payload without the data-plane prefix.
    pub payload: &'a [u8],
}

/// Parses a preview data-plane frame into a viewer update.
pub fn preview_update_from_bytes(bytes: &[u8]) -> Option<PreviewUpdate<'_>> {
    match split_preview_data_frame(bytes)? {
        PreviewDataFrame::DiffV1(payload) => Some(PreviewUpdate {
            reset_before_merge: false,
            payload,
        }),
        PreviewDataFrame::FullCurrent(payload) => Some(PreviewUpdate {
            reset_before_merge: true,
            payload,
        }),
    }
}

/// Builds the data-plane text message that asks Tinymist to resolve a click on
/// a page to the source location that produced it.
///
/// `page_index` is zero-based here and converted to Tinymist's one-based
/// `page_no`; `x` and `y` are Typst points from the page's top-left.
///
/// Tinymist answers with an LSP `window/showDocument` request carrying the file
/// and the source range, which the editor opens and selects.
pub fn source_point_message(page_index: usize, x: f64, y: f64) -> String {
    format!(
        "src-point {}",
        serde_json::json!({
            "page_no": page_index + 1,
            "x": x as f32,
            "y": y as f32,
        })
    )
}

/// A position on a page, in Typst points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PagePoint {
    /// Zero-based page index.
    pub page: usize,
    pub x: f32,
    pub y: f32,
}

/// A scrolling instruction Tinymist sends over the data plane.
#[derive(Debug, Clone, PartialEq)]
pub enum PreviewControl {
    /// The viewport should show this point (sent after editor-to-preview sync
    /// and when another client reports its position).
    Viewport(PagePoint),
    /// One or more positions to jump to (used when a source location resolves
    /// to several places).
    Jumps(Vec<PagePoint>),
}

/// Parses a non-vector binary frame emitted by Tinymist's preview server.
///
/// The webview receives these as binary frames too: `viewport,<page> <x> <y>`
/// and `jump,<page> <x> <y>,<page> <x> <y>,…`, with one-based pages.
pub fn parse_control_frame(bytes: &[u8]) -> Option<PreviewControl> {
    let text = std::str::from_utf8(bytes).ok()?;
    if let Some(rest) = text.strip_prefix("viewport,") {
        return parse_page_point(rest).map(PreviewControl::Viewport);
    }
    if let Some(rest) = text.strip_prefix("jump,") {
        let points: Vec<_> = rest.split(',').filter_map(parse_page_point).collect();
        return (!points.is_empty()).then_some(PreviewControl::Jumps(points));
    }
    None
}

fn parse_page_point(text: &str) -> Option<PagePoint> {
    let mut parts = text.split_whitespace();
    let page = parts.next()?.parse::<usize>().ok()?;
    let x = parts.next()?.parse::<f32>().ok()?;
    let y = parts.next()?.parse::<f32>().ok()?;
    if page == 0 {
        return None;
    }
    Some(PagePoint {
        page: page - 1,
        x,
        y,
    })
}

/// Converts a complete initial incremental update into a full-current update.
///
/// A fresh incremental renderer emits a self-contained `diff-v1,` payload. The
/// full-current data-plane event uses the same payload format with a `new,`
/// prefix so clients reset their local state before merging it. Tests use this
/// to replay a recorded delta as if it had arrived on connect.
pub fn full_current_frame_from_delta(delta_frame: &[u8]) -> Option<Vec<u8>> {
    let payload = delta_frame.strip_prefix(DIFF_V1_PREFIX)?;
    Some([NEW_PREFIX, payload].concat())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_v1_frame_merges_without_reset() {
        let frame = [DIFF_V1_PREFIX, b"x"].concat();

        let update = preview_update_from_bytes(&frame).expect("diff-v1 should be accepted");

        assert!(!update.reset_before_merge);
        assert_eq!(update.payload, b"x");
    }

    #[test]
    fn full_current_frame_resets_before_merge() {
        let frame = [NEW_PREFIX, b"x"].concat();

        let update = preview_update_from_bytes(&frame).expect("new should be accepted");

        assert!(update.reset_before_merge);
        assert_eq!(update.payload, b"x");
    }

    #[test]
    fn a_delta_is_promoted_to_a_full_current_frame() {
        let delta = [DIFF_V1_PREFIX, b"payload"].concat();

        let full = full_current_frame_from_delta(&delta).expect("promoted frame");

        assert_eq!(full, [NEW_PREFIX, b"payload"].concat());
        assert!(full_current_frame_from_delta(b"new,payload").is_none());
    }

    #[test]
    fn unknown_frames_are_ignored() {
        assert!(preview_update_from_bytes(b"outline-sync,[]").is_none());
        assert!(preview_update_from_bytes(b"").is_none());
    }

    #[test]
    fn source_point_message_uses_one_based_pages_and_points() {
        assert_eq!(
            source_point_message(0, 12.5, 34.25),
            r#"src-point {"page_no":1,"x":12.5,"y":34.25}"#
        );
        assert_eq!(
            source_point_message(2, 0.0, 1.0),
            r#"src-point {"page_no":3,"x":0.0,"y":1.0}"#
        );
    }

    #[test]
    fn viewport_frames_use_one_based_pages() {
        assert_eq!(
            parse_control_frame(b"viewport,2 12.5 34.25"),
            Some(PreviewControl::Viewport(PagePoint {
                page: 1,
                x: 12.5,
                y: 34.25,
            }))
        );
    }

    #[test]
    fn jump_frames_carry_every_position() {
        assert_eq!(
            parse_control_frame(b"jump,1 2 3,4 5 6"),
            Some(PreviewControl::Jumps(vec![
                PagePoint {
                    page: 0,
                    x: 2.0,
                    y: 3.0,
                },
                PagePoint {
                    page: 3,
                    x: 5.0,
                    y: 6.0,
                },
            ]))
        );
    }

    #[test]
    fn vector_frames_are_not_control_frames() {
        assert_eq!(parse_control_frame(b"diff-v1,payload"), None);
        assert_eq!(parse_control_frame(b"new,payload"), None);
        assert_eq!(parse_control_frame(b"viewport,0 1 2"), None);
    }
}
