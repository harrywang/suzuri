# Suzuri

Suzuri starts by bringing Obsidian's features to Zed, then adds Typst and LaTeX for academic writing. It's plain markdown on your machine: your agents draft, import, and cite; Suzuri renders it beautifully and keeps everything in one window — notes, citations, drafts, PDFs.

*Suzuri* (硯) is Japanese for inkstone — the stone a scholar grinds ink on before writing begins.

> [!NOTE]
> Suzuri is early and in active development, built in the open and dogfooded daily. Expect sharp edges.

## Features

**Writing** — Obsidian-style live preview: headings, emphasis, links, and code render in place and reveal their source when the cursor touches them. Tables are editable grids; images render as resizable widgets. Pandoc citations (`[@key]`) render as chips. Obsidian image embeds (`![[figure.png|640]]`) and drag-or-paste image attachments just work — paste a screenshot and it lands in your attachments folder, linked. Files autosave a second after you pause, so what agents see on disk is always what you see on screen.

**Connecting** — `[[wikilinks]]` with completion, ⌘-click follow, backlinks, and broken-link diagnostics, powered by a built-in [markdown-oxide](https://github.com/Feel-ix-343/markdown-oxide) language server (auto-provisioned; no extension required).

**Reading** — a native PDF viewer with continuous scroll, zoom, and glyph-accurate text selection, adopted from David Turnbull's contribution to Zed ([zed#51040](https://github.com/zed-industries/zed/pull/51040)) and built on [hayro](https://github.com/LaurenzV/hayro). PDFs reload in place when they change on disk.

**Typesetting** — write [Typst](https://typst.app) or LaTeX in one pane and watch the compiled PDF update in the other, a second after you stop typing. Compilers are found on your PATH or downloaded automatically on first use (Typst; [Tectonic](https://tectonic-typesetting.github.io) for LaTeX), so a fresh install needs no setup.

**Everything Zed does** — LSP, tree-sitter, multi-buffer editing, terminals, git, collaboration, and the rest of a mature, fast editor, because Suzuri is built on one.

## Install

Download the latest installer from [Releases](../../releases): `suzuri-aarch64.dmg` (Apple Silicon), `suzuri-x86_64.dmg` (Intel Mac), or `suzuri-windows-x86_64.exe`. Builds are currently unsigned: on macOS, right-click → Open on first launch; on Windows, choose "More info → Run anyway".

Or build from source, the same way Zed builds:

- [Building for macOS](./docs/src/development/macos.md)
- [Building for Linux](./docs/src/development/linux.md)
- [Building for Windows](./docs/src/development/windows.md)

## Credits

Suzuri stands on generous shoulders:

- [Zed](https://zed.dev) — the editor underneath everything
- [David Turnbull](https://github.com/dsturnbull) — the PDF viewer ([zed#51040](https://github.com/zed-industries/zed/pull/51040)) and the hayro text-extraction work it builds on
- [hayro](https://github.com/LaurenzV/hayro) by Laurenz Stampfl — pure-Rust PDF rendering
- [markdown-oxide](https://github.com/Feel-ix-343/markdown-oxide) by FelixZeller — PKM language server
- [Typst](https://github.com/typst/typst) and [Tectonic](https://github.com/tectonic-typesetting/tectonic) — the typesetting engines behind live preview

## Relationship to Zed

**Suzuri is a modified version of [Zed](https://github.com/zed-industries/zed), the editor by Zed Industries, Inc.** It tracks Zed's `main` branch and merges upstream changes; the modifications are additive and live mostly in fork-owned crates (`markdown_live_preview`, `pdf_viewer`, `typeset_preview`), with small changes to `crates/editor`, `crates/project_panel`, and `crates/languages`.

Suzuri is not affiliated with, endorsed by, or sponsored by Zed Industries, Inc. "Zed" is a trademark of Zed Industries, Inc.; it is used here only to identify the upstream project from which Suzuri is derived.

## Licensing

Suzuri, like Zed, is licensed primarily under GPL-3.0-or-later, with Apache-2.0 components where marked. See [LICENSE-GPL](./LICENSE-GPL) and [LICENSE-APACHE](./LICENSE-APACHE).

Copyright for the upstream code remains with Zed Industries, Inc. and Zed's contributors; per-file copyright notices are preserved.

License information for third party dependencies must be correctly provided for CI to pass. Suzuri uses [`cargo-about`](https://github.com/EmbarkStudios/cargo-about) to automatically comply with open source licenses, as Zed does. If CI is failing, check the following:

- Is it showing a `no license specified` error for a crate you've created? If so, add `publish = false` under `[package]` in your crate's Cargo.toml.
- Is the error `failed to satisfy license requirements` for a dependency? If so, first determine what license the project has and whether this system is sufficient to comply with this license's requirements. If you're unsure, ask a lawyer. Once you've verified that this system is acceptable add the license's SPDX identifier to the `accepted` array in `script/licenses/zed-licenses.toml`.
- Is `cargo-about` unable to find the license for a dependency? If so, add a clarification field at the end of `script/licenses/zed-licenses.toml`, as specified in the [cargo-about book](https://embarkstudios.github.io/cargo-about/cli/generate/config.html#crate-configuration).
