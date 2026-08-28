#!/usr/bin/env python3
"""Tests for script/suzuri-agent-log.py.

Run: python3 script/test_suzuri_agent_log.py

These pin the behaviours that were established empirically and are easy to
regress: routing must not use the slug, a session that moves between worktrees
must still be attributed, recording must stay off until enabled, and the manifest
chain must notice a retroactive edit.
"""

import importlib.util
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
SPEC = importlib.util.spec_from_file_location("sal", os.path.join(HERE, "suzuri-agent-log.py"))
sal = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(sal)


def slug_for(path):
    """Reproduce Claude Code's slug transform: every non-alphanumeric -> '-'."""
    import re

    return re.sub(r"[^A-Za-z0-9]", "-", path)


class Harness(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp()
        self.claude = os.path.join(self.tmp, "claude")
        os.makedirs(os.path.join(self.claude, "projects"))
        os.makedirs(os.path.join(self.claude, "file-history"))
        os.environ["CLAUDE_CONFIG_DIR"] = self.claude

    def tearDown(self):
        os.environ.pop("CLAUDE_CONFIG_DIR", None)
        shutil.rmtree(self.tmp, ignore_errors=True)

    def make_project(self, name, git=False):
        root = os.path.join(self.tmp, name)
        os.makedirs(root, exist_ok=True)
        with open(os.path.join(root, "README.md"), "w") as handle:
            handle.write("# %s\n" % name)
        if git:
            subprocess.run(["git", "init", "-q", root], check=True)
            for key, value in (("user.email", "t@t.t"), ("user.name", "t")):
                subprocess.run(["git", "-C", root, "config", key, value], check=True)
            subprocess.run(["git", "-C", root, "add", "-A"], check=True)
            subprocess.run(
                ["git", "-C", root, "commit", "-q", "-m", "init"],
                check=True,
                stdout=subprocess.DEVNULL,
            )
        return root

    def write_transcript(self, session_id, cwds, launch_cwd=None):
        """Write a transcript filed under the slug of its launch cwd."""
        launch = launch_cwd or cwds[0]
        slug_dir = os.path.join(self.claude, "projects", slug_for(launch))
        os.makedirs(slug_dir, exist_ok=True)
        path = os.path.join(slug_dir, "%s.jsonl" % session_id)
        with open(path, "w") as handle:
            for index, cwd in enumerate(cwds):
                handle.write(
                    json.dumps(
                        {
                            "type": "user",
                            "sessionId": session_id,
                            "cwd": cwd,
                            "timestamp": "2026-08-28T2%d:00:00.000Z" % index,
                            "entrypoint": "cli",
                            "message": {"role": "user", "content": "hello"},
                        }
                    )
                    + "\n"
                )
        return path


class TestRouting(Harness):
    def test_colliding_slugs_are_separated_by_cwd(self):
        """/x/audit-test-a and /x/audit/test-a collide into one slug directory.

        This is the case that proves routing cannot use the directory name.
        """
        a = self.make_project("audit-test-a")
        os.makedirs(os.path.join(self.tmp, "audit"), exist_ok=True)
        b = self.make_project(os.path.join("audit", "test-a"))
        self.assertEqual(slug_for(a), slug_for(b), "test premise: slugs must collide")

        self.write_transcript("aaaaaaaa", [a])
        self.write_transcript("bbbbbbbb", [b])
        self.write_transcript("cccccccc", [b])

        sal.set_enabled(a, True)
        sal.set_enabled(b, True)
        sal.sync_project(a, quiet=True)
        sal.sync_project(b, quiet=True)

        ids = lambda root: sorted(r["session_id"] for r in sal.manifest_records(root))
        self.assertEqual(ids(a), ["aaaaaaaa"])
        self.assertEqual(ids(b), ["bbbbbbbb", "cccccccc"])

    def test_unrelated_project_sharing_a_path_prefix_is_excluded(self):
        a = self.make_project("proj")
        other = self.make_project("proj-testbed")
        self.write_transcript("aaaaaaaa", [a])
        self.write_transcript("zzzzzzzz", [other])

        sal.set_enabled(a, True)
        sal.sync_project(a, quiet=True)
        self.assertEqual([r["session_id"] for r in sal.manifest_records(a)], ["aaaaaaaa"])

    def test_session_that_moves_between_cwds_is_attributed_to_both(self):
        """One session can touch several projects; neither may silently drop it."""
        a = self.make_project("alpha")
        b = self.make_project("beta")
        self.write_transcript("mmmmmmmm", [a, b], launch_cwd=a)

        for root in (a, b):
            sal.set_enabled(root, True)
            sal.sync_project(root, quiet=True)

        record_a = sal.manifest_records(a)[0]
        record_b = sal.manifest_records(b)[0]
        self.assertTrue(record_a["is_primary"], "launch cwd owns the session")
        self.assertFalse(record_b["is_primary"], "the other project records it as foreign")

    def test_git_worktree_outside_the_root_is_matched(self):
        """A worktree at ../name is not under the root path, so only the
        git-common-dir rule can associate it."""
        root = self.make_project("repo", git=True)
        worktree = os.path.join(self.tmp, "repo-wt")
        subprocess.run(
            ["git", "-C", root, "worktree", "add", "-q", "-b", "wt", worktree],
            check=True,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        self.assertFalse(sal.norm(worktree).startswith(sal.norm(root) + os.sep))

        self.write_transcript("wwwwwwww", [worktree])
        sal.set_enabled(root, True)
        sal.sync_project(root, quiet=True)
        self.assertEqual([r["session_id"] for r in sal.manifest_records(root)], ["wwwwwwww"])

    def test_deleted_cwd_still_attributed_by_containment(self):
        root = self.make_project("vault")
        gone = os.path.join(root, "subdir-since-deleted")
        self.write_transcript("dddddddd", [gone])
        self.assertFalse(os.path.isdir(gone))

        sal.set_enabled(root, True)
        sal.sync_project(root, quiet=True)
        self.assertEqual([r["session_id"] for r in sal.manifest_records(root)], ["dddddddd"])


class TestOptIn(Harness):
    def test_recording_is_off_by_default(self):
        root = self.make_project("proj")
        self.write_transcript("aaaaaaaa", [root])
        self.assertFalse(sal.is_enabled(root))
        sal.sync_project(root, quiet=True)
        self.assertEqual(sal.manifest_records(root), [])

    def test_enable_then_disable(self):
        root = self.make_project("proj")
        self.write_transcript("aaaaaaaa", [root])
        sal.set_enabled(root, True)
        sal.sync_project(root, quiet=True)
        self.assertEqual(len(sal.manifest_records(root)), 1)
        sal.set_enabled(root, False)
        self.assertFalse(sal.is_enabled(root))

    def test_archive_is_gitignored_on_creation(self):
        root = self.make_project("proj")
        sal.ensure_archive(root)
        with open(os.path.join(sal.archive_root(root), ".gitignore")) as handle:
            self.assertIn("*", handle.read())

    def test_hooks_install_and_remove_preserve_foreign_entries(self):
        root = self.make_project("proj")
        os.makedirs(os.path.join(root, ".claude"), exist_ok=True)
        sal.write_json(
            sal.settings_path(root),
            {"hooks": {"SessionEnd": [{"hooks": [{"type": "command", "command": "echo mine"}]}]}},
        )
        sal.install_hooks(root)
        settings = sal.load_settings(root)
        self.assertEqual(len(settings["hooks"]["SessionEnd"]), 2)

        sal.remove_hooks(root)
        settings = sal.load_settings(root)
        self.assertEqual(len(settings["hooks"]["SessionEnd"]), 1)
        self.assertIn("echo mine", json.dumps(settings))


class TestManifest(Harness):
    def test_chain_detects_a_retroactive_edit(self):
        root = self.make_project("proj")
        self.write_transcript("aaaaaaaa", [root])
        self.write_transcript("bbbbbbbb", [root])
        sal.set_enabled(root, True)
        sal.sync_project(root, quiet=True)
        self.assertEqual(sal.verify_chain(root), (True, None))

        path = sal.manifest_path(root)
        with open(path) as handle:
            lines = handle.read().splitlines()
        record = json.loads(lines[0])
        record["turns"] = 999
        lines[0] = json.dumps(record, sort_keys=True)
        with open(path, "w") as handle:
            handle.write("\n".join(lines) + "\n")

        ok, _ = sal.verify_chain(root)
        self.assertFalse(ok)

    def test_sync_is_idempotent(self):
        root = self.make_project("proj")
        self.write_transcript("aaaaaaaa", [root])
        sal.set_enabled(root, True)
        self.assertEqual(sal.sync_project(root, quiet=True), 1)
        self.assertEqual(sal.sync_project(root, quiet=True), 0)

    def test_resync_after_transcript_grows(self):
        """Freshness is by content hash: an appended turn must re-archive."""
        root = self.make_project("proj")
        path = self.write_transcript("aaaaaaaa", [root])
        sal.set_enabled(root, True)
        sal.sync_project(root, quiet=True)
        with open(path, "a") as handle:
            handle.write(
                json.dumps(
                    {"type": "user", "sessionId": "aaaaaaaa", "cwd": root,
                     "timestamp": "2026-08-28T23:00:00.000Z"}
                )
                + "\n"
            )
        self.assertEqual(sal.sync_project(root, quiet=True), 1)


class TestSidecars(Harness):
    def test_subagents_tool_results_and_file_history_are_copied(self):
        root = self.make_project("proj")
        path = self.write_transcript("aaaaaaaa", [root])
        sidecar = os.path.join(os.path.dirname(path), "aaaaaaaa")
        os.makedirs(os.path.join(sidecar, "subagents"))
        os.makedirs(os.path.join(sidecar, "tool-results"))
        history = os.path.join(self.claude, "file-history", "aaaaaaaa")
        os.makedirs(history)
        for rel, body in (
            (os.path.join(sidecar, "subagents", "agent-1.jsonl"), "{}\n"),
            (os.path.join(sidecar, "subagents", "agent-1.meta.json"), '{"agentType":"x"}'),
            (os.path.join(sidecar, "tool-results", "r1.txt"), "big output"),
            (os.path.join(history, "abc@v1"), "before"),
        ):
            with open(rel, "w") as handle:
                handle.write(body)

        sal.set_enabled(root, True)
        sal.sync_project(root, quiet=True)

        dest = os.path.join(sal.archive_root(root), sal.manifest_records(root)[0]["archived_dir"])
        for rel in (
            "subagents/agent-1.jsonl.gz",
            "subagents/agent-1.meta.json",
            "tool-results/r1.txt.gz",
            "file-history/abc@v1.gz",
        ):
            self.assertTrue(os.path.exists(os.path.join(dest, rel)), rel)


class TestTime(Harness):
    def test_utc_parsing_is_dst_independent(self):
        """Transcripts are UTC; a DST-sensitive conversion silently shifts every
        agent timestamp by an hour."""
        self.assertEqual(sal.parse_iso_utc("1970-01-01T00:00:00.000Z"), 0)
        self.assertEqual(sal.parse_iso_utc("2026-08-28T21:28:58.025Z"), 1787952538)
        self.assertIsNone(sal.parse_iso_utc(None))
        self.assertIsNone(sal.parse_iso_utc("not a timestamp"))


if __name__ == "__main__":
    unittest.main(verbosity=2)
