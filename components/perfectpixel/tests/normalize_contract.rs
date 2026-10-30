use perfectpixel::{
    normalize_sprite, validate_normalize_plan_contract, NormalizeChroma, NormalizeFit,
    NormalizeOutline, NormalizeQuality, NormalizeRequest, NormalizeStateImages,
    NormalizeStateRequest, NormalizeStateSource, PackingRequest, PpError, Raster,
};

fn empty_request(states: Vec<NormalizeStateRequest>) -> NormalizeRequest {
    NormalizeRequest {
        character: "hero".to_string(),
        sheet_image: "sprite-sheet.png".to_string(),
        cell_width: 16,
        cell_height: 16,
        safe_margin_x: 1,
        safe_margin_y: 1,
        packing: PackingRequest::default(),
        chroma: None,
        fit: NormalizeFit::default(),
        quality: NormalizeQuality::default(),
        states,
    }
}

fn state(name: &str, frames: Vec<&str>) -> NormalizeStateRequest {
    NormalizeStateRequest {
        name: name.to_string(),
        fps: 8,
        looped: true,
        frames: frames.into_iter().map(str::to_string).collect(),
        strip: None,
        frame_count: None,
    }
}

fn rect_frame(width: u32, height: u32, rect: (u32, u32, u32, u32), color: [u8; 4]) -> Raster {
    let mut pixels = vec![0u8; (width * height * 4) as usize];
    for y in rect.1..rect.1 + rect.3 {
        for x in rect.0..rect.0 + rect.2 {
            let index = ((y * width + x) * 4) as usize;
            pixels[index..index + 4].copy_from_slice(&color);
        }
    }
    Raster::new(width, height, pixels).unwrap()
}

fn colored_frame(width: u32, height: u32, points: &[(u32, u32, [u8; 4])]) -> Raster {
    let mut pixels = vec![0u8; (width * height * 4) as usize];
    for &(x, y, color) in points {
        let index = ((y * width + x) * 4) as usize;
        pixels[index..index + 4].copy_from_slice(&color);
    }
    Raster::new(width, height, pixels).unwrap()
}

#[test]
fn normalize_writes_bundle_compatible_request_and_fixed_cell_frames() {
    let request = empty_request(vec![state("idle", vec!["raw/a.png", "raw/b.png"])]);
    let source = NormalizeStateImages {
        name: "idle".to_string(),
        fps: 8,
        looped: true,
        source: NormalizeStateSource::Frames(vec![
            rect_frame(16, 16, (5, 5, 4, 8), [220, 20, 30, 255]),
            rect_frame(16, 16, (4, 3, 5, 10), [220, 20, 30, 255]),
        ]),
    };

    let plan = normalize_sprite(&request, vec![source]).unwrap();

    assert!(plan.report.ok, "errors: {:?}", plan.report.errors);
    assert_eq!(plan.states[0].frames.len(), 2);
    assert!(plan.states[0]
        .frames
        .iter()
        .all(|frame| frame.width() == 16 && frame.height() == 16));
    assert_eq!(
        plan.bundle_request.states[0].frames[0],
        "frames/idle/frame-00.png"
    );
    assert_eq!(
        plan.bundle_request.states[0].frames[1],
        "frames/idle/frame-01.png"
    );
    let bundle_request = serde_json::to_value(&plan.bundle_request).expect("bundle request JSON");
    assert_eq!(bundle_request["states"][0]["fps"], 8);
    assert_eq!(bundle_request["states"][0]["loop"], true);
    validate_normalize_plan_contract(&request, &plan).unwrap();

    let mut drifted_plan = plan;
    drifted_plan.bundle_request.states[0].name = "walk".to_string();
    assert!(validate_normalize_plan_contract(&request, &drifted_plan).is_err());
}

