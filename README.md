# Suzuri

**Code and write in one place.**

Suzuri adds a writing environment to Zed without giving up the code editor: Obsidian-style live preview for notes, Typst and LaTeX for papers, and a PDF pane for reading — all over plain markdown on your own machine. The agents you already code with draft, import, and cite; you verify and publish.

*Suzuri* (硯) is Japanese for inkstone — the stone a scholar grinds ink on before writing begins.

> [!NOTE]
> Suzuri is early and in active development, built in the open and dogfooded daily. Expect sharp edges.

## Key Features

- **Writing** — Markdown renders in place and reveals its source when your cursor touches it: editable tables, resizable images, citation chips, Obsidian-style embeds, and screenshots that attach themselves when you paste. Files autosave, so agents and you always see the same thing.

- **Reading** — A native PDF viewer with continuous scroll, zoom, and real text selection, built on [hayro](https://github.com/LaurenzV/hayro). PDFs reload in place when they change on disk.

- **Typesetting** — Write [Typst](https://typst.app) or LaTeX in one pane and watch the PDF update in the other, a second after you stop typing. If you have no compiler installed, the first preview offers to fetch one.

- **Linking** — `[[wikilinks]]` with completion, ⌘-click follow, backlinks, and broken-link diagnostics, built in with no extension to install.

- **Everything Zed does** — LSP, tree-sitter, multi-buffer editing, terminals, git, and the rest of a fast, mature editor.

## Install

Download the latest installer:

- [macOS — Apple Silicon](../../releases/latest/download/suzuri-aarch64.dmg)
- [macOS — Intel](../../releases/latest/download/suzuri-x86_64.dmg)
- [Windows](../../releases/latest/download/suzuri-windows-x86_64.exe)

The macOS builds are signed and notarized — open the DMG and drag Suzuri to Applications. The Windows installer is not yet signed, so SmartScreen shows a warning: choose "More info → Run anyway".

Or build from source, the same way Zed builds:

- [Building for macOS](./docs/src/development/macos.md)
- [Building for Linux](./docs/src/development/linux.md)
- [Building for Windows](./docs/src/development/windows.md)

## Testbed Data

[suzuri-testbed](https://github.com/harrywang/suzuri-testbed) is a companion vault of test data for exercising Suzuri by hand. Each folder targets one rendering surface — markdown live preview (including image reloading), Python REPL and notebook execution, LaTeX and Typst preview, and the PDF viewer — and several fixtures are self-checking notes with a checklist at the top, so you can open them and verify behavior by eye. Clone it and open the folder in Suzuri to smoke-test a build or reproduce a rendering bug against known fixtures.

## Credits

Suzuri stands on generous shoulders:

- [Zed](https://zed.dev) — the editor underneath everything
- [David Turnbull](https://github.com/dsturnbull) — the PDF viewer ([zed#51040](https://github.com/zed-industries/zed/pull/51040)) and the hayro text-extraction work it builds on
- [hayro](https://github.com/LaurenzV/hayro) by Laurenz Stampfl — pure-Rust PDF rendering
- [markdown-oxide](https://github.com/Feel-ix-343/markdown-oxide) by Felix Zeller — the PKM language server behind wikilinks, backlinks, and link diagnostics
- [Typst](https://github.com/typst/typst) — the compiler behind Typst live preview
- [TinyTeX](https://github.com/rstudio/tinytex) by Yihui Xie — the relocatable TeX Live behind LaTeX live preview

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
