# Suzuri

Suzuri is a research writing environment: an Obsidian-style live-preview
markdown editor with the speed and language tooling of a real code editor.

A 硯 (*suzuri*) is an inkstone — the tool you grind ink on before you write.

> [!NOTE]
> Suzuri is early and in active development. It is not yet packaged for
> installation; see [Building](#building) to run it from source.

## What it does today

- **Live preview markdown** — headings, emphasis, links, and code render in
  place as you type, and reveal their source when the cursor enters them.
  Tables are editable in place; images render as resizable widgets.
- **Wikilinks** — `[[note]]` links between files in a vault.
- **Everything Zed does** — LSP, tree-sitter, multi-buffer editing, git
  integration, and the rest of a mature editor, because Suzuri is built on it.

## Planned

- Backlinks, tags, unresolved-link creation, daily notes, and templates
- A reference manager: metadata resolution by DOI/arXiv, a local library,
  citation completion while writing, and BibTeX export

## Building

Suzuri builds the same way Zed does:

- [Building for macOS](./docs/src/development/macos.md)
- [Building for Linux](./docs/src/development/linux.md)
- [Building for Windows](./docs/src/development/windows.md)

## Relationship to Zed

**Suzuri is a modified version of [Zed](https://github.com/zed-industries/zed),
the editor by Zed Industries, Inc.** It tracks Zed's `main` branch and merges
upstream changes; the modifications are additive and live mostly in
`crates/markdown_live_preview`, with small changes to `crates/editor` and
`crates/project_panel`.

Modifications by Harry Wang, beginning August 2026. See the git history for the
complete list of changes relative to upstream Zed.

Suzuri is not affiliated with, endorsed by, or sponsored by Zed Industries, Inc.
"Zed" is a trademark of Zed Industries, Inc.; it is used here only to identify
the upstream project from which Suzuri is derived.

## Licensing

Suzuri, like Zed, is licensed primarily under GPL-3.0-or-later, with Apache-2.0
components where marked. See [LICENSE-GPL](./LICENSE-GPL) and
[LICENSE-APACHE](./LICENSE-APACHE).

Copyright for the upstream code remains with Zed Industries, Inc. and Zed's
contributors; per-file copyright notices are preserved.

License information for third party dependencies must be correctly provided for
CI to pass. Suzuri uses [`cargo-about`](https://github.com/EmbarkStudios/cargo-about)
to automatically comply with open source licenses, as Zed does. If CI is
failing, check the following:

- Is it showing a `no license specified` error for a crate you've created? If so, add `publish = false` under `[package]` in your crate's Cargo.toml.
- Is the error `failed to satisfy license requirements` for a dependency? If so, first determine what license the project has and whether this system is sufficient to comply with this license's requirements. If you're unsure, ask a lawyer. Once you've verified that this system is acceptable add the license's SPDX identifier to the `accepted` array in `script/licenses/zed-licenses.toml`.
- Is `cargo-about` unable to find the license for a dependency? If so, add a clarification field at the end of `script/licenses/zed-licenses.toml`, as specified in the [cargo-about book](https://embarkstudios.github.io/cargo-about/cli/generate/config.html#crate-configuration).

## Upstream

Zed is developed by **Zed Industries, Inc.** If Suzuri is useful to you, the
editor underneath it is worth supporting directly — see
[zed.dev](https://zed.dev) and Zed's
[GitHub Sponsors](https://github.com/sponsors/zed-industries).
