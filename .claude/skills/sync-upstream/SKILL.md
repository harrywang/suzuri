---
name: sync-upstream
description: Merge zed-industries/zed into Suzuri — report drift, preview conflicts, merge on a branch, verify in the order that surfaces API drift first, then fast-forward main. Use when the user says "sync upstream", "merge upstream", "pull from zed", "how far behind are we", or types /sync-upstream.
---

# Sync with upstream Zed

Suzuri tracks Zed's `main`. Zed lands roughly 15 commits a day, so **merge weekly**;
a month is the hard ceiling. Conflict pain grows faster than linearly with drift —
reconciling three overlapping refactors at once is far worse than three merges of one
refactor each.

**The conflict-free merge is the dangerous one.** A 41-commit merge once landed clean
and still failed to compile because `image_resolver` had gained an `&App` parameter.
Conflicts are loud; API drift is silent. That is why the check order below starts with
the fork's own crates.

## 1. Measure the drift

```sh
git remote get-url upstream >/dev/null 2>&1 || git remote add upstream https://github.com/zed-industries/zed.git
git fetch upstream main
git rev-list --count HEAD..upstream/main
git log -1 --format=%ad upstream/main
```

Report the count and how long it has been. Then preview the damage without committing:

```sh
git merge --no-commit --no-ff upstream/main >/dev/null 2>&1
git diff --name-only --diff-filter=U
git merge --abort
```

Also check whether upstream touched the files the fork's crates *call into* — drift here
produces no conflicts at all:

```sh
for f in crates/editor/src/editor.rs crates/editor/src/display_map.rs \
         crates/language/src/language.rs crates/markdown/src/markdown.rs \
         crates/workspace/src/item.rs; do
  n=$(git rev-list --count HEAD..upstream/main -- "$f")
  [ "$n" -gt 0 ] && echo "$n  $f"
done
```

Summarize for the user before merging: N commits, which conflicts, which risky APIs.

## 2. Merge on a branch

Never merge straight onto `main`.

```sh
git checkout -b merge-upstream-$(date +%Y-%m-%d)
git merge --no-ff upstream/main
```

## 3. Resolve

Conflicts recur in the same handful of registration points, because the fork's features
live in their own crates and only *register* into shared files:

| File | What is ours |
| --- | --- |
| `Cargo.toml` | `pdf_viewer` / `typeset_preview` member and path entries |
| `crates/zed/src/main.rs` | `markdown_live_preview::init`, `pdf_viewer::init`, `typeset_preview::init` |
| `crates/zed/src/zed.rs` | `PdfViewToolbarControls` in the toolbar block |
| `crates/zed/src/zed/quick_action_bar/preview.rs` | `PreviewTarget::Typeset` variant and its arms |
| `crates/project_panel/src/project_panel.rs` | header buttons, refresh, typeset menu entry |
| `crates/languages/src/lib.rs` | `markdown_oxide` module and its markdown adapter |
| `assets/settings/default.json` | `autosave` default, live-preview settings |

Rules of thumb: **take upstream's rename, keep our addition** (e.g. upstream renamed
`csv_preview` → `tabular_data_preview` while we had added a `Typeset` variant beside it).
Never resolve by deleting an upstream change you do not understand — read the upstream
commit first (`git log upstream/main -1 -- <file>`).

`git rerere` is enabled, so previously recorded resolutions replay automatically.
`README.md` carries `merge=ours` and never conflicts; that driver needs
`git config merge.ours.driver true` once per clone.

## 4. Verify, in this order

```sh
# 1. Fork-owned crates first — API drift surfaces here and nowhere earlier.
cargo check -p markdown_live_preview -p pdf_viewer -p typeset_preview -p languages

# 2. The app and the shared crates the fork patches.
#    DEVELOPER_DIR is required: CommandLineTools lacks the Metal shader compiler.
DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer cargo check -p zed -p editor -p project_panel

# 3. Contract tests catch semantic drift a clean compile hides.
cargo nextest run -p markdown_live_preview -p pdf_viewer -p typeset_preview

# 4. Shared crates.
cargo nextest run -p project_panel -p languages
```

