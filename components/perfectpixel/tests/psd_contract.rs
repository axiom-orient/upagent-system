use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use perfectpixel::{PngEncoder, Raster};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let count = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "perfectpixel-psd-contract-{}-{stamp}-{count}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("temp dir");
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

fn rgba(width: u32, height: u32, alpha: &[u8]) -> Raster {
    assert_eq!(alpha.len(), (width * height) as usize);
    let mut pixels = Vec::with_capacity(alpha.len() * 4);
    for (index, value) in alpha.iter().copied().enumerate() {
        pixels.extend_from_slice(&[
            (index as u8).wrapping_mul(17),
            40,
            220u8.wrapping_sub(index as u8),
            value,
        ]);
    }
    Raster::new(width, height, pixels).expect("raster")
}

fn write_png(path: &Path, image: &Raster) {
    fs::write(path, PngEncoder::encode_rgba(image).expect("png")).expect("write png");
}

fn run(request: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_perfectpixel"))
        .args(["psd", "--request"])
        .arg(request)
        .output()
        .expect("run psd")
}

fn request(root: &Path, input: &str, output: &str, threshold: u8, max_knots: usize) -> PathBuf {
    let path = root.join("request.json");
    fs::write(
        &path,
        serde_json::json!({
            "schemaVersion": 1,
            "operation": "export_psd",
            "input": input,
            "output": output,
            "path": {"alphaThreshold": threshold, "maxKnots": max_knots}
        })
        .to_string(),
    )
    .expect("request");
    path
}

fn read_u16(bytes: &[u8], offset: &mut usize) -> u16 {
    let end = *offset + 2;
    let value = u16::from_be_bytes(bytes[*offset..end].try_into().expect("u16"));
    *offset = end;
    value
}

fn read_u32(bytes: &[u8], offset: &mut usize) -> u32 {
    let end = *offset + 4;
    let value = u32::from_be_bytes(bytes[*offset..end].try_into().expect("u32"));
    *offset = end;
    value
}

#[derive(Debug)]
struct ParsedPsd {
    width: u32,
    height: u32,
    channels: u16,
    depth: u16,
    color_mode: u16,
    resources: BTreeMap<u16, (String, Vec<u8>)>,
    image_data: Vec<u8>,
}

fn parse_psd(bytes: &[u8]) -> ParsedPsd {
    assert!(bytes.len() >= 26);
    assert_eq!(&bytes[..4], b"8BPS");
    let mut offset = 4;
    assert_eq!(read_u16(bytes, &mut offset), 1);
    assert_eq!(&bytes[offset..offset + 6], &[0; 6]);
    offset += 6;
    let channels = read_u16(bytes, &mut offset);
    let height = read_u32(bytes, &mut offset);
    let width = read_u32(bytes, &mut offset);
    let depth = read_u16(bytes, &mut offset);
    let color_mode = read_u16(bytes, &mut offset);
    assert_eq!(read_u32(bytes, &mut offset), 0);
    let resource_len = read_u32(bytes, &mut offset) as usize;
    let resource_end = offset + resource_len;
    let mut resources = BTreeMap::new();
    while offset < resource_end {
        assert_eq!(&bytes[offset..offset + 4], b"8BIM");
        offset += 4;
        let id = read_u16(bytes, &mut offset);
        let name_len = bytes[offset] as usize;
        offset += 1;
        let name = String::from_utf8(bytes[offset..offset + name_len].to_vec()).expect("name");
        offset += name_len;
        if !(name_len + 1).is_multiple_of(2) {
            offset += 1;
        }
        let data_len = read_u32(bytes, &mut offset) as usize;
        let data = bytes[offset..offset + data_len].to_vec();
        offset += data_len;
        if !data_len.is_multiple_of(2) {
            offset += 1;
        }
        assert!(resources.insert(id, (name, data)).is_none());
    }
    assert_eq!(offset, resource_end);
    assert_eq!(read_u32(bytes, &mut offset), 0); // layer and mask section
    assert_eq!(read_u16(bytes, &mut offset), 0); // raw compression
    let image_data = bytes[offset..].to_vec();
    assert_eq!(
        image_data.len(),
        width as usize * height as usize * channels as usize
    );
    ParsedPsd {
        width,
        height,
        channels,
        depth,
        color_mode,
        resources,
        image_data,
    }
}

