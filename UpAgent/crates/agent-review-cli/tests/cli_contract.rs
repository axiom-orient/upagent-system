use std::{fs, process::Command};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

fn artifact_json(bytes: &[u8], media_type: &str, extension: &str) -> Value {
    let content_digest = format!("{:x}", Sha256::digest(bytes));
    let mut metadata_hasher = Sha256::new();
    metadata_hasher.update(b"agent-artifact-v1");
    for value in [
        content_digest.as_bytes(),
        media_type.as_bytes(),
        extension.as_bytes(),
    ] {
        metadata_hasher.update((value.len() as u64).to_be_bytes());
        metadata_hasher.update(value);
    }
    let metadata_digest = format!("{:x}", metadata_hasher.finalize());
    json!({
        "id": format!("art-{content_digest}-{metadata_digest}"),
        "sha256": content_digest,
        "mediaType": media_type,
        "byteLength": bytes.len(),
        "relativePath": format!(
            "{}/{}-{metadata_digest}.{extension}",
            &content_digest[..2],
            content_digest
        ),
    })
}

fn write_review_handoff(
    root: &std::path::Path,
    result: &[u8],
    evidence: &[u8],
) -> Result<(std::path::PathBuf, std::path::PathBuf, std::path::PathBuf), Box<dyn std::error::Error>>
{
    let artifact_root = root.join("artifacts");
    fs::create_dir_all(&artifact_root)?;
    let result_ref = artifact_json(result, "text/plain", "txt");
    let evidence_ref = artifact_json(evidence, "application/json", "json");
    for (reference, bytes) in [(&result_ref, result), (&evidence_ref, evidence)] {
        let relative = reference["relativePath"]
            .as_str()
            .ok_or("artifact relative path missing")?;
        let path = artifact_root.join(relative);
        fs::create_dir_all(path.parent().ok_or("artifact parent missing")?)?;
        fs::write(path, bytes)?;
    }
    let bundle_bytes = serde_json::to_vec(&json!({
        "schemaVersion": 3,
        "taskId": "task-handoff",
        "sessionId": "session-handoff",
        "taskDigest": "a".repeat(64),
        "compositionDigest": "b".repeat(64),
        "resultArtifact": result_ref,
        "resultUses": [{"sequence": 1, "event": "completed"}],
        "evidence": [{
            "artifact": evidence_ref,
            "uses": [{"sequence": 1, "event": "completed"}]
        }],
    }))?;
    let bundle_ref = artifact_json(
        &bundle_bytes,
        "application/vnd.upagent.review-subject+json; charset=utf-8",
        "json",
    );
    let bundle_relative = bundle_ref["relativePath"]
        .as_str()
        .ok_or("bundle relative path missing")?;
    let bundle_path = artifact_root.join(bundle_relative);
    fs::create_dir_all(bundle_path.parent().ok_or("bundle parent missing")?)?;
    fs::write(&bundle_path, bundle_bytes)?;
    let request_path = root.join("request.json");
    fs::write(
        &request_path,
        serde_json::to_vec(&json!({
            "schemaVersion": 2,
            "reviewId": "review-handoff",
            "subject": {
                "kind": "artifactBundle",
                "taskDigest": "a".repeat(64),
                "compositionDigest": "b".repeat(64),
                "bundle": bundle_ref,
                "includeArtifactIds": [
                    result_ref["id"].clone(),
                    evidence_ref["id"].clone()
                ]
            },
            "recipe": {
                "discoverers": [
                    {"role": "neutral", "routeId": "neutral"},
                    {"role": "critical", "routeId": "critical"},
                    {"role": "adversarial", "routeId": "adversarial"},
                ],
                "factCheckers": [
                    {"role": "fact_checker", "routeId": "validator-a"},
                    {"role": "fact_checker", "routeId": "validator-b"},
                ],
                "diversity": {
                    "required": false,
                    "minimumRouteCount": 1,
                    "minimumModelFamilies": 1,
                },
                "limits": {
                    "maximumFindings": 32,
                    "requiredConfirmations": 2,
                    "laneLeaseMs": 1000,
                },
            },
        }))?,
    )?;
    Ok((artifact_root, request_path, bundle_path))
}

fn run_handoff(
    root: &std::path::Path,
    artifact_root: &std::path::Path,
    request_path: &std::path::Path,
    routes_path: &std::path::Path,
    state_path: &std::path::Path,
) -> std::io::Result<std::process::Output> {
    Command::new(env!("CARGO_BIN_EXE_agent-review"))
        .args([
            "--state-root",
            state_path.to_str().ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "state path is not UTF-8")
            })?,
            "run",
            "--request",
            request_path.to_str().ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "request path is not UTF-8",
                )
            })?,
            "--routes",
            routes_path.to_str().ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "routes path is not UTF-8")
            })?,
            "--artifact-root",
            artifact_root.to_str().ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "artifact path is not UTF-8",
                )
            })?,
        ])
        .current_dir(root)
        .output()
}

