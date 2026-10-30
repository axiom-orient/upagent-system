use std::fs;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use perfectpixel::{
    agent_capability_manifest, apply_raster_edits, dependency_closure_sha256, sha256_hex,
    AgentArtifactDependency, AgentArtifactPinSet, ImageCodec, PngEncoder, Raster, RasterEdit,
    ResampleFilter, AGENT_BEHAVIOR_VERSION, AGENT_PIN_SET_SCHEMA, AGENT_PROTOCOL_SCHEMA,
    AGENT_PROTOCOL_VERSION,
};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn temp_root(label: &str) -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time")
        .as_nanos();
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "perfectpixel-agent-v2-{label}-{}-{nanos}-{sequence}",
        std::process::id()
    ))
}

fn valid_png() -> Vec<u8> {
    vec![
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x04, 0x00, 0x00, 0x00, 0xb5,
        0x1c, 0x0c, 0x02, 0x00, 0x00, 0x00, 0x0b, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0x64,
        0xf8, 0x0f, 0x00, 0x01, 0x05, 0x01, 0x01, 0x27, 0x18, 0xe3, 0x66, 0x00, 0x00, 0x00, 0x00,
        0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ]
}

#[test]
fn agent_manifest_exposes_v2_foundation_capabilities() {
    let manifest = agent_capability_manifest("0.3.0");
    assert_eq!(manifest.schema, AGENT_PROTOCOL_SCHEMA);
    assert_eq!(manifest.protocol_version, AGENT_PROTOCOL_VERSION);
    assert_eq!(manifest.behavior_version, AGENT_BEHAVIOR_VERSION);
    for required in [
        "inspect.basic",
        "artifact.content_addressed",
        "artifact.dependency_closure",
        "artifact.pin_set",
        "render.text_node",
        "compare.basic",
        "compare.exact_spec",
        "compare.regions",
        "compare.masks",
        "compare.geometry",
        "compare.preview",
    ] {
        assert!(manifest
            .capabilities
            .iter()
            .any(|capability| capability.name == required));
    }
}

#[test]
fn dependency_closure_is_order_independent_and_deduplicated() {
    let a = AgentArtifactDependency {
        sha256: "a".repeat(64),
        media_type: "image/png".to_owned(),
        byte_length: 10,
    };
    let b = AgentArtifactDependency {
        sha256: "b".repeat(64),
        media_type: "image/png".to_owned(),
        byte_length: 20,
    };
    let left = dependency_closure_sha256(&[a.clone(), b.clone(), a.clone()]).unwrap();
    let right = dependency_closure_sha256(&[b, a]).unwrap();
    assert_eq!(left, right);
}

#[test]
fn pin_set_rejects_duplicates() {
    let set = AgentArtifactPinSet {
        schema: AGENT_PIN_SET_SCHEMA.to_owned(),
        pins: vec!["a".repeat(64), "a".repeat(64)],
    };
    assert!(set.validate().is_err());
}

#[test]
fn agent_schema_cli_is_machine_readable() {
    let output = Command::new(env!("CARGO_BIN_EXE_perfectpixel"))
        .arg("agent-schema")
        .output()
        .expect("run agent-schema");
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).expect("json");
    assert_eq!(value["schema"], AGENT_PROTOCOL_SCHEMA);
    assert_eq!(value["protocolVersion"], AGENT_PROTOCOL_VERSION);
}

