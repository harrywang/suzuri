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
| LaTeX math rendering for live preview | `crates/math_render/` |
| PDF viewer (adopted from zed#51040) | `crates/pdf_viewer/` |
| Live Typst/LaTeX preview | `crates/typeset_preview/` |
| Concealment + highlight hooks live preview needs | `crates/editor/src/display_map.rs`, `display_map/fold_map.rs`, `fold.rs` |
| Markdown attachments: drag-and-drop and clipboard paste | `crates/editor/src/items.rs` |
| Project panel header (file/sort/refresh/collapse) and typeset preview menu entry | `crates/project_panel/src/project_panel.rs` |
| Built-in markdown-oxide language server | `crates/languages/src/markdown_oxide.rs`, `crates/languages/src/lib.rs` |
| Preview button for `.typ`/`.tex` | `crates/zed/src/zed/quick_action_bar/preview.rs` |
| Update notifications | `crates/suzuri_update/`, `crates/zed/src/zed/app_menus.rs` (the Check for Updates entry) |
| Settings plumbing | `crates/settings_content/`, `assets/settings/default.json` |
| Branding, CLI name, release infrastructure | `crates/zed/Cargo.toml` (bundle metadata), `crates/zed/resources/app-icon-suzuri*`, `crates/zed/resources/windows/app-icon-suzuri.ico`, `assets/images/suzuri_logo.svg`, `crates/install_cli/src/install_cli_binary.rs`, `script/bundle-mac`, `script/bundle-windows.ps1`, `.github/workflows/suzuri-release.yml` |

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

Merge weekly. Zed lands roughly 15 commits a day, and conflict pain grows faster than
linearly with drift: reconciling three separate refactors of the same function at once is
far worse than three merges of one refactor each. Treat a month as the hard ceiling.

Merge on a branch (`git checkout -b merge-upstream-YYYY-MM-DD`), never straight onto `main`.
The conflict-free case is the dangerous one — a 41-commit merge landed clean and still failed
to compile because `image_resolver` had gained an `&App` parameter. Order the checks so drift
surfaces early:

1. `cargo check -p markdown_live_preview -p pdf_viewer -p typeset_preview -p languages` first
   — signature drift in `editor`/`markdown`/`gpui`/`workspace` surfaces in the fork's own
   crates and nowhere earlier.
2. `cargo check -p zed -p editor -p project_panel` — the shared files the fork patches.
3. `cargo nextest run -p markdown_live_preview -p pdf_viewer -p typeset_preview` — the
   contract tests catch semantic drift a clean compile hides.
4. `cargo nextest run -p project_panel -p languages`. Note `undo_create_dirty_file` in
   `project_panel` fails on a clean upstream tree too; verify by stashing before blaming a merge.
5. Bundle and smoke-test the real app: live preview, a PDF, a Typst preview, the panel's
   refresh button.

Conflicts recur in the same handful of registration points (`Cargo.toml` members and paths,
`crates/zed/src/main.rs` init calls, `crates/zed/src/zed.rs` toolbar block,
`quick_action_bar/preview.rs`). `git rerere` is enabled, so a resolution once recorded
replays itself. `README.md` carries `merge=ours` in `.gitattributes` and never conflicts —
that driver needs `git config merge.ours.driver true` once per clone.

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

## Cutting a release

Suzuri ships on the **dev** release channel, which is what keeps Zed's auto-updater
dormant (`ReleaseChannel::poll_for_updates` returns false for `Dev`) and what makes
`[package.metadata.bundle]` — the block carrying Suzuri's branding — the one
`script/bundle-mac` selects. Do not switch the channel casually: `bundle-stable` is
still Zed's own metadata, so a stable build would install itself as `Zed.app`.

To release:

1. Bump `SUZURI_VERSION` in `crates/suzuri_update/src/suzuri_update.rs`.
2. Tag `suzuri-v<that same version>` and push the tag.

The two must agree: `SUZURI_VERSION` is what a running build compares against the
newest GitHub release, so a tag ahead of the constant ships a build that never
learns it is out of date. The `check-version` job in
`.github/workflows/suzuri-release.yml` fails the release early rather than letting
that happen.

**Suzuri's version is not the `version` in `crates/zed/Cargo.toml`.** That one is
upstream Zed's own (`Bump Zed to v1.17.0`), which upstream bumps on its own cadence
and every merge brings along — it tracks the Zed base this fork is built on, not
Suzuri's releases, and the two number lines are unrelated. `SUZURI_VERSION` lives in
a fork-owned file precisely so no merge can move it.

`suzuri_update` only *notifies*; it never installs. Zed's installer (`auto_update`)
is not wired up, and adopting it would need, at minimum, its hardcoded `Zed` DMG
mount path in `install_release_macos` reconciled with `bundle-mac`'s `-volname Suzuri`,
plus signing secrets present on every release build.
