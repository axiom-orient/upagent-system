#!/usr/bin/env python3
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile


root = Path(__file__).resolve().parents[1]
checker = 'scripts/verify-agent-only.py'


def copy_ignore(_directory, names):
    return {
        name for name in names
        if name in {'.git', '.agent', 'target'}
    }


def run_checker(fixture):
    return subprocess.run(
        [sys.executable, str(fixture / checker)],
        cwd=fixture,
        capture_output=True,
        text=True,
        timeout=20,
        check=False,
    )


def require(result, expected_exit, expected_text, label):
    if result.returncode != expected_exit or expected_text not in result.stdout:
        raise AssertionError(
            f'{label}: exit={result.returncode}, expected exit={expected_exit}, '
            f'expected {expected_text!r}\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}'
        )
    print(f'{label}: exit={result.returncode}; matched={expected_text}')


with tempfile.TemporaryDirectory(prefix='verify-agent-only-') as directory:
    fixture_root = Path(directory)
    fixture = fixture_root / 'upagent-system' / 'UpAgent'
    shutil.copytree(root, fixture, ignore=copy_ignore)
    # The checker only reads this pinned dependency. Keep the same sibling
    # layout as the product while all mutations remain inside the copied tree.
    (fixture_root / 'promptGen').symlink_to(root.parent.parent / 'promptGen', target_is_directory=True)

    require(
        run_checker(fixture),
        0,
        '"status": "STATIC_VERIFIED"',
        'clean fixture',
    )

    legacy_path = fixture / 'vendor' / 'RustProviderKit'
    legacy_path.mkdir(parents=True)
    require(
        run_checker(fixture),
        1,
        'legacy vendor path remains: vendor/RustProviderKit',
        'vendor path mutant',
    )
    shutil.rmtree(fixture / 'vendor')

    manifest_path = fixture / 'crates/agent-core/Cargo.toml'
    canonical_manifest = manifest_path.read_text(encoding='utf-8')
    with manifest_path.open('a', encoding='utf-8') as stream:
        stream.write('\n[dependencies.rust-provider-kit]\nversion = "1"\n')
    require(
        run_checker(fixture),
        1,
        'legacy manifest reference: crates/agent-core/Cargo.toml '
        'dependencies.rust-provider-kit',
        'manifest mutant',
    )

    manifest_path.write_text(canonical_manifest, encoding='utf-8')
    with manifest_path.open('a', encoding='utf-8') as stream:
        stream.write('\n[patch.crates-io]\nRustProviderKit = "1"\n')
    require(
        run_checker(fixture),
        1,
        'legacy manifest reference: crates/agent-core/Cargo.toml '
        'patch.crates-io.RustProviderKit',
        'patch mutant',
    )

    manifest_path.write_text(canonical_manifest, encoding='utf-8')
    with manifest_path.open('a', encoding='utf-8') as stream:
        stream.write('\n[replace]\n"RustProviderKit:0.1.0" = { path = "../legacy" }\n')
    require(
        run_checker(fixture),
        1,
        'legacy manifest reference: crates/agent-core/Cargo.toml '
        'replace.RustProviderKit:0.1.0',
        'replace mutant',
    )

    manifest_path.write_text(canonical_manifest, encoding='utf-8')

    source_path = fixture / 'crates/agent-core/src/lib.rs'
    with source_path.open('a', encoding='utf-8') as stream:
        stream.write('\nconst LEGACY_MARKER: &str = "RustProviderKit";\n')
    require(
        run_checker(fixture),
        1,
        'legacy Rust source reference: crates/agent-core/src/lib.rs',
        'Rust source mutant',
    )

print('VERIFY_AGENT_ONLY_TEST: PASS')
