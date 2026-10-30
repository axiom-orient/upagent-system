use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

use perfectpixel::{ImageCodec, PngEncoder, Raster};

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "perfectpixel-edit-contract-{}-{stamp}-{counter}",
            std::process::id(),
        ));
        fs::create_dir_all(&path).expect("create temp directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn run(request: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_perfectpixel"))
        .args(["edit", "--request"])
        .arg(request)
        .output()
        .expect("run edit command")
}

#[test]
fn edit_pipeline_publishes_strict_evidence_and_expected_pixels() {
    let root = TempDir::new();
    let input = root.path().join("source.png");
    let output = root.path().join("edited.png");
    let request = root.path().join("request.json");
    let image = Raster::new(
        2,
        3,
        vec![
            1, 0, 0, 255, 2, 0, 0, 255, 3, 0, 0, 255, 4, 0, 0, 255, 5, 0, 0, 255, 6, 0, 0, 255,
        ],
    )
    .expect("raster");
    fs::write(
        &input,
        PngEncoder::encode_rgba(&image).expect("encode input"),
    )
    .expect("input");
    let decoded = ImageCodec::decode_rgba(&input, Default::default()).expect("decode input");
    assert_eq!((decoded.width(), decoded.height()), (2, 3));
    fs::write(
        &request,
        serde_json::json!({
            "schemaVersion": 1,
            "operation": "edit",
            "input": "source.png",
            "output": "edited.png",
            "steps": [
                {"op": "crop", "x": 0, "y": 1, "width": 2, "height": 2},
                {"op": "rotate", "quarterTurns": 1},
                {"op": "flip", "axis": "horizontal"}
            ]
        })
        .to_string(),
    )
    .expect("request");

    let result = run(&request);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stdout)
    );
    let evidence: serde_json::Value = serde_json::from_slice(&result.stdout).expect("evidence");
    assert_eq!(evidence["schema"], "perfectpixel.image-edit/2");
    assert_eq!(
        evidence["steps"],
        serde_json::json!(["crop", "rotate", "flip"])
    );
    assert_eq!(evidence["outputWidth"], 2);
    assert_eq!(evidence["outputHeight"], 2);
    let edited = ImageCodec::decode_rgba(&output, Default::default()).expect("edited PNG");
    assert_eq!(edited.pixels()[0], 3);
    assert_eq!(edited.pixels()[4], 5);
}

#[test]
fn edit_pipeline_rejects_unknown_fields_without_publishing() {
    let root = TempDir::new();
    let input = root.path().join("source.png");
    let output = root.path().join("edited.png");
    let request = root.path().join("request.json");
    let image = Raster::blank(1, 1).expect("raster");
    fs::write(
        &input,
        PngEncoder::encode_rgba(&image).expect("encode input"),
    )
    .expect("input");
    fs::write(
        &request,
        serde_json::json!({
            "schemaVersion": 1,
            "operation": "edit",
            "input": "source.png",
            "output": "edited.png",
            "steps": [{"op": "flip", "axis": "horizontal", "unexpected": true}]
        })
        .to_string(),
    )
    .expect("request");

    let result = run(&request);
    assert_eq!(result.status.code(), Some(2));
    assert!(!output.exists());
    let error: serde_json::Value = serde_json::from_slice(&result.stdout).expect("error");
    assert_eq!(error["ok"], false);
}

#[test]
fn remove_background_rejects_missing_required_fields_without_publishing() {
    let root = TempDir::new();
    let input = root.path().join("source.png");
    let output = root.path().join("edited.png");
    let request = root.path().join("request.json");
    let image = Raster::blank(1, 1).expect("raster");
    fs::write(
        &input,
        PngEncoder::encode_rgba(&image).expect("encode input"),
    )
    .expect("input");
    fs::write(
        &request,
        serde_json::json!({
            "schemaVersion": 1,
            "operation": "edit",
            "input": "source.png",
            "output": "edited.png",
            "steps": [{"op": "remove_background", "keys": [[0, 0, 0]], "tolerance": 0}]
        })
        .to_string(),
    )
    .expect("request");

    let result = run(&request);
    assert_eq!(result.status.code(), Some(2));
    assert!(!output.exists());
}

