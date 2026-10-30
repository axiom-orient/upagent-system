use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    time::{SystemTime, UNIX_EPOCH},
};

use perfectpixel::{PngEncoder, Raster};

use std::os::unix::fs::symlink;

#[test]
fn bundle_rejects_a_symlinked_out_dir_without_writing_through_it() {
    let root = temp_case("bundle-symlink-out-dir");
    write_frames(&root);
    let real_out = root.join("real-out");
    let linked_out = root.join("linked-out");
    fs::create_dir(&real_out).unwrap();
    symlink(&real_out, &linked_out).unwrap();
    let request = root.join("request.json");
    write_request(
        &request,
        r#"{
          "character":"hero",
          "sheetImage":"sprite-sheet.png",
          "cellWidth":6,
          "cellHeight":6,
          "states":[{"name":"idle","fps":8,"loop":true,"frames":["frames/idle/frame-00.png"]}]
        }"#,
    );

    let output = run_bundle(&request, &linked_out);

    assert_eq!(output.status.code(), Some(2));
    assert!(fs::read_dir(&real_out).unwrap().next().is_none());
}

#[test]
fn normalize_rejects_a_symlinked_out_dir_without_writing_through_it() {
    let root = temp_case("normalize-symlink-out-dir");
    let input = root.join("input.png");
    write_png(&input, [255, 0, 0, 255]);
    let real_out = root.join("real-out");
    let linked_out = root.join("linked-out");
    fs::create_dir(&real_out).unwrap();
    symlink(&real_out, &linked_out).unwrap();
    let request = root.join("request.json");
    write_request(
        &request,
        r#"{
          "character":"hero",
          "sheetImage":"sprite-sheet.png",
          "cellWidth":6,
          "cellHeight":6,
          "states":[{"name":"idle","fps":8,"loop":true,"frames":["input.png"]}]
        }"#,
    );

    let output = Command::new(env!("CARGO_BIN_EXE_perfectpixel"))
        .arg("normalize")
        .arg("--request")
        .arg(&request)
        .arg("--out-dir")
        .arg(&linked_out)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert!(fs::read_dir(&real_out).unwrap().next().is_none());
}

#[test]
fn bundle_prunes_previous_managed_outputs_without_deleting_unrelated_files() {
    let root = temp_case("bundle-prunes-stale");
    write_frames(&root);
    let out_dir = root.join("out");
    fs::create_dir_all(&out_dir).unwrap();
    fs::write(out_dir.join("notes.txt"), "keep me").unwrap();
    write_request(
        &root.join("request-tight.json"),
        r#"{
          "character":"hero",
          "sheetImage":"sprite-sheet.png",
          "cellWidth":6,
          "cellHeight":6,
          "packing":{"maxWidth":8,"maxHeight":8,"padding":1,"trim":false,"allowRotation":false,"multipack":true},
          "states":[{"name":"idle","fps":8,"loop":true,"frames":["frames/idle/frame-00.png","frames/idle/frame-01.png","frames/idle/frame-02.png"]}]
        }"#,
    );
    write_request(
        &root.join("request-small.json"),
        r#"{
          "character":"hero",
          "sheetImage":"sprite-sheet.png",
          "cellWidth":6,
          "cellHeight":6,
          "states":[{"name":"idle","fps":8,"loop":true,"frames":["frames/idle/frame-00.png"]}]
        }"#,
    );

    assert_success(run_bundle(&root.join("request-tight.json"), &out_dir));
    assert!(out_dir.join("sprite-sheet-00.png").exists());
    assert_success(run_bundle(&root.join("request-small.json"), &out_dir));

    let files = relative_files(&out_dir);
    assert!(files.contains(&"notes.txt".to_string()));
    assert!(files.contains(&"manifest.json".to_string()));
    assert!(files.contains(&"sprite-sheet.png".to_string()));
    assert!(files.contains(&"sprite-sheet.json".to_string()));
    assert!(files.contains(&"frames/idle/frame-00.png".to_string()));
    assert!(!files.contains(&"sprite-sheet-00.png".to_string()));
    assert!(!files.contains(&"sprite-sheet-01.png".to_string()));
    assert!(!files.contains(&"sprite-sheet-02.png".to_string()));
    assert!(!files.contains(&"frames/idle/frame-01.png".to_string()));
    assert!(!files.contains(&"frames/idle/frame-02.png".to_string()));
}

