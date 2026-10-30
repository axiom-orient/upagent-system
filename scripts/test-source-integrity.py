#!/usr/bin/env python3
"""Unit regressions for source identity; fixtures do not attest live PromptGen."""
from __future__ import annotations

import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

from source_integrity import (
    IGNORED_PARTS,
    PROMPTGEN_REPOSITORY,
    included_files,
    load_promptgen_source,
    promptgen_origin,
    tree_identity,
    verify_promptgen_origin,
    write_manifest,
)


EXPECTED_MANIFEST = (
    "b6a98d9ce9a2d9149288fa3df42d377c3e42737afdcdaf714e33c0a100b51060  a.txt\n"
    "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855  nested/z.txt\n"
    "cb0f24046b508710d6315e71bd9b21b920cf15301b0cf055dc9569c507576ea3  한 글.txt\n"
).encode("utf-8")

# A deliberately small external-helper fixture. Its receipt validates the exact
# snapshot passed by the caller, without simulating a successful product gate.
PROMPTGEN_HELPER = '''\
import json

def make_snapshot(root):
    return {"revision": "fixture-revision", "tree_sha256": "fixture-tree"}

def verify_verification_identity(root, snapshot):
    receipt = json.loads((root / "VERIFICATION.json").read_text(encoding="utf-8"))
    if receipt.get("revision") != snapshot["revision"]:
        raise ValueError("fixture stale revision")
    if receipt.get("source_tree_sha256") != snapshot["tree_sha256"]:
        raise ValueError("fixture stale tree")
    return receipt
'''

# This helper fixture hashes a real source file so tests can distinguish source
# drift from Git/index state. It does not attest the production PromptGen gate.
SOURCE_TREE_HELPER = PROMPTGEN_HELPER.replace(
    'import json', 'import hashlib\nimport json'
).replace(
    'return {"revision": "fixture-revision", "tree_sha256": "fixture-tree"}',
    'return {"revision": "fixture-revision", "tree_sha256": '
    'hashlib.sha256((root / "source.txt").read_bytes()).hexdigest()}',
)


class SourceIdentityTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)

    def write(self, relative: str, data: bytes):
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)

    def seed_source(self):
        # Deliberately create files out of order; include UTF-8, a space, and an
        # empty file to pin the existing path/content hash wire representation.
        self.write("한 글.txt", "내용".encode("utf-8"))
        self.write("nested/z.txt", b"")
        self.write("a.txt", b"alpha\n")

    def test_tree_identity_preserves_fixed_hash_bytes(self):
        self.seed_source()
        self.assertEqual(
            tree_identity(self.root),
            (
                "35da2d253be67d8ba8aac6e6f038f5ad99893c8d1e8eee3cc61e67f78c06cfe3",
                3,
                12,
            ),
        )

    def test_manifest_preserves_format_order_and_excludes_itself(self):
        self.seed_source()
        output = self.root / "SOURCE_MANIFEST.sha256"
        output.write_text("old manifest\n", encoding="utf-8")
        write_manifest(self.root, output)
        self.assertEqual(output.read_bytes(), EXPECTED_MANIFEST)
        write_manifest(self.root, output)
        self.assertEqual(output.read_bytes(), EXPECTED_MANIFEST)

    def test_excluded_parts_do_not_change_identity_or_manifest(self):
        self.seed_source()
        expected_identity = tree_identity(self.root)
        excluded = {".git", "target", ".build-target", "dist", "__pycache__", "__MACOSX"}
        self.assertEqual(IGNORED_PARTS, excluded | {".DS_Store"})
        for part in excluded:
            self.write(f"{part}/top.bin", b"generated")
            self.write(f"nested/{part}/deep.bin", b"generated")
        self.assertEqual(tree_identity(self.root), expected_identity)
        output = self.root / "SOURCE_MANIFEST.sha256"
        write_manifest(self.root, output)
        self.assertEqual(output.read_bytes(), EXPECTED_MANIFEST)

    def test_finder_metadata_files_do_not_change_identity_or_manifest(self):
        self.seed_source()
        expected_identity = tree_identity(self.root)
        self.write(".DS_Store", b"root Finder metadata")
        self.write("nested/.DS_Store", b"nested Finder metadata")
        self.assertEqual(tree_identity(self.root), expected_identity)
        output = self.root / "SOURCE_MANIFEST.sha256"
        write_manifest(self.root, output)
        self.assertEqual(output.read_bytes(), EXPECTED_MANIFEST)

    def test_explicit_exclusion_is_relative_and_exact(self):
        self.write("skip.txt", b"skip")
        self.write("nested/skip.txt", b"keep")
        self.write("target.txt", b"keep")
        self.assertEqual(
            [relative.as_posix() for _, relative in included_files(self.root, exclude={"skip.txt"})],
            ["nested/skip.txt", "target.txt"],
        )

    def test_source_changes_affect_identity(self):
        self.seed_source()
        original = tree_identity(self.root)
        self.write("a.txt", b"changed\n")
        changed = tree_identity(self.root)
        self.assertNotEqual(changed[0], original[0])
        self.assertEqual(changed[1:], (3, 14))
        (self.root / "a.txt").rename(self.root / "b.txt")
        renamed = tree_identity(self.root)
        self.assertNotEqual(renamed[0], changed[0])
        self.assertEqual(renamed[1:], changed[1:])
        self.write("new.txt", b"new")
        added = tree_identity(self.root)
        self.assertNotEqual(added[0], renamed[0])
        self.assertEqual(added[1:], (4, 17))


class PromptGenLoaderTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        (self.root / "scripts").mkdir()
        (self.root / "scripts/source_snapshot.py").write_text(PROMPTGEN_HELPER, encoding="utf-8")

    def write_receipt(self, **overrides):
        receipt = {
            "status": "CURRENT_LOCAL_GATE_COMPLETED",
            "revision": "fixture-revision",
            "source_tree_sha256": "fixture-tree",
        }
        receipt.update(overrides)
        (self.root / "VERIFICATION.json").write_text(json.dumps(receipt), encoding="utf-8")

    def test_returns_loaded_module_and_verified_snapshot(self):
        self.write_receipt()
        module, snapshot = load_promptgen_source(self.root)
        self.assertTrue(callable(module.make_snapshot))
        self.assertEqual(snapshot, {"revision": "fixture-revision", "tree_sha256": "fixture-tree"})

    def test_rejects_incomplete_or_missing_success_status(self):
        for status in ("FAILED", "NOT_RUN", None):
            with self.subTest(status=status):
                self.write_receipt(status=status)
                with self.assertRaises(ValueError):
                    load_promptgen_source(self.root)

    def test_propagates_stale_receipt_failure(self):
        for overrides, message in (
            ({"revision": "old-revision"}, "fixture stale revision"),
            ({"source_tree_sha256": "old-tree"}, "fixture stale tree"),
        ):
            with self.subTest(overrides=overrides):
                self.write_receipt(**overrides)
                with self.assertRaisesRegex(ValueError, message):
                    load_promptgen_source(self.root)

    def test_missing_receipt_is_not_success(self):
        with self.assertRaises(FileNotFoundError):
            load_promptgen_source(self.root)


class PromptGenGitOriginTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.base = Path(temporary.name)
        self.root = self.base / "source"
        self.root.mkdir()
        (self.root / "scripts").mkdir()
        (self.root / "scripts/source_snapshot.py").write_text(
            SOURCE_TREE_HELPER, encoding="utf-8"
        )
        (self.root / "source.txt").write_text("initial source\n", encoding="utf-8")
        (self.root / ".gitignore").write_text("__pycache__/\n", encoding="utf-8")
        self.git("init", "--quiet")
        self.git("remote", "add", "origin", PROMPTGEN_REPOSITORY)
        self.record_fixture_receipt()
        self.git("add", ".")
        self.git("commit", "--quiet", "-m", "initial fixture source")

    def git(self, *args):
        result = subprocess.run(
            [
                "git", "-C", str(self.root),
                "-c", "user.name=Source fixture",
                "-c", "user.email=source-fixture@example.invalid",
                *args,
            ],
            text=True,
            capture_output=True,
            check=True,
        )
        return result.stdout.strip()

    def record_fixture_receipt(self):
        import hashlib

        snapshot = {
            "revision": "fixture-revision",
            "tree_sha256": hashlib.sha256((self.root / "source.txt").read_bytes()).hexdigest(),
        }
        (self.root / "VERIFICATION.json").write_text(
            json.dumps({
                "status": "CURRENT_LOCAL_GATE_COMPLETED",
                "revision": snapshot["revision"],
                "source_tree_sha256": snapshot["tree_sha256"],
            }),
            encoding="utf-8",
        )
        return snapshot

    def pin(self):
        _, snapshot = load_promptgen_source(self.root)
        return promptgen_origin(self.root, snapshot), snapshot

    def test_clean_checkout_keeps_the_existing_origin_kind(self):
        origin, snapshot = self.pin()
        self.assertEqual(origin, {
            "kind": "pinned-github-source",
            "repository": PROMPTGEN_REPOSITORY,
            "commit": self.git("rev-parse", "HEAD"),
            "sourceTreeSha256": snapshot["tree_sha256"],
        })
        verify_promptgen_origin(self.root, origin, snapshot)
        self.assertEqual(self.git("status", "--porcelain"), "")

    def test_modified_source_requires_an_explicit_local_pin(self):
        before_commit = self.git("rev-parse", "HEAD")
        source = self.root / "source.txt"
        source.write_text("user changes retained\n", encoding="utf-8")
        self.record_fixture_receipt()
        before_status = self.git("status", "--porcelain")
        origin, snapshot = self.pin()
        self.assertEqual(origin["kind"], "pinned-local-working-tree")
        self.assertEqual(origin["commit"], before_commit)
        verify_promptgen_origin(self.root, origin, snapshot)
        self.assertEqual(source.read_text(), "user changes retained\n")
        self.assertEqual(self.git("status", "--porcelain"), before_status)
        origin["kind"] = "pinned-github-source"
        with self.assertRaisesRegex(ValueError, "must be clean"):
            verify_promptgen_origin(self.root, origin, snapshot)

    def test_local_pin_tracks_working_source_independently_of_the_git_index(self):
        source = self.root / "source.txt"
        source.write_text("unchanged working source across index edits\n", encoding="utf-8")
        self.record_fixture_receipt()
        origin, snapshot = self.pin()
        source_bytes = source.read_bytes()
        base_commit = self.git("rev-parse", "HEAD")
        unstaged_status = self.git("status", "--porcelain")
        verify_promptgen_origin(self.root, origin, snapshot)

        # All index mutations are confined to this disposable fixture repo.
        self.git("add", "source.txt", "VERIFICATION.json")
        self.assertNotEqual(self.git("status", "--porcelain"), unstaged_status)
        _, staged_snapshot = load_promptgen_source(self.root)
        self.assertEqual(staged_snapshot, snapshot)
        self.assertEqual(source.read_bytes(), source_bytes)
        verify_promptgen_origin(self.root, origin, staged_snapshot)

        self.git("reset", "--mixed", "--quiet", "HEAD")
        self.assertEqual(self.git("status", "--porcelain"), unstaged_status)
        _, reset_snapshot = load_promptgen_source(self.root)
        self.assertEqual(reset_snapshot, snapshot)
        self.assertEqual(source.read_bytes(), source_bytes)
        self.assertEqual(self.git("rev-parse", "HEAD"), base_commit)
        verify_promptgen_origin(self.root, origin, reset_snapshot)

    def test_both_origin_kinds_require_the_expected_git_remote(self):
        origin, snapshot = self.pin()
        self.git("remote", "set-url", "origin", "https://example.invalid/other.git")
        with self.assertRaisesRegex(ValueError, "origin mismatch"):
            promptgen_origin(self.root, snapshot)
        for kind in ("pinned-github-source", "pinned-local-working-tree"):
            with self.subTest(kind=kind):
                origin["kind"] = kind
                with self.assertRaisesRegex(ValueError, "origin mismatch"):
                    verify_promptgen_origin(self.root, origin, snapshot)

    def test_new_head_is_rejected_even_when_source_and_receipt_are_unchanged(self):
        origin, snapshot = self.pin()
        self.git("commit", "--quiet", "--allow-empty", "-m", "different base commit")
        for kind in ("pinned-github-source", "pinned-local-working-tree"):
            with self.subTest(kind=kind):
                origin["kind"] = kind
                with self.assertRaisesRegex(ValueError, "commit mismatch"):
                    verify_promptgen_origin(self.root, origin, snapshot)

    def test_unknown_kind_repository_and_invalid_commit_are_rejected(self):
        origin, snapshot = self.pin()
        for changes, message in (
            ({"kind": "accept-any-source"}, "kind is unsupported"),
            ({"kind": []}, "kind is unsupported"),
            ({"repository": "https://example.invalid/other.git"}, "repository mismatch"),
            ({"commit": "not-a-commit"}, "commit is invalid"),
        ):
            with self.subTest(changes=changes):
                with self.assertRaisesRegex(ValueError, message):
                    verify_promptgen_origin(self.root, origin | changes, snapshot)

    def test_local_pin_rejects_freshly_verified_but_unpinned_source_changes(self):
        origin, _ = self.pin()
        origin["kind"] = "pinned-local-working-tree"
        (self.root / "source.txt").write_text("changed after pin\n", encoding="utf-8")
        self.record_fixture_receipt()
        _, changed_snapshot = load_promptgen_source(self.root)
        with self.assertRaisesRegex(ValueError, "tree digest mismatch"):
            verify_promptgen_origin(self.root, origin, changed_snapshot)

    def test_source_change_with_stale_receipt_cannot_be_pinned(self):
        (self.root / "source.txt").write_text("unverified source\n", encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "stale tree"):
            self.pin()

    def test_gitless_archives_keep_exact_source_and_receipt_verification(self):
        for dirty in (False, True):
            with self.subTest(dirty=dirty):
                if dirty:
                    (self.root / "source.txt").write_text("local archive source\n", encoding="utf-8")
                    self.record_fixture_receipt()
                origin, _ = self.pin()
                archive = self.base / f"archive-{dirty}"
                shutil.copytree(self.root, archive, ignore=shutil.ignore_patterns(".git", "__pycache__"))
                _, snapshot = load_promptgen_source(archive)
                verify_promptgen_origin(archive, origin, snapshot)
                with self.assertRaisesRegex(ValueError, "requires a Git source checkout"):
                    promptgen_origin(archive, snapshot)
                with self.assertRaisesRegex(ValueError, "kind is unsupported"):
                    verify_promptgen_origin(archive, origin | {"kind": "unknown"}, snapshot)
                (archive / "source.txt").write_text("archive drift\n", encoding="utf-8")
                with self.assertRaisesRegex(ValueError, "stale tree"):
                    load_promptgen_source(archive)


if __name__ == "__main__":
    unittest.main()
