#!/usr/bin/env python3
"""suzuri-agent-log - archive Claude Code agent transcripts into the project they belong to.

Claude Code keeps session transcripts under ~/.claude/projects/ for
`cleanupPeriodDays` (default 30) and then deletes them. This tool copies the
complete record into <project>/.suzuri/agent-logs/ before that happens.

A "complete record" is five things, not one file:

  ~/.claude/projects/<slug>/<session>.jsonl              main transcript
  ~/.claude/projects/<slug>/<session>/subagents/*        subagent transcripts
  ~/.claude/projects/<slug>/<session>/tool-results/*     externalized large outputs
  ~/.claude/file-history/<session>/*                     Write/Edit checkpoints
  (metadata distilled into meta.json)

Routing does NOT use the slug directory name. The slug is a lossy one-way
transform of the launch cwd - every non-alphanumeric byte becomes '-' - so
/x/foo-bar and /x/foo/bar collide into one directory. Routing instead reads the
`cwd` field inside the transcript, treats it as a SET (one session can move
between worktrees mid-run), and resolves each cwd to a project identity:

  git repo   -> (git-common-dir, root-commit)   identical across all worktrees
  otherwise  -> containment under the project root realpath

Resolution is frozen into the manifest at sync time, because a cwd that has since
been deleted can no longer be resolved by git and would otherwise become
permanently unattributable.

Usage
-----

Recording is off for every project until it is turned on:

    script/suzuri-agent-log.py enable    # opt in, install hooks, backfill
    script/suzuri-agent-log.py status    # archived vs pending
    script/suzuri-agent-log.py verify    # check the manifest hash chain
    script/suzuri-agent-log.py disable   # opt out, remove hooks, keep archive

`enable` installs SessionStart and SessionEnd hooks into the project's
.claude/settings.json, preserving any hooks already there. Both events are
needed: SessionEnd covers a normal exit, SessionStart sweeps sessions that were
killed before SessionEnd could fire.

What this does not capture: human keystrokes, paste versus typing, and files an
agent writes by any means other than the Write/Edit tools. A `cat > file <<EOF`
heredoc leaves no checkpoint and fires no tool hook, so tool-call capture alone
under-reports; only editor-side observation closes that gap.

Tests: python3 script/test_suzuri_agent_log.py
"""

import argparse
import calendar
import fcntl
import glob
import gzip
import hashlib
import json
import os
import shutil
import subprocess
import sys
import time
import uuid

SCHEMA_VERSION = 1
ARCHIVE_REL = os.path.join(".suzuri", "agent-logs")
GENESIS = "0" * 64

# Recording is opt-in per project. Nothing is copied until `enable` is run: the
# transcripts hold unredacted prompts, file contents, and shell output, so
# archiving them into a project folder must be a decision, never a default.
HOOK_EVENTS = ("SessionStart", "SessionEnd")
HOOK_MARKER = "suzuri-agent-log"


# --------------------------------------------------------------------------
# paths and normalization


def claude_home():
    return os.path.expanduser(os.environ.get("CLAUDE_CONFIG_DIR", "~/.claude"))


def projects_dir():
    return os.path.join(claude_home(), "projects")


def file_history_dir():
    return os.path.join(claude_home(), "file-history")


def norm(path):
    """realpath + case-fold.

    /tmp and /private/tmp are the same directory on macOS and APFS is
    case-insensitive; comparing raw strings silently splits one project into two
    archives.
    """
    if not path:
        return ""
    return os.path.normcase(os.path.realpath(os.path.expanduser(path)))


def sha256_file(path):
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def utc_iso(epoch=None):
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(epoch))


def parse_iso_utc(stamp):
    """Transcript timestamps are UTC ISO-8601; return epoch seconds.

    Never round-trip through local time. `time.timezone` is the standard-time
    offset, so using it during DST shifts every agent timestamp by an hour and
    silently moves human edits away from the agent activity they correlate with.
    """
    if not stamp:
        return None
    try:
        base = stamp.split(".")[0].rstrip("Z")
        return int(calendar.timegm(time.strptime(base, "%Y-%m-%dT%H:%M:%S")))
    except (ValueError, TypeError):
        return None


# --------------------------------------------------------------------------
# project identity


