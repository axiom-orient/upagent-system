use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use perfectpixel::{MotionCompiler, PngEncoder, Raster};

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "perfectpixel-cli-{label}-{}-{stamp}",
            std::process::id()
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

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_perfectpixel"))
        .args(args)
        .output()
        .expect("run perfectpixel")
}

fn assert_success(output: &Output) -> serde_json::Value {
    assert!(
        output.status.success(),
        "status={:?}, stdout={}, stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("JSON success output")
}

#[test]
fn cli_schema_and_help_define_the_deterministic_asset_compiler_boundary() {
    let schema = assert_success(&run(&["schema"]));
    assert_eq!(schema["role"], "deterministic-asset-compiler");
    assert_eq!(schema["modelInference"], false);
    assert_eq!(schema["networkRequired"], false);
    assert_eq!(schema["publicationPolicy"], "evaluate-before-publish");
    assert_eq!(
        schema["assetAdapter"]["inspectionSchema"],
        "perfectpixel.asset-inspection/1"
    );
    assert_eq!(
        schema["assetAdapter"]["transformSchema"],
        "perfectpixel.asset-transform/1"
    );
    assert_eq!(
        schema["assetAdapter"]["digestEncoding"],
        "sha256-lowercase-hex"
    );

    let help = String::from_utf8(run(&["--help"]).stdout).expect("UTF-8 help output");
    assert!(help.contains("deterministic asset compiler"));
    assert!(help.contains("AI-generated or authored local assets"));
    assert!(help.contains("does not generate images or run AI models"));
    assert!(help.contains("No network is required"));
    assert!(help.contains("evaluated before atomic publication"));
}

#[test]
fn cli_runs_schema_inspect_normalize_bundle_vector_and_invalid_request_paths() {
    let root = TempDir::new("workflow");
    let input = root.path().join("input.png");
    let mut pixels = Vec::with_capacity(16 * 16 * 4);
    for _ in 0..16 * 16 {
        pixels.extend_from_slice(&[220, 220, 220, 255]);
    }
    let raster = Raster::new(16, 16, pixels).expect("valid raster");
    fs::write(
        &input,
        PngEncoder::encode_rgba(&raster).expect("encode PNG"),
    )
    .expect("write input PNG");

    let schema = assert_success(&run(&["schema"]));
    assert_eq!(schema["bundleSchema"], "perfectpixel.sprite/3");
    assert_eq!(schema["motionSchema"], "perfectpixel.motion/1");

    let input_arg = input.to_string_lossy();
    let inspect = assert_success(&run(&["inspect", &input_arg]));
    assert_eq!(inspect["width"], 16);
    assert_eq!(inspect["height"], 16);

    let request_path = root.path().join("normalize-request.json");
    let request = serde_json::json!({
        "character": "hero",
        "sheetImage": "sprite-sheet.png",
        "cellWidth": 16,
        "cellHeight": 16,
        "states": [{
            "name": "idle",
            "fps": 8,
            "loop": true,
            "frames": ["input.png"]
        }]
    });
    fs::write(
        &request_path,
        serde_json::to_vec_pretty(&request).expect("serialize normalize request"),
    )
    .expect("write normalize request");
    let normalized = root.path().join("normalized");
    let request_arg = request_path.to_string_lossy();
    let normalized_arg = normalized.to_string_lossy();
    let normalize = assert_success(&run(&[
        "normalize",
        "--request",
        &request_arg,
        "--out-dir",
        &normalized_arg,
    ]));
    assert_eq!(normalize["frames"], 1);
    assert!(normalized.join("normalize-report.json").is_file());
    assert!(normalized.join("sprite-request.json").is_file());
    assert!(normalized.join("frames/idle/frame-00.png").is_file());
    assert!(normalized.join(".perfectpixel-generation.json").is_file());

    let bundle_request = normalized.join("sprite-request.json");
    let bundle_dir = root.path().join("bundle");
    let bundle_request_arg = bundle_request.to_string_lossy();
    let bundle_dir_arg = bundle_dir.to_string_lossy();
    let bundle = assert_success(&run(&[
        "bundle",
        "--request",
        &bundle_request_arg,
        "--out-dir",
        &bundle_dir_arg,
    ]));
    assert_eq!(bundle["animations"], 1);
    assert!(bundle_dir.join("manifest.json").is_file());
    assert!(bundle_dir.join("sprite-sheet.png").is_file());
    assert!(bundle_dir.join("sprite-sheet.json").is_file());
    assert!(bundle_dir.join(".perfectpixel-generation.json").is_file());

    let svg = root.path().join("output.svg");
    let vector_report = root.path().join("vector-report.json");
    let svg_arg = svg.to_string_lossy();
    let vector_report_arg = vector_report.to_string_lossy();
    let vector = assert_success(&run(&[
        "vector",
        &input_arg,
        "--out",
        &svg_arg,
        "--preset",
        "flat-icon",
        "--profile",
        "compact",
        "--detail",
        "auto",
        "--report",
        &vector_report_arg,
    ]));
    assert_eq!(vector["decision"], "approved");
    assert!(svg.is_file());
    assert!(vector_report.is_file());
    assert!(!fs::read_to_string(&svg)
        .expect("read SVG")
        .contains("<image"));

    let sparse_input = root.path().join("sparse.png");
    let mut sparse_pixels = vec![0; 16 * 16 * 4];
    sparse_pixels[..4].copy_from_slice(&[255, 255, 255, 255]);
    let sparse_raster = Raster::new(16, 16, sparse_pixels).expect("valid sparse raster");
    fs::write(
        &sparse_input,
        PngEncoder::encode_rgba(&sparse_raster).expect("encode sparse PNG"),
    )
    .expect("write sparse PNG");
    let sparse_request_path = root.path().join("sparse-request.json");
    let mut sparse_request = request.clone();
    sparse_request["states"][0]["frames"] = serde_json::json!(["sparse.png"]);
    sparse_request["chroma"] = serde_json::json!({"rgb": [255, 255, 255]});
    fs::write(
        &sparse_request_path,
        serde_json::to_vec_pretty(&sparse_request).expect("serialize sparse request"),
    )
    .expect("write sparse request");
    let sparse_out = root.path().join("sparse-output");
    let sparse_request_arg = sparse_request_path.to_string_lossy();
    let sparse_out_arg = sparse_out.to_string_lossy();
    let sparse = run(&[
        "normalize",
        "--request",
        &sparse_request_arg,
        "--out-dir",
        &sparse_out_arg,
    ]);
    assert_eq!(sparse.status.code(), Some(4));
    assert!(sparse_out.join("normalize-report.json").is_file());
    assert!(sparse_out.join(".perfectpixel-generation.json").is_file());
    assert!(!sparse_out.join("sprite-request.json").exists());
    assert!(!sparse_out.join("frames").exists());

    let invalid_request_path = root.path().join("invalid-request.json");
    let mut invalid_request = request;
    invalid_request["fit"] = serde_json::json!({"alignY": "center"});
    fs::write(
        &invalid_request_path,
        serde_json::to_vec_pretty(&invalid_request).expect("serialize invalid request"),
    )
    .expect("write invalid request");
    let invalid_out = root.path().join("invalid-output");
    let invalid_request_arg = invalid_request_path.to_string_lossy();
    let invalid_out_arg = invalid_out.to_string_lossy();
    let invalid = run(&[
        "normalize",
        "--request",
        &invalid_request_arg,
        "--out-dir",
        &invalid_out_arg,
    ]);
    assert_eq!(invalid.status.code(), Some(2));
    let invalid_json: serde_json::Value =
        serde_json::from_slice(&invalid.stdout).expect("JSON error output");
    assert_eq!(invalid_json["ok"], false);
    assert!(invalid_json["message"]
        .as_str()
        .expect("error message")
        .contains("fit.alignY must be bottom"));
    assert!(!invalid_out.exists());

    let malformed_request = root.path().join("malformed-request.json");
    fs::write(&malformed_request, b"{").expect("write malformed request");
    let malformed_request_arg = malformed_request.to_string_lossy();
    let malformed = run(&[
        "bundle",
        "--request",
        &malformed_request_arg,
        "--out-dir",
        &invalid_out_arg,
    ]);
    assert_eq!(malformed.status.code(), Some(2));
    let malformed_json: serde_json::Value =
        serde_json::from_slice(&malformed.stdout).expect("JSON malformed-request error");
    assert_eq!(malformed_json["ok"], false);
    assert!(malformed_json["message"]
        .as_str()
        .expect("malformed request message")
        .contains("invalid JSON request"));
    assert_eq!(
        malformed_json["path"],
        malformed_request.to_string_lossy().as_ref()
    );
}

#[test]
fn cli_classifies_svg_contract_failures_as_invalid_requests() {
    let root = TempDir::new("invalid-svg");
    let input = root.path().join("invalid.svg");
    fs::write(&input, r#"<svg><path d="not-a-path"/></svg>"#).expect("write invalid SVG");
    let out = root.path().join("motion");

    let result = run(&[
        "motion-scaffold",
        input.to_str().unwrap(),
        "--out-dir",
        out.to_str().unwrap(),
    ]);

    assert_eq!(result.status.code(), Some(2));
    let payload: serde_json::Value =
        serde_json::from_slice(&result.stdout).expect("JSON SVG error output");
    assert_eq!(payload["ok"], false);
}

#[test]
fn image_too_large_payload_carries_the_input_path() {
    let root = TempDir::new("image-too-large");
    let input = root.path().join("wide.png");
    let raster = Raster::new(8193, 1, vec![255; 8193 * 4]).expect("valid wide raster");
    fs::write(
        &input,
        PngEncoder::encode_rgba(&raster).expect("encode PNG"),
    )
    .expect("write PNG");

    let result = run(&["inspect", input.to_str().unwrap()]);

    assert_eq!(result.status.code(), Some(3));
    let payload: serde_json::Value =
        serde_json::from_slice(&result.stdout).expect("JSON image limit output");
    assert_eq!(payload["path"], input.to_string_lossy().as_ref());
}

#[test]
fn cli_keeps_explicit_continuous_tone_vectorization_diagnostic_only() {
    let root = TempDir::new("continuous-tone-vector");
    let input = root.path().join("continuous.png");
    let mut pixels = Vec::with_capacity(64 * 64 * 4);
    for y in 0..64u8 {
        for x in 0..64u8 {
            pixels.extend_from_slice(&[x.wrapping_mul(3), y.wrapping_mul(5), x ^ y, 255]);
        }
    }
    let raster = Raster::new(64, 64, pixels).expect("valid continuous-tone raster");
    fs::write(
        &input,
        PngEncoder::encode_rgba(&raster).expect("encode PNG"),
    )
    .expect("write input PNG");

    let output = root.path().join("output.svg");
    let diagnostics = root.path().join("diagnostics");
    let report = root.path().join("report.json");
    let input_arg = input.to_string_lossy();
    let output_arg = output.to_string_lossy();
    let diagnostics_arg = diagnostics.to_string_lossy();
    let report_arg = report.to_string_lossy();
    let result = run(&[
        "vector",
        &input_arg,
        "--out",
        &output_arg,
        "--preset",
        "line-art",
        "--profile",
        "compact",
        "--detail",
        "5",
        "--report",
        &report_arg,
        "--diagnostics",
        &diagnostics_arg,
    ]);

    assert_eq!(result.status.code(), Some(4));
    let rejection: serde_json::Value =
        serde_json::from_slice(&result.stdout).expect("JSON domain rejection output");
    assert_eq!(rejection["schema"], "perfectpixel.vector-result/1");
    assert_eq!(rejection["ok"], false);
    assert_eq!(rejection["decision"], "notApplicable");
    assert_eq!(rejection["transaction"]["report"], "committed");
    assert_eq!(rejection["transaction"]["diagnostics"], "committed");
    assert_eq!(rejection["transaction"]["finalCommit"], "notAttempted");
    assert!(report.is_file());
    assert!(diagnostics.is_dir());
    assert!(!output.exists());
}
#[test]
fn cli_scaffolds_and_builds_svg_and_lottie_motion() {
    let root = TempDir::new("motion");
    let input = root.path().join("source.svg");
    fs::write(
        &input,
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="32" height="32" viewBox="0 0 32 32"><path fill="#ffffff" d="M0 0 L32 0 L32 32 L0 32 Z"/><g transform="translate(4,4)"><path fill="#ff0000" d="M0 0 C8 -2 16 -2 24 0 L24 24 L0 24 Z"/></g></svg>"##,
    )
    .expect("write motion SVG");
    let out = root.path().join("motion");
    let input_arg = input.to_string_lossy();
    let out_arg = out.to_string_lossy();
    let scaffold = assert_success(&run(&[
        "motion-scaffold",
        &input_arg,
        "--out-dir",
        &out_arg,
    ]));
    assert_eq!(scaffold["paths"], 2);
    assert!(out.join("scene.svg").is_file());
    assert!(out.join("layers.json").is_file());
    assert!(out.join("motion-request.json").is_file());
    assert!(out.join("layer-inspector.html").is_file());
    let layers: serde_json::Value =
        serde_json::from_slice(&fs::read(out.join("layers.json")).expect("read layers"))
            .expect("parse layers");
    assert!(layers["layers"][1]["bounds"].is_array());

    let scene = fs::read_to_string(out.join("scene.svg")).expect("read scaffold scene");
    assert!(!scene.contains("transform="));
    assert!(scene.contains("d=\"M4 4 C12 2 20 2 28 4"));
    let request = out.join("motion-request.json");
    let mut request_json: serde_json::Value =
        serde_json::from_slice(&fs::read(&request).expect("read starter motion request"))
            .expect("parse starter motion request");
    assert_eq!(request_json["name"], "source");
    assert_eq!(request_json["sourceSvg"], "scene.svg");
    assert_eq!(
        request_json["sourceSvgSha256"],
        MotionCompiler::scene_sha256(&scene)
    );
    request_json["durationMs"] = serde_json::json!(100);
    request_json["loop"] = serde_json::json!(false);
    request_json["parts"] = serde_json::json!([
        {"id": "part", "pathIds": ["pp-path-0002"], "anchor": [4.0, 4.0]}
    ]);
    request_json["tracks"] = serde_json::json!([
        {"part": "part", "keyframes": [
            {"atMs": 0, "translate": [0.0, 0.0], "rotateDeg": 90.0, "scale": [1.0, 1.0], "opacity": 1.0},
            {"atMs": 100, "translate": [0.0, 0.0], "rotateDeg": 90.0, "scale": [1.0, 1.0], "opacity": 1.0}
        ]}
    ]);
    fs::write(
        &request,
        serde_json::to_vec(&request_json).expect("serialize caller request"),
    )
    .expect("write caller request");
    let request_arg = request.to_string_lossy();
    let build = assert_success(&run(&[
        "motion-build",
        "--request",
        &request_arg,
        "--out-dir",
        &out_arg,
    ]));
    assert_eq!(build["paths"], 2);
    assert_eq!(build["dotlottieArchiveCreated"], false);
    let animated = fs::read_to_string(out.join("animated.svg")).expect("read animated SVG");
    assert!(animated.contains("<g class=\"pp-motion-part\">"));
    assert!(!animated.contains("transform=\"translate(4,4)\""));
    assert!(animated.contains("rotate(90deg)"));
    let lottie: serde_json::Value =
        serde_json::from_slice(&fs::read(out.join("animation.json")).expect("read Lottie"))
            .expect("parse Lottie");
    assert_eq!(lottie["layers"].as_array().map(Vec::len), Some(2));
    assert_eq!(
        lottie["layers"][0]["shapes"][0]["ks"]["k"]["v"][0],
        serde_json::json!([4.0, 4.0])
    );
    assert_eq!(
        lottie["layers"][0]["ks"]["a"]["k"],
        serde_json::json!([4.0, 4.0, 0.0])
    );
    assert_eq!(
        lottie["layers"][0]["ks"]["r"]["k"][0]["s"],
        serde_json::json!([90.0])
    );
    let manifest: serde_json::Value = serde_json::from_slice(
        &fs::read(out.join("dotlottie/manifest.json")).expect("read dotLottie manifest"),
    )
    .expect("parse dotLottie manifest");
    assert_eq!(manifest["version"], "2");
    assert!(out.join(".perfectpixel-generation.json").is_file());

    let second_scaffold = assert_success(&run(&[
        "motion-scaffold",
        &input_arg,
        "--out-dir",
        &out_arg,
    ]));
    assert_eq!(second_scaffold["paths"], 2);
    assert!(out.join("scene.svg").is_file());
    assert!(out.join("motion-request.json").is_file());
    assert!(!out.join("animated.svg").exists());
    assert!(!out.join("animation.json").exists());
    assert!(!out.join("dotlottie/a/source.json").exists());

    for (label, field, value, message) in [
        (
            "fps-zero",
            "fps",
            serde_json::json!(0),
            "motion fps must be from 1 through 120",
        ),
        (
            "fps-too-high",
            "fps",
            serde_json::json!(121),
            "motion fps must be from 1 through 120",
        ),
        (
            "duration-zero",
            "durationMs",
            serde_json::json!(0),
            "motion durationMs must be from 1 through 600000",
        ),
        (
            "duration-too-high",
            "durationMs",
            serde_json::json!(600_001),
            "motion durationMs must be from 1 through 600000",
        ),
    ] {
        let invalid_request = out.join(format!("{label}.json"));
        let invalid_out = root.path().join(label);
        let mut invalid_json = request_json.clone();
        invalid_json[field] = value;
        fs::write(
            &invalid_request,
            serde_json::to_vec(&invalid_json).expect("serialize invalid motion request"),
        )
        .expect("write invalid motion request");

        let invalid_request_arg = invalid_request.to_string_lossy();
        let invalid_out_arg = invalid_out.to_string_lossy();
        let rejected = run(&[
            "motion-build",
            "--request",
            &invalid_request_arg,
            "--out-dir",
            &invalid_out_arg,
        ]);

        assert_eq!(rejected.status.code(), Some(2), "{label}");
        let payload: serde_json::Value =
            serde_json::from_slice(&rejected.stdout).expect("parse motion error payload");
        assert_eq!(payload["ok"], false, "{label}");
        assert_eq!(payload["message"], message, "{label}");
        assert!(!invalid_out.exists(), "{label} must not create outputs");
    }
}

#[test]
fn motion_build_replaces_a_renamed_animation_generation() {
    let root = TempDir::new("motion-rename");
    let input = root.path().join("source.svg");
    fs::write(
        &input,
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16"><path fill="#ff0000" d="M0 0 L16 0 L16 16 L0 16 Z"/></svg>"##,
    )
    .expect("write motion SVG");
    let out = root.path().join("motion");
    let input_arg = input.to_string_lossy();
    let out_arg = out.to_string_lossy();
    assert_success(&run(&[
        "motion-scaffold",
        &input_arg,
        "--out-dir",
        &out_arg,
    ]));

    let request = out.join("motion-request.json");
    let mut request_json: serde_json::Value =
        serde_json::from_slice(&fs::read(&request).expect("read motion request"))
            .expect("parse motion request");
    request_json["name"] = serde_json::json!("old-name");
    request_json["durationMs"] = serde_json::json!(100);
    request_json["parts"] = serde_json::json!([
        {"id": "part", "pathIds": ["pp-path-0001"], "anchor": [8.0, 8.0]}
    ]);
    request_json["tracks"] = serde_json::json!([
        {"part": "part", "keyframes": [
            {"atMs": 0, "translate": [0.0, 0.0], "rotateDeg": 0.0, "scale": [1.0, 1.0], "opacity": 1.0},
            {"atMs": 100, "translate": [0.0, 0.0], "rotateDeg": 90.0, "scale": [1.0, 1.0], "opacity": 1.0}
        ]}
    ]);
    fs::write(
        &request,
        serde_json::to_vec_pretty(&request_json).expect("serialize old request"),
    )
    .expect("write old request");
    let request_arg = request.to_string_lossy();
    assert_success(&run(&[
        "motion-build",
        "--request",
        &request_arg,
        "--out-dir",
        &out_arg,
    ]));
    assert!(out.join("dotlottie/a/old-name.json").is_file());

    request_json["name"] = serde_json::json!("new-name");
    fs::write(
        &request,
        serde_json::to_vec_pretty(&request_json).expect("serialize new request"),
    )
    .expect("write new request");
    assert_success(&run(&[
        "motion-build",
        "--request",
        &request_arg,
        "--out-dir",
        &out_arg,
    ]));

    assert!(out.join("dotlottie/a/new-name.json").is_file());
    assert!(!out.join("dotlottie/a/old-name.json").exists());
    let authority = fs::read_to_string(out.join(".perfectpixel-generation.json"))
        .expect("read generation authority");
    assert!(authority.contains("dotlottie/a/new-name.json"));
    assert!(!authority.contains("dotlottie/a/old-name.json"));
}

#[test]
fn corrupt_generation_authority_aborts_before_mutating_outputs() {
    let root = TempDir::new("motion-corrupt-authority");
    let input = root.path().join("source.svg");
    fs::write(
        &input,
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16"><path fill="#ff0000" d="M0 0 L16 0 L16 16 L0 16 Z"/></svg>"##,
    )
    .expect("write motion SVG");
    let out = root.path().join("motion");
    let input_arg = input.to_string_lossy();
    let out_arg = out.to_string_lossy();
    assert_success(&run(&[
        "motion-scaffold",
        &input_arg,
        "--out-dir",
        &out_arg,
    ]));

    let managed = [
        "scene.svg",
        "layers.json",
        "motion-request.json",
        "layer-inspector.html",
    ];
    let before = managed
        .iter()
        .map(|relative| {
            (
                *relative,
                fs::read(out.join(relative)).expect("read managed output"),
            )
        })
        .collect::<Vec<_>>();
    fs::write(out.join(".perfectpixel-generation.json"), b"{")
        .expect("corrupt generation authority");

    let result = run(&["motion-scaffold", &input_arg, "--out-dir", &out_arg]);

    assert_eq!(result.status.code(), Some(2));
    let payload: serde_json::Value =
        serde_json::from_slice(&result.stdout).expect("parse authority error");
    assert_eq!(payload["ok"], false);
    assert!(payload["message"]
        .as_str()
        .is_some_and(|message| message.contains("generation authority")));
    assert_eq!(
        fs::read(out.join(".perfectpixel-generation.json")).unwrap(),
        b"{"
    );
    for (relative, expected) in before {
        assert_eq!(
            fs::read(out.join(relative)).unwrap(),
            expected,
            "{relative}"
        );
    }

    let transaction_prefix = format!(
        ".{}.artifact-set.",
        out.file_name().unwrap().to_string_lossy()
    );
    let parent = out.parent().expect("output parent");
    assert!(!fs::read_dir(parent)
        .expect("read output parent")
        .filter_map(Result::ok)
        .any(|entry| entry
            .file_name()
            .to_string_lossy()
            .starts_with(&transaction_prefix)));
}
