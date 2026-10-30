#!/usr/bin/env python3
from __future__ import annotations
import hashlib, json, os, re, subprocess, sys, tomllib
from pathlib import Path

from source_integrity import included_files, load_promptgen_source, tree_identity, verify_promptgen_origin
from rust_delimiters import rust_delimiters

ROOT=Path(__file__).resolve().parents[1]
UP=ROOT/'UpAgent'; PP=ROOT/'components/perfectpixel'; VG=ROOT/'components/vergerail'
PG=Path(os.environ.get('PROMPTGEN_SOURCE', str(ROOT.parent/'promptGen'))).resolve()
errors=[]; notes=[]
GENERATED_PARTS = {'target', '.build-target', 'dist', '__pycache__', '.git', '.DS_Store'}

def generated(path): return any(part in GENERATED_PARTS for part in path.parts)

def fail(m): errors.append(m)
def display(path):
    try: return path.relative_to(ROOT).as_posix()
    except ValueError:
        try: return ('promptGen/' + path.relative_to(PG).as_posix()).rstrip('/')
        except ValueError: return str(path)
def text(p):
    try: return p.read_text(encoding='utf-8')
    except Exception as e: fail(f'{display(p)} unreadable: {e}'); return ''

def require(p,*needles):
    t=text(p)
    for n in needles:
        if n not in t: fail(f'{display(p)} missing {n!r}')

def require_rust_expression(p, expression):
    # Formatting and a trailing argument comma do not change a Rust expression.
    compact = lambda value: re.sub(r',\)', ')', re.sub(r'\s+', '', value))
    if compact(expression) not in compact(text(p)):
        fail(f'{display(p)} missing Rust expression {expression!r}')

def forbid(p,*needles,production_source=False):
    t=text(p)
    if production_source:
        t=t.split('#[cfg(test)]',1)[0]
    for n in needles:
        if n in t: fail(f'{display(p)} unexpectedly contains {n!r}')

def parse_all():
    manifests = list(ROOT.rglob('Cargo.toml')) + list(PG.rglob('Cargo.toml'))
    json_files = list(ROOT.rglob('*.json')) + list(PG.rglob('*.json'))
    for p in manifests:
        if generated(p): continue
        try: tomllib.loads(p.read_text(encoding='utf-8'))
        except Exception as e: fail(f'{display(p)} TOML: {e}')
    for p in json_files:
        if generated(p): continue
        try: json.loads(p.read_text(encoding='utf-8'))
        except Exception as e: fail(f'{display(p)} JSON: {e}')

def workspace_members():
    for workspace, label in ((UP, 'UpAgent'), (PG, 'PromptGen')):
        data=tomllib.loads((workspace/'Cargo.toml').read_text())
        for member in data['workspace']['members']:
            root=workspace/member
            if not (root/'Cargo.toml').is_file(): fail(f'{label} workspace member missing: {member}')
    manifests = list(ROOT.rglob('Cargo.toml')) + list(PG.rglob('Cargo.toml'))
    for manifest in manifests:
        if generated(manifest): continue
        data=tomllib.loads(manifest.read_text())
        for section in ('dependencies','dev-dependencies','build-dependencies'):
            for name,value in data.get(section,{}).items():
                if isinstance(value,dict) and 'path' in value:
                    target=(manifest.parent/value['path']).resolve()
                    if name == 'agent-contract' and target != PG/'crates/agent-contract':
                        fail(f'{display(manifest)} agent-contract must use the pinned PromptGen source: {target}')
                    if not (target/'Cargo.toml').is_file():
                        fail(f'{display(manifest)} dependency {name} path missing: {value["path"]}')

def lock_contains_direct_deps(component: Path):
    manifest=tomllib.loads((component/'Cargo.toml').read_text())
    lock=tomllib.loads((component/'Cargo.lock').read_text())
    packages=lock.get('package',[])
    pkg_name=manifest['package']['name'] if 'package' in manifest else None
    if not pkg_name: return
    roots=[p for p in packages if p['name']==pkg_name]
    if len(roots)!=1: fail(f'{display(component)} lock root package count {len(roots)}'); return
    locked={d.split(' ')[0] for d in roots[0].get('dependencies',[])}
    direct=set(manifest.get('dependencies',{}))
    missing=sorted(direct-locked)
    if missing: fail(f'{display(component)} Cargo.lock missing direct deps {missing}')