fn parse_path_records(data: &[u8]) -> Vec<(u16, Vec<u8>)> {
    assert_eq!(data.len() % 26, 0);
    data.as_chunks::<26>()
        .0
        .iter()
        .map(|record| (u16::from_be_bytes([record[0], record[1]]), record.to_vec()))
        .collect()
}

fn read_i32(bytes: &[u8], offset: &mut usize) -> i32 {
    let end = *offset + 4;
    let value = i32::from_be_bytes(bytes[*offset..end].try_into().expect("i32"));
    *offset = end;
    value
}

fn knot_points(record: &[u8]) -> [[i32; 2]; 3] {
    assert_eq!(record.len(), 26);
    let mut offset = 2;
    let mut points = [[0; 2]; 3];
    for point in &mut points {
        // PSD stores each point's vertical component before its horizontal
        // component.  The three points are preceding control, anchor, leaving
        // control, respectively.
        point[0] = read_i32(record, &mut offset);
        point[1] = read_i32(record, &mut offset);
    }
    points
}

#[test]
fn exports_flattened_rgba_planes_and_valid_path_resources() {
    let root = TempDir::new();
    let source = rgba(4, 3, &[0, 0, 0, 0, 0, 255, 255, 0, 0, 128, 255, 0]);
    write_png(&root.path().join("source.png"), &source);
    let req = request(root.path(), "source.png", "cutout.psd", 128, 8192);
    let result = run(&req);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stdout)
    );
    let evidence: serde_json::Value = serde_json::from_slice(&result.stdout).expect("evidence");
    assert_eq!(evidence["schema"], "perfectpixel.photoshop-export/1");
    assert_eq!(evidence["operation"], "export_psd");
    assert_eq!(evidence["channels"], 4);
    assert_eq!(evidence["depth"], 8);
    assert_eq!(evidence["colorMode"], "RGB");
    let bytes = fs::read(root.path().join("cutout.psd")).expect("PSD");
    assert_eq!(evidence["outputByteCount"], bytes.len());
    assert_eq!(evidence["outputSha256"], perfectpixel::sha256_hex(&bytes));
    let parsed = parse_psd(&bytes);
    assert_eq!((parsed.width, parsed.height), (4, 3));
    assert_eq!(
        (parsed.channels, parsed.depth, parsed.color_mode),
        (4, 8, 3)
    );
    assert_eq!(parsed.resources.len(), 3);
    assert_eq!(parsed.resources[&1025].0, "Working Path");
    assert_eq!(parsed.resources[&2000].0, "Cutout Path");
    assert_eq!(parsed.resources[&1025].1, parsed.resources[&2000].1);
    let path_records = parse_path_records(&parsed.resources[&2000].1);
    assert_eq!(path_records[0].0, 6);
    assert!(path_records[0].1[2..].iter().all(|byte| *byte == 0));
    assert_eq!(path_records[1].0, 0);
    assert_eq!(
        u16::from_be_bytes([path_records[1].1[2], path_records[1].1[3]]),
        4
    );
    assert_eq!(path_records[2].0, 2);
    assert_eq!(path_records[2].1.len(), 26);
    assert_eq!(&path_records[2].1[2..10], &path_records[2].1[10..18]);
    assert_eq!(&path_records[2].1[10..18], &path_records[2].1[18..26]);
    let clipping = &parsed.resources[&2999].1;
    assert_eq!(clipping[0], 11);
    assert_eq!(&clipping[1..12], b"Cutout Path");
    assert_eq!(&clipping[12..16], &0x0001_0000u32.to_be_bytes());
    assert_eq!(&clipping[16..18], &1u16.to_be_bytes());
    // Raw planar data is byte-for-byte R, G, B, A from the RGBA source.
    let mut expected = Vec::new();
    for channel in 0..4 {
        expected.extend(
            source
                .pixels()
                .as_chunks::<4>()
                .0
                .iter()
                .map(|pixel| pixel[channel]),
        );
    }
    assert_eq!(parsed.image_data, expected);
    assert!(parsed.image_data.contains(&128) || source.pixels().contains(&128));
}