def git_out(cwd, *args):
    try:
        result = subprocess.run(
            ["git", "-C", cwd, *args], capture_output=True, text=True, timeout=15
        )
    except (OSError, subprocess.SubprocessError):
        return None
    return result.stdout.strip() if result.returncode == 0 else None


def project_identity(root):
    """Identity of the project rooted at `root`.

    For a git repo this is (common-dir, root-commit). The common dir is shared by
    the main checkout and every worktree, so worktree sessions fold in
    automatically, while an unrelated repo that merely shares a path prefix does
    not. Plain folders and note vaults - the common case for a writing project -
    fall back to the realpath.
    """
    root = norm(root)
    common = git_out(root, "rev-parse", "--path-format=absolute", "--git-common-dir")
    if common:
        return {
            "kind": "git",
            "common_dir": norm(common),
            "root_commit": git_out(root, "rev-list", "--max-parents=0", "HEAD") or None,
            "root": root,
        }
    return {"kind": "path", "common_dir": None, "root_commit": None, "root": root}


def cwd_belongs(cwd, identity):
    """Does a transcript's cwd belong to this project?

    Containment is the fallback, which is also what keeps a session attributable
    after its worktree has been deleted and git can no longer resolve it.
    """
    cwd_n = norm(cwd)
    if not cwd_n:
        return False
    if identity["kind"] == "git" and os.path.isdir(cwd_n):
        common = git_out(cwd_n, "rev-parse", "--path-format=absolute", "--git-common-dir")
        if common and norm(common) == identity["common_dir"]:
            return True
    root = identity["root"]
    return cwd_n == root or cwd_n.startswith(root + os.sep)


# --------------------------------------------------------------------------
# transcript reading


def read_transcript(path):
    """Single pass over a transcript.

    Malformed lines are counted, never fatal: a partially written transcript from
    a killed session must still archive.
    """
    info = {
        "session_id": None,
        "cwds": [],
        "branches": [],
        "versions": [],
        "entrypoints": [],
        "session_kinds": [],
        "permission_modes": [],
        "first_ts": None,
        "last_ts": None,
        "turns_user": 0,
        "turns_assistant": 0,
        "tools": {},
        "models": [],
        "cost_usd": None,
        "lines": 0,
        "unparseable": 0,
    }

    def note(key, value):
        if value and value not in info[key]:
            info[key].append(value)

    try:
        handle = open(path, encoding="utf-8", errors="replace")
    except OSError as err:
        info["error"] = str(err)
        return info

    with handle:
        for line in handle:
            info["lines"] += 1
            line = line.strip()
            if not line:
                continue
            try:
                record = json.loads(line)
            except (ValueError, TypeError):
                info["unparseable"] += 1
                continue
            if not isinstance(record, dict):
                info["unparseable"] += 1
                continue

            info["session_id"] = info["session_id"] or record.get("sessionId")
            note("cwds", record.get("cwd"))
            note("branches", record.get("gitBranch"))
            note("versions", record.get("version"))
            note("entrypoints", record.get("entrypoint"))
            note("session_kinds", record.get("sessionKind"))
            note("permission_modes", record.get("permissionMode"))

            stamp = record.get("timestamp")
            if stamp:
                if info["first_ts"] is None or stamp < info["first_ts"]:
                    info["first_ts"] = stamp
                if info["last_ts"] is None or stamp > info["last_ts"]:
                    info["last_ts"] = stamp

            if record.get("type") == "cost-state":
                info["cost_usd"] = record.get("totalCostUSD")
            elif record.get("type") == "user":
                info["turns_user"] += 1
            elif record.get("type") == "assistant":
                info["turns_assistant"] += 1

            message = record.get("message")
            if isinstance(message, dict):
                note("models", message.get("model"))
                content = message.get("content")
                if isinstance(content, list):
                    for block in content:
                        if isinstance(block, dict) and block.get("type") == "tool_use":
                            name = block.get("name") or "?"
                            info["tools"][name] = info["tools"].get(name, 0) + 1
    return info


def all_transcripts():
    """Every main transcript under ~/.claude/projects/, across every slug.

    Sessions for one project scatter across slugs - a session launched inside a
    worktree gets its own - so every slug must be scanned, not just the one whose
    name resembles the project path.
    """
    return sorted(glob.glob(os.path.join(projects_dir(), "*", "*.jsonl")))