#[test]
fn bundle_rejects_blocked_output_parent_before_writing_partial_outputs() {
    let root = temp_case("bundle-blocked-parent");
    write_frames(&root);
    let out_dir = root.join("out");
    fs::create_dir_all(&out_dir).unwrap();
    fs::write(out_dir.join("frames"), "not a directory").unwrap();
    write_request(
        &root.join("request-small.json"),
        r#"{
          "character":"hero",
          "sheetImage":"sprite-sheet.png",
          "cellWidth":6,
          "cellHeight":6,
          "states":[{"name":"idle","fps":8,"loop":true,"frames":["frames/idle/frame-00.png"]}]
        }"#,
    );

    let output = run_bundle(&root.join("request-small.json"), &out_dir);
    assert_eq!(output.status.code(), Some(2));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\"ok\":false"));
    assert!(!out_dir.join("manifest.json").exists());
    assert!(!out_dir.join("sprite-sheet.png").exists());
    assert!(!out_dir.join("sprite-sheet.json").exists());
    assert_eq!(
        fs::read_to_string(out_dir.join("frames")).unwrap(),
        "not a directory"
    );
}

#[test]
fn bundle_rejects_explicit_zero_fps_without_writing_outputs() {
    let root = temp_case("bundle-zero-fps");
    write_frames(&root);
    let out_dir = root.join("out");
    write_request(
        &root.join("request-zero-fps.json"),
        r#"{
          "character":"hero",
          "sheetImage":"sprite-sheet.png",
          "cellWidth":6,
          "cellHeight":6,
          "states":[{"name":"idle","fps":0,"loop":true,"frames":["frames/idle/frame-00.png"]}]
        }"#,
    );

    let output = run_bundle(&root.join("request-zero-fps.json"), &out_dir);

    assert_eq!(output.status.code(), Some(2));
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(payload["ok"], false);
    assert_eq!(
        payload["message"],
        "state 'idle' fps must be from 1 through 1000"
    );
    assert!(!out_dir.exists());
}

#[test]
fn bundle_requires_fps_and_loop_without_writing_outputs() {
    let root = temp_case("bundle-required-state-fields");
    write_frames(&root);
    for (field, state) in [
        (
            "fps",
            r#"{"name":"idle","loop":true,"frames":["frames/idle/frame-00.png"]}"#,
        ),
        (
            "loop",
            r#"{"name":"idle","fps":8,"frames":["frames/idle/frame-00.png"]}"#,
        ),
    ] {
        let request = root.join(format!("request-missing-{field}.json"));
        let out_dir = root.join(format!("out-missing-{field}"));
        write_request(
            &request,
            &format!(
                r#"{{
                  "character":"hero",
                  "sheetImage":"sprite-sheet.png",
                  "cellWidth":6,
                  "cellHeight":6,
                  "states":[{state}]
                }}"#
            ),
        );

        let output = run_bundle(&request, &out_dir);

        assert_eq!(output.status.code(), Some(2));
        let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(payload["ok"], false);
        assert!(payload["message"]
            .as_str()
            .is_some_and(|message| message.contains(&format!("missing field `{field}`"))));
        assert!(!out_dir.exists());
    }
}

#[test]
fn normalize_gate_failure_replaces_previous_success_generation() {
    let root = temp_case("normalize-replaces-stale-success");
    let good = root.join("good.png");
    let sparse = root.join("sparse.png");
    write_png(&good, [255, 0, 0, 255]);
    write_sparse_png(&sparse);
    let success_request = root.join("normalize-success.json");
    let failing_request = root.join("normalize-failing.json");
    let out_dir = root.join("out");
    write_request(
        &success_request,
        r#"{
          "character":"hero",
          "sheetImage":"sprite-sheet.png",
          "cellWidth":6,
          "cellHeight":6,
          "states":[{"name":"idle","fps":8,"loop":true,"frames":["good.png"]}]
        }"#,
    );
    write_request(
        &failing_request,
        r#"{
          "character":"hero",
          "sheetImage":"sprite-sheet.png",
          "cellWidth":6,
          "cellHeight":6,
          "chroma":{"rgb":[255,255,255]},
          "states":[{"name":"idle","fps":8,"loop":true,"frames":["sparse.png"]}]
        }"#,
    );

    assert_success(run_normalize(&success_request, &out_dir));
    assert!(out_dir.join("sprite-request.json").is_file());
    assert!(out_dir.join("frames/idle/frame-00.png").is_file());

    let output = run_normalize(&failing_request, &out_dir);

    assert_eq!(output.status.code(), Some(4));
    assert!(out_dir.join("normalize-report.json").is_file());
    assert!(out_dir.join(".perfectpixel-generation.json").is_file());
    assert!(!out_dir.join("sprite-request.json").exists());
    assert!(!out_dir.join("frames/idle/frame-00.png").exists());
    let authority = fs::read_to_string(out_dir.join(".perfectpixel-generation.json")).unwrap();
    assert!(authority.contains("normalize-report.json"));
    assert!(!authority.contains("sprite-request.json"));
}