def contract_checks():
    pp_agent=PP/'src/agent.rs'; pp_handler=PP/'src/application/handlers/agent.rs'; up_image=UP/'crates/agent-runtime/src/image_tools.rs'
    required=[
      'inspect.basic','artifact.content_addressed','artifact.dependency_closure','artifact.pin_set',
      'extract.alpha','extract.chroma_key','extract.color_range','extract.alpha_component','extract.provided_mask','extract.matte_refine',
      'render.composition_dag','render.affine_3x3','render.source_over','render.text_node',
      'compare.basic','compare.exact_spec','compare.regions','compare.masks','compare.geometry','compare.preview']
    pa=text(pp_agent); ua=text(up_image)
    for cap in required:
        if f'name: "{cap}"' not in pa: fail(f'PerfectPixel manifest missing {cap}')
        if cap not in ua: fail(f'UpAgent runtime does not consume expected capability {cap}')
    forbid(pp_agent,'edit.deterministic')
    require(pp_handler,
      'z: i32',
      'kind: AgentRenderNodeKind',
      'operations: Vec<AgentRasterOperationRequest>',
      'AgentRenderNodeKind', 'AgentTextNodeRequest', 'read_agent_font',
      'apply_raster_edits(&raster, &edits)', 'TextNode {', 'text_nodes: Vec<AgentTextNodeEvidence>')
    forbid(pp_handler, 'kind: Option<AgentRenderNodeKind>')
    require(PP/'tests/agent_protocol_contract.rs', 'agent_render_applies_canonical_operations_in_order')
    require(PP/'src/text.rs','fallback is disabled','rustybuzz::shape','font_sha256','raster_sha256')
    require(PP/'src/core/transform.rs','Serialize, Deserialize','tag = "op"')
    # General model use must not require image configuration; image tools must.
    comp=UP/'crates/agent-core/src/composition.rs'
    require_rust_expression(comp,
      'let selected_image_tools = resolved.tools.iter().any(|tool| crate::is_image_tool_name(tool));')
    require(comp,
      'image tools require an explicit image PerfectPixel identity',
      '(false, Some(_))',
      'selected promptGen tool requires an explicit promptgen identity')
    # Vergerail is the stock provider for both planes and generation is a distinct operation.
    require(PG/'crates/agent-contract/src/provider.rs','pub const PROVIDER_PROTOCOL_SCHEMA: &str = "vergerail.upagent/2"')
    require(UP/'crates/agent-provider/src/lib.rs','agent_contract::provider::{','PROVIDER_PROTOCOL_SCHEMA','image_generate','model_turn')
    require(VG/'docs/PROVIDER_CONTRACT.md','vergerail.upagent/2','image_generate','model_turn')
    require(VG/'src/bin/vergerail_upagent_provider.rs','image_generate','model_turn','schemaVersion')
    require(UP/'crates/agent-provider-api/src/lib.rs','model','image','ProviderImage','ImageGeneration')
    require(UP/'crates/agent-runtime/src/image_tools/semantic_edit.rs','IMAGE_EDIT_SEMANTIC_TOOL','ResolutionRequired','automaticRetryAllowed')
    require(UP/'crates/agent-runtime/src/imagemaker.rs','promptgen_compile','pinned promptGen adapter')
    # promptGen integration: pure image compilation is the only compile path wired
    # into UpAgent; standalone Codex/Web/MCP/screen capabilities remain outside
    # UpAgent provider/task authority.
    if not re.fullmatch(r'promptgen-\d+\.\d+\.\d+-\d{8}\.\d+', text(PG/'REVISION').strip()):
        fail('PromptGen revision must be an explicit version/date/candidate identity')
    require(PG/'crates/promptgen-cli/src/main.rs','--max-fidelity-attempts','--codex-sha256','codex-imagegen')
    require(UP/'crates/agent-core/src/image_prompt.rs',
      'upagent.image-prompt-artifact/3','CompiledImagePrompt','normalized_input_sha256',
      'compiler_outcome_sha256','provider_size')
    forbid(UP/'crates/agent-core/src/image_prompt.rs','CompiledScreen','promptgen-screen-handoff','Transparent,')
    require(UP/'crates/agent-runtime/src/promptgen.rs',
      'compile_image','"image".into()','"prompt-only".into()',
      'accepts generate-mode image requests only','cannot require reference images',
      '"codex-imagegen".into()','"--codex-sha256".into()',
      '"--max-fidelity-attempts".into()','codex_sha256')
    forbid(UP/'crates/agent-runtime/src/promptgen.rs','OPENAI_API_KEY','openai-image-edit',production_source=True)
    require(UP/'crates/agent-runtime/src/image_tools/semantic_edit.rs',
      'chatgpt-subscription','codex_binary_sha256','.fidelity_checks','.last()')
    forbid(UP/'crates/agent-runtime/src/image_tools/semantic_edit.rs','OPENAI_API_KEY','openai-image-edit')
    require(UP/'crates/agent-core/src/composition.rs',
      'AGENT_SPEC_SCHEMA_VERSION: u32 = 5','PromptGenCodexRuntimeSpec',
      'semantic editing requires an explicit pinned PromptGen Codex runtime')
    forbid(UP/'crates/agent-runtime/src/imagemaker.rs','codex-imagegen')
    require(UP/'crates/agent-runtime/src/image_tools.rs',
      'resolve_generation_prompt','generation size or background conflicts with the compiled prompt artifact',
      'ImagePromptArtifact::resolve',
      'self.promptgen.as_ref().map(|adapter| adapter.sha256())',
      'compiled prompt artifacts require a pinned promptGen compiler identity',
      'compiled_prompt_artifact_is_bound_to_the_admitted_compiler_identity')
    require(UP/'crates/agent-core/src/image_prompt.rs',
      'expected_compiler_sha256',
      'value.compiler_sha256 != expected_compiler_sha256',
      'schema/task/compiler/content binding mismatch')
    require(PG/'docs/ARCHITECTURE.md',
      'core는 image/screen 입력 의미·검증·진단·deterministic render의 권위다',
      'Codex adapter')

