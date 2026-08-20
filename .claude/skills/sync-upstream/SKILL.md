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

Known-failing on a clean upstream tree: `project_panel tests::undo::undo_create_dirty_file`.
Before blaming a merge for any failure, stash the merge and confirm the test fails without it.

## 5. Smoke-test the real app

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