#[test]
fn normalize_preparation_failure_aborts_without_partial_outputs() {
    let root = temp_case("normalize-preparation-abort");
    let failing_strip = root.join("failing-strip.png");
    let valid_frame = root.join("valid.png");
    write_png(&failing_strip, [255, 0, 0, 255]);
    write_png(&valid_frame, [0, 255, 0, 255]);
    let request = root.join("normalize-request.json");
    let out_dir = root.join("out");
    write_request(
        &request,
        r#"{
          "character":"hero",
          "sheetImage":"sprite-sheet.png",
          "cellWidth":6,
          "cellHeight":6,
          "states":[
            {"name":"failing-state","fps":8,"loop":true,"strip":"failing-strip.png","frameCount":2},
            {"name":"valid-state","fps":8,"loop":true,"frames":["valid.png"]}
          ]
        }"#,
    );

    let output = run_normalize(&request, &out_dir);

    assert_eq!(output.status.code(), Some(2));
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(payload["ok"], false);
    assert!(payload["message"]
        .as_str()
        .is_some_and(|message| message.contains("failing-state")));
    assert!(!out_dir.join("sprite-request.json").exists());
    assert!(!out_dir.join("frames").exists());
    assert!(!out_dir.join("frames/valid-state/frame-00.png").exists());
    assert!(!out_dir.exists());
}

#[test]
fn bundle_rejects_stale_output_directory_before_replacing_current_outputs() {
    let root = temp_case("bundle-stale-dir-blocker");
    write_frames(&root);
    let out_dir = root.join("out");
    fs::create_dir_all(&out_dir).unwrap();
    fs::write(out_dir.join("notes.txt"), "keep me").unwrap();
    write_request(
        &root.join("request-tight.json"),
        r#"{
          "character":"hero",
          "sheetImage":"sprite-sheet.png",
          "cellWidth":6,
          "cellHeight":6,
          "packing":{"maxWidth":8,"maxHeight":8,"padding":1,"trim":false,"allowRotation":false,"multipack":true},
          "states":[{"name":"idle","fps":8,"loop":true,"frames":["frames/idle/frame-00.png","frames/idle/frame-01.png","frames/idle/frame-02.png"]}]
        }"#,
    );
    write_request(
        &root.join("request-small.json"),
        r#"{
          "character":"hero",
          "sheetImage":"sprite-sheet.png",
          "cellWidth":6,
          "cellHeight":6,
          "states":[{"name":"idle","fps":8,"loop":true,"frames":["frames/idle/frame-00.png"]}]
        }"#,
    );

    assert_success(run_bundle(&root.join("request-tight.json"), &out_dir));
    let previous_manifest = fs::read_to_string(out_dir.join("manifest.json")).unwrap();
    fs::remove_file(out_dir.join("frames/idle/frame-01.png")).unwrap();
    fs::create_dir(out_dir.join("frames/idle/frame-01.png")).unwrap();
    fs::write(
        out_dir.join("frames/idle/frame-01.png/note.txt"),
        "user data",
    )
    .unwrap();

    let output = run_bundle(&root.join("request-small.json"), &out_dir);
    assert_eq!(output.status.code(), Some(2));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("managed generated artifact must be a regular non-symlink file"));
    assert_eq!(
        fs::read_to_string(out_dir.join("manifest.json")).unwrap(),
        previous_manifest
    );
    assert!(!out_dir.join("sprite-sheet.png").exists());
    assert!(out_dir.join("sprite-sheet-00.png").exists());
    assert_eq!(
        fs::read_to_string(out_dir.join("frames/idle/frame-01.png/note.txt")).unwrap(),
        "user data"
    );
}

