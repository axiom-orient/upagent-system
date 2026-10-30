#!/usr/bin/env python3
from __future__ import annotations

import json
import os
import re
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
UP = ROOT / "UpAgent"
PP = ROOT / "components" / "perfectpixel"
VG = ROOT / "components" / "vergerail"
PG = Path(os.environ.get("PROMPTGEN_SOURCE", str(ROOT.parent / "promptGen"))).resolve()
errors: list[str] = []
checks: list[str] = []
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


def read(path: Path) -> str:
    try:
        return path.read_text(encoding="utf-8")
    except Exception as exc:
        errors.append(f"unreadable {display(path)}: {exc}")
        return ""


def require(path: Path, *needles: str) -> None:
    body = read(path)
    for needle in needles:
        if needle not in body:
            errors.append(f"{display(path)} missing {needle!r}")


def forbid(path: Path, *needles: str, production_source: bool = False) -> None:
    body = read(path)
    if production_source:
        body = body.split("#[cfg(test)]", 1)[0]
    for needle in needles:
        if needle in body:
            errors.append(f"{display(path)} unexpectedly contains {needle!r}")


def require_rust_expression(path: Path, expression: str) -> None:
    compact = lambda value: re.sub(r",\)", ")", re.sub(r"\s+", "", value))
    if compact(expression) not in compact(read(path)):
        errors.append(f"{display(path)} missing Rust expression {expression!r}")


def exact_strings(path: Path, expected: set[str], pattern: str, label: str) -> None:
    body = read(path)
    actual = set(re.findall(pattern, body))
    if actual != expected:
        errors.append(f"{label} mismatch: actual={sorted(actual)} expected={sorted(expected)}")
    else:
        checks.append(f"{label}={len(expected)}")


def workspace_and_paths() -> None:
    manifests = [p for p in ROOT.rglob("Cargo.toml") if not generated(p)]
    manifests.extend(p for p in PG.rglob("Cargo.toml") if not generated(p))
    for manifest in manifests:
        try:
            data = tomllib.loads(manifest.read_text(encoding="utf-8"))
        except Exception as exc:
            errors.append(f"invalid TOML {display(manifest)}: {exc}")
            continue
        if "workspace" in data:
            for member in data["workspace"].get("members", []):
                if not (manifest.parent / member / "Cargo.toml").is_file():
                    errors.append(f"missing workspace member {display(manifest)}::{member}")
        for section in ("dependencies", "dev-dependencies", "build-dependencies"):
            for name, value in data.get(section, {}).items():
                if isinstance(value, dict) and "path" in value:
                    target = (manifest.parent / value["path"]).resolve()
                    if not (target / "Cargo.toml").is_file():
                        errors.append(
                            f"broken path dependency {display(manifest)}::{name} -> {value['path']}"
                        )
    checks.append(f"cargo_manifests={len(manifests)}")


def workspace_reachability(root: Path, label: str) -> None:
    manifest = root / "Cargo.toml"
    data = tomllib.loads(manifest.read_text(encoding="utf-8"))
    members = data.get("workspace", {}).get("members", [])
    if not members:
        checks.append(f"{label}_workspace_reachable=single-package")
        return
    packages: dict[str, tuple[Path, dict]] = {}
    member_for_name: dict[str, str] = {}
    for member in members:
        member_manifest = root / member / "Cargo.toml"
        member_data = tomllib.loads(member_manifest.read_text(encoding="utf-8"))
        name = member_data.get("package", {}).get("name")
        if isinstance(name, str):
            packages[name] = (member_manifest.parent, member_data)
            member_for_name[name] = member
    graph: dict[str, set[str]] = {name: set() for name in packages}
    roots: set[str] = set()
    for name, (directory, member_data) in packages.items():
        has_bin_dir = (directory / "src/bin").is_dir() and any((directory / "src/bin").glob("*.rs"))
        if member_data.get("bin") or (directory / "src/main.rs").is_file() or has_bin_dir:
            roots.add(name)
        for section in ("dependencies", "dev-dependencies", "build-dependencies"):
            for dependency, spec in member_data.get(section, {}).items():
                if isinstance(spec, dict) and "path" in spec:
                    target = (directory / spec["path"]).resolve()
                    for target_name, (target_dir, _) in packages.items():
                        if target == target_dir.resolve():
                            graph[name].add(target_name)
                            break
    reachable: set[str] = set()
    stack = list(roots)
    while stack:
        name = stack.pop()
        if name in reachable:
            continue
        reachable.add(name)
        stack.extend(graph.get(name, ()))
    missing = sorted(set(packages) - reachable)
    if missing:
        errors.append(f"{label} workspace packages have no binary-root dependency path: {missing}")
    checks.append(f"{label}_workspace_reachable={len(reachable)}/{len(packages)}")