#[test]
fn normalize_chroma_unmix_preserves_soft_alpha_edge() {
    let mut request = empty_request(vec![state("idle", vec!["raw/a.png"])]);
    request.chroma = Some(NormalizeChroma {
        rgb: [255, 0, 255],
        threshold: 96.0,
        fringe_threshold: 180.0,
        fringe_delta: 18.0,
        unmix_reach: 4,
        spill_max_fraction: 0.005,
        adjacent_threshold: 150.0,
        adjacent_pixel_threshold: 120,
    });
    request.quality.max_registration_drift_px = 16.0;
    let mut pixels = vec![0u8; 4 * 4 * 4];
    for y in 0..4 {
        for x in 0..4 {
            let index = ((y * 4 + x) * 4) as usize;
            pixels[index..index + 4].copy_from_slice(&[255, 0, 255, 255]);
        }
    }
    let blend_index = (4 + 1) * 4;
    pixels[blend_index..blend_index + 4].copy_from_slice(&[190, 60, 170, 255]);
    let subject_index = (4 + 2) * 4;
    pixels[subject_index..subject_index + 4].copy_from_slice(&[80, 150, 70, 255]);
    let source = NormalizeStateImages {
        name: "idle".to_string(),
        fps: 8,
        looped: true,
        source: NormalizeStateSource::Frames(vec![Raster::new(4, 4, pixels).unwrap()]),
    };

    let plan = normalize_sprite(&request, vec![source]).unwrap();

    assert!(plan.report.ok, "errors: {:?}", plan.report.errors);
    let output = &plan.states[0].frames[0];
    assert!(output
        .pixels()
        .as_chunks::<4>()
        .0
        .iter()
        .any(|pixel| pixel[3] > 0 && pixel[3] < 255));
    assert_eq!(
        plan.report.states[0].frame_records[0].chroma_adjacent_pixels,
        0
    );
}

#[test]
fn normalize_crop_preserves_soft_pixel_outside_opaque_bbox() {
    let request = empty_request(vec![state("idle", vec!["raw/a.png"])]);
    let source = NormalizeStateImages {
        name: "idle".to_string(),
        fps: 8,
        looped: true,
        source: NormalizeStateSource::Frames(vec![colored_frame(
            6,
            3,
            &[(0, 0, [10, 20, 30, 10]), (5, 1, [40, 50, 60, 255])],
        )]),
    };

    let plan = normalize_sprite(&request, vec![source]).expect("normalize plan");
    let output = &plan.states[0].frames[0];
    let soft_index = output
        .pixels()
        .as_chunks::<4>()
        .0
        .iter()
        .position(|pixel| *pixel == [10, 20, 30, 10])
        .expect("soft pixel retained");
    let opaque_index = output
        .pixels()
        .as_chunks::<4>()
        .0
        .iter()
        .position(|pixel| *pixel == [40, 50, 60, 255])
        .expect("opaque pixel retained");
    assert_eq!(
        opaque_index % output.width() as usize,
        soft_index % output.width() as usize + 5
    );
}

#[test]
fn normalize_registration_keeps_frame_with_only_soft_alpha() {
    let request = empty_request(vec![state("idle", vec!["raw/a.png"])]);
    let source = NormalizeStateImages {
        name: "idle".to_string(),
        fps: 8,
        looped: true,
        source: NormalizeStateSource::Frames(vec![rect_frame(2, 1, (0, 0, 2, 1), [90, 80, 70, 1])]),
    };

    let plan = normalize_sprite(&request, vec![source]).expect("normalize plan");
    assert!(plan.states[0].frames[0]
        .pixels()
        .as_chunks::<4>()
        .0
        .iter()
        .any(|pixel| pixel[3] == 1));
}

#[test]
fn normalize_rejects_oversized_post_scale_placement_as_invalid_request() {
    let mut request = empty_request(vec![state("idle", vec!["raw/a.png", "raw/b.png"])]);
    request.safe_margin_x = 0;
    request.fit.pixel_perfect = true;
    request.fit.logical_height = Some(4);
    request.fit.outline = NormalizeOutline {
        enabled: false,
        strength: 0.0,
    };
    let first = rect_frame(4, 4, (0, 0, 4, 4), [255, 0, 0, 255]);
    let second = colored_frame(
        4,
        4,
        &[
            (0, 0, [0, 255, 0, 1]),
            (1, 0, [0, 255, 0, 1]),
            (2, 0, [0, 255, 0, 1]),
            (3, 0, [0, 255, 0, 1]),
            (0, 3, [0, 255, 0, 255]),
            (1, 3, [0, 255, 0, 255]),
            (2, 3, [0, 255, 0, 255]),
            (3, 3, [0, 255, 0, 255]),
        ],
    );
    let source = NormalizeStateImages {
        name: "idle".to_string(),
        fps: 8,
        looped: true,
        source: NormalizeStateSource::Frames(vec![first, second]),
    };

    let error = normalize_sprite(&request, vec![source]).expect_err("oversized placement");
    assert!(matches!(error, PpError::InvalidRequest(_)));
    let message = error.to_string();
    assert!(message.contains("frame 0 for state 'idle' is"), "{message}");
    assert!(message.contains("larger than cell 16x16"), "{message}");
}