#[test]
fn vector_rejection_preserves_existing_svg_output() {
    let root = temp_case("vector-continuous-tone");
    let input = root.join("continuous.png");
    let mut pixels = Vec::with_capacity(64 * 64 * 4);
    for y in 0..64u8 {
        for x in 0..64u8 {
            pixels.extend_from_slice(&[x.wrapping_mul(3), y.wrapping_mul(5), x ^ y, 255]);
        }
    }
    let raster = Raster::new(64, 64, pixels).unwrap();
    fs::write(&input, PngEncoder::encode_rgba(&raster).unwrap()).unwrap();

    let output = root.join("output.svg");
    fs::write(&output, "existing SVG must survive").unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_perfectpixel"))
        .args([
            "vector",
            input.to_str().unwrap(),
            "--out",
            output.to_str().unwrap(),
            "--preset",
            "line-art",
            "--profile",
            "compact",
            "--detail",
            "5",
        ])
        .output()
        .unwrap();

    assert_eq!(result.status.code(), Some(4));
    let payload: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(payload["schema"], "perfectpixel.vector-result/1");
    assert_eq!(payload["decision"], "notApplicable");
    assert_eq!(
        fs::read_to_string(&output).unwrap(),
        "existing SVG must survive"
    );
}
#[test]
fn vector_analysis_report_commit_failure_returns_transaction_truth() {
    let root = temp_case("vector-analysis-report-failure");
    let input = root.join("input.png");
    write_png(&input, [255, 0, 0, 255]);
    let blocker = root.join("not-a-directory");
    fs::write(&blocker, "block report parent").unwrap();

    let result = Command::new(env!("CARGO_BIN_EXE_perfectpixel"))
        .args([
            "vector-analyze",
            input.to_str().unwrap(),
            "--report",
            blocker.join("analysis.json").to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert_eq!(result.status.code(), Some(2));
    let payload: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["phase"], "vectorAnalyze");
    assert_eq!(payload["path"].as_str(), blocker.to_str());
    assert!(payload["originalError"]
        .as_str()
        .is_some_and(|message| !message.is_empty()));
    assert!(payload.get("transaction").is_none());
    assert!(!blocker.join("analysis.json").exists());
}
fn run_bundle(request: &Path, out_dir: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_perfectpixel"))
        .arg("bundle")
        .arg("--request")
        .arg(request)
        .arg("--out-dir")
        .arg(out_dir)
        .output()
        .unwrap()
}

fn run_normalize(request: &Path, out_dir: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_perfectpixel"))
        .arg("normalize")
        .arg("--request")
        .arg(request)
        .arg("--out-dir")
        .arg(out_dir)
        .output()
        .unwrap()
}

fn assert_success(output: Output) {
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn temp_case(name: &str) -> PathBuf {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let root = std::env::temp_dir().join(format!(
        "perfectpixel-{name}-{}-{millis}",
        std::process::id()
    ));
    if root.exists() {
        fs::remove_dir_all(&root).unwrap();
    }
    fs::create_dir_all(&root).unwrap();
    root
}

fn write_frames(root: &Path) {
    let frame_dir = root.join("frames/idle");
    fs::create_dir_all(&frame_dir).unwrap();
    write_png(&frame_dir.join("frame-00.png"), [255, 0, 0, 255]);
    write_png(&frame_dir.join("frame-01.png"), [0, 255, 0, 255]);
    write_png(&frame_dir.join("frame-02.png"), [0, 0, 255, 255]);
}

fn write_png(path: &Path, rgba: [u8; 4]) {
    let mut pixels = Vec::new();
    for _ in 0..36 {
        pixels.extend_from_slice(&rgba);
    }
    let raster = Raster::new(6, 6, pixels).unwrap();
    let png = PngEncoder::encode_rgba(&raster).unwrap();
    fs::write(path, png).unwrap();
}

fn write_sparse_png(path: &Path) {
    let mut pixels = vec![0; 6 * 6 * 4];
    pixels[..4].copy_from_slice(&[255, 255, 255, 255]);
    let raster = Raster::new(6, 6, pixels).unwrap();
    let png = PngEncoder::encode_rgba(&raster).unwrap();
    fs::write(path, png).unwrap();
}

fn write_request(path: &Path, json: &str) {
    fs::write(path, json).unwrap();
}

fn relative_files(root: &Path) -> Vec<String> {
    let mut files = Vec::new();
    collect_relative_files(root, root, &mut files);
    files.sort();
    files
}

fn collect_relative_files(root: &Path, dir: &Path, files: &mut Vec<String>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect_relative_files(root, &path, files);
        } else {
            files.push(
                path.strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
        }
    }
}
