use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    time::{SystemTime, UNIX_EPOCH},
};

use perfectpixel::{PngEncoder, Raster};

fn temp_case(label: &str) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "perfectpixel-vector-removal-{label}-{}-{stamp}",
        std::process::id()
    ));
    fs::create_dir_all(&path).unwrap();
    path
}

fn write_raster(root: &Path, name: &str, continuous: bool) -> PathBuf {
    let mut pixels = Vec::new();
    for y in 0..16u8 {
        for x in 0..16u8 {
            let pixel = if continuous {
                [x.wrapping_mul(13), y.wrapping_mul(17), x ^ y, 255]
            } else {
                [220, 220, 220, 255]
            };
            pixels.extend_from_slice(&pixel);
        }
    }
    let path = root.join(name);
    fs::write(
        &path,
        PngEncoder::encode_rgba(&Raster::new(16, 16, pixels).unwrap()).unwrap(),
    )
    .unwrap();
    path
}

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_perfectpixel"))
        .args(args)
        .output()
        .unwrap()
}
fn error(output: &Output) -> serde_json::Value {
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn removed_commands_are_unknown_and_never_redirect_or_dispatch() {
    let root = temp_case("commands");
    let input = write_raster(&root, "input.png", false);
    for command in ["vectorize", "vector-profile"] {
        let out = root.join(format!("{command}.svg"));
        let report = root.join(format!("{command}.json"));
        let diagnostics = root.join(format!("{command}-diagnostics"));
        let output = run(&[
            command,
            input.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
            "--report",
            report.to_str().unwrap(),
            "--diagnostics",
            diagnostics.to_str().unwrap(),
        ]);
        assert_eq!(output.status.code(), Some(2));
        assert_eq!(
            error(&output)["message"],
            format!("invalid option: unknown command '{command}'; use schema, agent-schema, agent-inspect, agent-extract, agent-render, agent-compare, inspect, convert, upscale, edit, psd, document-psd, chroma-plan, vector, vector-analyze, normalize, bundle, texture-compile, vision-foreground-instances, motion-scaffold, or motion-build")
        );
        assert!(
            !out.exists() && !report.exists() && !diagnostics.exists(),
            "removed command must not dispatch"
        );
    }
}

#[test]
fn removed_preset_aliases_are_canonical_errors_without_artifacts() {
    let root = temp_case("aliases");
    let input = write_raster(&root, "input.png", false);
    for alias in [
        concat!("legacy", "-", "lossless"),
        "lossless",
        "icon",
        "color",
    ] {
        let out = root.join(format!("{alias}.svg"));
        let report = root.join(format!("{alias}.json"));
        let diagnostics = root.join(format!("{alias}-diagnostics"));
        let output = run(&[
            "vector",
            input.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
            "--preset",
            alias,
            "--report",
            report.to_str().unwrap(),
            "--diagnostics",
            diagnostics.to_str().unwrap(),
        ]);
        assert_eq!(output.status.code(), Some(2));
        assert_eq!(error(&output)["message"], "--preset must be auto, pixel-art, exact-grid, flat-icon, line-art, or bounded-illustration");
        assert!(
            !out.exists() && !report.exists() && !diagnostics.exists(),
            "removed preset alias must not dispatch"
        );
    }
}

#[test]
fn report_diagnostics_and_final_follow_the_transaction_contract() {
    let root = temp_case("transaction");
    let input = write_raster(&root, "input.png", false);
    let report = root.join("report.json");
    let diagnostics = root.join("diagnostics");
    let final_svg = root.join("final.svg");
    let success = run(&[
        "vector",
        input.to_str().unwrap(),
        "--out",
        final_svg.to_str().unwrap(),
        "--preset",
        "flat-icon",
        "--profile",
        "compact",
        "--report",
        report.to_str().unwrap(),
        "--diagnostics",
        diagnostics.to_str().unwrap(),
    ]);
    assert_eq!(success.status.code(), Some(0));
    let payload: serde_json::Value = serde_json::from_slice(&success.stdout).unwrap();
    assert_eq!(
        payload["transaction"],
        serde_json::json!({"report":"committed","diagnostics":"committed","finalCommit":"committed"})
    );
    assert!(report.is_file() && diagnostics.is_dir() && final_svg.is_file());

    let rejected_input = write_raster(&root, "continuous.png", true);
    let rejected_report = root.join("rejected.json");
    let rejected_diagnostics = root.join("rejected-diagnostics");
    let rejected_final = root.join("rejected.svg");
    let rejected = run(&[
        "vector",
        rejected_input.to_str().unwrap(),
        "--out",
        rejected_final.to_str().unwrap(),
        "--preset",
        "line-art",
        "--profile",
        "compact",
        "--report",
        rejected_report.to_str().unwrap(),
        "--diagnostics",
        rejected_diagnostics.to_str().unwrap(),
    ]);
    assert_eq!(rejected.status.code(), Some(4));
    let rejected_payload = error(&rejected);
    assert_eq!(
        rejected_payload["transaction"],
        serde_json::json!({"report":"committed","diagnostics":"committed","finalCommit":"notAttempted"})
    );
    assert!(rejected_report.is_file() && rejected_diagnostics.is_dir() && !rejected_final.exists());
}

#[test]
fn failed_transaction_prevents_final_write_and_preserves_existing_destinations() {
    let root = temp_case("failed-transaction");
    let input = write_raster(&root, "input.png", false);
    let final_svg = root.join("final.svg");
    fs::write(&final_svg, "prior final").unwrap();
    let blocked_diagnostics = root.join("diagnostics");
    fs::write(&blocked_diagnostics, "prior diagnostics file").unwrap();
    let report = root.join("report.json");
    let output = run(&[
        "vector",
        input.to_str().unwrap(),
        "--out",
        final_svg.to_str().unwrap(),
        "--preset",
        "flat-icon",
        "--profile",
        "compact",
        "--report",
        report.to_str().unwrap(),
        "--diagnostics",
        blocked_diagnostics.to_str().unwrap(),
    ]);
    assert_eq!(output.status.code(), Some(2));
    let failure = error(&output);
    assert_eq!(failure["phase"], "vector");
    assert_eq!(
        failure["path"],
        blocked_diagnostics.to_string_lossy().as_ref()
    );
    assert!(failure["originalError"]
        .as_str()
        .unwrap()
        .contains("diagnostics destination must be a non-symlink directory"));
    assert!(
        !report.exists(),
        "preflight must run before the first commit"
    );
    assert_eq!(fs::read_to_string(&final_svg).unwrap(), "prior final");
    assert_eq!(
        fs::read_to_string(&blocked_diagnostics).unwrap(),
        "prior diagnostics file"
    );

    let blocked_final_parent = root.join("final-parent");
    fs::write(&blocked_final_parent, "not a directory").unwrap();
    let report_before_final = root.join("report-before-final.json");
    let diagnostics_before_final = root.join("diagnostics-before-final");
    let final_write_failure = run(&[
        "vector",
        input.to_str().unwrap(),
        "--out",
        blocked_final_parent.join("final.svg").to_str().unwrap(),
        "--preset",
        "flat-icon",
        "--profile",
        "compact",
        "--report",
        report_before_final.to_str().unwrap(),
        "--diagnostics",
        diagnostics_before_final.to_str().unwrap(),
    ]);
    assert_eq!(final_write_failure.status.code(), Some(2));
    let failure = error(&final_write_failure);
    assert_eq!(failure["phase"], "vector");
    assert_eq!(
        failure["path"],
        blocked_final_parent.to_string_lossy().as_ref()
    );
    assert!(!report_before_final.exists() && !diagnostics_before_final.exists());
}

#[test]
fn collisions_and_malformed_policy_fail_before_any_transaction_artifact() {
    let root = temp_case("preflight");
    let input = write_raster(&root, "input.png", false);
    let final_svg = root.join("collision.svg");
    let collision = run(&[
        "vector",
        input.to_str().unwrap(),
        "--out",
        final_svg.to_str().unwrap(),
        "--diagnostics",
        input.to_str().unwrap(),
    ]);
    assert_eq!(collision.status.code(), Some(2));
    assert_eq!(
        error(&collision)["message"],
        "vector input and diagnostics must not collide"
    );
    assert!(!final_svg.exists());

    let output_diagnostics_collision = root.join("same.svg");
    let output = run(&[
        "vector",
        input.to_str().unwrap(),
        "--out",
        output_diagnostics_collision.to_str().unwrap(),
        "--diagnostics",
        output_diagnostics_collision.to_str().unwrap(),
    ]);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        error(&output)["message"],
        "vector output and diagnostics must not collide"
    );
    assert!(!output_diagnostics_collision.exists());
    let malformed_policy = root.join("policy.json");
    fs::write(&malformed_policy, "{").unwrap();
    let out = root.join("out.svg");
    let report = root.join("report.json");
    let diagnostics = root.join("diagnostics");
    let policy_failure = run(&[
        "vector",
        input.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
        "--policy",
        malformed_policy.to_str().unwrap(),
        "--report",
        report.to_str().unwrap(),
        "--diagnostics",
        diagnostics.to_str().unwrap(),
    ]);
    assert_eq!(policy_failure.status.code(), Some(2));
    assert_eq!(
        error(&policy_failure)["message"],
        "--policy must reference a .json perfectpixel.vector-policy/1 document"
    );
    assert_eq!(
        error(&policy_failure)["path"],
        malformed_policy.to_string_lossy().as_ref()
    );
    assert!(error(&policy_failure)["originalError"]
        .as_str()
        .unwrap()
        .contains("EOF while parsing an object"));
    assert!(!out.exists() && !report.exists() && !diagnostics.exists());
}