# --------------------------------------------------------------------------
# archive layout


def archive_root(project_root):
    return os.path.join(norm(project_root), ARCHIVE_REL)


def write_json(path, payload):
    tmp = path + ".tmp"
    with open(tmp, "w", encoding="utf-8") as handle:
        json.dump(payload, handle, indent=1, sort_keys=True)
        handle.write("\n")
    os.replace(tmp, path)


def ensure_archive(project_root):
    """Create the archive and make it gitignored by default.

    Transcripts are stored by Claude Code in plaintext and contain far more than
    prompts: every file read (including files outside the project), all shell
    output, and fetched web pages. Nothing is redacted on disk. Committing them by
    accident would publish all of it, so the archive ships ignoring itself and
    committing is an explicit opt-in.
    """
    root = archive_root(project_root)
    os.makedirs(os.path.join(root, "sessions"), exist_ok=True)

    gitignore = os.path.join(root, ".gitignore")
    if not os.path.exists(gitignore):
        with open(gitignore, "w", encoding="utf-8") as handle:
            handle.write(
                "# Agent transcripts are stored in plaintext and contain every file read,\n"
                "# all shell output, and fetched pages - not just prompts. Nothing is\n"
                "# redacted. Committing this directory publishes all of it.\n"
                "#\n"
                "# manifest.jsonl carries metadata and hashes only (no prose), so it is the\n"
                "# safe thing to share. To version it, add an exception below:\n"
                "#   !manifest.jsonl\n"
                "*\n"
            )

    identity_path = os.path.join(root, "project.json")
    if not os.path.exists(identity_path):
        identity = project_identity(project_root)
        identity["project_uuid"] = str(uuid.uuid4())
        identity["created_at"] = utc_iso()
        identity["schema"] = SCHEMA_VERSION
        identity["enabled"] = False
        write_json(identity_path, identity)
    return root


def project_config(project_root):
    path = os.path.join(archive_root(project_root), "project.json")
    try:
        with open(path, encoding="utf-8") as handle:
            config = json.load(handle)
    except (OSError, ValueError):
        return {}
    return config if isinstance(config, dict) else {}


def is_enabled(project_root):
    return bool(project_config(project_root).get("enabled"))


def set_enabled(project_root, enabled):
    ensure_archive(project_root)
    path = os.path.join(archive_root(project_root), "project.json")
    config = project_config(project_root)
    config["enabled"] = bool(enabled)
    config["enabled_changed_at"] = utc_iso()
    write_json(path, config)


# --------------------------------------------------------------------------
# hook installation


def settings_path(project_root):
    return os.path.join(norm(project_root), ".claude", "settings.json")


def load_settings(project_root):
    path = settings_path(project_root)
    try:
        with open(path, encoding="utf-8") as handle:
            settings = json.load(handle)
    except (OSError, ValueError):
        return {}
    return settings if isinstance(settings, dict) else {}


def hook_command():
    return "%s hook" % os.path.abspath(__file__)


def install_hooks(project_root):
    """Add our SessionStart/SessionEnd hooks, preserving anything already there.

    Both events are needed: SessionEnd covers the normal exit, SessionStart
    sweeps sessions that were killed or crashed before SessionEnd could fire.
    """
    settings = load_settings(project_root)
    hooks = settings.setdefault("hooks", {})
    added = []
    for event in HOOK_EVENTS:
        matchers = hooks.setdefault(event, [])
        if any(HOOK_MARKER in json.dumps(entry) for entry in matchers):
            continue
        matchers.append({"hooks": [{"type": "command", "command": hook_command()}]})
        added.append(event)
    path = settings_path(project_root)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    write_json(path, settings)
    return added


def remove_hooks(project_root):
    """Remove only our entries; leave every other hook untouched."""
    settings = load_settings(project_root)
    hooks = settings.get("hooks")
    if not isinstance(hooks, dict):
        return []
    removed = []
    for event in HOOK_EVENTS:
        matchers = hooks.get(event)
        if not isinstance(matchers, list):
            continue
        kept = [entry for entry in matchers if HOOK_MARKER not in json.dumps(entry)]
        if len(kept) != len(matchers):
            removed.append(event)
        if kept:
            hooks[event] = kept
        else:
            hooks.pop(event, None)
    if not hooks:
        settings.pop("hooks", None)
    if os.path.exists(settings_path(project_root)):
        write_json(settings_path(project_root), settings)
    return removed


