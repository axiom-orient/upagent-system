use std::{
    fs,
    path::PathBuf,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use perfectpixel::{sha256_hex, PngEncoder, Raster};

#[test]
fn inspect_reports_content_identity_and_explicit_color_provenance() {
    let root = temp_root();
    let input = root.join("input.png");
    let encoded = PngEncoder::encode_rgba(
        &Raster::new(2, 1, vec![1, 2, 3, 255, 4, 5, 6, 0]).expect("raster"),
    )
    .expect("png");
    fs::write(&input, &encoded).expect("write input");

    let output = Command::new(env!("CARGO_BIN_EXE_perfectpixel"))
        .args(["inspect", input.to_str().expect("utf8 path")])
        .output()
        .expect("run inspect");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).expect("inspect json");

    assert_eq!(value["schema"], "perfectpixel.asset-inspection/1");
    assert_eq!(value["width"], 2);
    assert_eq!(value["height"], 1);
    assert_eq!(value["artifact"]["sha256"], sha256_hex(&encoded));
    assert_eq!(value["artifact"]["mediaType"], "image/png");
    assert_eq!(value["artifact"]["bytes"], encoded.len());
    assert_eq!(value["pixelSpec"]["pixelFormat"], "rgba8");
    assert_eq!(value["pixelSpec"]["alpha"], "straight");
    assert_eq!(value["pixelSpec"]["color"]["kind"], "unknown");
    assert_eq!(value["iccProfileByteCount"], 0);
    assert!(value.get("colorSpace").is_none());

    fs::remove_dir_all(root).expect("cleanup");
}

fn temp_root() -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "perfectpixel-inspect-provenance-{}-{stamp}",
        std::process::id()
    ));
    fs::create_dir(&root).expect("create root");
    root
}
