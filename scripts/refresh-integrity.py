#!/usr/bin/env python3
from __future__ import annotations

import json
import os
import tomllib
from datetime import date
from pathlib import Path

from source_integrity import load_promptgen_source, promptgen_origin, tree_identity, write_manifest

ROOT = Path(__file__).resolve().parents[1]
PROMPTGEN_SOURCE = Path(
    os.environ.get("PROMPTGEN_SOURCE", str(ROOT.parent / "promptGen"))
).resolve()
COMPONENTS = {
    "upagent": ROOT / "UpAgent",
    "perfectpixel": ROOT / "components/perfectpixel",
    "vergerail": ROOT / "components/vergerail",
}


def promptgen_identity(root: Path):
    _, snapshot = load_promptgen_source(root)
    origin = promptgen_origin(root, snapshot)
    package = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))
    version = package["workspace"]["package"]["version"]
    return snapshot, origin, version


# Component-local manifest owns current PerfectPixel bytes, not upstream identity.
write_manifest(COMPONENTS["perfectpixel"], COMPONENTS["perfectpixel"] / "SOURCE_MANIFEST.sha256")

lock_path = ROOT / "components.lock.json"
lock = json.loads(lock_path.read_text(encoding="utf-8"))
if lock.get("schema") != "upagent.components-lock/1":
    raise SystemExit("components.lock.json schema mismatch")
lock["generatedDate"] = date.today().isoformat()

components = lock["components"]
identities = {}
for name, path in COMPONENTS.items():
    entry = components.get(name)
    if not isinstance(entry, dict):
        raise SystemExit(f"components.lock.json missing {name}")
    digest, count, total = tree_identity(path)
    identities[name] = digest, count, total
    entry["path"] = path.relative_to(ROOT).as_posix()
    entry["localTreeSha256"] = digest
    entry["fileCount"] = count
    entry["byteCount"] = total

snapshot, origin, version = promptgen_identity(PROMPTGEN_SOURCE)
shared_package = tomllib.loads(
    (PROMPTGEN_SOURCE / "crates/agent-contract/Cargo.toml").read_text(encoding="utf-8")
)
components["promptGen"] = {
    "path": "../promptGen",
    "localTreeSha256": snapshot["tree_sha256"],
    "fileCount": snapshot["file_count"],
    "byteCount": snapshot["total_bytes"],
    "version": version,
    "revision": snapshot["revision"],
    "role": "canonical-external-prompt-compiler-and-semantic-edit-source",
    "origin": origin,
    "sharedContract": {
        "crate": shared_package["package"]["name"],
        "version": shared_package["package"]["version"],
        "manifest": "crates/agent-contract/Cargo.toml",
        "catalog": "agent.shared-contract/1",
        "defaultFeatures": shared_package["features"]["default"],
        "sourceAuthority": (
            "same pinned PromptGen base Git commit and verified local source tree"
            if origin["kind"] == "pinned-local-working-tree"
            else "same pinned PromptGen commit and source tree"
        ),
    },
    "integrationPolicy": {
        "upagentConsumes": [
            "agent-contract pure Rust types, validation and schemas",
            "image --mode prompt-only",
            "image --mode codex-imagegen with one base reference and one candidate",
        ],
        "standaloneOnly": ["web", "mcp", "screen"],
        "effectAuthority": "Vergerail owns image_generate; PromptGen Codex owns only explicitly approved semantic edit.",
    },
}

lock_path.write_text(json.dumps(lock, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")

# Root manifest is generated after components.lock.json so it binds the canonical identities.
write_manifest(ROOT, ROOT / "SOURCE_MANIFEST.sha256")
print("INTEGRITY_REFRESHED")
for name, (digest, count, total) in identities.items():
    print(f"{name} files={count} bytes={total} tree={digest}")
print(
    f"promptGen revision={snapshot['revision']} commit={origin['commit']} kind={origin['kind']} "
    f"files={snapshot['file_count']} tree={snapshot['tree_sha256']}"
)
