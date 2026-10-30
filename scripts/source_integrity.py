"""Canonical source identity rules for local verification and packaging."""
from __future__ import annotations

import hashlib
import importlib.util
import re
import subprocess
from pathlib import Path

IGNORED_PARTS = {".git", "target", ".build-target", "dist", "__pycache__", "__MACOSX", ".DS_Store"}
PROMPTGEN_REPOSITORY = "https://github.com/axiom-orient/promptGen.git"
PROMPTGEN_SOURCE_KINDS = {"pinned-github-source", "pinned-local-working-tree"}


def included_files(root: Path, *, exclude: set[str] | None = None):
    exclude = exclude or set()
    for path in sorted(p for p in root.rglob("*") if p.is_file()):
        relative = path.relative_to(root)
        if any(part in IGNORED_PARTS for part in relative.parts):
            continue
        if relative.as_posix() not in exclude:
            yield path, relative


def tree_identity(root: Path):
    digest = hashlib.sha256()
    count = total = 0
    for path, relative in included_files(root):
        data = path.read_bytes()
        digest.update(relative.as_posix().encode("utf-8") + b"\0")
        digest.update(hashlib.sha256(data).digest())
        count += 1
        total += len(data)
    return digest.hexdigest(), count, total


def write_manifest(root: Path, output: Path):
    output_relative = output.relative_to(root).as_posix()
    lines = [
        f"{hashlib.sha256(path.read_bytes()).hexdigest()}  {relative.as_posix()}\n"
        for path, relative in included_files(root, exclude={output_relative})
    ]
    output.write_text("".join(lines), encoding="utf-8")


def load_promptgen_source(root: Path):
    # PromptGen owns its snapshot format; it differs from the local tree digest.
    script = root / "scripts/source_snapshot.py"
    spec = importlib.util.spec_from_file_location("promptgen_source_snapshot", script)
    if spec is None or spec.loader is None:
        raise ValueError(f"cannot load PromptGen source snapshot helper: {script}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    snapshot = module.make_snapshot(root)
    receipt = module.verify_verification_identity(root, snapshot)
    if receipt.get("status") != "CURRENT_LOCAL_GATE_COMPLETED":
        raise ValueError("PromptGen full local verification receipt is not current and complete")
    return module, snapshot


def _git_value(root: Path, *args: str) -> str:
    result = subprocess.run(
        ["git", "-C", str(root), *args], text=True, capture_output=True, check=False
    )
    if result.returncode:
        raise ValueError(f"PromptGen Git identity failed: {result.stderr.strip()}")
    return result.stdout.strip()


def _verify_promptgen_remote(remote: str):
    expected = PROMPTGEN_REPOSITORY.removesuffix(".git")
    if remote.removesuffix(".git") != expected:
        raise ValueError(f"PromptGen origin mismatch: {remote!r} != {expected!r}")


def promptgen_origin(root: Path, snapshot: dict) -> dict:
    """Pin a verified source tree without changing the caller's Git state."""
    if not (root / ".git").exists():
        raise ValueError("PromptGen integrity refresh requires a Git source checkout")
    commit = _git_value(root, "rev-parse", "HEAD")
    dirty = _git_value(root, "status", "--porcelain", "--untracked-files=normal")
    _verify_promptgen_remote(_git_value(root, "remote", "get-url", "origin"))
    return {
        "kind": "pinned-local-working-tree" if dirty else "pinned-github-source",
        "repository": PROMPTGEN_REPOSITORY,
        "commit": commit,
        "sourceTreeSha256": snapshot["tree_sha256"],
    }


def verify_promptgen_origin(root: Path, origin: dict, snapshot: dict):
    """Verify provenance in addition to the exact source/receipt binding."""
    if (
        not isinstance(origin, dict)
        or not isinstance(origin.get("kind"), str)
        or origin["kind"] not in PROMPTGEN_SOURCE_KINDS
    ):
        raise ValueError("components.lock.json PromptGen origin kind is unsupported")
    if origin.get("repository") != PROMPTGEN_REPOSITORY:
        raise ValueError("components.lock.json PromptGen repository mismatch")
    commit = origin.get("commit")
    if not isinstance(commit, str) or not re.fullmatch(r"[0-9a-f]{40,64}", commit):
        raise ValueError("components.lock.json PromptGen commit is invalid")
    if origin.get("sourceTreeSha256") != snapshot["tree_sha256"]:
        raise ValueError("components.lock.json PromptGen origin tree digest mismatch")
    # Source archives have no Git metadata. Their exact source and successful
    # verification receipt remain bound to the lock before this check is called.
    if not (root / ".git").exists():
        return
    live_commit = _git_value(root, "rev-parse", "HEAD")
    _verify_promptgen_remote(_git_value(root, "remote", "get-url", "origin"))
    if commit != live_commit:
        raise ValueError("components.lock.json PromptGen commit mismatch")
    dirty = _git_value(root, "status", "--porcelain", "--untracked-files=normal")
    if dirty and origin["kind"] == "pinned-github-source":
        raise ValueError("PromptGen source checkout must be clean at the pinned commit")
