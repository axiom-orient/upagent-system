use perfectpixel::{compose_bundle_with_packing, FrameRect, PackingRequest, Raster, StateFrames};

#[test]
fn compose_bundle_uses_trimmed_maxrects_manifest_shape() {
    let frame_a = test_frame(4, 4, &[(1, 1), (2, 2)]);
    let frame_b = test_frame(4, 4, &[(0, 3)]);
    let plan = compose_bundle_with_packing(
        "hero",
        "sprite-sheet.png",
        vec![StateFrames {
            name: "idle".to_string(),
            fps: 8,
            looped: true,
            frames: vec![frame_a, frame_b],
        }],
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
    .expect("bundle plan");

    assert_eq!(plan.manifest.app, "perfectpixel");
    assert_eq!(plan.manifest.schema, "perfectpixel.sprite/3");
    assert_eq!(plan.manifest.packing.algorithm, "binpack2d/maxrects");
    assert_eq!(plan.sheets.len(), 1);
    assert_eq!(plan.manifest.sheets[0].image, "sprite-sheet.png");
    let idle = plan
        .manifest
        .animations
        .get("idle")
        .expect("idle animation");
    assert_eq!(idle.items.len(), 2);
    assert_eq!(idle.items[0].source_size.w, 4);
    assert_eq!(idle.items[0].source_size.h, 4);
    assert_eq!(
        idle.items[0].sprite_source_size,
        FrameRect {
            x: 1,
            y: 1,
            w: 2,
            h: 2
        }
    );
    assert_eq!(
        idle.items[1].sprite_source_size,
        FrameRect {
            x: 0,
            y: 3,
            w: 1,
            h: 1
        }
    );
    assert!(plan.sheets[0].aseprite_json.contains("\"frameTags\""));
}

#[test]
fn compose_bundle_creates_multiple_sheets_when_needed() {
    let plan = compose_bundle_with_packing(
        "hero",
        "sprite-sheet.png",
        vec![StateFrames {
            name: "idle".to_string(),
            fps: 8,
            looped: true,
            frames: vec![solid_frame(6, 6), solid_frame(6, 6), solid_frame(6, 6)],
        }],
        6,
        6,
        PackingRequest {
            max_width: 8,
            max_height: 8,
            padding: 1,
            trim: true,
            allow_rotation: false,
            multipack: true,
        },
    )
    .expect("multipack plan");

    assert_eq!(plan.sheets.len(), 3);
    assert_eq!(plan.manifest.sheets[0].image, "sprite-sheet-00.png");
    assert_eq!(plan.manifest.sheets[1].image, "sprite-sheet-01.png");
    assert_eq!(plan.manifest.sheets[2].image, "sprite-sheet-02.png");
}

#[test]
fn compose_bundle_keeps_manifest_rects_inside_sheets_without_overlap() {
    let plan = compose_bundle_with_packing(
        "hero",
        "sprite-sheet.png",
        vec![StateFrames {
            name: "idle".to_string(),
            fps: 8,
            looped: true,
            frames: vec![
                solid_frame(4, 3),
                solid_frame(2, 5),
                solid_frame(3, 3),
                solid_frame(1, 4),
            ],
        }],
        5,
        5,
        PackingRequest {
            max_width: 10,
            max_height: 10,
            padding: 1,
            trim: true,
            allow_rotation: false,
            multipack: true,
        },
    )
    .expect("bundle plan");

    let idle = &plan.manifest.animations["idle"];
    for sheet in &plan.manifest.sheets {
        let mut items = idle
            .items
            .iter()
            .filter(|item| item.sheet == sheet.index)
            .collect::<Vec<_>>();
        items.sort_by_key(|item| item.index);
        for item in &items {
            assert!(item.rect.x + item.rect.w <= sheet.width);
            assert!(item.rect.y + item.rect.h <= sheet.height);
        }
        for (left_index, left) in items.iter().enumerate() {
            for right in items.iter().skip(left_index + 1) {
                assert!(!frame_rects_overlap(left.rect, right.rect));
            }
        }
    }
}

#[test]
fn compose_bundle_rotation_disabled_never_rotates() {
    let plan = compose_bundle_with_packing(
        "hero",
        "sprite-sheet.png",
        vec![StateFrames {
            name: "idle".to_string(),
            fps: 8,
            looped: true,
            frames: vec![solid_frame(3, 1)],
        }],
        3,
        1,
        PackingRequest {
            max_width: 3,
            max_height: 1,
            padding: 0,
            trim: true,
            allow_rotation: false,
            multipack: true,
        },
    )
    .expect("bundle plan");

    assert!(!plan.manifest.animations["idle"].items[0].rotated);
}

#[test]
fn compose_bundle_rejects_when_multipack_is_disabled() {
    let result = compose_bundle_with_packing(
        "hero",
        "sprite-sheet.png",
        vec![StateFrames {
            name: "idle".to_string(),
            fps: 8,
            looped: true,
            frames: vec![solid_frame(6, 6), solid_frame(6, 6)],
        }],
        6,
        6,
        PackingRequest {
            max_width: 8,
            max_height: 8,
            padding: 1,
            trim: true,
            allow_rotation: false,
            multipack: false,
        },
    );
    assert!(result.is_err());
}

#[test]
fn compose_bundle_rejects_explicit_zero_fps() {
    let error = compose_bundle_with_packing(
        "hero",
        "sprite-sheet.png",
        vec![StateFrames {
            name: "idle".to_string(),
            fps: 0,
            looped: true,
            frames: vec![solid_frame(1, 1)],
        }],
        1,
        1,
        PackingRequest::default(),
    )
    .unwrap_err();

    assert_eq!(
        error.to_string(),
        "invalid request: state 'idle' fps must be from 1 through 1000"
    );
}

#[test]
fn compose_bundle_rejects_fps_above_one_thousand() {
    let error = compose_bundle_with_packing(
        "hero",
        "sprite-sheet.png",
        vec![StateFrames {
            name: "idle".to_string(),
            fps: 1001,
            looped: true,
            frames: vec![solid_frame(1, 1)],
        }],
        1,
        1,
        PackingRequest::default(),
    )
    .unwrap_err();

    assert_eq!(
        error.to_string(),
        "invalid request: state 'idle' fps must be from 1 through 1000"
    );
}

#[test]
fn compose_bundle_accepts_one_thousand_fps_with_one_ms_durations() {
    let plan = compose_bundle_with_packing(
        "hero",
        "sprite-sheet.png",
        vec![StateFrames {
            name: "idle".to_string(),
            fps: 1000,
            looped: true,
            frames: vec![solid_frame(1, 1)],
        }],
        1,
        1,
        PackingRequest::default(),
    )
    .expect("1000 fps bundle plan");

    assert_eq!(plan.manifest.animations["idle"].duration_ms, 1);
    let aseprite: serde_json::Value =
        serde_json::from_str(&plan.sheets[0].aseprite_json).expect("Aseprite JSON");
    assert_eq!(aseprite["frames"][0]["duration"], 1);
}

#[test]
fn compose_bundle_places_trimmed_pixels_at_manifest_rect() {
    let frame = colored_frame(4, 4, &[(2, 1, [10, 20, 30, 255])]);
    let plan = compose_bundle_with_packing(
        "hero",
        "sprite-sheet.png",
        vec![StateFrames {
            name: "idle".to_string(),
            fps: 8,
            looped: true,
            frames: vec![frame],
        }],
        4,
        4,
        PackingRequest {
            max_width: 8,
            max_height: 8,
            padding: 1,
            trim: true,
            allow_rotation: false,
            multipack: true,
        },
    )
    .expect("bundle plan");

    let item = &plan.manifest.animations["idle"].items[0];
    assert_eq!(
        item.sprite_source_size,
        FrameRect {
            x: 2,
            y: 1,
            w: 1,
            h: 1
        }
    );
    assert_eq!(
        pixel_at(&plan.sheets[0].image, item.rect.x, item.rect.y),
        [10, 20, 30, 255]
    );
}

#[test]
fn compose_bundle_trim_preserves_soft_alpha_edge_pixels() {
    let frame = colored_frame(
        5,
        3,
        &[
            (0, 1, [10, 20, 30, 1]),
            (2, 1, [40, 50, 60, 255]),
            (4, 1, [70, 80, 90, 10]),
        ],
    );
    let plan = compose_bundle_with_packing(
        "hero",
        "sprite-sheet.png",
        vec![StateFrames {
            name: "idle".to_string(),
            fps: 8,
            looped: true,
            frames: vec![frame.clone()],
        }],
        5,
        3,
        PackingRequest {
            max_width: 8,
            max_height: 8,
            padding: 1,
            trim: true,
            allow_rotation: false,
            multipack: true,
        },
    )
    .expect("bundle plan");

    let item = &plan.manifest.animations["idle"].items[0];
    assert_eq!(
        item.sprite_source_size,
        FrameRect {
            x: 0,
            y: 1,
            w: 5,
            h: 1,
        }
    );
    let mut reconstructed = Raster::blank(5, 3).expect("blank reconstruction");
    reconstructed
        .copy_region(
            &plan.sheets[item.sheet as usize].image,
            item.rect,
            item.sprite_source_size.x,
            item.sprite_source_size.y,
        )
        .expect("reconstruct trimmed frame");
    assert_eq!(reconstructed, frame);
}

#[test]
fn compose_bundle_keeps_transparent_frame_placeholder_manifest_entry() {
    let frame = Raster::blank(4, 3).expect("blank frame");
    let plan = compose_bundle_with_packing(
        "hero",
        "sprite-sheet.png",
        vec![StateFrames {
            name: "idle".to_string(),
            fps: 8,
            looped: true,
            frames: vec![frame],
        }],
        4,
        3,
        PackingRequest {
            max_width: 4,
            max_height: 4,
            padding: 0,
            trim: true,
            allow_rotation: false,
            multipack: true,
        },
    )
    .expect("bundle plan");

    let item = &plan.manifest.animations["idle"].items[0];
    assert_eq!(item.source_size.w, 4);
    assert_eq!(item.source_size.h, 3);
    assert_eq!(item.sprite_source_size, FrameRect::default());
    assert_eq!(item.rect.w, 1);
    assert_eq!(item.rect.h, 1);
    assert_eq!(
        pixel_at(&plan.sheets[0].image, item.rect.x, item.rect.y),
        [0; 4]
    );
}

#[test]
fn compose_bundle_can_rotate_when_explicitly_enabled() {
    let frame = colored_frame(
        3,
        1,
        &[
            (0, 0, [255, 0, 0, 255]),
            (1, 0, [0, 255, 0, 255]),
            (2, 0, [0, 0, 255, 255]),
        ],
    );
    let plan = compose_bundle_with_packing(
        "hero",
        "sprite-sheet.png",
        vec![StateFrames {
            name: "idle".to_string(),
            fps: 8,
            looped: true,
            frames: vec![frame],
        }],
        3,
        1,
        PackingRequest {
            max_width: 1,
            max_height: 3,
            padding: 0,
            trim: true,
            allow_rotation: true,
            multipack: true,
        },
    )
    .expect("rotated bundle plan");

    let item = &plan.manifest.animations["idle"].items[0];
    assert!(item.rotated);
    assert_eq!(
        item.rect,
        FrameRect {
            x: 0,
            y: 0,
            w: 1,
            h: 3
        }
    );
    assert_eq!(pixel_at(&plan.sheets[0].image, 0, 0), [255, 0, 0, 255]);
    assert_eq!(pixel_at(&plan.sheets[0].image, 0, 1), [0, 255, 0, 255]);
    assert_eq!(pixel_at(&plan.sheets[0].image, 0, 2), [0, 0, 255, 255]);
}

#[test]
fn compose_bundle_rejects_unsafe_state_name() {
    for state_name in ["../idle", " idle", "idle "] {
        let result = compose_bundle_with_packing(
            "hero",
            "sprite-sheet.png",
            vec![StateFrames {
                name: state_name.to_string(),
                fps: 8,
                looped: true,
                frames: vec![test_frame(4, 4, &[(1, 1)])],
            }],
            4,
            4,
            PackingRequest::default(),
        );
        assert!(
            result.is_err(),
            "state name {state_name:?} must be rejected"
        );
    }
}

#[test]
fn compose_bundle_rejects_unsafe_sheet_image_names() {
    for sheet_image in [
        "../sheet.png",
        "..",
        ".",
        "dir/sheet.png",
        "dir\\sheet.png",
        "sheet.jpg",
        "manifest.json",
        "manifest.png",
        "manifest-00.png",
        "sprite-sheet.json",
        "frames",
        " sheet.png",
        "sheet.png ",
    ] {
        let result = compose_bundle_with_packing(
            "hero",
            sheet_image,
            vec![StateFrames {
                name: "idle".to_string(),
                fps: 8,
                looped: true,
                frames: vec![test_frame(4, 4, &[(1, 1)])],
            }],
            4,
            4,
            PackingRequest::default(),
        );
        assert!(
            result.is_err(),
            "sheet image {sheet_image:?} must be rejected"
        );
    }
}

#[test]
fn compose_bundle_rejects_duplicate_state_names() {
    let result = compose_bundle_with_packing(
        "hero",
        "sprite-sheet.png",
        vec![
            StateFrames {
                name: "idle".to_string(),
                fps: 8,
                looped: true,
                frames: vec![test_frame(4, 4, &[(1, 1)])],
            },
            StateFrames {
                name: "idle".to_string(),
                fps: 8,
                looped: true,
                frames: vec![test_frame(4, 4, &[(2, 2)])],
            },
        ],
        4,
        4,
        PackingRequest::default(),
    );
    assert!(result.is_err());
}

#[test]
fn compose_bundle_rejects_oversized_sheet_before_allocating() {
    let result = compose_bundle_with_packing(
        "hero",
        "sprite-sheet.png",
        vec![StateFrames {
            name: "idle".to_string(),
            fps: 8,
            looped: true,
            frames: vec![test_frame(1, 1, &[(0, 0)])],
        }],
        1,
        1,
        PackingRequest {
            max_width: 8193,
            max_height: 1,
            padding: 0,
            trim: true,
            allow_rotation: false,
            multipack: true,
        },
    );
    assert!(result.is_err());
}

fn test_frame(width: u32, height: u32, points: &[(u32, u32)]) -> Raster {
    let mut pixels = vec![0; (width * height * 4) as usize];
    for (x, y) in points {
        let i = ((*y * width + *x) * 4) as usize;
        pixels[i] = 255;
        pixels[i + 1] = 0;
        pixels[i + 2] = 0;
        pixels[i + 3] = 255;
    }
    Raster::new(width, height, pixels).expect("valid raster")
}

fn colored_frame(width: u32, height: u32, points: &[(u32, u32, [u8; 4])]) -> Raster {
    let mut pixels = vec![0; (width * height * 4) as usize];
    for (x, y, rgba) in points {
        let i = ((*y * width + *x) * 4) as usize;
        pixels[i..i + 4].copy_from_slice(rgba);
    }
    Raster::new(width, height, pixels).expect("valid raster")
}

fn frame_rects_overlap(left: FrameRect, right: FrameRect) -> bool {
    left.x < right.x + right.w
        && left.x + left.w > right.x
        && left.y < right.y + right.h
        && left.y + left.h > right.y
}

fn pixel_at(image: &Raster, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * image.width() + x) * 4) as usize;
    let pixels = image.pixels();
    [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
}

fn solid_frame(width: u32, height: u32) -> Raster {
    let mut pixels = vec![0; (width * height * 4) as usize];
    for rgba in pixels.as_chunks_mut::<4>().0 {
        rgba[0] = 255;
        rgba[1] = 255;
        rgba[2] = 255;
        rgba[3] = 255;
    }
    Raster::new(width, height, pixels).expect("valid raster")
}
