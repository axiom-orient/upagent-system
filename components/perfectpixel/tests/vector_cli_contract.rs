use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use perfectpixel::{sha256_hex, PngEncoder, Raster};

fn temp_case(label: &str) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "perfectpixel-vector-cli-{label}-{}-{stamp}",
        std::process::id()
    ));
    fs::create_dir_all(&path).unwrap();
    path
}

fn raster(root: &Path, name: &str, continuous: bool) -> PathBuf {
    let path = root.join(name);
    let mut pixels = Vec::new();
    for y in 0..16u8 {
        for x in 0..16u8 {
            let rgba = if continuous {
                [x.wrapping_mul(13), y.wrapping_mul(17), x ^ y, 255]
            } else {
                [220, 220, 220, 255]
            };
            pixels.extend_from_slice(&rgba);
        }
    }
    let image = Raster::new(16, 16, pixels).unwrap();
    fs::write(&path, PngEncoder::encode_rgba(&image).unwrap()).unwrap();
    path
}

fn run(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_perfectpixel"))
        .args(args)
        .output()
        .unwrap()
}

fn json(output: &std::process::Output) -> serde_json::Value {
    serde_json::from_slice(&output.stdout).unwrap()
}
fn object_keys(value: &serde_json::Value) -> BTreeSet<&str> {
    value
        .as_object()
        .expect("JSON object")
        .keys()
        .map(String::as_str)
        .collect()
}

#[test]
fn vector_schema_help_and_option_vocabulary_are_exact() {
    let schema = json(&run(&["schema"]));
    assert_eq!(
        schema["commands"],
        serde_json::json!([
            "schema",
            "agent-schema",
            "agent-inspect",
            "agent-extract",
            "agent-render",
            "agent-compare",
            "inspect",
            "convert",
            "upscale",
            "edit",
            "psd",
            "document-psd",
            "chroma-plan",
            "vector",
            "vector-analyze",
            "normalize",
            "bundle",
            "texture-compile",
            "vision-foreground-instances",
            "motion-scaffold",
            "motion-build"
        ])
    );
    assert_eq!(schema["vectorPolicySchema"], "perfectpixel.vector-policy/1");
    assert_eq!(
        schema["vectorEvaluationSchema"],
        "perfectpixel.vector-evaluation/3"
    );
    assert_eq!(
        schema["vectorAnalysisSchema"],
        "perfectpixel.vector-analysis/1"
    );
    assert_eq!(
        schema["vectorPresets"],
        serde_json::json!([
            "auto",
            "pixel-art",
            "exact-grid",
            "flat-icon",
            "line-art",
            "bounded-illustration"
        ])
    );
    assert_eq!(
        schema["vectorProfiles"],
        serde_json::json!(["compact", "motion-structure-ready"])
    );
    assert_eq!(
        schema["vectorCommand"],
        serde_json::json!({
            "arguments":["<input.png|jpg|jpeg|webp>"],
            "options":["--out","--preset","--profile","--detail","--min-quality","--max-quality-loss","--max-paths","--policy","--report","--diagnostics"],
            "defaults":{"preset":"auto","profile":"compact","detail":"auto","minQuality":null,"maxQualityLoss":null,"maxPaths":null},
            "publicationOrder":["report","diagnostics","finalSvg"],
            "artifactOrder":["candidate.svg (image/svg+xml)","render-back.png (image/png)"]
        })
    );
    assert_eq!(
        schema["vectorAnalyzeCommand"],
        serde_json::json!({
            "arguments":["<input.png|jpg|jpeg|webp>"],
            "options":["--preset","--profile","--policy","--report"],
            "defaults":{"preset":"auto","profile":"compact"},
            "publicationOrder":["report"],
            "artifactOrder":[],
            "publishesSvg":false
        })
    );
    assert_eq!(
        schema["vectorAuthority"],
        "Embedded routes and thresholds are immutable; --policy may only select or tighten request constraints."
    );
    assert!(schema.get("vector").is_none());

    let help = String::from_utf8(run(&["--help"]).stdout).unwrap();
    assert!(help.contains("perfectpixel vector <input.png|jpg|jpeg|webp> --out <output.svg> [--preset auto|pixel-art|exact-grid|flat-icon|line-art|bounded-illustration] [--profile compact|motion-structure-ready] [--detail auto|1|2|3|4|5] [--min-quality <0..1>] [--max-quality-loss <0..1>] [--max-paths <positive integer>] [--policy <vector-policy.json>] [--report <evaluation.json>] [--diagnostics <dir>]"));
    assert!(help.contains("perfectpixel vector-analyze <input.png|jpg|jpeg|webp> [--preset auto|pixel-art|exact-grid|flat-icon|line-art|bounded-illustration] [--profile compact|motion-structure-ready] [--policy <vector-policy.json>] [--report <analysis.json>]"));
    assert!(help.contains("vector is the sole raster-to-SVG quality-gated publication command"));
    assert!(help.contains(
        "motion-build may publish derived animated.svg from an accepted raster-free SVG source"
    ));
    assert!(!help.contains("vector is the sole SVG publication command"));
    let bare_help = run(&["help"]);
    assert_eq!(bare_help.status.code(), Some(2));
    assert_eq!(
        json(&bare_help)["message"],
        "invalid option: unknown command 'help'; use schema, agent-schema, agent-inspect, agent-extract, agent-render, agent-compare, inspect, convert, upscale, edit, psd, document-psd, chroma-plan, vector, vector-analyze, normalize, bundle, texture-compile, vision-foreground-instances, motion-scaffold, or motion-build"
    );
}

