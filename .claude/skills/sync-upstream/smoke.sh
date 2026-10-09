#!/bin/bash
# Smoke-tests a Suzuri build through the real GUI: live preview, the PDF viewer, Typst
# live preview, and the project panel. Runs an isolated nightly-channel instance with
# its own data dir, drives it with ad-hoc keymap chords, and checks window titles, files
# on disk and Zed.log. Exits non-zero when a check fails; screenshots are left in
# <work-dir>/shots for review.
#
# usage: smoke.sh [app-binary] [work-dir]
#   app-binary defaults to /Applications/Suzuri.app/Contents/MacOS/zed
#   work-dir   defaults to a fresh mktemp dir
#
# It takes keyboard focus for about a minute. Needs Accessibility permission for the
# terminal (osascript), plus screencapture, swiftc, typst and pdftotext.
set -uo pipefail

APP=${1:-/Applications/Suzuri.app/Contents/MacOS/zed}
T=${2:-$(mktemp -d -t suzuri-smoke)}
LOG="$HOME/Library/Logs/Zed/Zed.log"
SKILL_DIR=$(cd "$(dirname "$0")" && pwd)
failures=0
pass() { echo "PASS $*"; }
fail() { echo "FAIL $*"; failures=$((failures + 1)); }

rm -rf "$T/vault" "$T/data" "$T/shots"
mkdir -p "$T/vault" "$T/data/config" "$T/shots"
cat > "$T/vault/note.md" <<'EOF'
# Smoke heading

Some **bold** text, *italic* text, `code`, and a [link](other.md).

| Name | Value |
| ---- | ----- |
| alpha | 1 |
| beta | 2 |

Inline math $e^{i\pi} + 1 = 0$ and a list:

- first item
- second item

> A block quote.
EOF
echo '# Other' > "$T/vault/other.md"
cat > "$T/vault/paper.typ" <<'EOF'
= Smoke Paper
Hello from the merge smoke test. $ integral_0^1 x dif x = 1/2 $
EOF
printf '= Sample PDF\nOpened directly in the viewer.\n' > "$T/sample.typ"
typst compile "$T/sample.typ" "$T/vault/sample.pdf" || { echo "typst missing or failed"; exit 2; }

# auto_update off: a nightly-channel build would otherwise ask Zed's server and pull Zed.
# trust_all_worktrees: the Restricted Mode modal otherwise swallows the first chords.
cat > "$T/data/config/settings.json" <<'EOF'
{ "auto_update": false, "session": { "trust_all_worktrees": true }, "restore_on_startup": "none",
  "telemetry": { "diagnostics": false, "metrics": false } }
EOF
# Chords on page-up/page-down/home/end are the keys known to reach Zed; F13+ and
# typing into the command palette do not.
cat > "$T/data/config/keymap.json" <<'EOF'
[
  { "context": "Workspace", "bindings": {
      "ctrl-alt-cmd-pageup": "typeset_preview::OpenLivePreview",
      "ctrl-alt-cmd-end": "pane::ActivatePreviousItem",
      "ctrl-alt-cmd-home": "project_panel::ToggleFocus" } },
  { "context": "ProjectPanel", "bindings": {
      "ctrl-alt-cmd-pagedown": "project_panel::RefreshFileTree" } }
]
EOF

WINFO="$T/winfo"
swiftc -O -o "$WINFO" "$SKILL_DIR/winfo.swift" 2>/dev/null || { echo "swiftc failed"; exit 2; }

log_start=$(wc -c < "$LOG" 2>/dev/null || echo 0)
ZED_RELEASE_CHANNEL=nightly ZED_LOG=info,pdf_viewer=debug,typeset_preview=debug \
  nohup "$APP" --user-data-dir "$T/data" "$T/vault" \
  "$T/vault/paper.typ" "$T/vault/sample.pdf" "$T/vault/note.md" > "$T/stderr.log" 2>&1 &
