use std::{fs, path::Path, process::Command};

use serde_json::Value;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

fn write_provider_config(
    temp: &Path,
    name: &str,
    executable: &Path,
    protocol_schema: &str,
) -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    let digest = format!("{:x}", Sha256::digest(fs::read(executable)?));
    let plugins = Command::new(env!("CARGO_BIN_EXE_agent"))
        .args(["plugins"])
        .output()?;
    assert!(plugins.status.success(), "plugin catalog command failed");
    let plugins: Value = serde_json::from_slice(&plugins.stdout)?;
    let provider = plugins["plugins"]
        .as_array()
        .and_then(|plugins| {
            plugins.iter().find(|plugin| {
                plugin["id"]
                    .as_str()
                    .is_some_and(|id| id.contains(".provider."))
            })
        })
        .ok_or("linked provider plugin missing from catalog")?;
    let path = temp.join(name);
    fs::write(
        &path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "pluginId": provider["id"],
            "manifestSha256": provider["linkedManifestSha256"],
            "configuration": {
                "protocolSchema": protocol_schema,
                "executable": executable,
                "executableSha256": digest,
                "runtimePackage": temp.join("runtime-package"),
                "managedHome": temp.join("managed-home"),
                "model": "test-model",
                "workspace": temp.join("workspace")
            }
        }))?,
    )?;
    Ok(path)
}

#[test]
fn parser_errors_are_json_on_stdout() -> Result<(), Box<dyn std::error::Error>> {
    for arguments in [&["--definitely-invalid"][..], &["schema", "nope"][..]] {
        let output = Command::new(env!("CARGO_BIN_EXE_agent"))
            .args(arguments)
            .output()?;
        assert_eq!(output.status.code(), Some(2), "arguments: {arguments:?}");
        assert!(
            output.stderr.is_empty(),
            "parser errors must not write human text to stderr: {arguments:?}"
        );
        let value: Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(value["ok"], false, "arguments: {arguments:?}");
        assert_eq!(value["error"]["code"], "cli_failed");
    }
    Ok(())
}

#[test]
fn inspect_command_is_flat_and_validates_identifiers() -> Result<(), Box<dyn std::error::Error>> {
    let state = TempDir::new()?;
    let output = Command::new(env!("CARGO_BIN_EXE_agent"))
        .args([
            "--state-root",
            state.path().to_str().ok_or("state path is not UTF-8")?,
            "inspect",
            "--session",
            "bad/id",
            "--task",
            "task-test",
        ])
        .output()?;
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(output.stderr, [] as [u8; 0]);
    let value: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], "cli_failed");
    assert!(
        value["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("invalid") || message.contains("identifier"))
    );
    Ok(())
}

#[test]
fn general_agent_does_not_expose_review_campaign() -> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let state = temp.path().join("missing-state");
    let state_arg = state.to_str().ok_or("state path is not UTF-8")?;
    for command in ["review", "agent-review"] {
        let output = Command::new(env!("CARGO_BIN_EXE_agent"))
            .args(["--state-root", state_arg, command])
            .output()?;
        assert_eq!(output.status.code(), Some(2), "command: {command}");
        assert!(output.stderr.is_empty(), "command: {command}");
        let value: Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(value["ok"], false, "command: {command}");
    }
    assert!(!state.exists());
    Ok(())
}