#[test]
fn analysis_is_identity_report_with_default_environment_nulls_and_no_unrequested_output() {
    let root = temp_case("analysis");
    let input = raster(&root, "input.png", false);
    let report = root.join("analysis.json");
    let output = run(&[
        "vector-analyze",
        input.to_str().unwrap(),
        "--preset",
        "flat-icon",
        "--profile",
        "compact",
        "--report",
        report.to_str().unwrap(),
    ]);
    assert_eq!(output.status.code(), Some(0));
    let stdout = json(&output);
    let saved: serde_json::Value = serde_json::from_slice(&fs::read(&report).unwrap()).unwrap();
    assert_eq!(stdout["schema"], "perfectpixel.vector-analysis-result/1");
    assert_eq!(stdout["analysis"], saved);
    assert_eq!(
        object_keys(&stdout),
        BTreeSet::from(["analysis", "ok", "schema", "transaction"])
    );
    assert_eq!(saved["schema"], "perfectpixel.vector-analysis/1");
    assert_eq!(saved["preset"], "flat-icon");
    assert_eq!(saved["profile"], "compact");
    for key in [
        "renderTimeMs",
        "peakMemoryBytes",
        "throughputPixelsPerSecond",
        "machineProtocolId",
    ] {
        assert!(saved["environmentMeasurements"][key].is_null());
    }
    assert_eq!(
        stdout["transaction"],
        serde_json::json!({"report":"committed","diagnostics":"notRequested","finalCommit":"notRequested"})
    );
    assert!(saved.get("actualDecision").is_none());
    assert!(saved.get("svg").is_none());

    let no_report = run(&["vector-analyze", input.to_str().unwrap()]);
    assert_eq!(no_report.status.code(), Some(0));
    let default_analysis = json(&no_report)["analysis"].clone();
    assert_eq!(default_analysis["preset"], "auto");
    assert_eq!(default_analysis["profile"], "compact");
    assert_eq!(
        fs::read_dir(&root).unwrap().count(),
        2,
        "analysis without --report must not create artifacts"
    );
}

#[test]
fn vector_snapshot_guard_is_raw_file_evidence_while_input_digest_is_semantic_rgba() {
    let root = temp_case("snapshot-semantic-digest");
    let png = raster(&root, "input.png", false);
    let webp = root.join("input.webp");
    let converted = run(&[
        "convert",
        png.to_str().unwrap(),
        "--out",
        webp.to_str().unwrap(),
    ]);
    assert_eq!(converted.status.code(), Some(0));
    assert_ne!(
        sha256_hex(&fs::read(&png).unwrap()),
        sha256_hex(&fs::read(&webp).unwrap()),
        "the raw source files must remain distinct evidence"
    );

    let png_analysis = run(&[
        "vector-analyze",
        png.to_str().unwrap(),
        "--preset",
        "flat-icon",
    ]);
    let webp_analysis = run(&[
        "vector-analyze",
        webp.to_str().unwrap(),
        "--preset",
        "flat-icon",
    ]);
    assert_eq!(png_analysis.status.code(), Some(0));
    assert_eq!(webp_analysis.status.code(), Some(0));
    let png_json = json(&png_analysis);
    let webp_json = json(&webp_analysis);
    assert_eq!(
        png_json["analysis"]["inputDigest"], webp_json["analysis"]["inputDigest"],
        "semantic RGBA identity must not become a raw file digest"
    );
}