def gzip_copy(src, dst):
    os.makedirs(os.path.dirname(dst), exist_ok=True)
    with open(src, "rb") as fin, gzip.open(dst, "wb") as fout:
        shutil.copyfileobj(fin, fout)


def session_dirname(info, session_id):
    epoch = parse_iso_utc(info.get("first_ts"))
    prefix = time.strftime("%Y-%m-%dT%H-%M-%SZ", time.gmtime(epoch)) if epoch else "unknown"
    return "%s_%s" % (prefix, (session_id or "unknown")[:8])


# --------------------------------------------------------------------------
# manifest (append-only, hash-chained)


def manifest_path(project_root):
    return os.path.join(archive_root(project_root), "manifest.jsonl")


def manifest_records(project_root):
    path = manifest_path(project_root)
    if not os.path.exists(path):
        return []
    records = []
    with open(path, encoding="utf-8", errors="replace") as handle:
        for line in handle:
            line = line.strip()
            if not line:
                continue
            try:
                records.append(json.loads(line))
            except ValueError:
                continue
    return records


def manifest_append(project_root, entry):
    """Append one chained record.

    Each line carries the sha256 of the previous line, so a retroactive edit or a
    deletion is detectable. The chain requires read-last-line and append to be
    serialized, hence the lock - several sessions can end at once. It is held for
    microseconds and only by this tool.
    """
    path = manifest_path(project_root)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "a+", encoding="utf-8") as handle:
        fcntl.flock(handle.fileno(), fcntl.LOCK_EX)
        try:
            handle.seek(0)
            previous = GENESIS
            count = 0
            for line in handle:
                line = line.strip()
                if line:
                    previous = hashlib.sha256(line.encode("utf-8")).hexdigest()
                    count += 1
            entry = dict(entry)
            entry["v"] = SCHEMA_VERSION
            entry["seq"] = count
            entry["prev"] = previous
            entry["sig"] = None  # reserved: signing arrives with the shadow repo
            handle.write(json.dumps(entry, sort_keys=True) + "\n")
            handle.flush()
            os.fsync(handle.fileno())
        finally:
            fcntl.flock(handle.fileno(), fcntl.LOCK_UN)
    return entry


def verify_chain(project_root):
    """Recompute the chain. Returns (ok, first_bad_seq)."""
    path = manifest_path(project_root)
    if not os.path.exists(path):
        return True, None
    previous = GENESIS
    with open(path, encoding="utf-8", errors="replace") as handle:
        for index, line in enumerate(handle):
            line = line.strip()
            if not line:
                continue
            try:
                record = json.loads(line)
            except ValueError:
                return False, index
            if record.get("prev") != previous:
                return False, record.get("seq", index)
            previous = hashlib.sha256(line.encode("utf-8")).hexdigest()
    return True, None


# --------------------------------------------------------------------------
# archiving