def verify_manifest(root: Path, label: str):
    manifest=root/'SOURCE_MANIFEST.sha256'
    for lineno,line in enumerate(manifest.read_text(encoding='utf-8').splitlines(),1):
        if not line.strip(): continue
        match=re.fullmatch(r'([0-9a-f]{64})  (.+)',line)
        if not match:
            fail(f'{label} SOURCE_MANIFEST.sha256 invalid line {lineno}'); continue
        expected_hash,relative=match.groups(); target=root/relative
        if not target.is_file():
            fail(f'{label} source manifest missing file {relative}'); continue
        actual=hashlib.sha256(target.read_bytes()).hexdigest()
        if actual!=expected_hash:
            fail(f'{label} source manifest hash mismatch {relative}')

def verify_integrity():
    lock_path=ROOT/'components.lock.json'
    try: lock=json.loads(lock_path.read_text(encoding='utf-8'))
    except Exception as e:
        fail(f'components.lock.json unreadable: {e}'); return
    if lock.get('schema')!='upagent.components-lock/1':
        fail('components.lock.json schema mismatch')
    expected={'upagent':UP,'perfectpixel':PP,'vergerail':VG}
    components=lock.get('components',{})
    if set(components)!=set(expected)|{'promptGen'}:
        fail(f'components.lock.json component set mismatch: {sorted(components)}')
    for name,root in expected.items():
        entry=components.get(name)
        if not isinstance(entry,dict): continue
        if entry.get('path')!=root.relative_to(ROOT).as_posix():
            fail(f'components.lock.json {name} path mismatch')
        digest,count,total=tree_identity(root)
        if entry.get('localTreeSha256')!=digest:
            fail(f'components.lock.json {name} localTreeSha256 mismatch')
        if entry.get('fileCount')!=count or entry.get('byteCount')!=total:
            fail(f'components.lock.json {name} inventory mismatch')
    prompt_entry=components.get('promptGen',{})
    try:
        _,snapshot=load_promptgen_source(PG)
        if prompt_entry.get('path')!='../promptGen':
            fail('components.lock.json PromptGen path must name the sibling source checkout')
        if prompt_entry.get('revision')!=snapshot['revision']:
            fail('components.lock.json PromptGen revision mismatch')
        package=tomllib.loads((PG/'Cargo.toml').read_text(encoding='utf-8'))
        version=package['workspace']['package']['version']
        if prompt_entry.get('version')!=version:
            fail('components.lock.json PromptGen version mismatch')
        shared_package=tomllib.loads((PG/'crates/agent-contract/Cargo.toml').read_text(encoding='utf-8'))
        shared=prompt_entry.get('sharedContract',{})
        if (shared.get('crate')!=shared_package['package']['name']
            or shared.get('version')!=shared_package['package']['version']
            or shared.get('manifest')!='crates/agent-contract/Cargo.toml'
            or shared.get('catalog')!='agent.shared-contract/1'
            or shared.get('defaultFeatures')!=[]):
            fail('components.lock.json shared contract identity mismatch')
        if prompt_entry.get('localTreeSha256')!=snapshot['tree_sha256']:
            fail('components.lock.json PromptGen source tree digest mismatch')
        if prompt_entry.get('fileCount')!=snapshot['file_count'] or prompt_entry.get('byteCount')!=snapshot['total_bytes']:
            fail('components.lock.json PromptGen source inventory mismatch')
        verify_promptgen_origin(PG,prompt_entry.get('origin',{}),snapshot)
    except Exception as e:
        fail(f'PromptGen source identity could not be verified: {e}')
    provenance_path=ROOT/'PACKAGE_PROVENANCE.json'
    try:
        provenance=json.loads(provenance_path.read_text(encoding='utf-8'))
    except Exception as e:
        fail(f'PACKAGE_PROVENANCE.json unreadable: {e}'); provenance={}
    if provenance.get('schema')!='upagent.package-provenance/1':
        fail('PACKAGE_PROVENANCE.json schema mismatch')
    expected_inputs={
        'upagent-perfectpixel-source-2026-09-13.tar(1).gz':'d84e265a958c85f1cf910121443374ba1def7fec7913fef587d17de5346430ce',
        'upagent(5).zip':'c3d88710958562bb9f34935909cc9045e54f7886b61cfe0f0370eb7e133a62dd',
        'promptGen(20260913-092415).zip':'4e8609a41ea15ca6dd10e46bb292beb0d163c9ef0b8c77c9cd3b3ac804e1e9c3',
        'Agent-Native-Tool-Design(6).md':'fad877b6bda9c32e6245b90e104de07ea84ccf8d5cb3ba02db898253b7d4cb79',
    }
    observed_inputs={entry.get('name'):entry.get('sha256') for entry in provenance.get('inputs',[]) if isinstance(entry,dict)}
    if observed_inputs!=expected_inputs:
        fail('PACKAGE_PROVENANCE.json input identity mismatch')
    manifest=PP/'SOURCE_MANIFEST.sha256'
    if not manifest.is_file():
        fail('PerfectPixel SOURCE_MANIFEST.sha256 missing')
    else:
        verify_manifest(PP,'PerfectPixel')
    root_manifest=ROOT/'SOURCE_MANIFEST.sha256'
    if root_manifest.is_file():
        verify_manifest(ROOT,'root')