#[test]
fn invalid_geometry_preserves_existing_destination_byte_for_byte() {
    let root = TempDir::new();
    let input = root.path().join("source.png");
    let output = root.path().join("edited.png");
    let request = root.path().join("request.json");
    let image = Raster::blank(2, 2).expect("raster");
    fs::write(
        &input,
        PngEncoder::encode_rgba(&image).expect("encode input"),
    )
    .expect("input");
    let destination = b"pre-existing destination bytes\n";
    fs::write(&output, destination).expect("destination");
    fs::write(
        &request,
        serde_json::json!({
            "schemaVersion": 1,
            "operation": "edit",
            "input": "source.png",
            "output": "edited.png",
            "steps": [{"op": "crop", "x": 1, "y": 1, "width": 2, "height": 2}]
        })
        .to_string(),
    )
    .expect("request");

    let result = run(&request);
    assert_eq!(result.status.code(), Some(2));
    assert_eq!(fs::read(&output).expect("destination remains"), destination);
}

#[test]
fn schema_advertises_versioned_edit_contract() {
    let output = Command::new(env!("CARGO_BIN_EXE_perfectpixel"))
        .arg("schema")
        .output()
        .expect("run schema command");
    assert!(output.status.success());
    let schema: serde_json::Value = serde_json::from_slice(&output.stdout).expect("schema JSON");
    assert_eq!(
        schema["editCommand"]["requestSchema"],
        "perfectpixel.image-edit/2"
    );
    assert_eq!(
        schema["editCommand"]["operations"],
        serde_json::json!([
            "crop",
            "rotate",
            "flip",
            "resize",
            "remove_background",
            "remove_background_auto"
        ])
    );
    assert_eq!(schema["inspectSchema"], "perfectpixel.asset-inspection/1");
    assert_eq!(schema["editCommand"]["semanticEditing"], false);
}

#[test]
fn remove_background_clears_checkerboard_edges_and_preserves_white_subject() {
    let root = TempDir::new();
    let input = root.path().join("checkerboard.png");
    let output = root.path().join("transparent.png");
    let request = root.path().join("request.json");
    let mut pixels = Vec::new();
    for y in 0..5 {
        for x in 0..5 {
            let rgb = if (1..=3).contains(&x) && (1..=3).contains(&y) {
                [255, 255, 255]
            } else if (x + y) % 2 == 0 {
                [220, 10, 20]
            } else {
                [10, 30, 220]
            };
            pixels.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
        }
    }
    let image = Raster::new(5, 5, pixels).expect("checkerboard raster");
    fs::write(
        &input,
        PngEncoder::encode_rgba(&image).expect("encode checkerboard"),
    )
    .expect("checkerboard input");
    fs::write(
        &request,
        serde_json::json!({
            "schemaVersion": 1,
            "operation": "edit",
            "input": "checkerboard.png",
            "output": "transparent.png",
            "steps": [{
                "op": "remove_background",
                "keys": [[220, 10, 20], [10, 30, 220]],
                "tolerance": 0,
                "feather": 0
            }]
        })
        .to_string(),
    )
    .expect("request");

    let result = run(&request);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stdout)
    );
    let evidence: serde_json::Value = serde_json::from_slice(&result.stdout).expect("evidence");
    assert_eq!(evidence["steps"], serde_json::json!(["remove_background"]));
    let edited = ImageCodec::decode_rgba(&output, Default::default()).expect("edited PNG");
    assert_eq!(edited.pixels()[3], 0);
    let center = ((2 * edited.width() + 2) * 4) as usize;
    assert_eq!(&edited.pixels()[center..center + 4], &[255, 255, 255, 255]);
}

#[test]
fn automatic_background_removal_publishes_plan_evidence_and_true_alpha() {
    let root = TempDir::new();
    let input = root.path().join("checkerboard.png");
    let output = root.path().join("transparent.png");
    let request = root.path().join("request.json");
    let mut pixels = Vec::new();
    for y in 0..5 {
        for x in 0..5 {
            let rgb = if (1..=3).contains(&x) && (1..=3).contains(&y) {
                [255, 255, 255]
            } else if (x + y) % 2 == 0 {
                [220, 10, 20]
            } else {
                [10, 30, 220]
            };
            pixels.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
        }
    }
    let image = Raster::new(5, 5, pixels).expect("checkerboard raster");
    fs::write(
        &input,
        PngEncoder::encode_rgba(&image).expect("encode checkerboard"),
    )
    .expect("checkerboard input");
    fs::write(
        &request,
        serde_json::json!({
            "schemaVersion": 1,
            "operation": "edit",
            "input": "checkerboard.png",
            "output": "transparent.png",
            "steps": [{
                "op": "remove_background_auto",
                "maxKeys": 2,
                "minEdgeCoverageBasisPoints": 10000,
                "tolerance": 0,
                "feather": 0
            }]
        })
        .to_string(),
    )
    .expect("request");

    let result = run(&request);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stdout)
    );
    let evidence: serde_json::Value = serde_json::from_slice(&result.stdout).expect("evidence");
    assert_eq!(
        evidence["steps"],
        serde_json::json!(["remove_background_auto"])
    );
    assert_eq!(
        evidence["autoBackground"][0]["edgeCoverageBasisPoints"],
        10000
    );
    assert_eq!(
        evidence["autoBackground"][0]["selectedKeys"],
        serde_json::json!([[10, 30, 220], [220, 10, 20]])
    );
    let edited = ImageCodec::decode_rgba(&output, Default::default()).expect("edited PNG");
    assert_eq!(edited.pixels()[3], 0);
    let center = ((2 * edited.width() + 2) * 4) as usize;
    assert_eq!(&edited.pixels()[center..center + 4], &[255, 255, 255, 255]);
}