def archive_session(project_root, transcript_path, info, identity, primary):
    """Copy one session's five locations into the project archive.

    Returns the manifest entry, or None if it is already archived unchanged.
    Freshness is decided by content hash, never mtime: Claude Code rewrites
    trailing `cost-state` and `last-prompt` lines while a session stays open, so
    an mtime-based sync re-copies idle sessions forever.
    """
    session_id = info.get("session_id") or os.path.basename(transcript_path)[: -len(".jsonl")]
    root = ensure_archive(project_root)
    dest = os.path.join(root, "sessions", session_dirname(info, session_id))

    source_hash = sha256_file(transcript_path)
    for record in manifest_records(project_root):
        if (
            record.get("session_id") == session_id
            and record.get("transcript_sha256") == source_hash
        ):
            return None

    os.makedirs(dest, exist_ok=True)
    gzip_copy(transcript_path, os.path.join(dest, "transcript.jsonl.gz"))

    slug_dir = os.path.dirname(transcript_path)
    sidecar = os.path.join(slug_dir, session_id)
    copied = {"subagents": 0, "tool_results": 0, "file_history": 0}

    for name, key in (("subagents", "subagents"), ("tool-results", "tool_results")):
        source = os.path.join(sidecar, name)
        if not os.path.isdir(source):
            continue
        for entry in sorted(os.listdir(source)):
            src = os.path.join(source, entry)
            if not os.path.isfile(src):
                continue
            if entry.endswith(".meta.json"):
                # tiny, and worth keeping readable: agentType, description, depth
                os.makedirs(os.path.join(dest, name), exist_ok=True)
                shutil.copy2(src, os.path.join(dest, name, entry))
            else:
                gzip_copy(src, os.path.join(dest, name, entry + ".gz"))
            copied[key] += 1

    history = os.path.join(file_history_dir(), session_id)
    if os.path.isdir(history):
        for entry in sorted(os.listdir(history)):
            src = os.path.join(history, entry)
            if os.path.isfile(src):
                gzip_copy(src, os.path.join(dest, "file-history", entry + ".gz"))
                copied["file_history"] += 1

    write_json(
        os.path.join(dest, "meta.json"),
        {
            "session_id": session_id,
            "cwds": info["cwds"],
            "branches": info["branches"],
            "versions": info["versions"],
            "entrypoints": info["entrypoints"],
            "session_kinds": info["session_kinds"],
            "permission_modes": info["permission_modes"],
            "models": info["models"],
            "first_ts": info["first_ts"],
            "last_ts": info["last_ts"],
            "turns_user": info["turns_user"],
            "turns_assistant": info["turns_assistant"],
            "tools": info["tools"],
            "cost_usd": info["cost_usd"],
            "unparseable_lines": info["unparseable"],
            "source_slug": os.path.basename(slug_dir),
            "primary_project": primary,
            "archived_at": utc_iso(),
        },
    )

    return manifest_append(
        project_root,
        {
            "ts": utc_iso(),
            "session_id": session_id,
            "archived_dir": os.path.relpath(dest, root),
            "transcript_sha256": source_hash,
            "transcript_bytes": os.path.getsize(transcript_path),
            "cwds": info["cwds"],
            "primary_project": primary,
            "is_primary": primary == identity["root"],
            "first_ts": info["first_ts"],
            "last_ts": info["last_ts"],
            "turns": info["turns_user"] + info["turns_assistant"],
            "tools": info["tools"],
            "cost_usd": info["cost_usd"],
            "sidecars": copied,
            "source_slug": os.path.basename(slug_dir),
        },
    )


def sync_project(project_root, quiet=False, force=False):
    project_root = norm(project_root)
    if not force and not is_enabled(project_root):
        if not quiet:
            print(
                "recording is off for %s\n"
                "nothing was copied. turn it on with:\n"
                "  %s enable --project %s"
                % (project_root, os.path.abspath(__file__), project_root)
            )
        return 0

    identity = project_identity(project_root)
    ensure_archive(project_root)

    archived, current, foreign, cross = 0, 0, 0, 0
    for transcript in all_transcripts():
        info = read_transcript(transcript)
        if not info["cwds"]:
            continue
        if not any(cwd_belongs(cwd, identity) for cwd in info["cwds"]):
            foreign += 1
            continue

        # The launch cwd is the first one seen; that project owns the session.
        launch = info["cwds"][0]
        primary = identity["root"] if cwd_belongs(launch, identity) else norm(launch)
        if primary != identity["root"]:
            cross += 1

        entry = archive_session(project_root, transcript, info, identity, primary)
        if entry is None:
            current += 1
        else:
            archived += 1
            if not quiet:
                print(
                    "  archived %s  (%d turns)  %s"
                    % (entry["session_id"][:8], entry["turns"], entry["archived_dir"])
                )

    render(project_root)
    if not quiet:
        ok, bad = verify_chain(project_root)
        print(
            "\n  %d newly archived, %d already current, %d foreign skipped"
            % (archived, current, foreign)
        )
        if cross:
            print("  %d session(s) started in another project (recorded, not owned)" % cross)
        print("  manifest chain: %s" % ("intact" if ok else "BROKEN at seq %s" % bad))
        print("  archive: %s" % archive_root(project_root))
    return archived


# --------------------------------------------------------------------------
# rendering


