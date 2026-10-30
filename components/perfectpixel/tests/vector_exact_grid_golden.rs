use perfectpixel::{
    ApprovedVectorOutput, DiagnosticsIntent, Raster, SvgProfile, VectorOutcome, VectorPolicy,
    VectorPresetSelection, VectorRequest, Vectorizer,
};

const EXACT_GRID_BACKEND_DIGEST: &str =
    "sha256-5d0b59f6b5074850f89ddf0c8fdd3e18bf2ddd56f8d19aa2ddee8cabfa572c92";
const PIXEL_BACKEND_DIGEST: &str =
    "sha256-5ab06506789fdc574dbf9f468805d6bf37ace87cfed87277fd02d3917ef48da2";

fn request(preset: VectorPresetSelection) -> VectorRequest {
    VectorRequest::new(
        preset,
        SvgProfile::Compact,
        None,
        None,
        None,
        None,
        VectorPolicy::default(),
        DiagnosticsIntent::none(),
    )
    .expect("valid request")
}

fn approved(
    vectorizer: &Vectorizer,
    raster: &Raster,
    preset: VectorPresetSelection,
) -> ApprovedVectorOutput {
    match vectorizer
        .run(raster, &request(preset))
        .expect("vectorization succeeds")
    {
        VectorOutcome::Approved(output) => output,
        VectorOutcome::Rejected(output) => panic!("unexpected rejection: {:?}", output.report()),
    }
}

#[test]
fn exact_grid_svg_bytes_are_golden_frozen_through_public_approval() {
    let vectorizer = Vectorizer::new().expect("embedded authority");
    let cases = [
        (
            Raster::new(2, 1, vec![255, 0, 0, 255, 255, 0, 0, 255]).expect("opaque raster"),
            b"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"2\" height=\"1\" viewBox=\"0 0 2 1\"><path fill=\"#ff0000\" d=\"M0 0h2v1h-2z\"/></svg>".as_slice(),
            "sha256-683f46af5e2fdf7aa3b4a3b0ce634dff6ae51cc17c6e35604f110d06a13fc6cb",
        ),
        (
            Raster::new(2, 1, vec![0, 0, 0, 0, 0, 255, 0, 255]).expect("transparent raster"),
            b"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"2\" height=\"1\" viewBox=\"0 0 2 1\"><path fill=\"#00ff00\" d=\"M1 0h1v1h-1z\"/></svg>".as_slice(),
            "sha256-c7c74855e35f00c44c096fd4814b211c0092287d10fc37580aeb14cd8e2eb0bb",
        ),
        (
            Raster::new(1, 1, vec![18, 52, 86, 128]).expect("alpha raster"),
            b"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"1\" height=\"1\" viewBox=\"0 0 1 1\"><path fill=\"#123456\" fill-opacity=\"0.50196078\" d=\"M0 0h1v1h-1z\"/></svg>".as_slice(),
            "sha256-19a104438d729be60f18504c714a7b61b78785127e32b2c3121047f51ba797c5",
        ),
    ];

    for (raster, expected_svg, expected_sha256) in cases {
        let output = approved(&vectorizer, &raster, VectorPresetSelection::ExactGrid);
        assert_eq!(output.exact_svg_bytes(), expected_svg);
        assert_eq!(output.svg_sha256(), expected_sha256);
        assert_eq!(
            output.report().digests().backend(),
            Some(EXACT_GRID_BACKEND_DIGEST)
        );
    }
}

#[test]
fn promoted_pixel_route_remains_independent_of_exact_grid() {
    let vectorizer = Vectorizer::new().expect("embedded authority");
    let raster = Raster::new(2, 1, vec![255, 0, 0, 255, 255, 0, 0, 255]).expect("raster");

    let exact_before = approved(&vectorizer, &raster, VectorPresetSelection::ExactGrid);
    let pixel = approved(&vectorizer, &raster, VectorPresetSelection::PixelArt);
    let exact_after = approved(&vectorizer, &raster, VectorPresetSelection::ExactGrid);

    assert_eq!(
        pixel.report().digests().backend(),
        Some(PIXEL_BACKEND_DIGEST)
    );
    assert_ne!(
        pixel.report().digests().backend(),
        exact_before.report().digests().backend()
    );
    assert_ne!(pixel.exact_svg_bytes(), exact_before.exact_svg_bytes());
    assert_eq!(
        exact_after.exact_svg_bytes(),
        exact_before.exact_svg_bytes()
    );
    assert_eq!(exact_after.svg_sha256(), exact_before.svg_sha256());
}