#[test]
fn dynamic_extension_drift_and_v1_spec_fail_before_state_or_provider()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let skills_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills");
    let package_path = temp.path().join("pack.json");
    fs::write(
        &package_path,
        r#"{"schemaVersion":1,"id":"creator/pack","version":"1","profiles":["creator"],"requiredCapabilities":[],"skills":[{"id":"creator/pack-extra","version":"1","description":"extra","profiles":["creator"],"requiredCapabilities":[],"instructions":"Use the supplied facts."}]}"#,
    )?;
    let package_path = package_path.canonicalize()?;
    let spec_path = temp.path().join("spec.json");
    fs::write(
        &spec_path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "schemaVersion": 5,
            "profile": "creator",
            "skills": ["creator/chat"],
            "tools": ["request_input"],
            "memory": "none",
            "review": "none",
            "effects": "none",
            "effectApproval": "deny",
            "isolation": {"kind":"native"},
            "extensions": [{"tier":"dynamic","id":"creator/pack","source":package_path}]
        }))?,
    )?;
    let provider_executable = std::env::current_exe()?;
    let provider_config_path = write_provider_config(
        temp.path(),
        "provider.json",
        &provider_executable,
        "vergerail.upagent/2",
    )?;
    let request_path = temp.path().join("request.json");
    fs::write(
        &request_path,
        r#"{"schemaVersion":3,"taskId":"task-dynamic-test","sessionId":"session-dynamic-test","objective":"compose"}"#,
    )?;
    let output = Command::new(env!("CARGO_BIN_EXE_agent"))
        .args([
            "--skills-root",
            skills_root.to_str().ok_or("skills path is not UTF-8")?,
            "compose",
            "--spec",
            spec_path.to_str().ok_or("spec path is not UTF-8")?,
            "--provider-config",
            provider_config_path
                .to_str()
                .ok_or("provider config path is not UTF-8")?,
            "--request",
            request_path.to_str().ok_or("request path is not UTF-8")?,
        ])
        .output()?;
    assert!(
        output.status.success(),
        "compose failed: status={:?} stdout={} stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let composed: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(composed["providerConnected"], false);
    assert_eq!(composed["stateWritten"], false);
    let digest = composed["compositionDigest"]
        .as_str()
        .ok_or("compose digest missing")?
        .to_owned();

    let mut package = fs::read_to_string(&package_path)?;
    package = package.replacen("Use", "use", 1);
    fs::write(&package_path, package)?;
    let state = temp.path().join("state");
    let drift = Command::new(env!("CARGO_BIN_EXE_agent"))
        .args([
            "--state-root",
            state.to_str().ok_or("state path is not UTF-8")?,
            "--skills-root",
            skills_root.to_str().ok_or("skills path is not UTF-8")?,
            "run",
            "--spec",
            spec_path.to_str().ok_or("spec path is not UTF-8")?,
            "--provider-config",
            provider_config_path
                .to_str()
                .ok_or("provider config path is not UTF-8")?,
            "--request",
            request_path.to_str().ok_or("request path is not UTF-8")?,
            "--expect-digest",
            &digest,
        ])
        .output()?;
    assert_eq!(drift.status.code(), Some(2));
    assert!(!state.exists(), "digest drift must precede state creation");
    let drift_output: Value = serde_json::from_slice(&drift.stdout)?;
    assert!(
        drift_output["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("composition digest mismatch"))
    );

    // Restore the data extension and drift only the provider route. The
    // provider binding is part of the immutable composition identity, so the
    // mismatch must be rejected before state or provider connection.
    fs::write(
        &package_path,
        r#"{"schemaVersion":1,"id":"creator/pack","version":"1","profiles":["creator"],"requiredCapabilities":[],"skills":[{"id":"creator/pack-extra","version":"1","description":"extra","profiles":["creator"],"requiredCapabilities":[],"instructions":"Use the supplied facts."}]}"#,
    )?;
    let mut provider_config: Value = serde_json::from_slice(&fs::read(&provider_config_path)?)?;
    provider_config["configuration"]["model"] = Value::String("different-model".to_owned());
    fs::write(
        &provider_config_path,
        serde_json::to_vec_pretty(&provider_config)?,
    )?;
    let provider_drift_state = temp.path().join("provider-drift-state");
    let provider_drift = Command::new(env!("CARGO_BIN_EXE_agent"))
        .args([
            "--state-root",
            provider_drift_state
                .to_str()
                .ok_or("provider drift state path is not UTF-8")?,
            "--skills-root",
            skills_root.to_str().ok_or("skills path is not UTF-8")?,
            "run",
            "--spec",
            spec_path.to_str().ok_or("spec path is not UTF-8")?,
            "--provider-config",
            provider_config_path
                .to_str()
                .ok_or("provider config path is not UTF-8")?,
            "--request",
            request_path.to_str().ok_or("request path is not UTF-8")?,
            "--expect-digest",
            &digest,
        ])
        .output()?;
    assert_eq!(provider_drift.status.code(), Some(2));
    assert!(
        !provider_drift_state.exists(),
        "provider route drift must fail before state creation"
    );
    let provider_drift_output: Value = serde_json::from_slice(&provider_drift.stdout)?;
    assert!(
        provider_drift_output["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("composition digest mismatch"))
    );

    let v1_path = temp.path().join("v1-spec.json");
    let mut v1: Value = serde_json::from_slice(&fs::read(&spec_path)?)?;
    v1["schemaVersion"] = Value::from(1);
    fs::write(&v1_path, serde_json::to_vec(&v1)?)?;
    let v1_output = Command::new(env!("CARGO_BIN_EXE_agent"))
        .args([
            "--skills-root",
            skills_root.to_str().ok_or("skills path is not UTF-8")?,
            "compose",
            "--spec",
            v1_path.to_str().ok_or("v1 spec path is not UTF-8")?,
            "--provider-config",
            provider_config_path
                .to_str()
                .ok_or("provider config path is not UTF-8")?,
            "--request",
            request_path.to_str().ok_or("request path is not UTF-8")?,
        ])
        .output()?;
    assert_eq!(v1_output.status.code(), Some(2));
    let v1_error: Value = serde_json::from_slice(&v1_output.stdout)?;
    assert!(
        v1_error["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("unsupported AgentSpec schema"))
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn semantic_mask_helper_is_pinned_by_cli_composition_and_drift_fails_before_state()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let skills_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills");
    let schema_output = Command::new(env!("CARGO_BIN_EXE_agent"))
        .args(["schema", "agent-spec"])
        .output()?;
    assert!(schema_output.status.success());
    let schema: Value = serde_json::from_slice(&schema_output.stdout)?;
    assert!(schema["properties"]["semanticMask"].is_object());
    assert!(schema["definitions"]["SemanticMaskSpec"]["properties"]["timeoutMs"].is_object());
    let provider = temp.path().join("provider.sh");
    fs::write(
        &provider,
        "#!/bin/sh\nIFS= read -r request\nrequest_id=$(printf '%s' \"$request\" | sed -n 's/.*\"requestId\":\"\\([^\"]*\\)\".*/\\1/p')\nprintf '%s\\n' \"{\\\"schemaVersion\\\":2,\\\"requestId\\\":\\\"$request_id\\\",\\\"operation\\\":\\\"model_turn\\\",\\\"text\\\":\\\"configured\\\",\\\"toolCalls\\\":[],\\\"usage\\\":{\\\"inputTokens\\\":1,\\\"outputTokens\\\":1,\\\"totalTokens\\\":2}}\"\n",
    )?;
    let mut provider_permissions = fs::metadata(&provider)?.permissions();
    provider_permissions.set_mode(0o700);
    fs::set_permissions(&provider, provider_permissions)?;
    let provider_config_path = write_provider_config(
        temp.path(),
        "provider-v2.json",
        &provider,
        "vergerail.upagent/2",
    )?;
    let shell_digest = format!("{:x}", Sha256::digest(fs::read("/bin/sh")?));
    let helper = temp.path().join("semantic-helper.sh");
    fs::write(&helper, "#!/bin/sh\nexit 0\n")?;
    let mut permissions = fs::metadata(&helper)?.permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&helper, permissions)?;
    let helper_digest = format!("{:x}", Sha256::digest(fs::read(&helper)?));
    let spec_path = temp.path().join("spec.json");
    fs::write(
        &spec_path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "schemaVersion": 5,
            "profile": "creator",
            "skills": ["creator/chat"],
            "tools": ["image_extract"],
            "memory": "none",
            "review": "none",
            "effects": "none",
            "effectApproval": "deny",
            "isolation": {"kind":"native"},
            "image": {
                "perfectpixelExecutable":"/bin/sh",
                "perfectpixelSha256":shell_digest,
                "protocolSchema":"perfectpixel.agent-image/2",
                "protocolVersion":"2.0.0",
                "behaviorVersion":"2.0.0",
                "capabilityManifestSha256":"cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
                "implementationVersion":"0.3.0"
            },
            "semanticMask": {
                "executable": helper,
                "executableSha256": helper_digest,
                "helperIdentity": "fixture.semantic/1",
                "helperVersion": "2026.08",
                "timeoutMs": 1000
            }
        }))?,
    )?;
    let request_path = temp.path().join("request.json");
    fs::write(
        &request_path,
        r#"{"schemaVersion":3,"taskId":"task-semantic-cli","sessionId":"session-semantic-cli","objective":"compose semantic mask","imageTask":{"schema":"upagent.image-task/5","goal":"extract a foreground","source":{"mode":"provided","artifact":{"id":"art-0000000000000000000000000000000000000000000000000000000000000000","sha256":"0000000000000000000000000000000000000000000000000000000000000000","mediaType":"image/png","byteLength":1,"relativePath":"00/0000000000000000000000000000000000000000000000000000000000000000.png"}},"maximumAttempts":1,"visualCriteria":["foreground is extracted"],"verification":{"visualReview":true}}}"#,
    )?;
    let compose = Command::new(env!("CARGO_BIN_EXE_agent"))
        .args([
            "--skills-root",
            skills_root.to_str().ok_or("skills path is not UTF-8")?,
            "compose",
            "--spec",
            spec_path.to_str().ok_or("spec path is not UTF-8")?,
            "--provider-config",
            provider_config_path
                .to_str()
                .ok_or("provider config path is not UTF-8")?,
            "--request",
            request_path.to_str().ok_or("request path is not UTF-8")?,
        ])
        .output()?;
    assert!(
        compose.status.success(),
        "semantic helper compose failed: stdout={} stderr={}",
        String::from_utf8_lossy(&compose.stdout),
        String::from_utf8_lossy(&compose.stderr)
    );
    let composed: Value = serde_json::from_slice(&compose.stdout)?;
    assert_eq!(
        composed["composition"]["spec"]["semanticMask"]["helperIdentity"],
        "fixture.semantic/1"
    );
    assert_eq!(
        composed["composition"]["spec"]["semanticMask"]["executableSha256"],
        helper_digest
    );
    assert_eq!(composed["providerConnected"], false);
    let state = temp.path().join("state");
    let composition_digest = composed["compositionDigest"]
        .as_str()
        .ok_or("composition digest missing")?;
    let run = Command::new(env!("CARGO_BIN_EXE_agent"))
        .args([
            "--state-root",
            state.to_str().ok_or("state path is not UTF-8")?,
            "--skills-root",
            skills_root.to_str().ok_or("skills path is not UTF-8")?,
            "run",
            "--spec",
            spec_path.to_str().ok_or("spec path is not UTF-8")?,
            "--provider-config",
            provider_config_path
                .to_str()
                .ok_or("provider config path is not UTF-8")?,
            "--request",
            request_path.to_str().ok_or("request path is not UTF-8")?,
            "--expect-digest",
            composition_digest,
        ])
        .output()?;
    assert_ne!(
        run.status.code(),
        Some(2),
        "configured run failed before durable result: stdout={} stderr={}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let run_result: Value = serde_json::from_slice(&run.stdout)?;
    assert!(run_result.get("error").is_none());
    assert!(
        state.exists(),
        "configured CLI run must create durable state"
    );

    fs::write(&helper, "#!/bin/sh\nexit 17\n")?;
    let drift_state = temp.path().join("drift-state");
    let drift = Command::new(env!("CARGO_BIN_EXE_agent"))
        .args([
            "--state-root",
            drift_state
                .to_str()
                .ok_or("drift state path is not UTF-8")?,
            "--skills-root",
            skills_root.to_str().ok_or("skills path is not UTF-8")?,
            "compose",
            "--spec",
            spec_path.to_str().ok_or("spec path is not UTF-8")?,
            "--provider-config",
            provider_config_path
                .to_str()
                .ok_or("provider config path is not UTF-8")?,
            "--request",
            request_path.to_str().ok_or("request path is not UTF-8")?,
        ])
        .output()?;
    assert_eq!(drift.status.code(), Some(2));
    assert!(
        !drift_state.exists(),
        "helper drift must fail before state creation"
    );
    let drift_output: Value = serde_json::from_slice(&drift.stdout)?;
    assert!(
        drift_output["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("semantic-mask") && message.contains("digest"))
    );
    Ok(())
}