#[test]
fn generation_and_domain_rejection_have_distinct_structured_results() {
    let root = temp_case("results");
    let accepted_input = raster(&root, "flat.png", false);
    let final_svg = root.join("final.svg");
    let generated = run(&[
        "vector",
        accepted_input.to_str().unwrap(),
        "--out",
        final_svg.to_str().unwrap(),
        "--preset",
        "flat-icon",
        "--profile",
        "compact",
        "--detail",
        "auto",
    ]);
    let generated_json = json(&generated);
    assert_eq!(generated.status.code(), Some(0));
    assert_eq!(generated_json["schema"], "perfectpixel.vector-result/1");
    assert_eq!(generated_json["ok"], true);
    assert_eq!(generated_json["decision"], "approved");
    assert_eq!(
        object_keys(&generated_json),
        BTreeSet::from(["decision", "ok", "report", "schema", "transaction"])
    );
    assert_eq!(
        generated_json["transaction"],
        serde_json::json!({"report":"notRequested","diagnostics":"notRequested","finalCommit":"committed"})
    );
    assert_eq!(
        generated_json["report"]["schema"],
        "perfectpixel.vector-evaluation/3"
    );
    assert!(final_svg.is_file());

    let rejected_input = raster(&root, "continuous.png", true);
    let rejected_svg = root.join("rejected.svg");
    let rejected = run(&[
        "vector",
        rejected_input.to_str().unwrap(),
        "--out",
        rejected_svg.to_str().unwrap(),
        "--preset",
        "line-art",
        "--profile",
        "compact",
    ]);
    let rejected_json = json(&rejected);
    assert_eq!(rejected.status.code(), Some(4));
    assert_eq!(rejected_json["schema"], "perfectpixel.vector-result/1");
    assert_eq!(rejected_json["ok"], false);
    assert_eq!(rejected_json["decision"], "notApplicable");
    assert_eq!(
        object_keys(&rejected_json),
        BTreeSet::from(["decision", "ok", "report", "schema", "transaction"])
    );
    assert_eq!(rejected_json["transaction"]["finalCommit"], "notAttempted");
    assert_eq!(rejected_json["report"]["actualDecision"], "notApplicable");
    assert_eq!(
        rejected_json["report"]["schema"],
        "perfectpixel.vector-evaluation/3"
    );
    assert!(!rejected_svg.exists());
}

