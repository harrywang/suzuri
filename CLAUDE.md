# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

The shared agent rules (Rust conventions, GPUI primer, PR hygiene) live in `.rules`,
which `AGENTS.md` and `GEMINI.md` also symlink to. They apply here too:

@.rules

Per `.rules`, do not edit `.rules` inline during feature work — propose additions in
the PR description instead. This file (`CLAUDE.md`) is the Suzuri-specific layer and
is safe to edit.

## What this repository is

Suzuri is a **fork of [Zed](https://github.com/zed-industries/zed)** that turns it into
a research writing environment: an Obsidian-style live-preview markdown editor. It tracks
Zed's `main` and merges upstream. Almost everything in `crates/` (243 crates) is upstream
Zed and should be treated as vendor code.

The fork's own changes are small and additive:

| Area | Files |
| --- | --- |
| Live preview (the feature) | `crates/markdown_live_preview/` |
| Concealment + highlight hooks it needs | `crates/editor/src/display_map.rs`, `display_map/fold_map.rs`, `fold.rs` |
| Markdown drag-and-drop attachments | `crates/editor/src/items.rs` |
| Project panel header (file/sort/collapse buttons) | `crates/project_panel/src/project_panel.rs` |
| Settings plumbing | `crates/settings_content/`, `assets/settings/default.json` |

`git log --author="Harry Wang" --name-only` is the authoritative list of touched files.
When editing anything outside that set, assume you are modifying upstream code and keep
the diff minimal — every line added to a shared Zed file is a future merge conflict.

## Commands

```sh
cargo run                                  # debug build (slow; fine for iteration)
cargo run --profile release-fast           # the profile used day to day
cargo run ~/path/to/other/project          # open a different folder (see note below)

./script/clippy                            # NOT cargo clippy; adds --release --all-targets
                                           # --all-features --deny warnings, plus cargo-machete,
                                           # typos, and buf lint when installed locally
cargo fmt --all -- --check

cargo nextest run --workspace --no-fail-fast --no-tests=warn   # what CI runs
cargo nextest run -p markdown_live_preview                     # one crate
cargo nextest run -p markdown_live_preview test_per_token_reveal   # one test (substring filter)
cargo nextest run -E 'test(test_table_structure_extraction)'       # one test (expression filter)
```

`cargo nextest` is strongly preferred over `cargo test` here — `.config/nextest.toml`
sets a 60s slow-timeout that terminates hung tests, serializes `db` tests, and grants
300s to specific known-slow tests. Plain `cargo test --workspace` also tends to fail on
macOS with `Too many open files (os error 24)`.

Other checks CI runs: `./script/prettier`, `./script/check-todos`, `./script/check-keymaps`.
Docs are mdBook and must pass Prettier at 80 cols: `cd docs && npx prettier --write src/`.
Visual regression tests: `cargo run -p zed --bin zed_visual_test_runner --features visual-tests`
(prefix with `UPDATE_BASELINE=1` to re-record).

Gotcha: running Zed from `cargo run` and then opening *this* repo inside it leaks Cargo's
env vars into the inner rust-analyzer, causing it to fight the outer build for the target
dir. Open a different folder when dogfooding.

## Live preview architecture

`crates/markdown_live_preview/src/markdown_live_preview.rs` (~3.3k lines) is the whole
feature. There is no rendering pipeline of its own — it is an **`Editor` addon** that
drives existing editor primitives.

**Registration.** `init(cx)` calls `cx.observe_new(register_editor)`, so every `Editor`
gets a `LivePreviewAddon` if its buffer is markdown. Per-editor state (selected image,
active table cell, drag state, explicitly source-revealed block) lives on that addon and
is reached with `editor.addon_mut::<LivePreviewAddon>()`.

**The cycle.** On buffer edit / selection change / theme change:
`extract_markers` walks the tree-sitter tree (`Markdown` + `Markdown-Inline` grammars) into
a `MarkerSet` of `InlineMarker`s and `BlockMarker`s → `recompute` → `apply_emphasis_highlights`
+ `apply_decorations`. `apply_decorations` writes three kinds of decoration:

1. **Concealments** — `editor.set_concealments(owner, ..)` hides syntax markers (`**`, `` ` ``,
   link targets, list bullets). This is a *rendering-only* mechanism added by this fork in
   `fold_map.rs`, deliberately separate from real folds: user folds and other fold consumers
   must not see them. `test_concealments_invisible_to_fold_machinery` pins that.
2. **Text highlights** — all under `HighlightKey::MarkdownLivePreview(index)`, where the
   index is one of the module-level constants `STRIKE`/`ITALIC`/`BOLD`/`LINK`/`DEFINITION`/
   `ORDERED_MARKER`. Each index is an independent namespace so one can be cleared without
   disturbing the others; that is a contract, not an implementation detail.
3. **Replace blocks** — `editor.insert_blocks` with `BlockPlacement::Replace` for headings,
   tables, images, rules, frontmatter, mermaid.

**Reveal semantics.** Inline constructs reveal their source when the selection *touches*
them; blocks reveal when the selection reaches their lines. Tables and images are the
exception — they are edited through their widgets and only reveal source via the `</>`
button (`source_revealed`).

**Do not resize your own `BlockStyle::Flex` blocks from a render closure.** The editor
measures and resizes them during prepaint; doing it yourself fights the editor. This
invalidates the obvious-looking fix for image-sizing bugs.

## Tests

`crates/markdown_live_preview/src/tests.rs` uses `EditorTestContext` with `cx.set_state`
markers (`ˇ` for the cursor) and `cx.executor().run_until_parked()`. `markdown_test_context`
registers both the `Markdown` and hidden `Markdown-Inline` languages — live preview needs
both trees, so a test that registers only one silently sees no inline markers.

The `--- Contract tests ---` section at the bottom of that file is load-bearing: those tests
pin behavior this crate *borrows* from `editor` and `gpui`, so an upstream merge that changes
those semantics fails loudly instead of silently degrading live preview. Add to that section
whenever you start depending on a new upstream behavior.

## Upstream merges

Merging `upstream/main` is routine and the conflict-free case is the dangerous one — a
41-commit merge landed clean and still failed to compile because `image_resolver` had gained
an `&App` parameter. Order the checks accordingly:

1. `cargo check -p markdown_live_preview` first — signature drift in `editor`/`markdown`/`gpui`
   surfaces here and nowhere earlier.
2. `cargo nextest run -p markdown_live_preview` — the contract tests catch semantic drift.
3. `cargo nextest run -p editor -p project_panel` — the shared files the fork patches.

## Adding a setting

The chain is spread across crates and the compiler only catches part of it:

1. `crates/settings_content/src/settings_content.rs` — the `*SettingsContent` struct and its
   field on `SettingsContent`.
2. A `Settings` impl in the consuming crate (e.g. `MarkdownLivePreviewSettings`), reading
   from that content with defaults applied.
3. `crates/settings/src/vscode_import.rs` — constructs the content struct exhaustively, so a
   missing field is a compile error here.
4. `assets/settings/default.json` — the default value plus its user-facing comment.
5. `crates/settings_ui/src/page_data.rs` if it should appear in the settings UI, and
   `docs/src/reference/all-settings.md` if it should be documented.