launcher=$!
disown "$launcher"
sleep 15
pid=$(pgrep -f "$APP --user-data-dir $T/data" | head -1)
pid=${pid:-$launcher}
if kill -0 "$pid" 2>/dev/null; then pass "launched (pid $pid)"; else fail "app exited during launch"; tail -20 "$T/stderr.log"; exit 1; fi

# Another app (the terminal, a finishing build) can take focus back mid-run, and a chord
# sent then lands in the wrong app, so focus is retried and re-checked before every key.
front() {
  local attempt
  for attempt in 1 2 3 4 5; do
    osascript -e "tell application \"System Events\" to set frontmost of (first process whose unix id is $pid) to true" >/dev/null 2>&1
    sleep 1
    [ "$(osascript -e 'tell application "System Events" to unix id of first process whose frontmost is true' 2>/dev/null)" = "$pid" ] && return 0
  done
  return 1
}
# key codes: page-up 116, page-down 121, home 115, end 119
chord() {
  if front; then
    osascript -e "tell application \"System Events\" to key code $1 using {control down, option down, command down}"
  else
    fail "instance not frontmost; chord $1 not sent"
  fi
}
title() {
  osascript -e "tell application \"System Events\" to get name of front window of (first process whose unix id is $pid)" 2>/dev/null
}
shot() {
  local id
  id=$("$WINFO" list | awk -F'|' -v p="$pid" '$2==p {print $3; exit}')
  [ -n "$id" ] && screencapture -x -o -l "$id" "$T/shots/$1.png"
}
expect_title() {
  local actual; actual=$(title)
  if [[ "$actual" == *"$1" ]]; then pass "$2 ($actual)"; else fail "$2: window title is '$actual', expected '*$1'"; fi
  shot "$3"
}

# Files passed on the command line open concurrently, so the tab order and the active
# tab differ between runs. Step through the tabs until the wanted one is active.
goto() {
  local step
  for step in 1 2 3 4; do
    [[ "$(title)" == *"$1" ]] && break
    chord 119; sleep 3
  done
  sleep 2
  expect_title "$1" "$2" "$3"
}

goto note.md "live preview tab" 01-note
goto sample.pdf "PDF viewer tab" 02-pdf
goto paper.typ "paper.typ tab" 03-typ
chord 116; sleep 12; expect_title paper.pdf "Typst preview opened the compiled PDF" 04-typst-preview
if pdftotext "$T/vault/paper.pdf" - 2>/dev/null | grep -q "Smoke Paper"; then
  pass "Typst preview compiled paper.pdf"
else
  fail "paper.pdf missing or without the expected text"
fi
chord 115; sleep 2; chord 121; sleep 3; shot 05-panel-refresh
if kill -0 "$pid" 2>/dev/null; then pass "alive after panel focus and refresh"; else fail "app exited during the run"; fi

kill -TERM "$pid" 2>/dev/null; sleep 3; kill -0 "$pid" 2>/dev/null && kill -KILL "$pid"
# Zed.log rotates to Zed.log.old at about 1 MB, which can happen mid-run.
if [ "$(wc -c < "$LOG")" -lt "$log_start" ]; then
  { tail -c +"$((log_start + 1))" "$LOG.old"; cat "$LOG"; } > "$T/run.log"
else
  tail -c +"$((log_start + 1))" "$LOG" > "$T/run.log"
fi

rendered=$(grep -c 'pdf_viewer: page 1 rendered' "$T/run.log")
if [ "$rendered" -ge 2 ]; then pass "PDF viewer rendered pages ($rendered renders)"; else fail "PDF viewer rendered $rendered pages, expected at least 2 (sample.pdf and paper.pdf)"; fi
if grep -qi 'panic' "$T/run.log" "$T/stderr.log"; then fail "panic logged"; grep -i -m5 panic "$T/run.log" "$T/stderr.log"; else pass "no panic"; fi

# Zed.log is shared with any other running instance, so these are listed for review,
# not failed on.
echo "--- ERROR/WARN lines during the run (any instance):"
grep -E ' (ERROR|WARN) ' "$T/run.log" | cut -c1-220 | head -20

echo "--- screenshots: $T/shots"
echo "--- $failures failure(s)"
[ "$failures" -eq 0 ]
