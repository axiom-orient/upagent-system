use perfectpixel::{inspect_raster, EdgePaletteEntry, FrameRect, Point, Raster};

#[test]
fn inspect_raster_reports_bbox_center_ground_and_edge_touch() {
    let image = frame(4, 4, &[(1, 1), (2, 2), (3, 2)]);
    let report = inspect_raster(&image);

    assert_eq!(report.width, 4);
    assert_eq!(report.height, 4);
    assert_eq!(report.foreground_pixels, 3);
    assert_eq!(
        report.content_box,
        FrameRect {
            x: 1,
            y: 1,
            w: 3,
            h: 2
        }
    );
    assert_eq!(report.center, Point { x: 2, y: 2 });
    assert_eq!(report.ground_y, 3);
    assert!(report.touches_edge);
    assert!((report.alpha_ratio - 0.1875).abs() < f64::EPSILON);
    assert!(report.has_alpha);
    assert_eq!(report.pixel_format, "rgba8");
    assert_eq!(report.edge_pixel_count, 12);
    assert_eq!(
        report.edge_palette,
        vec![
            EdgePaletteEntry {
                rgb: [0, 0, 0],
                count: 11,
            },
            EdgePaletteEntry {
                rgb: [255, 255, 255],
                count: 1,
            },
        ]
    );
}

#[test]
fn inspect_raster_reports_empty_content_as_zero_shape() {
    let image = frame(3, 3, &[]);
    let report = inspect_raster(&image);

    assert_eq!(report.foreground_pixels, 0);
    assert_eq!(report.content_box, FrameRect::default());
    assert_eq!(report.center, Point::default());
    assert_eq!(report.ground_y, 0);
    assert!(!report.touches_edge);
    assert!(report.has_alpha);
    assert_eq!(report.edge_pixel_count, 8);
    assert_eq!(report.edge_palette[0].count, 8);
}

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