#[test]
fn agent_inspect_binds_request_source_and_dependency_receipt() {
    let root = temp_root("success");
    fs::create_dir_all(&root).unwrap();
    let input = root.join("input.png");
    let png = valid_png();
    fs::write(&input, &png).unwrap();
    let request = root.join("request.json");
    let digest = sha256_hex(&png);
    let body = serde_json::json!({
        "schema": "perfectpixel.agent-image/inspect/2",
        "requestId": "req-success",
        "source": {
            "path": input,
            "expectedSha256": digest,
            "mediaType": "image/png",
            "byteLength": png.len(),
            "kind": "working_raster",
            "pixelSpec": {"format":"rgba8","colorSpace":"srgb","alphaMode":"straight"}
        }
    });
    let request_bytes = serde_json::to_vec(&body).unwrap();
    fs::write(&request, &request_bytes).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_perfectpixel"))
        .args(["agent-inspect", "--request"])
        .arg(&request)
        .output()
        .expect("run agent-inspect");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let dependency = AgentArtifactDependency {
        sha256: digest.clone(),
        media_type: "image/png".to_owned(),
        byte_length: png.len() as u64,
    };
    assert_eq!(value["schema"], "perfectpixel.agent-image/inspect-result/2");
    assert_eq!(value["source"]["sha256"], digest);
    assert_eq!(value["inspection"]["pixelFormat"], "rgba8");
    assert_eq!(value["inspection"]["colorSpace"], "srgb");
    assert_eq!(
        value["receipt"]["requestSha256"],
        sha256_hex(&request_bytes)
    );
    assert_eq!(
        value["receipt"]["dependencyClosureSha256"],
        dependency_closure_sha256(&[dependency]).unwrap()
    );
    assert_eq!(value["receipt"]["determinism"], "bit_exact");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn agent_inspect_rejects_content_address_mismatch() {
    let root = temp_root("mismatch");
    fs::create_dir_all(&root).unwrap();
    let input = root.join("input.png");
    let png = valid_png();
    fs::write(&input, &png).unwrap();
    let request = root.join("request.json");
    let body = serde_json::json!({
        "schema": "perfectpixel.agent-image/inspect/2",
        "requestId": "req-mismatch",
        "source": {
            "path": input,
            "expectedSha256": "0".repeat(64),
            "mediaType": "image/png",
            "byteLength": png.len(),
            "kind": "working_raster",
            "pixelSpec": {"format":"rgba8","colorSpace":"srgb","alphaMode":"straight"}
        }
    });
    fs::write(&request, serde_json::to_vec(&body).unwrap()).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_perfectpixel"))
        .args(["agent-inspect", "--request"])
        .arg(&request)
        .output()
        .expect("run agent-inspect");
    assert!(!output.status.success());
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn agent_inspect_rejects_uppercase_and_mixed_case_source_digests() {
    let root = temp_root("noncanonical-digest");
    fs::create_dir_all(&root).unwrap();
    let input = root.join("input.png");
    let png = valid_png();
    fs::write(&input, &png).unwrap();
    let digest = sha256_hex(&png);
    let mut variants = vec![digest.to_ascii_uppercase()];
    let mut mixed = digest.clone().into_bytes();
    if let Some(byte) = mixed
        .iter_mut()
        .find(|byte| byte.is_ascii_hexdigit() && byte.is_ascii_alphabetic())
    {
        *byte = byte.to_ascii_uppercase();
    } else {
        mixed[0] = b'A';
    }
    variants.push(String::from_utf8(mixed).unwrap());
    for (index, noncanonical) in variants.into_iter().enumerate() {
        let request = root.join(format!("request-{index}.json"));
        let body = serde_json::json!({
            "schema": "perfectpixel.agent-image/inspect/2",
            "requestId": format!("req-noncanonical-{index}"),
            "source": {
                "path": input,
                "expectedSha256": noncanonical,
                "mediaType": "image/png",
                "byteLength": png.len(),
                "kind": "working_raster",
                "pixelSpec": {"format":"rgba8","colorSpace":"srgb","alphaMode":"straight"}
            }
        });
        fs::write(&request, serde_json::to_vec(&body).unwrap()).unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_perfectpixel"))
            .args(["agent-inspect", "--request"])
            .arg(&request)
            .output()
            .expect("run agent-inspect");
        assert!(!output.status.success(), "noncanonical digest was accepted");
    }
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn agent_render_applies_canonical_operations_in_order() {
    let root = temp_root("render-operations");
    fs::create_dir_all(&root).unwrap();
    let input = root.join("input.png");
    let output_dir = root.join("rendered");
    let image = Raster::new(
        2,
        3,
        vec![
            1, 0, 0, 255, 2, 0, 0, 255, 3, 0, 0, 255, 4, 0, 0, 255, 5, 0, 0, 255, 6, 0, 0, 255,
        ],
    )
    .unwrap();
    let encoded = PngEncoder::encode_rgba(&image).unwrap();
    fs::write(&input, &encoded).unwrap();
    let expected = apply_raster_edits(
        &image,
        &[
            RasterEdit::Crop {
                x: 0,
                y: 1,
                width: 2,
                height: 2,
            },
            RasterEdit::RotateQuarterTurns { quarter_turns: 1 },
            RasterEdit::FlipHorizontal,
            RasterEdit::Resize {
                width: 2,
                height: 2,
                filter: ResampleFilter::Nearest,
            },
        ],
    )
    .unwrap();
    let request = root.join("request.json");
    let body = serde_json::json!({
        "schema": "perfectpixel.agent-image/render/2",
        "requestId": "req-render-operations",
        "canvas": {"width": 2, "height": 2, "background": [0, 0, 0, 0]},
        "nodes": [{
            "id": "source",
            "z": 0,
            "kind": "raster",
            "source": {
                "path": input,
                "expectedSha256": sha256_hex(&encoded),
                "mediaType": "image/png",
                "byteLength": encoded.len(),
                "kind": "working_raster",
                "pixelSpec": {"format":"rgba8","colorSpace":"srgb","alphaMode":"straight"}
            },
            "transform": {"matrix": [1.0,0.0,0.0,0.0,1.0,0.0,0.0,0.0,1.0]},
            "opacity": 255,
            "filter": "nearest",
            "operations": [
                {"op":"crop","x":0,"y":1,"width":2,"height":2},
                {"op":"rotate","quarterTurns":1},
                {"op":"flip","axis":"horizontal"},
                {"op":"resize","width":2,"height":2,"filter":"nearest"}
            ]
        }]
    });
    fs::write(&request, serde_json::to_vec(&body).unwrap()).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_perfectpixel"))
        .args(["agent-render", "--request"])
        .arg(&request)
        .args(["--out-dir"])
        .arg(&output_dir)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stdout)
    );
    let evidence: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(evidence["status"], "committed");
    let rendered =
        ImageCodec::decode_rgba(output_dir.join("render.png"), Default::default()).unwrap();
    assert_eq!(rendered, expected);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_compare_returns_rejected_diagnostic_result_without_process_failure() {
    let root = temp_root("compare-rejected");
    fs::create_dir_all(&root).unwrap();
    let before = root.join("before.png");
    let after = root.join("after.png");
    let png = valid_png();
    fs::write(&before, &png).unwrap();
    fs::write(&after, &png).unwrap();
    let digest = sha256_hex(&png);
    let request = root.join("compare.json");
    let output_dir = root.join("compare-out");
    let artifact = |path: &std::path::Path| {
        serde_json::json!({
            "path": path,
            "expectedSha256": digest,
            "mediaType": "image/png",
            "byteLength": png.len(),
            "kind": "working_raster",
            "pixelSpec": {"format":"rgba8","colorSpace":"srgb","alphaMode":"straight"}
        })
    };
    let body = serde_json::json!({
        "schema": "perfectpixel.agent-image/compare/2",
        "requestId": "compare-rejected",
        "before": artifact(&before),
        "after": artifact(&after),
        "assertions": [{
            "type": "changed_ratio",
            "id": "must-change",
            "severity": "required",
            "minimum": 1.0
        }],
        "preview": {"difference": false, "maskOverlay": false, "maximumEdge": 256}
    });
    fs::write(&request, serde_json::to_vec(&body).unwrap()).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_perfectpixel"))
        .args(["agent-compare", "--request"])
        .arg(&request)
        .args(["--out-dir"])
        .arg(&output_dir)
        .output()
        .expect("run agent-compare");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["schema"], "perfectpixel.agent-image/compare-result/2");
    assert_eq!(value["status"], "rejected");
    assert_eq!(value["allRequiredPassed"], false);
    assert_eq!(value["assertions"][0]["id"], "must-change");
    assert_eq!(value["assertions"][0]["passed"], false);
    assert_eq!(value["receipt"]["status"], "rejected");
    fs::write(output_dir.join("stale.txt"), b"stale").unwrap();
    let rerun = Command::new(env!("CARGO_BIN_EXE_perfectpixel"))
        .args(["agent-compare", "--request"])
        .arg(&request)
        .args(["--out-dir"])
        .arg(&output_dir)
        .output()
        .expect("rerun agent-compare");
    assert!(
        !rerun.status.success(),
        "non-empty output dir must fail closed"
    );
    assert!(output_dir.join("stale.txt").is_file());
    let _ = fs::remove_dir_all(&root);
}