#[test]
fn analysis_rejects_generation_controls_and_generation_validates_exact_options() {
    let root = temp_case("options");
    let input = raster(&root, "input.png", false);
    let missing_output = run(&["vector", input.to_str().unwrap()]);
    assert_eq!(missing_output.status.code(), Some(2));
    assert_eq!(
        json(&missing_output)["message"],
        "vector requires <input> and --out <output.svg>"
    );
    for option in [
        "--out",
        "--detail",
        "--min-quality",
        "--max-quality-loss",
        "--max-paths",
        "--diagnostics",
    ] {
        let output = run(&["vector-analyze", input.to_str().unwrap(), option, "value"]);
        assert_eq!(output.status.code(), Some(2));
        assert_eq!(json(&output)["message"], "vector-analyze does not accept --out, candidate-detail, quality-limit, path-limit, or diagnostics options");
    }
    for (option, value, message) in [
        (
            "--detail",
            "0",
            "--detail must be auto or an integer from 1 through 5",
        ),
        (
            "--min-quality",
            "1.1",
            "--min-quality and --max-quality-loss must be finite numbers from 0 through 1",
        ),
        (
            "--max-quality-loss",
            "NaN",
            "--min-quality and --max-quality-loss must be finite numbers from 0 through 1",
        ),
        ("--max-paths", "0", "--max-paths must be a positive integer"),
    ] {
        let output = run(&[
            "vector",
            input.to_str().unwrap(),
            "--out",
            root.join("out.svg").to_str().unwrap(),
            option,
            value,
        ]);
        assert_eq!(output.status.code(), Some(2));
        assert_eq!(json(&output)["message"], message);
    }
}
#[test]
fn generation_preflight_rejects_canonical_input_and_destination_collisions() {
    let root = temp_case("preflight");
    let input = raster(&root, "input.png", false);
    let missing_input = run(&["vector", "--out", root.join("out.svg").to_str().unwrap()]);
    assert_eq!(missing_input.status.code(), Some(2));
    assert_eq!(
        json(&missing_input)["message"],
        "vector requires <input> and --out <output.svg>"
    );

    let policy = root.join("policy.json");
    fs::write(
        &policy,
        r#"{"schema":"perfectpixel.vector-policy/1","version":"v","allowedPalette":[],"requiredPalette":[],"rejectUnmapped":false,"allowDropNoise":false}"#,
    )
    .unwrap();
    let policy_report = run(&[
        "vector-analyze",
        input.to_str().unwrap(),
        "--policy",
        policy.to_str().unwrap(),
        "--report",
        policy.to_str().unwrap(),
    ]);
    assert_eq!(policy_report.status.code(), Some(2));
    assert_eq!(
        json(&policy_report)["message"],
        "vector policy and report must not collide"
    );

    let overlap_diagnostics = root.join("overlap-diagnostics");
    let overlap_report = overlap_diagnostics.join("report.json");
    let overlap = run(&[
        "vector",
        input.to_str().unwrap(),
        "--out",
        root.join("out.svg").to_str().unwrap(),
        "--report",
        overlap_report.to_str().unwrap(),
        "--diagnostics",
        overlap_diagnostics.to_str().unwrap(),
    ]);
    assert_eq!(overlap.status.code(), Some(2));
    assert_eq!(
        json(&overlap)["message"],
        "vector report and diagnostics must not collide"
    );

    let nested_report = root.join("nested-report.json");
    fs::create_dir_all(&nested_report).unwrap();
    let nested_output = nested_report.join("out.svg");
    let nested_collision = run(&[
        "vector",
        input.to_str().unwrap(),
        "--out",
        nested_output.to_str().unwrap(),
        "--report",
        nested_report.to_str().unwrap(),
    ]);
    assert_eq!(nested_collision.status.code(), Some(2));
    assert_eq!(
        json(&nested_collision)["message"],
        "vector output and report must not collide"
    );
}
#[test]
fn vector_singleton_controls_reject_duplicates_before_artifacts() {
    let root = temp_case("duplicate-controls");
    let input = raster(&root, "input.png", false);
    for (option, value) in [
        ("--out", "duplicate.svg"),
        ("--preset", "flat-icon"),
        ("--report", "duplicate.json"),
        ("--diagnostics", "duplicate-diagnostics"),
    ] {
        let out = root.join("out.svg");
        let mut args = vec![
            "vector".to_owned(),
            input.display().to_string(),
            "--out".to_owned(),
            out.display().to_string(),
            option.to_owned(),
            value.to_owned(),
            option.to_owned(),
            value.to_owned(),
        ];
        let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
        let result = run(&refs);
        assert_eq!(result.status.code(), Some(2));
        assert_eq!(
            json(&result)["message"],
            format!("duplicate option '{option}'")
        );
        assert!(!out.exists());
        args.clear();
    }
}
#[test]
fn vector_static_destinations_precede_decode_and_reject_unsafe_endpoints() {
    let root = temp_case("static-destination");
    let missing_input = root.join("missing.png");
    let output = root.join("output.svg");
    let report = root.join("report.json");
    fs::create_dir(&report).unwrap();
    let result = run(&[
        "vector",
        missing_input.to_str().unwrap(),
        "--out",
        output.to_str().unwrap(),
        "--report",
        report.to_str().unwrap(),
    ]);
    assert_eq!(result.status.code(), Some(2));
    let payload = json(&result);
    assert_eq!(payload["phase"], "vector");
    assert_eq!(payload["path"], report.to_string_lossy().as_ref());
    assert!(payload["originalError"]
        .as_str()
        .unwrap()
        .contains("file destination must be a regular non-symlink file"));
    assert!(!output.exists());

    let input = raster(&root, "input.png", false);
    let diagnostics = root.join("diagnostics");
    fs::create_dir(&diagnostics).unwrap();
    fs::write(diagnostics.join("candidate.svg"), "<svg/>").unwrap();
    fs::write(diagnostics.join("render-back.png"), "lookalike").unwrap();
    let result = run(&[
        "vector",
        input.to_str().unwrap(),
        "--out",
        output.to_str().unwrap(),
        "--diagnostics",
        diagnostics.to_str().unwrap(),
    ]);
    assert_eq!(result.status.code(), Some(2));
    assert_eq!(
        json(&result)["path"],
        diagnostics.to_string_lossy().as_ref()
    );
    assert!(fs::read_dir(&diagnostics)
        .unwrap()
        .all(|entry| entry.unwrap().file_name() != ".perfectpixel-vector-diagnostics.json"));
}
#[test]
fn managed_diagnostics_manifest_allows_a_verified_rerun() {
    let root = temp_case("managed-diagnostics");
    let input = raster(&root, "input.png", false);
    let output = root.join("output.svg");
    let diagnostics = root.join("diagnostics");
    let args = [
        "vector",
        input.to_str().unwrap(),
        "--out",
        output.to_str().unwrap(),
        "--preset",
        "flat-icon",
        "--diagnostics",
        diagnostics.to_str().unwrap(),
    ];
    assert_eq!(run(&args).status.code(), Some(0));
    let ownership: serde_json::Value = serde_json::from_slice(
        &fs::read(diagnostics.join(".perfectpixel-vector-diagnostics.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        ownership["schema"],
        "perfectpixel.vector-diagnostics-ownership/1"
    );
    assert_eq!(run(&args).status.code(), Some(0));
}
