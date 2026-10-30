use super::{
    composite_source_over_linear_srgb, AlphaMode, ArtifactRef, BlendMode, ColorSpec, Document,
    Layer, PixelFormat, PixelSpec, PpError, PpResult, Raster,
};

/// One immutable raster resolved for a DocumentIR artifact reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedDocumentRaster {
    pub artifact: ArtifactRef,
    pub raster: Raster,
    pub pixel: PixelSpec,
}

/// Deterministic merged appearance authority for DocumentIR v1.
///
/// Layer arrays use painter order: first is backmost, last is frontmost. Groups are isolated and
/// then composited using their own opacity/blend. Pixel layers support integer translation and an
/// optional alpha mask. PSD and every future document backend must embed this result rather than
/// implementing an independent compositor.
pub fn composite_document(
    document: &Document,
    resolved: &[ResolvedDocumentRaster],
) -> PpResult<Raster> {
    document.validate()?;
    require_srgb_canvas(&document.canvas.pixel)?;
    reject_duplicate_resolved_artifacts(resolved)?;
    let mut canvas = Raster::blank(document.canvas.width, document.canvas.height)?;
    composite_layers(&mut canvas, &document.layers, document, resolved)?;
    Ok(canvas)
}

fn composite_layers(
    destination: &mut Raster,
    layers: &[Layer],
    document: &Document,
    resolved: &[ResolvedDocumentRaster],
) -> PpResult<()> {
    for layer in layers {
        if !layer.common().visible || layer.common().opacity == 0 {
            continue;
        }
        let source = match layer {
            Layer::Pixel(pixel) => render_pixel_layer(pixel, document, resolved)?,
            Layer::Group(group) => {
                let mut isolated = Raster::blank(document.canvas.width, document.canvas.height)?;
                composite_layers(&mut isolated, &group.children, document, resolved)?;
                apply_opacity(&isolated, group.common.opacity)?
            }
        };
        let blend = layer.common().blend;
        *destination = composite_source_over_linear_srgb(destination, &source, blend)?;
    }
    Ok(())
}

fn render_pixel_layer(
    layer: &super::PixelLayer,
    document: &Document,
    resolved: &[ResolvedDocumentRaster],
) -> PpResult<Raster> {
    let source = resolve(&layer.artifact, resolved, "pixel layer")?;
    require_srgb_source(&source.pixel, "pixel layer")?;
    let mask = layer
        .raster_mask
        .as_ref()
        .map(|artifact| resolve(artifact, resolved, "raster mask"))
        .transpose()?;
    if let Some(mask) = mask {
        if mask.raster.width() != source.raster.width()
            || mask.raster.height() != source.raster.height()
        {
            return Err(PpError::InvalidRequest(format!(
                "raster mask for layer '{}' must match its pixel raster dimensions",
                layer.common.id
            )));
        }
    }

    let mut placed =
        vec![0u8; document.canvas.width as usize * document.canvas.height as usize * 4];
    let canvas_width = i64::from(document.canvas.width);
    let canvas_height = i64::from(document.canvas.height);
    let opacity = u32::from(layer.common.opacity);
    for sy in 0..source.raster.height() {
        for sx in 0..source.raster.width() {
            let dx = i64::from(sx) + i64::from(layer.common.offset_x);
            let dy = i64::from(sy) + i64::from(layer.common.offset_y);
            if dx < 0 || dy < 0 || dx >= canvas_width || dy >= canvas_height {
                continue;
            }
            let source_index = (sy as usize * source.raster.width() as usize + sx as usize) * 4;
            let destination_index =
                (dy as usize * document.canvas.width as usize + dx as usize) * 4;
            placed[destination_index..destination_index + 3]
                .copy_from_slice(&source.raster.pixels()[source_index..source_index + 3]);
            let mut alpha = u32::from(source.raster.pixels()[source_index + 3]);
            if let Some(mask) = mask {
                let mask_alpha = u32::from(mask.raster.pixels()[source_index + 3]);
                alpha = (alpha * mask_alpha + 127) / 255;
            }
            alpha = (alpha * opacity + 127) / 255;
            placed[destination_index + 3] = alpha.min(255) as u8;
        }
    }
    Raster::new(document.canvas.width, document.canvas.height, placed)
}

fn apply_opacity(raster: &Raster, opacity: u8) -> PpResult<Raster> {
    if opacity == 255 {
        return Ok(raster.clone());
    }
    let opacity = u32::from(opacity);
    let mut pixels = raster.pixels().to_vec();
    for pixel in pixels.as_chunks_mut::<4>().0 {
        pixel[3] = ((u32::from(pixel[3]) * opacity + 127) / 255).min(255) as u8;
    }
    Raster::new(raster.width(), raster.height(), pixels)
}

fn resolve<'a>(
    artifact: &ArtifactRef,
    resolved: &'a [ResolvedDocumentRaster],
    label: &str,
) -> PpResult<&'a ResolvedDocumentRaster> {
    resolved
        .iter()
        .find(|candidate| candidate.artifact.sha256() == artifact.sha256())
        .ok_or_else(|| {
            PpError::InvalidRequest(format!(
                "{label} artifact {} is not resolved",
                artifact.sha256().as_str()
            ))
        })
}