def run_audit(name, label):
    script=ROOT/'scripts'/name
    if not script.is_file():
        fail(f'scripts/{name} missing')
        return
    result=subprocess.run([sys.executable,str(script)],cwd=ROOT,text=True,capture_output=True)
    if result.returncode!=0:
        fail(f'{label} failed: '+(result.stdout+result.stderr).strip())

def hygiene():
    if list(ROOT.rglob('__MACOSX')): fail('package contains __MACOSX')
    files=list(included_files(ROOT))
    patches=[relative.as_posix() for p,relative in files if p.name.endswith(('.orig','.rej'))]
    if patches: fail(f'package contains patch leftovers: {patches[:8]}')
    font_suffixes={'.ttf','.otf','.woff','.woff2'}
    fonts=[relative.as_posix() for p,relative in files if p.suffix.lower() in font_suffixes]
    if fonts: fail(f'package contains font files: {fonts[:8]}')

def main():
    for p in [UP/'Cargo.toml',PP/'Cargo.toml',VG/'Cargo.toml',PG/'Cargo.toml',ROOT/'components.lock.json']:
        if not p.is_file(): fail(f'missing {display(p)}')
    parse_all(); workspace_members();
    for c in (PP,VG,PG): lock_contains_direct_deps(c)
    rust_delimiters(ROOT, PG, generated, text, fail, display)
    contract_checks()
    run_audit('audit-connectivity.py', 'A-to-Z connectivity audit')
    run_audit('audit-schemas.py', 'schema audit')
    verify_integrity()
    hygiene()
    if errors:
        print('STATIC_VERIFY_FAILED')
        for e in errors: print('-',e)
        return 1
    files=[p for p in ROOT.rglob('*') if p.is_file() and not generated(p)]
    h=hashlib.sha256()
    for p in sorted(files):
        rel=p.relative_to(ROOT).as_posix().encode(); h.update(rel+b'\0'); h.update(hashlib.sha256(p.read_bytes()).digest())
    print('STATIC_VERIFY_PASS')
    print('files=',len(files),sep='')
    print('sourceAggregateSha256=',h.hexdigest(),sep='')
    print('runtimeBuild=NOT_RUN(not executed by this static gate)')
    return 0
if __name__=='__main__': sys.exit(main())