def render(project_root):
    """Regenerate AUTHORING.md from the manifest.

    Written on demand rather than on a timer: the project folder is watched by the
    editor, and rewriting a markdown file inside it while a buffer has it open
    causes reload churn.
    """
    root = archive_root(project_root)
    records = manifest_records(project_root)
    latest = {}
    for record in records:
        latest[record.get("session_id")] = record
    sessions = sorted(latest.values(), key=lambda r: r.get("first_ts") or "")

    ok, bad = verify_chain(project_root)
    lines = [
        "# Authoring record",
        "",
        "Agent sessions archived for `%s`." % os.path.basename(norm(project_root)),
        "",
        "Generated by `script/suzuri-agent-log.py`. The authoritative record is",
        "`manifest.jsonl` (append-only, hash-chained); this file renders it.",
        "",
        "- sessions archived: **%d**" % len(sessions),
        "- manifest integrity: **%s**" % ("intact" if ok else "BROKEN at seq %s" % bad),
        "- last updated: %s" % utc_iso(),
        "",
        "## Sessions",
        "",
        "| started (UTC) | session | kind | turns | tools | cost | owned |",
        "| --- | --- | --- | --- | --- | --- | --- |",
    ]

    total_cost = 0.0
    for record in sessions:
        tools = record.get("tools") or {}
        tool_text = ", ".join("%s x%d" % (k, v) for k, v in sorted(tools.items())) or "-"
        cost = record.get("cost_usd")
        if isinstance(cost, (int, float)):
            total_cost += cost

        # sessionKind is only present on background sessions; entrypoint is
        # always there, so it is the more useful fallback.
        kind = "?"
        meta_path = os.path.join(root, record.get("archived_dir") or "", "meta.json")
        if os.path.exists(meta_path):
            try:
                with open(meta_path, encoding="utf-8") as handle:
                    meta = json.load(handle)
                labels = meta.get("session_kinds") or meta.get("entrypoints") or []
                kind = ", ".join(labels) if labels else "?"
            except (OSError, ValueError):
                pass

        lines.append(
            "| %s | `%s` | %s | %d | %s | %s | %s |"
            % (
                (record.get("first_ts") or "?")[:19].replace("T", " "),
                (record.get("session_id") or "?")[:8],
                kind,
                record.get("turns", 0),
                tool_text,
                ("$%.2f" % cost) if isinstance(cost, (int, float)) else "-",
                "yes" if record.get("is_primary") else "no (started elsewhere)",
            )
        )

    lines += ["", "Total recorded cost: $%.2f" % total_cost, ""]

    foreign = [r for r in sessions if not r.get("is_primary")]
    if foreign:
        lines += [
            "## Sessions started in another project",
            "",
            "These touched this project but were launched elsewhere, so another",
            "archive owns the full record. They are listed so the trail is not",
            "silently incomplete.",
            "",
        ]
        for record in foreign:
            lines.append(
                "- `%s` - launched in `%s`"
                % ((record.get("session_id") or "?")[:8], record.get("primary_project"))
            )
        lines.append("")

    lines += [
        "## What is and is not captured",
        "",
        "Captured: every Claude Code session routed to this project, with subagent",
        "transcripts, externalized tool results, and Write/Edit checkpoints.",
        "",
        "Not captured: human keystrokes, paste versus typing, and files written by",
        "means other than the Write/Edit tools - a `cat > file` heredoc leaves no",
        "checkpoint and fires no tool hook. Those require editor-side capture.",
        "",
    ]

    with open(os.path.join(root, "AUTHORING.md"), "w", encoding="utf-8") as handle:
        handle.write("\n".join(lines))


# --------------------------------------------------------------------------
# commands


def cmd_sync(args):
    sync_project(args.project or os.getcwd())
    return 0


def cmd_enable(args):
    project_root = norm(args.project or os.getcwd())
    set_enabled(project_root, True)
    added = install_hooks(project_root)
    print("recording ON for %s" % project_root)
    print("  archive : %s  (gitignored)" % archive_root(project_root))
    print("  hooks   : %s" % (", ".join(added) if added else "already installed"))
    # The hook stores this script's absolute path. A worktree is temporary, so a
    # path inside one leaves a dangling hook the moment the worktree is removed.
    if os.path.join(".claude", "worktrees") in os.path.abspath(__file__):
        print(
            "\n  warning: hooks now point at a copy of this script inside a git\n"
            "  worktree, which will break when that worktree is removed. Re-run\n"
            "  `enable` from your main checkout to repoint them."
        )
    print("\nbackfilling sessions already on disk...")
    sync_project(project_root)
    return 0