#[test]
fn preserves_holes_and_disconnected_components_with_even_odd_paths() {
    let root = TempDir::new();
    let mut alpha = vec![0; 8 * 8];
    for y in 1..7 {
        for x in 1..7 {
            alpha[y * 8 + x] = 255;
        }
    }
    alpha[0] = 255;
    for y in 3..5 {
        for x in 3..5 {
            alpha[y * 8 + x] = 0;
        }
    }
    let source = rgba(8, 8, &alpha);
    write_png(&root.path().join("source.png"), &source);
    let req = request(root.path(), "source.png", "cutout.psd", 128, 8192);
    let result = run(&req);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stdout)
    );
    let bytes = fs::read(root.path().join("cutout.psd")).expect("PSD");
    let parsed = parse_psd(&bytes);
    let records = parse_path_records(&parsed.resources[&2000].1);
    assert_eq!(records[0].0, 6);
    let mut offset = 1;
    let mut contours = 0;
    while offset < records.len() {
        assert_eq!(records[offset].0, 0);
        let count = u16::from_be_bytes([records[offset].1[2], records[offset].1[3]]) as usize;
        assert!(count >= 4);
        for record in &records[offset + 1..offset + 1 + count] {
            assert_eq!(record.0, 2);
        }
        offset += count + 1;
        contours += 1;
    }
    assert_eq!(contours, 3, "outer, hole, and isolated component");
}

#[test]
fn rejects_empty_foreground_and_preserves_existing_output() {
    let root = TempDir::new();
    let source = rgba(2, 2, &[0, 0, 0, 0]);
    write_png(&root.path().join("source.png"), &source);
    let output = root.path().join("cutout.psd");
    let original = b"existing PSD";
    fs::write(&output, original).expect("existing output");
    let req = request(root.path(), "source.png", "cutout.psd", 128, 8192);
    let result = run(&req);
    assert!(!result.status.success());
    assert_eq!(fs::read(&output).expect("output"), original);
}

#[test]
fn rejects_unknown_request_fields_and_path_collisions() {
    let root = TempDir::new();
    let source = rgba(1, 1, &[255]);
    write_png(&root.path().join("source.png"), &source);
    let unknown = root.path().join("unknown.json");
    fs::write(
        &unknown,
        serde_json::json!({
            "schemaVersion": 1,
            "operation": "export_psd",
            "input": "source.png",
            "output": "out.psd",
            "path": {"alphaThreshold": 128, "maxKnots": 8192},
            "unexpected": true
        })
        .to_string(),
    )
    .expect("unknown request");
    assert_eq!(run(&unknown).status.code(), Some(2));

    let collision = request(root.path(), "source.png", "source.png", 128, 8192);
    assert_eq!(run(&collision).status.code(), Some(2));
    assert!(!root.path().join("source.png.psd").exists());
}

#[test]
fn threshold_and_complexity_are_bounded_and_deterministic() {
    let root = TempDir::new();
    let source = rgba(
        4,
        4,
        &[
            1, 255, 1, 255, 255, 1, 255, 1, 1, 255, 1, 255, 255, 1, 255, 1,
        ],
    );
    write_png(&root.path().join("source.png"), &source);
    let low = request(root.path(), "source.png", "too-complex.psd", 128, 4);
    let failed = run(&low);
    assert!(!failed.status.success());
    assert!(!root.path().join("too-complex.psd").exists());

    let first_req = request(root.path(), "source.png", "first.psd", 2, 8192);
    assert!(run(&first_req).status.success());
    let first = fs::read(root.path().join("first.psd")).expect("first PSD");
    let second_req = request(root.path(), "source.png", "second.psd", 2, 8192);
    assert!(run(&second_req).status.success());
    let second = fs::read(root.path().join("second.psd")).expect("second PSD");
    assert_eq!(first, second);
}