fn reject_duplicate_resolved_artifacts(resolved: &[ResolvedDocumentRaster]) -> PpResult<()> {
    for (index, left) in resolved.iter().enumerate() {
        if resolved[index + 1..]
            .iter()
            .any(|right| right.artifact.sha256() == left.artifact.sha256())
        {
            return Err(PpError::InvalidRequest(format!(
                "document resolver repeats artifact {}",
                left.artifact.sha256().as_str()
            )));
        }
    }
    Ok(())
}

fn require_srgb_canvas(pixel: &PixelSpec) -> PpResult<()> {
    require_srgb_source(pixel, "document canvas")
}

fn require_srgb_source(pixel: &PixelSpec, label: &str) -> PpResult<()> {
    if pixel.pixel_format != PixelFormat::Rgba8
        || pixel.alpha == AlphaMode::Premultiplied
        || pixel.color != ColorSpec::Srgb
    {
        return Err(PpError::InvalidRequest(format!(
            "{label} must be straight/opaque RGBA8 sRGB before document compositing"
        )));
    }
    Ok(())
}

/// Maps PerfectPixel blend semantics to the PSD four-byte key without making PSD the owner.
pub(crate) fn psd_blend_key(blend: BlendMode) -> [u8; 4] {
    match blend {
        BlendMode::Normal => *b"norm",
        BlendMode::Multiply => *b"mul ",
        BlendMode::Screen => *b"scrn",
        BlendMode::Overlay => *b"over",
        BlendMode::Darken => *b"dark",
        BlendMode::Lighten => *b"lite",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CanvasSpec, GroupLayer, LayerCommon, PixelLayer, Sha256Digest};

    fn common(id: &str, opacity: u8, blend: BlendMode) -> LayerCommon {
        LayerCommon {
            id: id.to_string(),
            name: id.to_string(),
            visible: true,
            opacity,
            blend,
            offset_x: 0,
            offset_y: 0,
        }
    }

    fn srgb() -> PixelSpec {
        PixelSpec::new(PixelFormat::Rgba8, AlphaMode::Straight, ColorSpec::Srgb)
    }

    #[test]
    fn painter_order_is_deterministic() -> PpResult<()> {
        let red = Raster::new(1, 1, vec![255, 0, 0, 255])?;
        let blue = Raster::new(1, 1, vec![0, 0, 255, 255])?;
        let red_ref = ArtifactRef::from_bytes("image/png", b"red")?;
        let blue_ref = ArtifactRef::from_bytes("image/png", b"blue")?;
        let document = Document::new(
            CanvasSpec {
                width: 1,
                height: 1,
                pixel: srgb(),
            },
            vec![
                Layer::Pixel(PixelLayer {
                    common: common("red", 255, BlendMode::Normal),
                    artifact: red_ref.clone(),
                    raster_mask: None,
                }),
                Layer::Pixel(PixelLayer {
                    common: common("blue", 255, BlendMode::Normal),
                    artifact: blue_ref.clone(),
                    raster_mask: None,
                }),
            ],
        )?;
        let merged = composite_document(
            &document,
            &[
                ResolvedDocumentRaster {
                    artifact: red_ref,
                    raster: red,
                    pixel: srgb(),
                },
                ResolvedDocumentRaster {
                    artifact: blue_ref,
                    raster: blue,
                    pixel: srgb(),
                },
            ],
        )?;
        assert_eq!(merged.pixels(), &[0, 0, 255, 255]);
        Ok(())
    }

    #[test]
    fn missing_artifact_fails_closed() -> PpResult<()> {
        let missing = ArtifactRef::new(Sha256Digest::from_bytes(b"missing"), "image/png", 7)?;
        let document = Document::new(
            CanvasSpec {
                width: 1,
                height: 1,
                pixel: srgb(),
            },
            vec![Layer::Pixel(PixelLayer {
                common: common("missing", 255, BlendMode::Normal),
                artifact: missing,
                raster_mask: None,
            })],
        )?;
        assert!(composite_document(&document, &[]).is_err());
        Ok(())
    }

    #[test]
    fn group_isolation_uses_core_composite() -> PpResult<()> {
        let artifact = ArtifactRef::from_bytes("image/png", b"pixel")?;
        let document = Document::new(
            CanvasSpec {
                width: 1,
                height: 1,
                pixel: srgb(),
            },
            vec![Layer::Group(GroupLayer {
                common: common("g", 128, BlendMode::Normal),
                children: vec![Layer::Pixel(PixelLayer {
                    common: common("p", 255, BlendMode::Normal),
                    artifact: artifact.clone(),
                    raster_mask: None,
                })],
            })],
        )?;
        let merged = composite_document(
            &document,
            &[ResolvedDocumentRaster {
                artifact,
                raster: Raster::new(1, 1, vec![255, 255, 255, 255])?,
                pixel: srgb(),
            }],
        )?;
        assert_eq!(merged.pixels()[3], 128);
        Ok(())
    }
}