def cmd_disable(args):
    project_root = norm(args.project or os.getcwd())
    set_enabled(project_root, False)
    removed = remove_hooks(project_root)
    print("recording OFF for %s" % project_root)
    print("  hooks removed: %s" % (", ".join(removed) if removed else "none were installed"))
    print("  the existing archive was left in place; delete %s to discard it."
          % archive_root(project_root))
    return 0


def cmd_status(args):
    project_root = norm(args.project or os.getcwd())
    identity = project_identity(project_root)
    print("project : %s" % project_root)
    print("identity: %s (%s)" % (identity["common_dir"] or identity["root"], identity["kind"]))
    print("recording: %s" % ("ON" if is_enabled(project_root) else "OFF (default)"))

    known = {r.get("session_id") for r in manifest_records(project_root)}
    available, foreign = [], 0
    for transcript in all_transcripts():
        info = read_transcript(transcript)
        if info["cwds"] and any(cwd_belongs(c, identity) for c in info["cwds"]):
            available.append(info["session_id"])
        else:
            foreign += 1

    pending = [s for s in available if s not in known]
    print("\nsessions on disk for this project : %d" % len(available))
    print("already archived                  : %d" % len(known))
    print("pending sync                      : %d" % len(pending))
    print("other projects' sessions skipped  : %d" % foreign)
    ok, bad = verify_chain(project_root)
    print("manifest chain                    : %s" % ("intact" if ok else "BROKEN at seq %s" % bad))
    return 0


def cmd_verify(args):
    project_root = norm(args.project or os.getcwd())
    ok, bad = verify_chain(project_root)
    print("manifest chain: %s" % ("intact" if ok else "BROKEN at seq %s" % bad))
    return 0 if ok else 1


def cmd_render(args):
    project_root = norm(args.project or os.getcwd())
    render(project_root)
    print("wrote %s" % os.path.join(archive_root(project_root), "AUTHORING.md"))
    return 0


def cmd_hook(args):
    """Hook entry point; reads Claude Code's JSON payload on stdin.

    SessionEnd syncs the session that just finished. SessionStart sweeps, because
    SessionEnd does not fire when the process is killed or crashes - without the
    sweep those sessions are never archived and then expire.
    """
    payload = {}
    try:
        raw = sys.stdin.read()
        if raw.strip():
            payload = json.loads(raw)
    except (ValueError, OSError):
        payload = {}
    if not isinstance(payload, dict):
        payload = {}

    project_root = payload.get("cwd") or args.project or os.getcwd()
    if not is_enabled(project_root):
        return 0  # opt-in: a project that never enabled recording is left alone
    try:
        sync_project(project_root, quiet=True)
    except Exception as err:  # a hook must never break the user's session
        print("suzuri-agent-log: %s" % err, file=sys.stderr)
    return 0


def main():
    parser = argparse.ArgumentParser(
        prog="suzuri-agent-log",
        description="Archive Claude Code transcripts into the project they belong to.",
    )
    sub = parser.add_subparsers(dest="command")
    for name, handler, help_text in (
        ("enable", cmd_enable, "turn recording on for this project and backfill"),
        ("disable", cmd_disable, "turn recording off and remove the hooks"),
        ("sync", cmd_sync, "copy this project's sessions into .suzuri/agent-logs/"),
        ("status", cmd_status, "show archived vs pending sessions"),
        ("render", cmd_render, "regenerate AUTHORING.md from the manifest"),
        ("verify", cmd_verify, "check the manifest hash chain"),
        ("hook", cmd_hook, "hook entry point (reads JSON on stdin)"),
    ):
        child = sub.add_parser(name, help=help_text)
        child.add_argument("--project", help="project root (default: cwd)")
        child.set_defaults(func=handler)

    args = parser.parse_args()
    if not getattr(args, "command", None):
        parser.print_help()
        return 2
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main())
