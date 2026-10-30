use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

use perfectpixel::{DecodeLimits, ImageCodec, PngEncoder, Raster};

#[test]
fn rgba8_decode_succeeds_at_the_accounted_pixel_limit() {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root =
        std::env::temp_dir().join(format!("perfectpixel-codec-{}-{stamp}", std::process::id()));
    fs::create_dir(&root).unwrap();
    let path = root.join("rgba.png");
    let raster = Raster::new(2, 1, vec![255; 2 * 4]).unwrap();
    fs::write(&path, PngEncoder::encode_rgba(&raster).unwrap()).unwrap();

    let decoded = ImageCodec::decode_rgba(
        &path,
        DecodeLimits {
            max_dimension: 2,
            max_pixels: 2,
        },
    )
    .unwrap();

    assert_eq!((decoded.width(), decoded.height()), (2, 1));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn byte_snapshot_decode_matches_path_decode() {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "perfectpixel-codec-bytes-{}-{stamp}",
        std::process::id()
    ));
    fs::create_dir(&root).unwrap();
    let path = root.join("snapshot.png");
    let source = Raster::new(
        2,
        2,
        vec![0, 1, 2, 255, 3, 4, 5, 255, 6, 7, 8, 255, 9, 10, 11, 255],
    )
    .unwrap();
    let encoded = PngEncoder::encode_rgba(&source).unwrap();
    fs::write(&path, &encoded).unwrap();

    let from_path = ImageCodec::decode_rgba(&path, DecodeLimits::default()).unwrap();
    let from_bytes =
        ImageCodec::decode_rgba_bytes(&path, &encoded, DecodeLimits::default()).unwrap();

    assert_eq!(from_bytes, from_path);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn byte_snapshot_decode_reports_the_logical_source_path() {
    let path = std::path::Path::new("logical-input.png");

    let error = ImageCodec::decode_rgba_bytes(path, b"not an image", DecodeLimits::default())
        .expect_err("malformed snapshot must fail");

    assert!(error.to_string().contains("logical-input.png"));
}

#[test]
fn byte_snapshot_decode_enforces_pixel_limit() {
    let source = Raster::new(2, 2, vec![255; 2 * 2 * 4]).unwrap();
    let encoded = PngEncoder::encode_rgba(&source).unwrap();

    let error = ImageCodec::decode_rgba_bytes(
        "oversized.png",
        &encoded,
        DecodeLimits {
            max_dimension: 2,
            max_pixels: 3,
        },
    )
    .expect_err("pixel limit must apply to byte snapshots");

    assert!(error.to_string().contains("oversized.png"));
}
