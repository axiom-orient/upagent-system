use perfectpixel::{
    compose_bundle_with_packing, content_bbox, FrameRect, PackingRequest, Raster, StateFrames,
};

fn frame(width: u32, height: u32, alpha_points: &[(u32, u32)]) -> Raster {
    let mut pixels = vec![0u8; (width * height * 4) as usize];
    for &(x, y) in alpha_points {
        let i = ((y * width + x) * 4) as usize;
        pixels[i] = 255;
        pixels[i + 1] = 255;
        pixels[i + 2] = 255;
        pixels[i + 3] = 255;
    }
    Raster::new(width, height, pixels).unwrap()
}

#[test]
fn content_bbox_matches_alpha_contract() {
    let image = frame(4, 4, &[(1, 2), (2, 3)]);
    assert_eq!(
        content_bbox(&image),
        FrameRect {
            x: 1,
            y: 2,
            w: 2,
            h: 2
        }
    );
}

#[test]
fn content_bbox_keeps_thresholded_inspection_semantics() {
    let image = Raster::new(2, 1, vec![1, 2, 3, 10, 4, 5, 6, 11]).unwrap();

    assert_eq!(
        content_bbox(&image),
        FrameRect {
            x: 1,
            y: 0,
            w: 1,
            h: 1,
        }
    );
}

#[test]
fn atlas_manifest_matches_perfectpixel_v3_shape() {
    let idle0 = frame(4, 4, &[(1, 3)]);
    let idle1 = frame(4, 4, &[(2, 2)]);
    let attack0 = frame(4, 4, &[(0, 0), (3, 3)]);
    let plan = compose_bundle_with_packing(
        "hero",
        "sprite-sheet.png",
        vec![
            StateFrames {
                name: "idle".to_string(),
                fps: 8,
                looped: true,
                frames: vec![idle0, idle1],
            },
            StateFrames {
                name: "attack".to_string(),
                fps: 12,
                looped: false,
                frames: vec![attack0],
            },
        ],
        4,
        4,
        PackingRequest {
            max_width: 16,
            max_height: 16,
            padding: 1,
            trim: true,
            allow_rotation: false,
            multipack: true,
        },
    )
    .unwrap();

    assert_eq!(plan.manifest.app, "perfectpixel");
    assert_eq!(plan.manifest.generator, "perfectpixel/maxrects-atlas");
    assert_eq!(plan.manifest.schema, "perfectpixel.sprite/3");
    assert_eq!(plan.manifest.sheets.len(), 1);
    assert_eq!(plan.manifest.sheets[0].image, "sprite-sheet.png");
    assert_eq!(plan.manifest.animations["idle"].order, 0);
    assert_eq!(plan.manifest.animations["attack"].order, 1);
    assert_eq!(plan.manifest.animations["idle"].frames, 2);
    assert_eq!(plan.manifest.animations["attack"].duration_ms, 83);
    assert!(plan.sheets[0].aseprite_json.contains("\"frameTags\""));
    assert!(plan.sheets[0].aseprite_json.contains("\"repeat\""));
}

#[test]
fn manifest_json_uses_loop_key_not_looped() {
    let image = frame(2, 2, &[(0, 0)]);
    let plan = compose_bundle_with_packing(
        "hero",
        "sprite-sheet.png",
        vec![StateFrames {
            name: "idle".to_string(),
            fps: 8,
            looped: true,
            frames: vec![image],
        }],
        2,
        2,
        PackingRequest::default(),
    )
    .unwrap();
    let json = serde_json::to_string(&plan.manifest).unwrap();
    assert!(json.contains("\"loop\":true"));
    assert!(!json.contains("looped"));
}

#[test]
fn copy_from_rejects_coordinate_overflow() {
    let mut destination = frame(2, 2, &[]);
    let source = frame(1, 1, &[(0, 0)]);
    let result = destination.copy_from(&source, u32::MAX, 0);
    assert!(result.is_err());
}
