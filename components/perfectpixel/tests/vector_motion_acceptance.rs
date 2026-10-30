use perfectpixel::{
    assess_motion_structure, MotionAssessmentNotEvaluatedReason, MotionAssessmentRequest,
    MotionAssessmentStatus, MotionCompiler, MotionRequest, SvgContract,
    MOTION_ASSESSMENT_MAX_GEOMETRY_BYTES, MOTION_ASSESSMENT_MAX_PATHS,
};

const SVG: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16"><path id="pp-path-0001" fill="#112233" d="M0 0 L16 0 L16 16 Z"/></svg>"##;

fn caller_request() -> MotionAssessmentRequest {
    MotionAssessmentRequest { requested: true }
}

fn caller_authored_request(scene_svg: &str, path_id: &str) -> MotionRequest {
    MotionRequest {
        schema: "perfectpixel.motion/1".to_string(),
        name: "motion".to_string(),
        source_svg: "scene.svg".to_string(),
        source_svg_sha256: MotionCompiler::scene_sha256(scene_svg),
        fps: 30,
        duration_ms: 100,
        looped: false,
        authored_paths: Vec::new(),
        parts: vec![perfectpixel::MotionPart {
            id: "part".to_string(),
            path_ids: vec![path_id.to_string()],
            anchor: [8.0, 8.0],
        }],
        tracks: vec![perfectpixel::MotionTrack {
            part: "part".to_string(),
            interpolation: perfectpixel::MotionInterpolation::Linear,
            keyframes: vec![
                perfectpixel::MotionKeyframe {
                    at_ms: 0,
                    translate: [0.0, 0.0],
                    rotate_deg: 0.0,
                    scale: [1.0, 1.0],
                    opacity: 1.0,
                },
                perfectpixel::MotionKeyframe {
                    at_ms: 100,
                    translate: [0.0, 0.0],
                    rotate_deg: 0.0,
                    scale: [1.0, 1.0],
                    opacity: 1.0,
                },
            ],
        }],
        markers: Vec::new(),
    }
}

#[test]
fn caller_supplied_assessment_cannot_grant_promotion_or_output() {
    let assessment = assess_motion_structure(&caller_request());
    assert_eq!(assessment.status(), MotionAssessmentStatus::NotEvaluated);
    assert_eq!(
        assessment.not_evaluated_reason(),
        Some(MotionAssessmentNotEvaluatedReason::PrerequisiteUnavailable)
    );
    assert!(assessment.evidence().binding.is_none());
    assert!(!serde_json::to_value(&assessment)
        .expect("assessment serializes evidence")
        .as_object()
        .expect("assessment is an object")
        .contains_key("approvedMotionOutputBytes"));
}

#[test]
fn unrequested_assessment_does_not_inspect_bytes() {
    let assessment = assess_motion_structure(&MotionAssessmentRequest::default());
    assert_eq!(assessment.status(), MotionAssessmentStatus::NotEvaluated);
    assert_eq!(
        assessment.not_evaluated_reason(),
        Some(MotionAssessmentNotEvaluatedReason::Unrequested)
    );
}

#[test]
fn scaffold_rejects_duplicate_ids_and_active_markup() {
    let duplicate = r#"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16"><path id="pp-path-0001" d="M0 0 L1 1"/><path id="pp-path-0001" d="M1 1 L2 2"/></svg>"#;
    assert!(MotionCompiler::scaffold(duplicate).is_err());
    let active = r#"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16"><script>alert(1)</script><path id="pp-path-0001" d="M0 0 L1 1"/></svg>"#;
    assert!(MotionCompiler::scaffold(active).is_err());
}

#[test]
fn motion_build_rejects_transform_bypass_with_scaffold_direction() {
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16"><path id="path" transform="translate(1 2)" fill="#112233" d="M0 0L1 1"/></svg>"##;
    let request = caller_authored_request(svg, "path");
    let error = MotionCompiler::build(svg, &request)
        .unwrap_err()
        .to_string();
    assert!(error.contains("rerun motion-scaffold"), "{error}");
}

#[test]
fn digit_initial_motion_ids_are_accepted() {
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16"><path id="1-path" fill="#112233" d="M0 0L1 1"/></svg>"##;
    let scaffold = MotionCompiler::scaffold(svg).expect("digit-initial path scaffolds");
    let mut request = caller_authored_request(&scaffold.scene_svg, "1-path");
    request.name = "2-motion".to_string();
    request.parts[0].id = "3-part".to_string();
    request.tracks[0].part = "3-part".to_string();
    MotionCompiler::build(&scaffold.scene_svg, &request)
        .expect("digit-initial motion names and part IDs build");
}

#[test]
fn scaffold_enforces_aggregate_path_data_byte_cap() {
    let mut data = "M0 0L1 1".to_string();
    data.push_str(&" ".repeat(MOTION_ASSESSMENT_MAX_GEOMETRY_BYTES + 1 - data.len()));
    let svg = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16"><path fill="#112233" d="{data}"/></svg>"##
    );
    let error = MotionCompiler::scaffold(&svg).unwrap_err().to_string();
    assert!(
        error.contains(&MOTION_ASSESSMENT_MAX_GEOMETRY_BYTES.to_string()),
        "{error}"
    );
}