def literal_includes() -> None:
    pattern = re.compile(r'include_(?:str|bytes)!\(\s*"([^"]+)"\s*\)')
    count = 0
    sources = list(ROOT.rglob("*.rs")) + list(PG.rglob("*.rs"))
    for source in sources:
        if generated(source):
            continue
        for relative in pattern.findall(read(source)):
            count += 1
            target = (source.parent / relative).resolve()
            if not target.is_file():
                errors.append(
                    f"missing Rust literal include {display(source)} -> {relative}"
                )
    checks.append(f"rust_literal_includes={count}")


def json_contracts() -> None:
    count = 0
    for path in list(ROOT.rglob("*.json")) + list(PG.rglob("*.json")):
        if generated(path):
            continue
        count += 1
        try:
            json.loads(path.read_text(encoding="utf-8"))
        except Exception as exc:
            errors.append(f"invalid JSON {display(path)}: {exc}")
    checks.append(f"json_contracts={count}")


def upagent_surfaces() -> None:
    effect_names = {
        "repo_map", "search_code", "read_file", "write_file", "replace_text",
        "run_check", "git_status", "inspect_diff",
    }
    catalog = UP / "crates/agent-effects/src/catalog.rs"
    dispatch = UP / "crates/agent-effects/src/lib.rs"
    cat_body = read(catalog)
    dispatch_body = read(dispatch)
    for name in sorted(effect_names):
        if f'"{name}"' not in cat_body:
            errors.append(f"UpAgent effect definition missing {name}")
        if f'"{name}" =>' not in dispatch_body:
            errors.append(f"UpAgent effect handler missing {name}")
    checks.append(f"upagent_effect_tools={len(effect_names)}")

    memory_names = {"remember", "forget_memory"}
    memory = UP / "crates/agent-runtime/src/memory_tools.rs"
    body = read(memory)
    for name in sorted(memory_names):
        if f'name: "{name}".to_owned()' not in body:
            errors.append(f"UpAgent memory definition missing {name}")
        if f'"{name}" =>' not in body:
            errors.append(f"UpAgent memory handler missing {name}")
    checks.append(f"upagent_memory_tools={len(memory_names)}")

    interaction = UP / "crates/agent-runtime/src/interaction_tools.rs"
    require(interaction, 'name: "request_input".to_owned()', "pub fn decode(call: &ToolCall)")
    require(
        UP / "crates/agent-runtime/src/session/tool_dispatch.rs",
        "is_interaction_tool(&call.name)",
        "interaction_tools::decode(call)",
        "is_effect_tool(&call.name)",
        "dispatch_effect",
        "is_memory_tool(&call.name)",
        "memory_tools::execute",
        "extension_tools.get(&call.name)",
    )

    image_names = {
        "IMAGE_GENERATE_TOOL": "image_generate",
        "IMAGE_INSPECT_TOOL": "image_inspect",
        "IMAGE_EXTRACT_TOOL": "image_extract",
        "IMAGE_RENDER_TOOL": "image_render",
        "IMAGE_COMPARE_TOOL": "image_compare",
        "IMAGE_EDIT_SEMANTIC_TOOL": "image_edit_semantic",
    }
    policy = read(UP / "crates/agent-core/src/image_policy.rs")
    image_runtime = read(UP / "crates/agent-runtime/src/image_tools.rs")
    composition = read(UP / "crates/agent-runtime/src/composition.rs")
    for constant, wire in image_names.items():
        if f'pub const {constant}: &str = "{wire}"' not in policy:
            errors.append(f"UpAgent image policy missing {constant}={wire}")
        if f"{constant} =>" not in image_runtime:
            errors.append(f"UpAgent image runtime handler missing {constant}")
        if constant not in image_runtime and constant != "IMAGE_EDIT_SEMANTIC_TOOL":
            errors.append(f"UpAgent image runtime does not reference {constant}")
    require(
        UP / "crates/agent-runtime/src/composition.rs",
        "crate::image_tools::tool_definitions()",
        "crate::image_tools::semantic_tool_definition()?",
        "crate::imagemaker::definitions()?",
    )
    require(
        UP / "crates/agent-cli/src/main.rs",
        "ImageToolModule::new_with_semantic_mask_adapter",
        ".with_promptgen(promptgen.clone(), &selected_tools)",
    )
    require_rust_expression(
        UP / "crates/agent-cli/src/main.rs",
        "ImageMakerToolModule::new(&selected_tools, promptgen)",
    )
    checks.append(f"upagent_image_tools={len(image_names)}")

    maker_names = {
        "IMAGE_CAPABILITIES_TOOL": "image_capabilities",
        "IMAGEMAKER_PREPARE_TOOL": "imagemaker_prepare",
        "PROMPTGEN_COMPILE_TOOL": "promptgen_compile",
    }
    maker_runtime = read(UP / "crates/agent-runtime/src/imagemaker.rs")
    for constant, wire in maker_names.items():
        if f'pub const {constant}: &str = "{wire}"' not in policy:
            errors.append(f"UpAgent ImageMaker policy missing {constant}={wire}")
        if f"{constant} =>" not in maker_runtime:
            errors.append(f"UpAgent ImageMaker handler missing {constant}")
    require(
        UP / "crates/agent-runtime/src/session/tool_output.rs",
        "persist_module_artifacts",
        ".put(&artifact.bytes, &artifact.media_type, &artifact.extension)",
    )
    # Prompt artifacts must be usable and bound to the exact admitted compiler identity.
    require(
        UP / "crates/agent-core/src/image_prompt.rs",
        "expected_compiler_sha256",
        "value.compiler_sha256 != expected_compiler_sha256",
        "compiled prompt artifact schema/task/compiler/content binding mismatch",
    )
    require(
        UP / "crates/agent-runtime/src/image_tools.rs",
        "self.promptgen.as_ref().map(|adapter| adapter.sha256())",
        "compiled prompt artifacts require a pinned promptGen compiler identity",
        "ImagePromptArtifact::resolve(",
        "compiled_prompt_artifact_is_bound_to_the_admitted_compiler_identity",
    )
    checks.append(f"upagent_imagemaker_tools={len(maker_names)}")