#[test]
fn normalize_rejects_pixel_fields_without_pixel_perfect() {
    let mut request = empty_request(vec![state("idle", vec!["raw/a.png"])]);
    request.fit.logical_height = Some(8);
    request.fit.pitch_hint = Some(2);
    let source = NormalizeStateImages {
        name: "idle".to_string(),
        fps: 8,
        looped: true,
        source: NormalizeStateSource::Frames(vec![rect_frame(8, 8, (2, 2, 3, 3), [1, 2, 3, 255])]),
    };

    let error = normalize_sprite(&request, vec![source]).unwrap_err();
    let message = error.to_string();
    assert!(
        message.contains("fit.logicalHeight requires fit.pixelPerfect=true"),
        "{message}"
    );
    assert!(
        message.contains("fit.pitchHint requires fit.pixelPerfect=true"),
        "{message}"
    );
}

#[test]
fn normalize_rejects_pitch_hint_below_two() {
    let mut request = empty_request(vec![state("idle", vec!["raw/a.png"])]);
    request.safe_margin_x = 0;
    request.fit.pixel_perfect = true;
    request.fit.pitch_hint = Some(1);
    let source = NormalizeStateImages {
        name: "idle".to_string(),
        fps: 8,
        looped: true,
        source: NormalizeStateSource::Frames(vec![rect_frame(8, 8, (2, 2, 3, 3), [1, 2, 3, 255])]),
    };

    let error = normalize_sprite(&request, vec![source]).unwrap_err();
    assert!(error
        .to_string()
        .contains("fit.pitchHint must be at least 2"));
}

#[test]
fn normalize_rejects_safe_margin_x_in_pixel_perfect_mode() {
    let mut request = empty_request(vec![state("idle", vec!["raw/a.png"])]);
    request.fit.pixel_perfect = true;
    let source = NormalizeStateImages {
        name: "idle".to_string(),
        fps: 8,
        looped: true,
        source: NormalizeStateSource::Frames(vec![rect_frame(8, 8, (2, 2, 3, 3), [1, 2, 3, 255])]),
    };

    let error = normalize_sprite(&request, vec![source]).unwrap_err();
    assert!(error
        .to_string()
        .contains("safeMarginX is unsupported when fit.pixelPerfect=true"));
}

#[test]
fn normalize_rejects_parent_directory_frame_path() {
    let request = empty_request(vec![state("idle", vec!["../x.png"])]);
    let source = NormalizeStateImages {
        name: "idle".to_string(),
        fps: 8,
        looped: true,
        source: NormalizeStateSource::Frames(vec![rect_frame(8, 8, (2, 2, 3, 3), [1, 2, 3, 255])]),
    };

    let error = normalize_sprite(&request, vec![source]).unwrap_err();
    assert!(error
        .to_string()
        .contains("frame path '../x.png' must not contain '..'"));
}

#[test]
fn normalize_rejects_unsafe_state_as_invalid_request() {
    let request = empty_request(vec![state("../idle", vec!["raw/a.png"])]);
    let source = NormalizeStateImages {
        name: "../idle".to_string(),
        fps: 8,
        looped: true,
        source: NormalizeStateSource::Frames(vec![rect_frame(8, 8, (2, 2, 3, 3), [1, 2, 3, 255])]),
    };

    let error = normalize_sprite(&request, vec![source]).unwrap_err();

    assert!(error.to_string().contains("not safe for output paths"));
}

