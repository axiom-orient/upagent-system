use perfectpixel::{NormalizeRequest, SpriteBundleRequest};

#[test]
fn sprite_request_accepts_loop_key_and_default_packing() {
    let request: SpriteBundleRequest = serde_json::from_str(
        r#"{"character":"hero","cellWidth":2,"cellHeight":2,"states":[{"name":"idle","fps":8,"loop":false,"frames":["idle/frame-00.png"]}]}"#,
    )
    .unwrap();
    assert_eq!(request.sheet_image, "sprite-sheet.png");
    assert_eq!(request.packing.max_width, 2048);
    assert_eq!(request.packing.max_height, 2048);
    assert_eq!(request.packing.padding, 2);
    assert!(request.packing.trim);
    assert!(!request.packing.allow_rotation);
    assert!(request.packing.multipack);
    assert_eq!(request.states[0].fps, 8);
    assert!(!request.states[0].looped);
}

#[test]
fn sprite_request_requires_fps_and_loop() {
    for state in [
        r#"{"name":"idle","loop":true,"frames":["idle/frame-00.png"]}"#,
        r#"{"name":"idle","fps":8,"frames":["idle/frame-00.png"]}"#,
    ] {
        let json =
            format!(r#"{{"character":"hero","cellWidth":2,"cellHeight":2,"states":[{state}]}}"#);
        assert!(serde_json::from_str::<SpriteBundleRequest>(&json).is_err());
    }
}

#[test]
fn sprite_request_accepts_explicit_packing() {
    let request: SpriteBundleRequest = serde_json::from_str(
        r#"{"character":"hero","cellWidth":2,"cellHeight":2,"packing":{"maxWidth":64,"maxHeight":32,"padding":0,"trim":false,"allowRotation":true,"multipack":false},"states":[{"name":"idle","fps":8,"loop":true,"frames":["idle/frame-00.png"]}]}"#,
    )
    .unwrap();
    assert_eq!(request.packing.max_width, 64);
    assert_eq!(request.packing.max_height, 32);
    assert_eq!(request.packing.padding, 0);
    assert!(!request.packing.trim);
    assert!(request.packing.allow_rotation);
    assert!(!request.packing.multipack);
}

#[test]
fn sprite_request_rejects_unknown_fields_at_every_request_boundary() {
    let cases = [
        r#"{"character":"hero","cellWidth":2,"cellHeight":2,"unexpected":true,"states":[{"name":"idle","fps":8,"loop":true,"frames":["idle.png"]}]}"#,
        r#"{"character":"hero","cellWidth":2,"cellHeight":2,"packing":{"unexpected":true},"states":[{"name":"idle","fps":8,"loop":true,"frames":["idle.png"]}]}"#,
        r#"{"character":"hero","cellWidth":2,"cellHeight":2,"states":[{"name":"idle","fps":8,"loop":true,"frames":["idle.png"],"unexpected":true}]}"#,
    ];

    for json in cases {
        let error = serde_json::from_str::<SpriteBundleRequest>(json)
            .expect_err("unknown sprite request fields must fail closed");
        assert!(error.to_string().contains("unknown field"), "{error}");
    }
}

#[test]
fn normalize_request_rejects_unknown_fields_at_every_request_boundary() {
    let base = |extra: &str| {
        format!(
            r#"{{"character":"hero","cellWidth":16,"cellHeight":16,"states":[{{"name":"idle","frames":["idle.png"]}}]{extra}}}"#
        )
    };
    let cases = [
        base(r#","unexpected":true"#),
        r#"{"character":"hero","cellWidth":16,"cellHeight":16,"fit":{"unexpected":true},"states":[{"name":"idle","frames":["idle.png"]}]}"#.to_string(),
        r#"{"character":"hero","cellWidth":16,"cellHeight":16,"quality":{"unexpected":true},"states":[{"name":"idle","frames":["idle.png"]}]}"#.to_string(),
        r#"{"character":"hero","cellWidth":16,"cellHeight":16,"chroma":{"rgb":[255,0,255],"unexpected":true},"states":[{"name":"idle","frames":["idle.png"]}]}"#.to_string(),
        r#"{"character":"hero","cellWidth":16,"cellHeight":16,"fit":{"outline":{"unexpected":true}},"states":[{"name":"idle","frames":["idle.png"]}]}"#.to_string(),
        r#"{"character":"hero","cellWidth":16,"cellHeight":16,"states":[{"name":"idle","frames":["idle.png"],"unexpected":true}]}"#.to_string(),
    ];

    for json in cases {
        let error = serde_json::from_str::<NormalizeRequest>(&json)
            .expect_err("unknown normalize request fields must fail closed");
        assert!(error.to_string().contains("unknown field"), "{error}");
    }
}
