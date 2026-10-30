//! Repository contract checks that keep documentation attached to the current transport surface.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn markdown_files(directory: &Path, found: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory).expect("readable documentation directory") {
        let path = entry.expect("readable directory entry").path();
        if path.is_dir() {
            markdown_files(&path, found);
        } else if path.extension().is_some_and(|value| value == "md") {
            found.push(path);
        }
    }
}

fn all_markdown() -> Vec<PathBuf> {
    let root = repo_root();
    let mut found = vec![root.join("README.md"), root.join("THIRD_PARTY_NOTICES.md")];
    markdown_files(&root.join("docs"), &mut found);
    found.sort();
    found
}

fn local_link_targets(text: &str) -> Vec<String> {
    let mut targets = Vec::new();
    let mut index = 0usize;
    while let Some(open) = text[index..].find("](") {
        let start = index + open + 2;
        let Some(close) = text[start..].find(')') else {
            break;
        };
        let target = &text[start..start + close];
        index = start + close;
        let target = target.split_whitespace().next().unwrap_or(target);
        if target.is_empty()
            || target.starts_with('#')
            || target.starts_with("http://")
            || target.starts_with("https://")
            || target.starts_with("mailto:")
        {
            continue;
        }
        targets.push(target.split('#').next().unwrap_or(target).to_string());
    }
    targets
}

#[test]
fn every_local_documentation_link_resolves() {
    let root = repo_root();
    let mut broken = Vec::new();
    for document in all_markdown() {
        let text = fs::read_to_string(&document).expect("readable markdown document");
        let parent = document.parent().expect("document has parent");
        for target in local_link_targets(&text) {
            if !parent.join(&target).exists() {
                broken.push(format!(
                    "{} -> {target}",
                    document.strip_prefix(&root).unwrap_or(&document).display()
                ));
            }
        }
    }
    assert!(
        broken.is_empty(),
        "broken documentation links:\n  {}",
        broken.join("\n  ")
    );
}

#[test]
fn readme_documents_the_complete_verification_gate() {
    let readme = fs::read_to_string(repo_root().join("README.md")).expect("readable README");
    for command in [
        "cargo fmt --all -- --check",
        "cargo clippy --locked --workspace --all-targets --all-features -- -D warnings",
        "cargo test --locked --workspace --all-targets --all-features",
    ] {
        assert!(
            readme.contains(command),
            "README missing verification command: {command}"
        );
    }
}

#[test]
fn unix_only_compile_contract_and_documentation_are_consistent() {
    const COMPILE_CONTRACT: &str =
        "#[cfg(not(unix))]\ncompile_error!(\"perfectpixel supports Unix targets only\");";
    let lib = fs::read_to_string(repo_root().join("src/lib.rs")).expect("readable crate root");
    assert!(lib.starts_with(COMPILE_CONTRACT));
    assert_eq!(lib.matches(COMPILE_CONTRACT).count(), 1);
}

/// Reads transport syntax from the actual CLI adapter. Semantics remain owned by `Operation`;
/// this parser exists only to ensure documentation/schema do not advertise unreachable syntax.
fn dispatched_commands() -> BTreeSet<String> {
    let source = fs::read_to_string(repo_root().join("src/application/cli.rs"))
        .expect("readable CLI adapter");
    let start = source.find("match command {").expect("CLI command match");
    let dispatch = &source[start..];
    let end = dispatch.find("other => Err").expect("unknown-command arm");
    let mut commands = BTreeSet::new();
    for line in dispatch[..end].lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix('"') else {
            continue;
        };
        let Some(name) = rest.split('"').next() else {
            continue;
        };
        if line.contains("=>") && !name.starts_with('-') && !name.is_empty() {
            commands.insert(name.to_string());
        }
    }
    assert!(!commands.is_empty(), "failed to parse CLI commands");
    commands
}

fn advertised_commands() -> BTreeSet<String> {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_perfectpixel"))
        .arg("schema")
        .output()
        .expect("run perfectpixel schema");
    assert!(output.status.success(), "schema command must succeed");
    let payload: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("schema emits JSON");
    payload["commands"]
        .as_array()
        .expect("schema commands array")
        .iter()
        .map(|value| value.as_str().expect("command string").to_string())
        .collect()
}

#[test]
fn schema_output_lists_exactly_the_dispatched_commands() {
    assert_eq!(advertised_commands(), dispatched_commands());
}

#[test]
fn capability_matrix_traces_every_public_command() {
    let matrix = fs::read_to_string(repo_root().join("docs/FUNCTION_MATRIX.md"))
        .expect("readable capability matrix");
    let missing = advertised_commands()
        .into_iter()
        .filter(|command| command != "schema")
        .filter(|command| !matrix.contains(command))
        .collect::<Vec<_>>();
    assert!(
        missing.is_empty(),
        "capability matrix missing: {}",
        missing.join(", ")
    );
}

#[test]
fn no_generated_or_platform_metadata_files_are_packaged() {
    fn scan_patch_files(directory: &Path, root: &Path, offenders: &mut BTreeSet<String>) {
        for entry in fs::read_dir(directory).expect("readable directory") {
            let entry = entry.expect("readable entry");
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if matches!(
                name.as_ref(),
                "target" | ".git" | ".build-target" | "dist" | "__pycache__" | "__MACOSX"
            ) {
                continue;
            }
            if entry.file_type().expect("readable file type").is_dir() {
                scan_patch_files(&path, root, offenders);
            } else if name.ends_with(".orig") || name.ends_with(".rej") {
                offenders.insert(
                    path.strip_prefix(root)
                        .expect("source path")
                        .display()
                        .to_string(),
                );
            }
        }
    }
    let manifest = fs::read_to_string(repo_root().join("SOURCE_MANIFEST.sha256"))
        .expect("readable source manifest");
    let offenders = manifest
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| {
            let (_, relative) = line.split_once("  ").expect("source manifest entry");
            let name = Path::new(relative)
                .file_name()
                .and_then(|value| value.to_str())
                .expect("source file name");
            (name == ".DS_Store" || name.ends_with(".orig") || name.ends_with(".rej"))
                .then_some(relative.to_owned())
        })
        .collect::<BTreeSet<_>>();
    let mut offenders = offenders;
    scan_patch_files(&repo_root(), &repo_root(), &mut offenders);
    assert!(
        offenders.is_empty(),
        "generated/platform files packaged: {}",
        offenders.into_iter().collect::<Vec<_>>().join(", ")
    );
}