#[test]
fn normalize_rejects_explicit_zero_fps() {
    let mut request = empty_request(vec![state("idle", vec!["raw/a.png"])]);
    request.states[0].fps = 0;
    let source = NormalizeStateImages {
        name: "idle".to_string(),
        fps: 0,
        looped: true,
        source: NormalizeStateSource::Frames(vec![rect_frame(8, 8, (2, 2, 3, 3), [1, 2, 3, 255])]),
    };

    let error = normalize_sprite(&request, vec![source]).unwrap_err();

    assert!(error
        .to_string()
        .contains("state 'idle' fps must be from 1 through 1000"));
}

#[test]
fn normalize_rejects_fps_above_one_thousand() {
    let mut request = empty_request(vec![state("idle", vec!["raw/a.png"])]);
    request.states[0].fps = 1001;
    let source = NormalizeStateImages {
        name: "idle".to_string(),
        fps: 1001,
        looped: true,
        source: NormalizeStateSource::Frames(vec![rect_frame(8, 8, (2, 2, 3, 3), [1, 2, 3, 255])]),
    };

    let error = normalize_sprite(&request, vec![source]).unwrap_err();

    assert!(error
        .to_string()
        .contains("state 'idle' fps must be from 1 through 1000"));
}

#[test]
fn normalize_rejects_unimplemented_vertical_alignment() {
    let mut request = empty_request(vec![state("idle", vec!["raw/a.png"])]);
    request.fit.align_y = "center".to_string();
    let source = NormalizeStateImages {
        name: "idle".to_string(),
        fps: 8,
        looped: true,
        source: NormalizeStateSource::Frames(vec![rect_frame(8, 8, (2, 2, 3, 3), [1, 2, 3, 255])]),
    };

    let error = normalize_sprite(&request, vec![source]).unwrap_err();

    assert!(error.to_string().contains("fit.alignY must be bottom"));
}

#[test]
fn normalize_rejects_non_finite_numeric_policy_values() {
    let mut request = empty_request(vec![state("idle", vec!["raw/a.png"])]);
    request.fit.outline.strength = f64::NAN;
    request.quality.max_registration_drift_px = f64::INFINITY;
    request.chroma = Some(NormalizeChroma {
        rgb: [255, 0, 255],
        threshold: f64::NAN,
        fringe_threshold: 180.0,
        fringe_delta: 18.0,
        unmix_reach: 4,
        spill_max_fraction: 0.005,
        adjacent_threshold: 150.0,
        adjacent_pixel_threshold: 120,
    });
    let source = NormalizeStateImages {
        name: "idle".to_string(),
        fps: 8,
        looped: true,
        source: NormalizeStateSource::Frames(vec![rect_frame(8, 8, (2, 2, 3, 3), [1, 2, 3, 255])]),
    };

    let error = normalize_sprite(&request, vec![source]).expect_err("non-finite policy");
    let message = error.to_string();
    assert!(message.contains("fit.outline.strength"), "{message}");
    assert!(
        message.contains("chroma thresholds must be finite"),
        "{message}"
    );
    assert!(
        message.contains("quality.maxRegistrationDriftPx must be finite"),
        "{message}"
    );
}

#[test]
fn normalize_rejects_unbounded_palette_and_chroma_work() {
    let mut request = empty_request(vec![state("idle", vec!["raw/a.png"])]);
    request.fit.palette_size = 257;
    request.chroma = Some(NormalizeChroma {
        rgb: [255, 0, 255],
        threshold: 96.0,
        fringe_threshold: 180.0,
        fringe_delta: 18.0,
        unmix_reach: 33,
        spill_max_fraction: 1.01,
        adjacent_threshold: 150.0,
        adjacent_pixel_threshold: 120,
    });
    let source = NormalizeStateImages {
        name: "idle".to_string(),
        fps: 8,
        looped: true,
        source: NormalizeStateSource::Frames(vec![rect_frame(8, 8, (2, 2, 3, 3), [1, 2, 3, 255])]),
    };

    let error = normalize_sprite(&request, vec![source]).expect_err("unbounded policy");
    let message = error.to_string();
    assert!(
        message.contains("fit.paletteSize must be from 1 through 256"),
        "{message}"
    );
    assert!(
        message.contains("chroma.unmixReach must be at most 32"),
        "{message}"
    );
    assert!(
        message.contains("chroma.spillMaxFraction must be between 0 and 1"),
        "{message}"
    );
}
