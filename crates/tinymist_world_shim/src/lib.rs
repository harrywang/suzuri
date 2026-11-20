//! A shim standing in for `tinymist-world`, the compiler-side world
//! implementation.
//!
//! `reflexo` depends on the real crate for exactly one thing: re-exporting its
//! `debug_loc` module from `reflexo`'s root. The vector IR and its renderers
//! never call into it. Depending on the real `tinymist-world` would drag the
//! whole Typst compiler world (and its package, VFS, and bundle crates) into
//! the GUI, so the workspace patches `tinymist-world` to this empty module
//! instead.

/// Placeholder for `tinymist_world::debug_loc`.
///
/// Nothing on the preview client path reads it.
pub mod debug_loc {}