#[test]
fn paired_paths_are_wrapped_as_complete_elements() {
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16"><path id="pp-path-0001" fill="#112233" d="M0 0 L16 0 L16 16 Z"></path ></svg>"##;
    let scaffold = MotionCompiler::scaffold(svg).expect("paired SVG path scaffolds");
    let request = caller_authored_request(&scaffold.scene_svg, "pp-path-0001");
    let output =
        MotionCompiler::build(&scaffold.scene_svg, &request).expect("paired SVG path builds");
    assert!(output
        .animated_svg
        .contains("<g class=\"pp-motion-part\"><path"));
    assert!(output.animated_svg.contains("</path ></g>"));
}
#[test]
fn motion_build_supports_multi_path_parts_inside_static_groups() {
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16"><g fill="#112233"><path id="pp-path-0001" d="M0 0 L8 0 L8 8 Z"/><g><path id="pp-path-0002" d="M8 8 L16 8 L16 16 Z"/></g></g></svg>"##;
    let scaffold = MotionCompiler::scaffold(svg).expect("nested static groups scaffold");
    let mut request = caller_authored_request(&scaffold.scene_svg, "pp-path-0001");
    request.parts[0].path_ids.push("pp-path-0002".to_string());
    let output = MotionCompiler::build(&scaffold.scene_svg, &request)
        .expect("multi-path part builds inside nested static groups");
    assert_eq!(
        output
            .animated_svg
            .matches("class=\"pp-motion-part\"")
            .count(),
        2
    );
    assert!(output.animated_svg.contains("<g fill=\"#112233\">"));
    assert!(output
        .animated_svg
        .contains("<g><g class=\"pp-motion-part\">"));
}

#[test]
fn authored_paths_use_parsed_prefixed_root_closing_tag() {
    let svg = r##"<svg:svg xmlns:svg="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16"><svg:path id="pp-path-0001" fill="#112233" d="M0 0 L16 0 L16 16 Z"/></svg:svg><!-- </svg> -->"##;
    let scaffold = MotionCompiler::scaffold(svg).expect("prefixed SVG root scaffolds");
    let mut request = caller_authored_request(&scaffold.scene_svg, "pp-path-0001");
    request
        .authored_paths
        .push(perfectpixel::MotionAuthoredPath {
            id: "authored".to_string(),
            d: "M1 1 L2 1 L2 2 Z".to_string(),
            fill: "#112233".to_string(),
            fill_opacity: 1.0,
        });
    let output = MotionCompiler::build(&scaffold.scene_svg, &request)
        .expect("authored path inserts before parsed root close");
    let authored = output.animated_svg.find("id=\"authored\"").unwrap();
    let root_close = output.animated_svg.find("</svg:svg>").unwrap();
    assert!(authored < root_close);
    assert!(output.animated_svg.ends_with("<!-- </svg> -->"));
}

#[test]
fn scaffold_rejects_path_counts_above_related_layer_cap() {
    let paths = (0..513)
        .map(|index| format!(r##"<path fill="#112233" d="M0 0 L1 0 L1 1 Z" id="p-{index}"/>"##))
        .collect::<String>();
    let svg = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16">{paths}</svg>"##
    );
    assert!(MotionCompiler::scaffold(&svg).is_err());
}
#[test]
fn scaffold_rejects_over_cap_paths_before_missing_id_annotation() {
    let paths = (0..513)
        .map(|_| r##"<path fill="#112233" d="M0 0 L1 0 L1 1 Z"/>"##)
        .collect::<String>();
    let svg = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16">{paths}</svg>"##
    );
    assert_eq!(MOTION_ASSESSMENT_MAX_PATHS, 512);
    assert!(MotionCompiler::scaffold(&svg).is_err());
}

#[test]
fn scaffold_rejects_path_ids_above_the_individual_byte_limit() {
    let id = format!("p{}", "a".repeat(256));
    let svg = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16"><path id="{id}" fill="#112233" d="M0 0 L1 0 L1 1 Z"/></svg>"##
    );
    assert!(MotionCompiler::scaffold(&svg).is_err());
}

#[test]
fn scaffold_bounds_aggregate_related_path_id_bytes() {
    let paths = (0..512)
        .map(|index| {
            let id = format!("p-{index:04}-{}", "a".repeat(249));
            format!(r##"<path id="{id}" fill="#112233" d="M0 0 L1 0 L1 1 Z"/>"##)
        })
        .collect::<String>();
    let svg = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16">{paths}</svg>"##
    );
    assert!(MotionCompiler::scaffold(&svg).is_err());
}

