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

Never merge straight onto `main`. Record the visual-test baselines (step 5) after creating
the branch and before running `git merge`, because they have to show the pre-merge app.

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

Claude runs this, every sync; do not hand it to the user. The nextest gates are
text-only: they never paint, so a rendering regression passes them. A concealment
placeholder that painted a visible blank at every hidden marker got past them exactly
that way. Two automated layers cover what the tests cannot.

**Visual test runner.** It renders offscreen with real Metal, needs no human and no
Screen Recording permission, so it runs in a background job too. It covers live preview
rendering, link clicks and source reveal, the citation pipeline, and math.

Its baselines are gitignored (`crates/zed/test_fixtures/visual_tests/`, upstream's
choice), so a fresh worktree has none and every test "fails" with `Baseline not found`,
which proves nothing. Record them from the pre-merge commit, **before step 2's
`git merge`**, in the branch's own worktree, then compare after verifying:

```sh
# Before merging: record what main looks like.
UPDATE_BASELINE=1 DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer \
  cargo run -p zed --bin zed_visual_test_runner --features visual-tests

# After step 4: compare the merge against it.
DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer \
  cargo run -p zed --bin zed_visual_test_runner --features visual-tests \
  && echo OK || echo FAILED
```

A mismatch is a finding, not something to re-record: open the `_diff.png` and the new
screenshot in `target/visual_tests/`. The fork-relevant tests are `math_rendering`,
`link_click`, `citation_pipeline`, `project_panel` and `workspace_with_editor`. The agent
sidebar and settings tests drift with upstream's own UI and with machine state (an
"Import Threads" banner appears once another release channel has thread data), so judge
those from the diff image rather than the percentage. Do not borrow baselines from
another checkout; they are as old as whoever last recorded them.

**GUI smoke script.** The runner never opens a PDF, compiles Typst or touches the
project panel. [`smoke.sh`](smoke.sh) launches the real app as an isolated
nightly-channel instance (its own data dir, auto-update off, so it runs safely beside
the user's open Suzuri), drives it with keymap chords, and checks window titles, the
compiled PDF on disk and `Zed.log`. It prints PASS/FAIL per check and exits non-zero on
any failure, leaving screenshots for review:

```sh
DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer cargo build -p zed \
  && .claude/skills/sync-upstream/smoke.sh "$PWD/target/debug/zed" "$CLAUDE_JOB_DIR/tmp/smoke"
```

It takes keyboard focus for about a minute; do not run it while another job is driving
the GUI. Look at `shots/01-note.png` yourself: titles prove the tab opened, not that live
preview concealed its markers. The ERROR/WARN lines it lists come from every running
instance and include network flakes (`tls handshake eof` on a GitHub download); a failure
is a regression only if it repeats on a rerun.

**Bundle** only when the merge touched `script/bundle-*`, a release tag follows, or the
user asks for the build installed, then point `smoke.sh` at the installed app (its
default) instead of the debug binary. Installing replaces the copy the user is running:
quit it first with `osascript -e 'tell application id "app.suzuri.Suzuri" to quit'`
(autosave is on) and relaunch it afterwards.

```sh
LK_CUSTOM_WEBRTC=$HOME/.cache/suzuri-webrtc/mac-arm64-release \
MACOS_SIGNING_KEY=17F4C95D6660786229871DFD1B491A1AC2A326DB \
  DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer ./script/bundle-mac \
  && rm -rf /Applications/Suzuri.app \
  && cp -R target/aarch64-apple-darwin/release/dmg/Suzuri.app /Applications/
```

`MACOS_SIGNING_KEY` must be the certificate's SHA-1 hash, not its name: `bundle-mac`
expands it unquoted, so a name with spaces word-splits and codesign dies mid-script.
`LK_CUSTOM_WEBRTC` skips the flaky ~1 GB WebRTC download; it must match the `WEBRTC_TAG`
of the pinned livekit revision. Wrap long builds with `&& echo OK || echo FAILED` — a
trailing `; echo $?` hides failure.

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
