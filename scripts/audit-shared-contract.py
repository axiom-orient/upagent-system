#!/usr/bin/env python3
"""Compare the actual three CLI catalogs and validate a real pure compilation."""
from __future__ import annotations

import argparse
import hashlib
import json
import subprocess
from pathlib import Path

from jsonschema.validators import validator_for


def invoke(binary: Path, *args: str):
    result = subprocess.run(
        [str(binary.resolve()), *args], check=True, capture_output=True, text=True,
        timeout=30,
    )
    return json.loads(result.stdout)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--agent", required=True, type=Path)
    parser.add_argument("--promptgen", required=True, type=Path)
    parser.add_argument("--vergerail", required=True, type=Path)
    parser.add_argument("--image-input", required=True, type=Path)
    args = parser.parse_args()
    catalog = invoke(args.promptgen, "schema", "shared-contract")
    assert catalog == invoke(args.agent, "schema", "shared-contract"), "UpAgent catalog drift"
    assert catalog == invoke(args.vergerail, "--schema"), "Vergerail catalog drift"
    assert catalog["schema"] == "agent.shared-contract/1"
    assert catalog["providerProtocol"] == "vergerail.upagent/2"
    validators = {}
    for name, schema in catalog["schemas"].items():
        cls = validator_for(schema)
        cls.check_schema(schema)
        validators[name] = cls(schema)
    result = invoke(args.promptgen, "image", "--mode", "prompt-only", "--format", "json",
                    "--input", str(args.image_input.resolve()))
    validators["compilation"].validate(result)
    assert result["status"] in ("valid", "valid_with_warnings")
    assert isinstance(result["prompt"], str) and result["prompt"].strip()
    # Exercise published identifier grammar against every ASCII character.
    tool_id_schema = catalog["schemas"]["modelResponse"]["definitions"]["ToolCall"]["properties"]["id"]
    tool_id_validator = validator_for(tool_id_schema)(tool_id_schema)
    for code in range(128):
        expected = 33 <= code <= 126 and chr(code) not in '\"\'\\'
        assert tool_id_validator.is_valid(chr(code)) == expected, f"ToolCallId grammar: {code}"
    for value in ["", "call\n", "call\r", "call\t", "x" * 193]:
        assert not tool_id_validator.is_valid(value), f"invalid ToolCallId: {value!r}"
    assert tool_id_validator.is_valid("x" * 192)
    digest = hashlib.sha256(json.dumps(catalog, sort_keys=True, separators=(",", ":")).encode()).hexdigest()
    print(f"SHARED_CONTRACT_PASS schemas={len(validators)} catalog_sha256={digest}")
    print("runtime=pure CLI compile/schema; account-based provider effects NOT_RUN")


if __name__ == "__main__":
    main()