#[test]
fn scaffold_generated_ids_skip_later_authored_generated_names() {
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16"><path fill="#112233" d="M0 0 L1 0 L1 1 Z"/><path id="pp-path-0001" fill="#112233" d="M2 2 L3 2 L3 3 Z"/></svg>"##;
    let scaffold = MotionCompiler::scaffold(svg).expect("generated IDs avoid authored collisions");
    assert!(scaffold.scene_svg.contains("id=\"pp-path-0002\""));
    assert_eq!(scaffold.layers.layers[1].id, "pp-path-0001");
}
#[test]
fn close_path_starts_a_new_lottie_subpath_at_its_svg_start() {
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16"><path id="pp-path-0001" fill="#112233" d="M0 0 L10 0 Z l5 0 L5 5"/></svg>"##;
    let scaffold = MotionCompiler::scaffold(svg).expect("scaffold close-path SVG");
    let request = caller_authored_request(&scaffold.scene_svg, "pp-path-0001");
    let output = MotionCompiler::build(&scaffold.scene_svg, &request).expect("export subpaths");
    assert_eq!(output.report.lottie_shape_count, 2);
}

#[test]
fn tiny_positive_scales_are_preserved_in_svg_output() {
    let scaffold =
        MotionCompiler::scaffold(std::str::from_utf8(SVG).expect("UTF-8 SVG")).expect("scaffold");
    let mut request = caller_authored_request(&scaffold.scene_svg, "pp-path-0001");
    for keyframe in &mut request.tracks[0].keyframes {
        keyframe.scale = [0.00001, 0.00001];
    }
    let output = MotionCompiler::build(&scaffold.scene_svg, &request).expect("build tiny scale");
    assert!(output.animated_svg.contains("scale(0.00001,0.00001)"));
}

#[test]
fn unicode_color_is_rejected_without_panicking() {
    let svg = "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"16\" height=\"16\"><path id=\"pp-path-0001\" fill=\"#éaéa\" d=\"M0 0 L16 0 L16 16 Z\"/></svg>";
    assert!(MotionCompiler::scaffold(svg).is_err());
}

#[test]
fn motion_request_decoding_rejects_unknown_fields() {
    let decoded = serde_json::from_str::<MotionRequest>(
        r#"{"schema":"perfectpixel.motion/1","name":"motion","sourceSvg":"scene.svg","sourceSvgSha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef","fps":30,"durationMs":100,"loop":false,"parts":[],"tracks":[],"unknown":true}"#,
    );
    assert!(decoded.is_err());
}

#[test]
fn scaffold_request_is_bound_to_exact_scene_bytes() {
    let scaffold = MotionCompiler::scaffold(std::str::from_utf8(SVG).unwrap()).expect("scaffold");
    let request = caller_authored_request(&scaffold.scene_svg, "pp-path-0001");
    MotionCompiler::build(&scaffold.scene_svg, &request)
        .expect("exact scaffold scene remains bound");

    let mut modified_scene = scaffold.scene_svg.clone();
    modified_scene.push('\n');
    assert!(MotionCompiler::build(&modified_scene, &request).is_err());

    let mut modified_request = request;
    let replacement = if modified_request.source_svg_sha256.starts_with('0') {
        "1"
    } else {
        "0"
    };
    modified_request
        .source_svg_sha256
        .replace_range(..1, replacement);
    assert!(MotionCompiler::build(&scaffold.scene_svg, &modified_request).is_err());
}

#[test]
fn generated_motion_validator_rejects_active_payloads() {
    let generated = r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16"><style>
.pp-motion-subject{transform-box:view-box;transform-origin:8px 8px;animation:pp-motion-subject 1200ms linear infinite;}
@keyframes pp-motion-subject{
0%{transform:translate(0px,0px) rotate(0deg) scale(1,1);opacity:1;}
}
</style><g class="pp-motion-subject"><path id="pp-path-0001" fill="#112233" d="M0 0 L16 0 L16 16 Z"/></g></svg>"##;
    assert!(SvgContract::validate_generated_motion(generated).is_ok());

    let active = generated.replace(
        "</style>",
        "@import url(https://example.invalid/payload.css);</style>",
    );
    assert!(SvgContract::validate_generated_motion(&active).is_err());
}

#[test]
fn motion_request_aggregate_item_boundary_is_enforced_before_build() {
    let scaffold = MotionCompiler::scaffold(std::str::from_utf8(SVG).unwrap()).expect("scaffold");
    let mut request = caller_authored_request(&scaffold.scene_svg, "pp-path-0001");
    request.markers = (0..4_096)
        .map(|index| perfectpixel::MotionMarker {
            name: format!("marker-{index}"),
            from_ms: 0,
            to_ms: 1,
        })
        .collect();
    let exact = MotionCompiler::build(&scaffold.scene_svg, &request)
        .expect("aggregate item limit remains inclusive");
    assert_eq!(exact.report.marker_count, 4_096);

    request.markers.push(perfectpixel::MotionMarker {
        name: "marker-over-limit".to_string(),
        from_ms: 0,
        to_ms: 1,
    });
    assert!(MotionCompiler::build(&scaffold.scene_svg, &request).is_err());
}