#[test]
fn review_binary_owns_review_only_commands() -> Result<(), Box<dyn std::error::Error>> {
    let output = Command::new(env!("CARGO_BIN_EXE_agent-review"))
        .arg("--help")
        .output()?;
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout)?;
    assert!(help.contains("run"));
    assert!(help.contains("claim-repair"));
    Ok(())
}

#[test]
fn read_only_review_commands_do_not_create_missing_state() -> Result<(), Box<dyn std::error::Error>>
{
    let temp = TempDir::new()?;
    let state = temp.path().join("missing-state");
    let state_arg = state.to_str().ok_or("state path is not UTF-8")?;
    for command in ["inspect", "verify", "recover", "claim-repair"] {
        let output = Command::new(env!("CARGO_BIN_EXE_agent-review"))
            .args([
                "--state-root",
                state_arg,
                command,
                "--review",
                "review-test",
            ])
            .output()?;
        assert_eq!(output.status.code(), Some(2), "command: {command}");
        assert!(output.stderr.is_empty(), "command: {command}");
        let value: Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(value["ok"], false, "command: {command}");
    }
    assert!(!state.exists());

    let existing_state = temp.path().join("existing-state");
    fs::create_dir(&existing_state)?;
    let existing_state_arg = existing_state
        .to_str()
        .ok_or("existing state path is not UTF-8")?;
    let output = Command::new(env!("CARGO_BIN_EXE_agent-review"))
        .args([
            "--state-root",
            existing_state_arg,
            "claim-repair",
            "--review",
            "review-test",
        ])
        .output()?;
    assert_eq!(output.status.code(), Some(2));
    assert!(!existing_state.join("reviews").exists());
    Ok(())
}

#[test]
fn bundle_ingress_is_provider_free_and_precedes_routes_or_state()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let result = b"exact review output";
    let evidence = b"evidence";
    let (artifact_root, request_path, bundle_path) =
        write_review_handoff(temp.path(), result, evidence)?;
    let routes_path = temp.path().join("invalid-routes.json");
    fs::write(&routes_path, br#"{"schemaVersion":1,"routes":[]}"#)?;
    let state_path = temp.path().join("state");

    let output = run_handoff(
        temp.path(),
        &artifact_root,
        &request_path,
        &routes_path,
        &state_path,
    )?;
    assert_eq!(output.status.code(), Some(2));
    let stdout: Value = serde_json::from_slice(&output.stdout)?;
    let message = stdout["error"]["message"]
        .as_str()
        .ok_or("CLI error message missing")?;
    assert!(message.contains("routes") || message.contains("route") || message.contains("skills"));
    assert!(!message.contains("inline review subject"));
    assert!(
        !state_path.exists(),
        "bundle preflight must not create state"
    );

    fs::write(
        artifact_root.join(
        serde_json::from_slice::<Value>(&fs::read(&bundle_path)?)?["resultArtifact"]
                ["relativePath"]
                .as_str()
                .ok_or("result path missing")?,
        ),
        b"tampered review output",
    )?;
    let tampered = run_handoff(
        temp.path(),
        &artifact_root,
        &request_path,
        &routes_path,
        &state_path,
    )?;
    assert_eq!(tampered.status.code(), Some(2));
    let tampered_stdout: Value = serde_json::from_slice(&tampered.stdout)?;
    let tampered_message = tampered_stdout["error"]["message"]
        .as_str()
        .ok_or("tamper error message missing")?;
    assert!(
        tampered_message.contains("content does not match")
            || tampered_message.contains("byte length does not match")
            || tampered_message.contains("review artifact")
            || tampered_message.contains("skills")
    );
    assert!(!state_path.exists());

    let bundle: Value = serde_json::from_slice(&fs::read(&bundle_path)?)?;
    let result_path = artifact_root.join(
        bundle["resultArtifact"]["relativePath"]
            .as_str()
            .ok_or("result path missing")?,
    );
    fs::write(result_path, result)?;
    let mut request: Value = serde_json::from_slice(&fs::read(&request_path)?)?;
    request["subject"]["taskDigest"] = Value::String("c".repeat(64));
    fs::write(&request_path, serde_json::to_vec(&request)?)?;
    let mismatch = run_handoff(
        temp.path(),
        &artifact_root,
        &request_path,
        &routes_path,
        &state_path,
    )?;
    assert_eq!(mismatch.status.code(), Some(2));
    let mismatch_stdout: Value = serde_json::from_slice(&mismatch.stdout)?;
    let mismatch_message = mismatch_stdout["error"]["message"]
        .as_str()
        .ok_or("inline mismatch error message missing")?;
    assert!(mismatch_message.contains("task/composition") || mismatch_message.contains("digest"));
    assert!(!state_path.exists());
    Ok(())
}