def upagent_cli_surface() -> None:
    cli = read(UP / "crates/agent-cli/src/main.rs")
    commands = {
        "Profiles", "Skills", "Plugins", "Compose", "Schema", "Doctor",
        "Run", "Inspect", "Recover", "Memory", "Approvals",
    }
    for variant in sorted(commands):
        if f"Command::{variant}" not in cli:
            errors.append(f"UpAgent CLI command lacks execution arm: {variant}")
    for variant in ["TaskList", "TaskResolve", "EffectList", "EffectResolve"]:
        if f"RecoverCommand::{variant}" not in cli:
            errors.append(f"UpAgent recovery command lacks execution arm: {variant}")
    for variant in ["List", "Approve", "Deny"]:
        if f"ApprovalCommand::{variant}" not in cli:
            errors.append(f"UpAgent approval command lacks execution arm: {variant}")
    for variant in ["Snapshot", "Recall"]:
        if f"MemoryCommand::{variant}" not in cli:
            errors.append(f"UpAgent memory CLI command lacks execution arm: {variant}")
    for variant in ["TaskV3", "AgentSpec", "Composition", "Result", "Profile", "SharedContract"]:
        if f"SchemaKind::{variant}" not in cli:
            errors.append(f"UpAgent schema command lacks output arm: {variant}")
    require(
        UP / "crates/agent-cli/src/main.rs",
        "CompositionStore::open",
        "connect_provider_envelope",
        "AgentRuntime::open_with_plugins_and_catalog_and_memory",
        "run_with_signals",
    )
    checks.append(f"upagent_cli_commands={len(commands)}")
    checks.append("upagent_recovery_commands=4")
    checks.append("upagent_approval_commands=3")
    checks.append("upagent_memory_cli_commands=2")
    checks.append("upagent_schema_outputs=6")


def vergerail_surface() -> None:
    provider = VG / "src/bin/vergerail_upagent_provider.rs"
    up_provider = UP / "crates/agent-provider/src/lib.rs"
    require(
        provider,
        "provider::{",
        "ProviderOperation",
        "ModelTurn",
        "ImageGenerate",
        '"image_generate requires prompt"',
        '"model_turn requires at least one message"',
        "run_model_turn",
        "run_image_generation",
    )
    require(
        up_provider,
        "agent_contract::provider::{",
        "PROVIDER_PROTOCOL_SCHEMA",
        '"model_turn"',
        '"image_generate"',
        "decode_success",
    )
    require(PG / "crates/agent-contract/src/provider.rs",
            'pub const PROVIDER_PROTOCOL_SCHEMA: &str = "vergerail.upagent/2"')
    require(provider, "agent_contract::schema::catalog_json()")
    forbid(provider, "struct ProviderRequest", "struct ProviderMessage", "struct ProviderTool")
    forbid(up_provider, "struct ModelRequest", "struct WireToolCall", "struct WireUsage")
    checks.append("vergerail_operations=2")


