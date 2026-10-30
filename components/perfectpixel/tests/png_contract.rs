use std::io::Cursor;

use image::ImageReader;
use perfectpixel::{PngEncoder, Raster};

#[test]
fn png_encoder_writes_decodable_rgba_png() {
    let raster = Raster::new(1, 1, vec![10, 20, 30, 255]).expect("valid raster");
    let bytes = PngEncoder::encode_rgba(&raster).expect("encode PNG");
    let decoded = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .expect("format")
        .decode()
        .expect("decode")
        .to_rgba8();

    assert_eq!(decoded.width(), 1);
    assert_eq!(decoded.height(), 1);
    assert_eq!(decoded.as_raw(), &[10, 20, 30, 255]);
}
