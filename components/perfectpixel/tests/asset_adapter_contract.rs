use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    time::{SystemTime, UNIX_EPOCH},
};

use image::ImageReader;
use perfectpixel::{sha256_hex, PngEncoder, Raster};

#[test]
fn inspect_reports_exact_input_evidence() {
    let root = temp_case("inspect-evidence");
    let input = root.join("input.png");
    write_png(&input, Raster::blank(2, 3).unwrap());

    let result = run(&["inspect", input.to_str().unwrap()]);
    assert_success(&result);
    let summary: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    let input_bytes = fs::read(&input).unwrap();
    assert_eq!(summary["schema"], "perfectpixel.asset-inspection/1");
    assert_eq!(summary["ok"], true);
    assert_eq!(summary["input"], input.to_string_lossy().as_ref());
    assert_eq!(summary["inputSha256"], sha256_hex(&input_bytes));
    assert_eq!(summary["inputByteCount"], input_bytes.len());
    assert_eq!(summary["width"], 2);
    assert_eq!(summary["height"], 3);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn schema_lists_the_complete_raster_adapter_command_set() {
    let result = run(&["schema"]);
    assert_success(&result);
    let schema: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(
        schema["assetAdapter"]["commands"],
        serde_json::json!(["inspect", "convert", "upscale"])
    );
}

#[test]
fn convert_writes_lossless_webp_and_accepts_it_as_a_raster_input() {
    let root = temp_case("convert-webp");
    let input = root.join("input.png");
    let webp = root.join("converted.webp");
    let png = root.join("round-trip.png");
    write_png(
        &input,
        Raster::new(2, 1, vec![255, 0, 0, 255, 0, 255, 0, 64]).unwrap(),
    );

    let converted = run(&[
        "convert",
        input.to_str().unwrap(),
        "--out",
        webp.to_str().unwrap(),
        "--width",
        "4",
        "--filter",
        "nearest",
    ]);
    assert_success(&converted);
    let summary: serde_json::Value = serde_json::from_slice(&converted.stdout).unwrap();
    let input_bytes = fs::read(&input).unwrap();
    assert_eq!(summary["schema"], "perfectpixel.asset-transform/1");
    assert_eq!(summary["inputSha256"], sha256_hex(&input_bytes));
    assert_eq!(summary["inputByteCount"], input_bytes.len());
    let decoded = decode_rgba(&webp);
    let output_bytes = fs::read(&webp).unwrap();
    assert_eq!(summary["outputSha256"], sha256_hex(&output_bytes));
    assert_eq!(summary["outputByteCount"], output_bytes.len());
    assert_eq!(summary["format"], "webp-lossless");
    assert_eq!((decoded.width(), decoded.height()), (4, 2));
    assert_eq!(decoded.as_raw()[3], 255);
    assert_eq!(decoded.as_raw()[7], 255);
    assert_eq!(decoded.as_raw()[11], 64);

    assert_success(&run(&[
        "convert",
        webp.to_str().unwrap(),
        "--out",
        png.to_str().unwrap(),
    ]));
    assert_eq!(decode_rgba(&png).as_raw(), decoded.as_raw());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn transparent_jpeg_requires_a_matte_and_writes_when_one_is_supplied() {
    let root = temp_case("convert-jpeg-alpha");
    let input = root.join("input.png");
    let output = root.join("output.jpg");
    write_png(&input, Raster::new(1, 1, vec![255, 0, 0, 128]).unwrap());

    let rejected = run(&[
        "convert",
        input.to_str().unwrap(),
        "--out",
        output.to_str().unwrap(),
    ]);
    assert_eq!(rejected.status.code(), Some(2));
    assert!(!output.exists());
    assert!(String::from_utf8_lossy(&rejected.stdout).contains("requires --background"));

    assert_success(&run(&[
        "convert",
        input.to_str().unwrap(),
        "--out",
        output.to_str().unwrap(),
        "--background",
        "#0000ff",
        "--jpeg-quality",
        "100",
    ]));
    let decoded = decode_rgba(&output);
    assert_eq!((decoded.width(), decoded.height()), (1, 1));
    assert_eq!(decoded.as_raw()[3], 255);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn upscale_defaults_to_exact_nearest_neighbor_pixels() {
    let root = temp_case("upscale-nearest");
    let input = root.join("input.png");
    let output = root.join("output.png");
    write_png(
        &input,
        Raster::new(2, 1, vec![1, 2, 3, 255, 4, 5, 6, 255]).unwrap(),
    );

    let result = run(&[
        "upscale",
        input.to_str().unwrap(),
        "--out",
        output.to_str().unwrap(),
        "--scale",
        "2",
    ]);
    assert_success(&result);
    let summary: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    let input_bytes = fs::read(&input).unwrap();
    let output_bytes = fs::read(&output).unwrap();
    assert_eq!(summary["schema"], "perfectpixel.asset-transform/1");
    assert_eq!(summary["inputSha256"], sha256_hex(&input_bytes));
    assert_eq!(summary["inputByteCount"], input_bytes.len());
    assert_eq!(summary["outputSha256"], sha256_hex(&output_bytes));
    assert_eq!(summary["outputByteCount"], output_bytes.len());
    assert_eq!(summary["filter"], "nearest");
    let decoded = decode_rgba(&output);
    assert_eq!(
        decoded.as_raw(),
        &[
            1, 2, 3, 255, 1, 2, 3, 255, 4, 5, 6, 255, 4, 5, 6, 255, 1, 2, 3, 255, 1, 2, 3, 255, 4,
            5, 6, 255, 4, 5, 6, 255,
        ]
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn transform_writer_failure_emits_no_success_digest_evidence_or_output() {
    let root = temp_case("transform-writer-failure");
    let input = root.join("input.png");
    let blocked_parent = root.join("blocked-parent");
    let output = blocked_parent.join("output.png");
    write_png(&input, Raster::blank(1, 1).unwrap());
    fs::write(&blocked_parent, b"preserve this blocking file").unwrap();

    let result = run(&[
        "convert",
        input.to_str().unwrap(),
        "--out",
        output.to_str().unwrap(),
    ]);

    assert_eq!(result.status.code(), Some(2));
    let payload: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(payload["ok"], false);
    assert!(payload.get("outputSha256").is_none());
    assert!(payload.get("outputByteCount").is_none());
    assert!(!output.exists());
    assert_eq!(
        fs::read(&blocked_parent).unwrap(),
        b"preserve this blocking file"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn asset_options_reject_non_jpeg_background_and_same_destination() {
    let root = temp_case("asset-options");
    let input = root.join("input.png");
    write_png(&input, Raster::blank(1, 1).unwrap());

    let non_jpeg_background = run(&[
        "convert",
        input.to_str().unwrap(),
        "--out",
        root.join("output.png").to_str().unwrap(),
        "--background",
        "#ffffff",
    ]);
    assert_eq!(non_jpeg_background.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&non_jpeg_background.stdout).contains("only valid for JPEG"));

    let same_destination = run(&[
        "convert",
        input.to_str().unwrap(),
        "--out",
        input.to_str().unwrap(),
    ]);
    assert_eq!(same_destination.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&same_destination.stdout).contains("must not collide"));
    fs::remove_dir_all(root).unwrap();
}

fn write_png(path: &Path, image: Raster) {
    fs::write(path, PngEncoder::encode_rgba(&image).unwrap()).unwrap();
}

fn decode_rgba(path: &Path) -> image::RgbaImage {
    ImageReader::open(path)
        .unwrap()
        .decode()
        .unwrap()
        .into_rgba8()
}

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_perfectpixel"))
        .args(args)
        .output()
        .unwrap()
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "status={:?}; stdout={}; stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

fn temp_case(name: &str) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "perfectpixel-{name}-{}-{stamp}",
        std::process::id()
    ));
    fs::create_dir(&root).unwrap();
    root
}
