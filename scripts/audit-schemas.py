#!/usr/bin/env python3
from __future__ import annotations

import hashlib
import json
import os
import sys
from collections import defaultdict
from pathlib import Path
from urllib.parse import urlparse

try:
    from jsonschema import Draft202012Validator, RefResolver
except Exception as exc:  # pragma: no cover - explicit environment gate
    print(f"SCHEMA_AUDIT_NOT_RUN: jsonschema unavailable: {exc}")
    raise SystemExit(2)

ROOT = Path(__file__).resolve().parents[1]
UP = ROOT / "UpAgent"
PG = Path(os.environ.get("PROMPTGEN_SOURCE", str(ROOT.parent / "promptGen"))).resolve()
errors: list[str] = []
notes: list[str] = []
GENERATED_PARTS = {"target", ".build-target", "dist", "__pycache__", ".git", "var", "__MACOSX"}

def generated(path: Path) -> bool:
    return any(part in GENERATED_PARTS for part in path.parts)


def display(path: Path) -> str:
    try:
        return path.relative_to(ROOT).as_posix()
    except ValueError:
        try:
            return ("promptGen/" + path.relative_to(PG).as_posix()).rstrip("/")
        except ValueError:
            return str(path)


def load(path: Path):
    return json.loads(path.read_text(encoding="utf-8"))


def check_meta_schemas() -> list[Path]:
    schemas = sorted(
        [
            p
            for p in ROOT.rglob("*.schema.json")
            if not generated(p)
        ]
        + [p for p in PG.rglob("*.schema.json") if not generated(p)]
    )
    ids: dict[str, list[Path]] = defaultdict(list)
    for path in schemas:
        try:
            schema = load(path)
            Draft202012Validator.check_schema(schema)
        except Exception as exc:
            errors.append(f"invalid Draft 2020-12 schema {display(path)}: {exc}")
            continue
        schema_id = schema.get("$id")
        if isinstance(schema_id, str):
            ids[schema_id].append(path)
        for ref in _refs(schema):
            parsed = urlparse(ref)
            if parsed.scheme or ref.startswith("#"):
                continue
            target_name = ref.split("#", 1)[0]
            target = path.parent / target_name
            if target_name and not target.is_file():
                errors.append(
                    f"unresolved local $ref {display(path)} -> {target_name}"
                )
    for schema_id, paths in ids.items():
        if len(paths) <= 1:
            continue
        digests = {hashlib.sha256(path.read_bytes()).hexdigest() for path in paths}
        if len(digests) != 1:
            errors.append(
                f"duplicate schema $id has divergent bytes: {schema_id}: "
                + ", ".join(display(p) for p in paths)
            )
        else:
            notes.append(
                f"identical projection aliases share $id {schema_id}: "
                + ", ".join(display(p) for p in paths)
            )
    return schemas


def _refs(value):
    if isinstance(value, dict):
        ref = value.get("$ref")
        if isinstance(ref, str):
            yield ref
        for child in value.values():
            yield from _refs(child)
    elif isinstance(value, list):
        for child in value:
            yield from _refs(child)


def validate(instance_path: Path, schema_path: Path) -> None:
    schema = load(schema_path)
    instance = load(instance_path)
    try:
        # RefResolver is deprecated upstream, but is sufficient here for the
        # local file-relative Draft 2020-12 projection used by this source audit.
        resolver = RefResolver(base_uri=schema_path.resolve().as_uri(), referrer=schema)
        Draft202012Validator(schema, resolver=resolver).validate(instance)
    except Exception as exc:
        errors.append(
            f"fixture does not satisfy schema: {display(instance_path)} -> "
            f"{display(schema_path)}: {exc}"
        )


def promptgen_fixtures() -> int:
    image_schema = PG / "schemas" / "image.schema.json"
    interview_schema = PG / "schemas" / "interview.schema.json"
    count = 0
    for path in sorted((PG / "examples").glob("image-*.json")):
        validate(path, image_schema)
        count += 1
    for path in sorted((PG / "examples").glob("interview-image*.json")):
        validate(path, interview_schema)
        count += 1
    return count


def upagent_contract_index() -> int:
    index_path = UP / "contracts" / "schema-index.json"
    index = load(index_path)
    count = 0
    for entry in index.get("schemas", []):
        file = entry.get("file")
        digest = entry.get("sha256")
        if not isinstance(file, str) or not isinstance(digest, str):
            errors.append("UpAgent schema-index contains invalid entry")
            continue
        target = UP / "contracts" / file
        if not target.is_file():
            errors.append(f"UpAgent schema-index target missing: {file}")
            continue
        actual = hashlib.sha256(target.read_bytes()).hexdigest()
        if actual != digest:
            errors.append(f"UpAgent schema-index digest mismatch: {file}")
        count += 1
    validate(
        UP / "examples" / "image-v2" / "artifact-descriptor.json",
        UP / "docs" / "schemas" / "image-artifact-descriptor-v2.schema.json",
    )
    count += 1
    return count


def main() -> int:
    schemas = check_meta_schemas()
    promptgen_count = promptgen_fixtures()
    upagent_count = upagent_contract_index()
    if errors:
        print("SCHEMA_AUDIT_FAILED")
        for error in errors:
            print(f"- {error}")
        return 1
    print("SCHEMA_AUDIT_PASS")
    print(f"draft202012_schemas={len(schemas)}")
    print(f"promptgen_schema_fixtures={promptgen_count}")
    print(f"upagent_index_and_fixtures={upagent_count}")
    for note in notes:
        print(f"note={note}")
    print("generated_rust_schemas=NOT_RUN(static audit; audit-shared-contract.py checks actual CLI catalogs)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
