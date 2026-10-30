#!/usr/bin/env python3
from pathlib import Path
import hashlib
import json
import os
import re
import sys
import tomllib

root = Path(__file__).resolve().parents[1]
errors = []
shared_contract = (root.parent.parent / "promptGen/crates/agent-contract").resolve()
manifest = tomllib.loads((root / 'Cargo.toml').read_text())
members = manifest.get('workspace', {}).get('members', [])
legacy = 'rustproviderkit'
dependency_tables = {'dependencies', 'dev-dependencies', 'build-dependencies'}


def rel(path):
    return path.relative_to(root).as_posix()


def has_legacy(value):
    return isinstance(value, str) and legacy in re.sub(r'[-_]', '', value).casefold()


def context(path):
    return '.'.join(str(item) if not isinstance(item, int) else f'[{item}]' for item in path)


def manifest_legacy(value, path=()):
    found = []
    if isinstance(value, dict):
        for key, child in value.items():
            key = str(key)
            child_path = path + (key,)
            parent = path[-1] if path and isinstance(path[-1], str) else ''
            dependency_name = parent.casefold() in dependency_tables or parent.casefold() == 'replace' or (
                len(path) > 1 and isinstance(path[-2], str) and path[-2].casefold() == 'patch'
            )
            package_name = key.casefold() == 'name' and parent.casefold() == 'package'
            relevant = dependency_name or package_name or key.casefold() in {'package', 'path', 'source'}
            if relevant and has_legacy(key if dependency_name else child):
                found.append((context(child_path), key if dependency_name else child))
            found.extend(manifest_legacy(child, child_path))
    elif isinstance(value, list):
        for index, child in enumerate(value):
            found.extend(manifest_legacy(child, path + (index,)))
    return found


def active_sources(member):
    base = root / member
    paths = [base / 'build.rs'] if (base / 'build.rs').is_file() else []
    for directory in ('src', 'tests', 'examples', 'benches'):
        candidate = base / directory
        if candidate.is_dir():
            paths.extend(path for path in sorted(candidate.rglob('*.rs')) if path.is_file())
    return [path for path in paths if path.resolve().is_relative_to(root.resolve())]


def zero_legacy(documents):
    found = []
    vendor_path = root / 'vendor' / 'RustProviderKit'
    if os.path.lexists(vendor_path):
        found.append(f'legacy vendor path remains: {rel(vendor_path)}')
    for manifest_path, data in documents:
        found.extend(
            f'legacy manifest reference: {rel(manifest_path)} {where}={value!r}'
            for where, value in manifest_legacy(data)
        )
    for member in members:
        for source_path in active_sources(member):
            if has_legacy(source_path.read_text(errors='ignore')):
                found.append(f'legacy Rust source reference: {rel(source_path)}')
    return found


documents = [(root / 'Cargo.toml', manifest)]
for banned in ('agent-axd-contract', 'axdev-'):
    if any(banned in member for member in members):
        errors.append(f'front member remains: {banned}')
packages = {}
for member in members:
    path = root / member / 'Cargo.toml'
    if not path.exists():
        errors.append(f'missing workspace member manifest: {member}')
        continue
    try:
        data = tomllib.loads(path.read_text())
    except Exception as exception:
        errors.append(f'invalid TOML {path.relative_to(root)}: {exception}')
        continue
    documents.append((path, data))
    if name := data.get('package', {}).get('name'):
        packages[name] = path.parent
    for section in ('dependencies', 'dev-dependencies', 'build-dependencies'):
        for dependency, spec in data.get(section, {}).items():
            if isinstance(spec, dict) and 'path' in spec:
                target = (path.parent / spec['path']).resolve()
                if not target.is_relative_to(root.resolve()) and not (
                    dependency == 'agent-contract' and target == shared_contract
                ):
                    errors.append(f'external path dependency {path.relative_to(root)} {dependency} -> {target}')
# The only external source dependency is the pure contract, never a provider runtime.
try:
    contract_manifest = tomllib.loads((shared_contract / 'Cargo.toml').read_text())
    if contract_manifest['package']['name'] != 'agent-contract':
        errors.append('shared contract package identity mismatch')
    if set(contract_manifest.get('dependencies', {})) != {'serde', 'serde_json', 'schemars'}:
        errors.append('shared contract must not acquire runtime or I/O dependencies')
    if contract_manifest.get('features', {}).get('default') != []:
        errors.append('shared contract default features must remain dependency-free')
except (OSError, KeyError, tomllib.TOMLDecodeError) as exception:
    errors.append(f'shared contract source is unavailable or invalid: {exception}')
for member in members:
    source = root / member / 'src'
    if source.exists():
        for path in source.rglob('*.rs'):
            if re.search(
                r'\b(?:agent_axd_contract|axdev_orchestrator)::|'
                r'\buse\s+(?:agent_axd_contract|axdev_orchestrator)\b',
                path.read_text(errors='ignore'),
            ):
                errors.append(f'front implementation reference: {path.relative_to(root)}')
errors.extend(zero_legacy(documents))
index = json.loads((root / 'contracts/schema-index.json').read_text())
for schema in index['schemas']:
    path = root / 'contracts' / schema['file']
    if not path.exists():
        errors.append(f'missing schema {schema["file"]}')
    elif hashlib.sha256(path.read_bytes()).hexdigest() != schema['sha256']:
        errors.append(f'schema hash mismatch {schema["file"]}')
if errors:
    print(json.dumps({'status': 'FAIL', 'errors': errors}, indent=2))
    sys.exit(1)
print(json.dumps({
    'status': 'STATIC_VERIFIED',
    'workspace_members': members,
    'package_count': len(packages),
    'schema_count': len(index['schemas']),
}, indent=2))
