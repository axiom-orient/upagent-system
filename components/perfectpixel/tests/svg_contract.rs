use perfectpixel::{MotionCompiler, SvgContract};

const SVG_NS: &str = "http://www.w3.org/2000/svg";

fn svg(body: &str) -> String {
    format!("<svg xmlns=\"{SVG_NS}\" width=\"8\" height=\"8\" viewBox=\"0 0 8 8\">{body}</svg>")
}

#[test]
fn svg_contract_accepts_path_svg() {
    let report = SvgContract::validate(&svg(r##"<path d="M0 0L1 1Z" fill="#fff"/>"##)).unwrap();
    assert_eq!(report.width, 8);
    assert_eq!(report.height, 8);
    assert_eq!(report.path_count, 1);
    assert_eq!(report.color_count, 1);
    assert_eq!(report.node_count, 1);
    assert_eq!(report.curve_segment_count, 0);
    assert_eq!(report.closed_path_count, 1);
    assert!(!report.contains_raster_payload);
}
#[test]
fn svg_contract_accepts_exact_canvas_and_coordinate_bounds() {
    let svg = format!(
        "<svg xmlns=\"{SVG_NS}\" width=\"8192\" height=\"8192\" viewBox=\"0 0 8192 8192\"><path fill=\"#000\" d=\"M0 0L1000000 1\"/></svg>"
    );
    assert!(SvgContract::validate(&svg).is_ok());
}

#[test]
fn svg_contract_requires_namespaced_rendering_tree() {
    for invalid in [
        r#"<svg width="8" height="8" viewBox="0 0 8 8"><path d="M0 0L1 1"/></svg>"#.to_string(),
        svg(""),
        svg("<g/>"),
    ] {
        assert!(SvgContract::validate(&invalid).is_err(), "{invalid}");
    }
}

#[test]
fn svg_contract_requires_positive_paired_bounded_canvas_and_view_box() {
    for invalid in [
        format!("<svg xmlns=\"{SVG_NS}\" height=\"8\" viewBox=\"0 0 8 8\"><path d=\"M0 0L1 1\"/></svg>"),
        format!("<svg xmlns=\"{SVG_NS}\" width=\"0\" height=\"8\" viewBox=\"0 0 8 8\"><path d=\"M0 0L1 1\"/></svg>"),
        format!("<svg xmlns=\"{SVG_NS}\" width=\"8193\" height=\"8\" viewBox=\"0 0 8 8\"><path d=\"M0 0L1 1\"/></svg>"),
        format!("<svg xmlns=\"{SVG_NS}\" width=\"8\" height=\"8\"><path d=\"M0 0L1 1\"/></svg>"),
        format!("<svg xmlns=\"{SVG_NS}\" width=\"8\" height=\"8\" viewBox=\"0 0 8 0\"><path d=\"M0 0L1 1\"/></svg>"),
        format!("<svg xmlns=\"{SVG_NS}\" width=\"8\" height=\"8\" viewBox=\"0,,0 8 8\"><path d=\"M0 0L1 1\"/></svg>"),
    ] {
        assert!(SvgContract::validate(&invalid).is_err(), "{invalid}");
    }
}

#[test]
fn svg_contract_rejects_unsupported_motion_render_semantics_and_nesting() {
    for invalid in [
        format!("<svg xmlns=\"{SVG_NS}\" width=\"8\" height=\"8\" viewBox=\"0 0 16 16\"><path d=\"M0 0L1 1\"/></svg>"),
        svg(r##"<g opacity="0.5"><path fill="#000" d="M0 0L1 1"/></g>"##),
        svg(r##"<path fill="#000" d="M0 0L1 1"><g/></path>"##),
        svg(r##"<path fill="#000" d="M0 0L1 1"><path fill="#000" d="M1 1L2 2"/></path>"##),
    ] {
        assert!(SvgContract::validate(&invalid).is_err(), "{invalid}");
    }
}
#[test]
fn svg_contract_rejects_raster_and_active_payloads() {
    for invalid in [
        svg(r#"<image href="data:image/png;base64,aa"/>"#),
        svg(r#"<filter><feImage href="texture.png"/></filter>"#),
        format!("<svg xmlns=\"{SVG_NS}\" xmlns:x=\"urn:foreign\" width=\"8\" height=\"8\" viewBox=\"0 0 8 8\"><x:image href=\"texture.png\"/></svg>"),
        format!("<?xml-stylesheet href=\"https://example.test/x.css\"?><svg xmlns=\"{SVG_NS}\" width=\"8\" height=\"8\" viewBox=\"0 0 8 8\"><path d=\"M0 0L1 1\"/></svg>"),
    ] {
        assert!(SvgContract::validate(&invalid).is_err(), "{invalid}");
    }
}

#[test]
fn svg_contract_canonicalizes_equivalent_hex_paints() {
    let output = MotionCompiler::scaffold(&svg(
        r##"<path d="M0 0L1 1" fill="#ABC"/><path d="M1 1L2 2" fill="#aabbcc"/>"##,
    ))
    .unwrap();
    assert_eq!(output.layers.layers[0].fill, "#aabbcc");
    assert_eq!(output.layers.layers[1].fill, "#aabbcc");
}

#[test]
fn svg_contract_rejects_non_rendering_and_malformed_geometry() {
    for invalid in [
        svg(r##"<path d="M0 0L1 1" fill="#000" opacity="0"/>"##),
        svg(r##"<path d="M0 0" fill="#000"/>"##),
        svg(r##"<path d="M0 0,,L1 1" fill="#000"/>"##),
        svg(r##"<path d="M0 0L1000001 1" fill="#000"/>"##),
        svg(r##"<path transform="matrix(1 0 0 1 1000001 0)" d="M0 0L1 1" fill="#000"/>"##),
        svg(
            r##"<g transform="scale(1000000)"><path transform="scale(1000000)" d="M0 0L1 1" fill="#000"/></g>"##,
        ),
        svg(r##"<path transform="translate(1,,2)" d="M0 0L1 1" fill="#000"/>"##),
    ] {
        assert!(SvgContract::validate(&invalid).is_err(), "{invalid}");
    }
}

#[test]
fn svg_contract_reports_colors_and_curve_segments() {
    let report = SvgContract::validate(&svg(
        r##"<path fill="#111111" d="M0 0C1 1 2 2 3 3Z"/><path fill="#222222" d="M1 1Q2 2 3 3Z"/>"##,
    ))
    .unwrap();
    assert_eq!(report.path_count, 2);
    assert_eq!(report.color_count, 2);
    assert_eq!(report.curve_segment_count, 2);
    assert_eq!(report.closed_path_count, 2);
}

#[test]
fn svg_contract_resolves_supported_inherited_rendered_facts_and_rejects_group_opacity() {
    let output = MotionCompiler::scaffold(&svg(
        r##"<g fill="#123456" transform="translate(2 3)"><path d="M0 0L1 1Z" fill-opacity="0.5"/></g>"##,
    ))
    .unwrap();
    assert_eq!(output.layers.layers[0].fill, "#123456");
    assert_eq!(output.layers.layers[0].fill_opacity, 0.5);
    assert!(output.scene_svg.contains("id=\"pp-path-0001\""));
    assert!(output.scene_svg.contains("d=\"M2 3 L3 4 Z\""));
    assert!(!output.scene_svg.contains("transform="));
    assert_eq!(output.layers.layers[0].bounds, [2.0, 3.0, 1.0, 1.0]);

    let composited = svg(r##"<g opacity="0.5"><path fill="#123456" d="M0 0L1 1Z"/></g>"##);
    assert!(SvgContract::validate(&composited).is_err());
}

#[test]
fn motion_scaffold_flattens_relative_smooth_and_multi_subpath_geometry() {
    let output = MotionCompiler::scaffold(&svg(
        r##"<g transform="translate(2 3)"><path fill="#123456" d="m1 1 h2 v2 s1 1 2 0 q1 -1 2 0 t2 0 z m1 1 l1 0"/></g>"##,
    ))
    .expect("translation-only scene flattens");
    assert!(output
        .scene_svg
        .contains("d=\"M3 4 L5 4 L5 6 S6 7 7 6 Q8 5 9 6 T11 6 Z M4 5 L5 5\""));
    assert!(!output.scene_svg.contains("transform="));
}

#[test]
fn motion_scaffold_rejects_non_translation_transform_provenance() {
    for transform in ["scale(2) scale(.5)", "matrix(1 0 0 1 2 3)", "rotate(0)"] {
        let input = svg(&format!(
            r##"<g id="group" transform="{transform}"><path fill="#123456" d="M0 0L1 1"/></g>"##
        ));
        let error = MotionCompiler::scaffold(&input).unwrap_err().to_string();
        assert!(error.contains("g 'group'"), "{error}");
        assert!(error.contains(transform), "{error}");
    }
}

#[test]
fn motion_scaffold_reparse_rejects_translation_past_coordinate_limit() {
    let input = svg(
        r##"<path id="limit" transform="translate(1 0)" fill="#123456" d="M1000000 0L999999 1"/>"##,
    );
    assert!(MotionCompiler::scaffold(&input).is_err());
}
#[test]
fn svg_contract_rejects_accumulated_relative_path_geometry() {
    for path in [
        "M0 0l1000000 0l1000000 0",
        "M1000000 0c1 0 1 0 1 0",
        "M0 0l1000000 0zl1000000 0l1 0",
    ] {
        assert!(
            SvgContract::validate(&svg(&format!(r##"<path fill="#000" d="{path}"/>"##))).is_err()
        );
    }
}

#[test]
fn generated_motion_contract_rejects_unbound_and_invalid_typed_css() {
    let valid = format!(
        r##"<svg xmlns="{SVG_NS}" width="8" height="8" viewBox="0 0 8 8"><style>.pp-motion-a{{transform-box:view-box;transform-origin:0px 0px;animation:pp-motion-a 1ms linear infinite;}}
@keyframes pp-motion-a{{
0%{{transform:translate(0px,0px) rotate(0deg) scale(1,1);opacity:0;}}
100%{{transform:translate(0px,0px) rotate(0deg) scale(1,1);opacity:1;}}
}}</style><g class="pp-motion-a"><path fill="#000" d="M0 0L1 1"/></g></svg>"##
    );
    assert!(SvgContract::validate_generated_motion(&valid).is_ok());
    for invalid in [
        valid.replacen(" 1ms ", " 0ms ", 1),
        valid.replacen("100%{", "101%{", 1),
        valid.replacen("scale(1,1)", "scale(0,1)", 1),
        valid.replacen("opacity:0;", "opacity:2;", 1),
        valid.replacen("0%{", "100%{", 1),
        valid.replacen(r#"class="pp-motion-a""#, r#"class="pp-motion-b""#, 1),
    ] {
        assert!(SvgContract::validate_generated_motion(&invalid).is_err());
    }
    let repeated_wrappers = format!(
        r##"<svg xmlns="{SVG_NS}" width="8" height="8" viewBox="0 0 8 8"><style>.pp-motion-a{{transform-box:view-box;transform-origin:0px 0px;animation:pp-motion-a 1ms linear infinite;}}
@keyframes pp-motion-a{{
0%{{transform:translate(0px,0px) rotate(0deg) scale(1,1);opacity:0;}}
100%{{transform:translate(0px,0px) rotate(0deg) scale(1,1);opacity:1;}}
}}</style><g><g class="pp-motion-a"><path fill="#000" d="M0 0L1 1"/></g><g class="pp-motion-a"><path fill="#000" d="M1 1L2 2"/></g></g></svg>"##
    );
    assert!(SvgContract::validate_generated_motion(&repeated_wrappers).is_ok());
}