#[test]
fn automatic_background_removal_rejects_heterogeneous_edges_without_publishing() {
    let root = TempDir::new();
    let input = root.path().join("photo-like.png");
    let output = root.path().join("transparent.png");
    let request = root.path().join("request.json");
    let mut pixels = vec![255_u8; 17 * 17 * 4];
    let mut next = 0_u8;
    for y in 0..17_u32 {
        for x in 0..17_u32 {
            if x == 0 || y == 0 || x == 16 || y == 16 {
                let index = ((y * 17 + x) * 4) as usize;
                pixels[index..index + 4].copy_from_slice(&[next, next / 2, 255 - next, 255]);
                next = next.wrapping_add(1);
            }
        }
    }
    let image = Raster::new(17, 17, pixels).expect("heterogeneous raster");
    fs::write(
        &input,
        PngEncoder::encode_rgba(&image).expect("encode raster"),
    )
    .expect("input");
    let original = b"existing destination";
    fs::write(&output, original).expect("destination");
    fs::write(
        &request,
        serde_json::json!({
            "schemaVersion": 1,
            "operation": "edit",
            "input": "photo-like.png",
            "output": "transparent.png",
            "steps": [{
                "op": "remove_background_auto",
                "maxKeys": 16,
                "minEdgeCoverageBasisPoints": 9000,
                "tolerance": 0,
                "feather": 0
            }]
        })
        .to_string(),
    )
    .expect("request");

    let result = run(&request);
    assert_eq!(result.status.code(), Some(2));
    let error: serde_json::Value = serde_json::from_slice(&result.stdout).expect("error");
    assert_eq!(error["ok"], false);
    assert!(error["message"]
        .as_str()
        .is_some_and(|message| message.contains("edge coverage")));
    assert_eq!(fs::read(&output).expect("destination remains"), original);
}

#[test]
fn inspect_response_is_versioned_content_addressed_and_agent_readable() {
    let root = TempDir::new();
    let input = root.path().join("source.png");
    let image = Raster::new(2, 2, [7, 8, 9, 255].repeat(4)).expect("raster");
    let encoded = PngEncoder::encode_rgba(&image).expect("encode input");
    fs::write(&input, &encoded).expect("input");
    let result = Command::new(env!("CARGO_BIN_EXE_perfectpixel"))
        .arg("inspect")
        .arg(&input)
        .output()
        .expect("inspect");
    assert!(result.status.success());
    let evidence: serde_json::Value = serde_json::from_slice(&result.stdout).expect("inspection");
    assert_eq!(evidence["schema"], "perfectpixel.asset-inspection/1");
    assert_eq!(evidence["schemaVersion"], 1);
    assert_eq!(evidence["inputSha256"], perfectpixel::sha256_hex(&encoded));
    assert_eq!(evidence["hasAlpha"], false);
    assert_eq!(evidence["pixelSpec"]["pixelFormat"], "rgba8");
    assert_eq!(evidence["pixelSpec"]["alpha"], "straight");
    assert_eq!(evidence["pixelSpec"]["color"]["kind"], "unknown");
    assert!(evidence.get("colorSpace").is_none());
    assert_eq!(evidence["edgePixelCount"], 4);
    assert_eq!(
        evidence["edgePalette"][0]["rgb"],
        serde_json::json!([7, 8, 9])
    );
    assert_eq!(evidence["edgePalette"][0]["count"], 4);
}