The fork breaks a fixed set of upstream tests on every merge — 11 save-prompt tests in
`workspace`/`zed` plus `project_panel tests::undo::undo_create_dirty_file` (the fork's
`autosave` default in `assets/settings/default.json`), `zed tests::test_action_namespaces`
(the fork's two extra action namespaces), and the `collab *_postgres` tests (no local
database). See "Tests the fork breaks permanently" in CLAUDE.md. Before blaming a merge for
anything else, check whether the fork touches the files involved at all:
`git diff $(git merge-base upstream/main main) main -- <path>`.

## 5. Smoke-test the real app

Every sync gets a GUI check, because the nextest gates are text-only: they never paint,
so a rendering regression passes them. A concealment placeholder that painted a visible
blank at every hidden marker got past them exactly that way. The check comes in two tiers.

**Always: the visual test runner.** It renders offscreen with real Metal, needs no human
and no Screen Recording permission, so it runs in a background job too. It covers live
preview rendering, link clicks and source reveal, the citation pipeline, and math:

```sh
DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer \
  cargo run -p zed --bin zed_visual_test_runner --features visual-tests \
  && echo OK || echo FAILED
```

A baseline mismatch is a finding, not something to re-record. Look at the diff image
first, and run with `UPDATE_BASELINE=1` only when the change is upstream's intended look.

**Also bundle and test by hand** when any of these holds, and otherwise offer it rather
than doing it:

- the merge touched what the fork patches for rendering or input: `crates/editor/src/element.rs`,
  `display_map*`, `crates/gpui/`, `crates/workspace/`, or the `script/bundle-*` scripts;
- a conflict was resolved in a fork-patched Rust file (a resolution can compile, pass the
  tests and still be wrong on screen);
- a release tag follows this merge.

The runner does not cover the PDF viewer, Typst preview, or the project panel, so these
are the cases where it is not enough. Installing over `/Applications/Suzuri.app` replaces
the copy the user is using, so ask before doing it from a background job.

```sh
MACOS_SIGNING_KEY=17F4C95D6660786229871DFD1B491A1AC2A326DB \
  DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer ./script/bundle-mac \
  && rm -rf /Applications/Suzuri.app \
  && cp -R target/aarch64-apple-darwin/release/dmg/Suzuri.app /Applications/
```

`MACOS_SIGNING_KEY` must be the certificate's SHA-1 hash, not its name: `bundle-mac`
expands it unquoted, so a name with spaces word-splits and codesign dies mid-script.
Wrap long builds with `&& echo OK || echo FAILED` — a trailing `; echo $?` hides failure.

Then exercise the fork's features by hand: live preview reveal-at-cursor, an editable
table, a PDF in the viewer, a Typst live preview, the panel's refresh button.

## 6. Land it

```sh
git checkout main
git merge --ff-only merge-upstream-<date>
git push origin main
git branch -d merge-upstream-<date>
```

Offer a release tag (`suzuri-vX.Y.Z`) only if the user wants the merge shipped — tagging
triggers the full signed-and-notarized build, which takes hours on hosted runners.

## 7. Sweep stale build artifacts

A merge bumps dependency versions, and Cargo never deletes the artifacts of the old
ones — `target/debug` once reached 272G this way. Sweep every target dir that still
exists, keeping anything touched in the last 30 days so the hot cache survives:

```sh
for d in . .claude/worktrees/* ../suzuri-headings ../suzuri-upstream; do
  [ -d "$d/target" ] && cargo sweep --time 30 "$d"
done
df -h / | tail -1
```

`cargo-sweep` is installed via `cargo install cargo-sweep`. Dependencies rebuild from
the shared `sccache` cache (`rustc-wrapper` in `~/.cargo/config.toml`), so a sweep
that removes too much costs minutes, not the near-full rebuild a `rm -rf` does.