#[test]
fn edge_touch_and_full_canvas_foreground_remain_closed_paths() {
    let root = TempDir::new();
    let source = rgba(2, 3, &[255, 255, 255, 255, 255, 255]);
    write_png(&root.path().join("source.png"), &source);
    let req = request(root.path(), "source.png", "full.psd", 255, 8192);
    let result = run(&req);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stdout)
    );
    let parsed = parse_psd(&fs::read(root.path().join("full.psd")).expect("PSD"));
    let records = parse_path_records(&parsed.resources[&1025].1);
    assert_eq!(records[0].0, 6);
    assert_eq!(records[1].0, 0);
    assert_eq!(u16::from_be_bytes([records[1].1[2], records[1].1[3]]), 4);
}

#[test]
fn diagonal_foreground_pixels_stay_two_independent_four_knot_subpaths() {
    let root = TempDir::new();
    for (label, alpha) in [
        ("descending", vec![255, 0, 0, 255]),
        ("ascending", vec![0, 255, 255, 0]),
    ] {
        let source = rgba(2, 2, &alpha);
        write_png(&root.path().join(format!("{label}.png")), &source);
        let first_request = request(
            root.path(),
            &format!("{label}.png"),
            &format!("{label}-first.psd"),
            128,
            8192,
        );
        let first_result = run(&first_request);
        assert!(
            first_result.status.success(),
            "{}",
            String::from_utf8_lossy(&first_result.stdout)
        );
        let evidence: serde_json::Value =
            serde_json::from_slice(&first_result.stdout).expect("evidence");
        assert_eq!(evidence["contourCount"], 2);
        assert_eq!(evidence["knotCount"], 8);
        let first = fs::read(root.path().join(format!("{label}-first.psd"))).expect("first PSD");
        let records = parse_path_records(&parse_psd(&first).resources[&2000].1);
        assert_eq!(records[0].0, 6);
        assert_eq!(records.len(), 1 + 2 * (1 + 4));
        for start in [1usize, 6] {
            assert_eq!(records[start].0, 0);
            assert_eq!(
                u16::from_be_bytes([records[start].1[2], records[start].1[3]]),
                4
            );
            for knot in &records[start + 1..start + 5] {
                assert_eq!(knot.0, 2);
            }
        }

        let second_request = request(
            root.path(),
            &format!("{label}.png"),
            &format!("{label}-second.psd"),
            128,
            8192,
        );
        let second_result = run(&second_request);
        assert!(second_result.status.success());
        let second = fs::read(root.path().join(format!("{label}-second.psd"))).expect("second PSD");
        assert_eq!(
            first, second,
            "diagonal path encoding must be deterministic"
        );
    }
}

#[test]
fn non_square_offset_contour_decodes_vertical_first_8_24_coordinates() {
    let root = TempDir::new();
    // A 3x3 foreground rectangle offset one pixel from the left edge in a
    // 5x3 document.  Its top/left and bottom/right boundaries exercise both
    // normalized zero and one values while x and y use different extents.
    let source = rgba(
        5,
        3,
        &[
            0, 255, 255, 255, 0, 0, 255, 255, 255, 0, 0, 255, 255, 255, 0,
        ],
    );
    write_png(&root.path().join("offset.png"), &source);
    let req = request(root.path(), "offset.png", "offset.psd", 128, 8192);
    let result = run(&req);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stdout)
    );
    let parsed = parse_psd(&fs::read(root.path().join("offset.psd")).expect("PSD"));
    let records = parse_path_records(&parsed.resources[&2000].1);
    assert_eq!(records[0].0, 6);
    assert_eq!(records[1].0, 0);
    assert_eq!(u16::from_be_bytes([records[1].1[2], records[1].1[3]]), 4);
    let expected = [
        [[0, 0x0033_3333]; 3],
        [[0, 0x00cc_cccd]; 3],
        [[0x0100_0000, 0x00cc_cccd]; 3],
        [[0x0100_0000, 0x0033_3333]; 3],
    ];
    let actual: Vec<_> = records[2..6]
        .iter()
        .map(|record| knot_points(&record.1))
        .collect();
    assert_eq!(actual, expected);
}