def perfectpixel_surface() -> None:
    # Product operation registry is the semantic authority for the standalone/MCP plane.
    request = read(PP / "src/operation/mod.rs")
    app = read(PP / "src/application/mod.rs")
    expected_variants = {
        "Schema", "Inspect", "Convert", "Upscale", "Edit", "ExportPsd",
        "CompileDocumentPsd", "ChromaPlan", "NormalizeSprite", "CompileSprite",
        "CompileTexture", "CompileVector", "AnalyzeVector", "AppleVisionForegroundInstances",
        "ScaffoldMotion", "CompileMotion",
    }
    enum_match = re.search(r"pub enum Operation\s*\{(.*?)\n\}", request, re.S)
    if not enum_match:
        errors.append("PerfectPixel Operation enum not found")
    else:
        actual = set(re.findall(r"^\s*([A-Z][A-Za-z0-9_]*)(?:\s*\{|\s*,)", enum_match.group(1), re.M))
        if actual != expected_variants:
            errors.append(f"PerfectPixel Operation variants mismatch: actual={sorted(actual)}")
        for variant in sorted(expected_variants):
            if f"Operation::{variant}" not in app:
                errors.append(f"PerfectPixel handler mapping missing Operation::{variant}")
    checks.append(f"perfectpixel_product_operations={len(expected_variants)}")

    agent = PP / "src/application/handlers/agent.rs"
    require(
        agent,
        "fn agent_inspect",
        "fn agent_extract",
        "fn agent_render",
        "fn agent_compare",
        "AgentRenderNodeKind::Text",
        "apply_raster_edits(&raster, &edits)",
        "compare_images(",
    )
    require(
        PP / "src/agent.rs",
        'name: "render.text_node"',
        'name: "compare.exact_spec"',
    )
    checks.append("perfectpixel_agent_operations=5")


def promptgen_surface() -> None:
    cli = PG / "crates/promptgen-cli/src/main.rs"
    cli_body = read(cli)
    commands = {"screen", "image", "interview", "serve", "mcp", "schema", "catalog", "lut-presets"}
    for command in sorted(commands):
        if f'"{command}" =>' not in cli_body:
            errors.append(f"promptGen CLI command lacks dispatch: {command}")
    require(
        cli,
        '"prompt-only" => Ok(Self::PromptOnly)',
        '"codex-imagegen" => Ok(Self::CodexImagegen)',
        '"--max-fidelity-attempts"',
        '"--codex-sha256"',
        "compile_image_prompt",
        "execute_image_generation",
    )
    checks.append(f"promptgen_cli_commands={len(commands)}")

    mcp = PG / "crates/promptgen-mcp/src/lib.rs"
    mcp_body = read(mcp)
    mcp_tools = {"promptgen_compile_image", "promptgen_interview", "promptgen_catalog", "promptgen_lut_presets"}
    for name in sorted(mcp_tools):
        if mcp_body.count(f'"{name}"') < 2:
            errors.append(f"promptGen MCP tool not both listed/dispatched: {name}")
    checks.append(f"promptgen_mcp_tools={len(mcp_tools)}")

    web = PG / "crates/promptgen-web/src/lib.rs"
    require(
        web,
        '"/api/v3/health"',
        '"/api/v3/catalog"',
        '"/api/v3/lut-presets"',
        '"/api/v3/interview"',
        '"/api/v3/generate"',
        "serve_catalog_asset",
        "serve_artifact",
        "handle_interview",
        "handle_generate",
    )
    checks.append("promptgen_web_routes=7+")

    # UpAgent consumes the pure compiler and an explicitly approved, one-candidate Codex edit.
    adapter = UP / "crates/agent-runtime/src/promptgen.rs"
    require(
        adapter,
        '"prompt-only".into()',
        '"codex-imagegen".into()',
        '"--codex-sha256".into()',
        '"--max-fidelity-attempts".into()',
        '"1".into()',
    )
    forbid(adapter, "OPENAI_API_KEY", "openai-image-edit", production_source=True)
    forbid(UP / "crates/agent-runtime/src/imagemaker.rs", "codex-imagegen")
    require(
        UP / "crates/agent-core/src/composition.rs",
        "PromptGenCodexRuntimeSpec",
        "semantic editing requires an explicit pinned PromptGen Codex runtime",
    )


def planning_only_not_exposed() -> None:
    # Known future SceneAssetCompiler item may remain documented, but must not be registered as a public tool.
    for source in list((UP / "crates").rglob("*.rs")):
        if "SceneAssetCompiler" in read(source):
            errors.append(f"planned SceneAssetCompiler leaked into executable Rust surface: {source.relative_to(ROOT)}")
    checks.append("planned_unwired_features_not_exposed=1")


def main() -> int:
    workspace_and_paths()
    workspace_reachability(UP, "upagent")
    workspace_reachability(PG, "promptgen")
    literal_includes()
    json_contracts()
    upagent_surfaces()
    upagent_cli_surface()
    vergerail_surface()
    perfectpixel_surface()
    promptgen_surface()
    planning_only_not_exposed()
    if errors:
        print("CONNECTIVITY_AUDIT_FAILED")
        for error in errors:
            print(f"- {error}")
        return 1
    print("CONNECTIVITY_AUDIT_PASS")
    for check in checks:
        print(check)
    print("runtime=NOT_RUN(static connectivity audit; component gates provide runtime evidence)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
