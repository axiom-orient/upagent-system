use serde::Serialize;

use crate::core::{
    composite_document, psd_blend_key, ArtifactRef, Document, Layer, LayerCommon, PpError,
    PpResult, Raster, ResolvedDocumentRaster, Sha256Digest,
};

const PSD_MAX_LAYER_RECORDS: usize = 32_767;
const PSD_MAX_BYTES: usize = 512 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayeredPsd {
    bytes: Vec<u8>,
    merged: Raster,
    structure: PsdStructureReport,
}

impl LayeredPsd {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn merged(&self) -> &Raster {
        &self.merged
    }

    pub fn layer_records(&self) -> usize {
        self.structure.layer_records
    }

    pub fn pixel_layers(&self) -> usize {
        self.structure.pixel_layers
    }

    pub fn groups(&self) -> usize {
        self.structure.group_openers
    }

    pub fn masks(&self) -> usize {
        self.structure.masks
    }

    pub(crate) fn structure(&self) -> &PsdStructureReport {
        &self.structure
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PsdStructureReport {
    pub width: u32,
    pub height: u32,
    pub channels: u16,
    pub layer_records: usize,
    pub pixel_layers: usize,
    pub group_openers: usize,
    pub group_dividers: usize,
    pub masks: usize,
    pub merged_sha256: Sha256Digest,
}

struct SerializedLayer<'a> {
    common: &'a LayerCommon,
    kind: SerializedLayerKind<'a>,
}

enum SerializedLayerKind<'a> {
    Pixel {
        source: &'a ResolvedDocumentRaster,
        mask: Option<&'a ResolvedDocumentRaster>,
    },
    GroupOpen,
    GroupEnd,
}

struct ChannelPlane {
    id: i16,
    bytes: Vec<u8>,
}

/// Serializes DocumentIR v1 to bounded PSD v1 bytes with layered v2 semantics.
/// The merged image is computed first by the core and then embedded unchanged.
pub fn encode_layered_psd(
    document: &Document,
    resolved: &[ResolvedDocumentRaster],
) -> PpResult<LayeredPsd> {
    document.validate()?;
    let merged = composite_document(document, resolved)?;
    let mut layers = Vec::new();
    flatten_psd_layers(&document.layers, resolved, &mut layers)?;
    if layers.len() > PSD_MAX_LAYER_RECORDS {
        return Err(PpError::InvalidRequest(format!(
            "PSD layer record count exceeds {PSD_MAX_LAYER_RECORDS}"
        )));
    }

    let mut layer_records = Vec::new();
    let mut channel_stream = Vec::new();
    let mut pixel_layers = 0usize;
    let mut groups = 0usize;
    let mut masks = 0usize;
    for layer in &layers {
        let channels = encode_layer_record(layer, &mut layer_records)?;
        match layer.kind {
            SerializedLayerKind::Pixel { mask, .. } => {
                pixel_layers += 1;
                masks += usize::from(mask.is_some());
            }
            SerializedLayerKind::GroupOpen => groups += 1,
            SerializedLayerKind::GroupEnd => {}
        }
        for plane in channels {
            channel_stream.extend_from_slice(&0u16.to_be_bytes()); // raw compression
            channel_stream.extend_from_slice(&plane.bytes);
        }
    }

    let mut layer_info = Vec::new();
    let count = i16::try_from(layers.len())
        .map_err(|_| PpError::InvalidRequest("PSD layer count overflow".to_string()))?;
    // Negative means merged transparency is present as the first extra channel.
    layer_info.extend_from_slice(&(-count).to_be_bytes());
    layer_info.extend_from_slice(&layer_records);
    layer_info.extend_from_slice(&channel_stream);
    if layer_info.len() % 2 != 0 {
        layer_info.push(0);
    }

    let mut layer_and_mask_body = Vec::new();
    put_len32(&mut layer_and_mask_body, layer_info.len(), "PSD layer info")?;
    layer_and_mask_body.extend_from_slice(&layer_info);
    layer_and_mask_body.extend_from_slice(&0u32.to_be_bytes()); // global layer mask length

    let mut bytes = Vec::new();
    encode_header(document, &mut bytes)?;
    bytes.extend_from_slice(&0u32.to_be_bytes()); // color mode data
    bytes.extend_from_slice(&0u32.to_be_bytes()); // image resources
    put_len32(
        &mut bytes,
        layer_and_mask_body.len(),
        "PSD layer/mask section",
    )?;
    bytes.extend_from_slice(&layer_and_mask_body);
    encode_merged(&merged, &mut bytes);
    if bytes.len() > PSD_MAX_BYTES {
        return Err(PpError::InvalidRequest(format!(
            "layered PSD exceeds {PSD_MAX_BYTES}-byte limit"
        )));
    }

    // A parser independent from the serializer path validates lengths, hierarchy markers,
    // channel counts, masks, and exact merged image bytes before publication.
    let structure = inspect_layered_psd(&bytes)?;
    if structure.width != document.canvas.width
        || structure.height != document.canvas.height
        || structure.layer_records != layers.len()
        || structure.pixel_layers != pixel_layers
        || structure.group_openers != groups
        || structure.group_dividers != groups
        || structure.masks != masks
        || structure.merged_sha256 != Sha256Digest::from_bytes(merged.pixels())
    {
        return Err(PpError::InvalidRequest(
            "layered PSD structural readback does not match DocumentIR".to_string(),
        ));
    }

    Ok(LayeredPsd {
        bytes,
        merged,
        structure,
    })
}

fn flatten_psd_layers<'a>(
    layers: &'a [Layer],
    resolved: &'a [ResolvedDocumentRaster],
    output: &mut Vec<SerializedLayer<'a>>,
) -> PpResult<()> {
    // PSD records are top-to-bottom; DocumentIR is painter order back-to-front.
    for layer in layers.iter().rev() {
        match layer {
            Layer::Pixel(pixel) => {
                let source = resolve(&pixel.artifact, resolved, "pixel")?;
                let mask = pixel
                    .raster_mask
                    .as_ref()
                    .map(|artifact| resolve(artifact, resolved, "mask"))
                    .transpose()?;
                if let Some(mask) = mask {
                    if mask.raster.width() != source.raster.width()
                        || mask.raster.height() != source.raster.height()
                    {
                        return Err(PpError::InvalidRequest(format!(
                            "PSD mask for layer '{}' must match source dimensions",
                            pixel.common.id
                        )));
                    }
                }
                output.push(SerializedLayer {
                    common: &pixel.common,
                    kind: SerializedLayerKind::Pixel { source, mask },
                });
            }
            Layer::Group(group) => {
                output.push(SerializedLayer {
                    common: &group.common,
                    kind: SerializedLayerKind::GroupOpen,
                });
                flatten_psd_layers(&group.children, resolved, output)?;
                output.push(SerializedLayer {
                    common: &group.common,
                    kind: SerializedLayerKind::GroupEnd,
                });
            }
        }
    }
    Ok(())
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
                "PSD {label} artifact {} is not resolved",
                artifact.sha256().as_str()
            ))
        })
}

fn encode_layer_record(
    layer: &SerializedLayer<'_>,
    output: &mut Vec<u8>,
) -> PpResult<Vec<ChannelPlane>> {
    match &layer.kind {
        SerializedLayerKind::Pixel { source, mask, .. } => {
            let bounds = layer_bounds(layer.common, &source.raster)?;
            put_i32(output, bounds.0);
            put_i32(output, bounds.1);
            put_i32(output, bounds.2);
            put_i32(output, bounds.3);
            let mut channels = pixel_planes(&source.raster);
            if let Some(mask) = mask {
                channels.push(ChannelPlane {
                    id: -2,
                    bytes: alpha_plane(&mask.raster),
                });
            }
            output.extend_from_slice(&(channels.len() as u16).to_be_bytes());
            for channel in &channels {
                output.extend_from_slice(&channel.id.to_be_bytes());
                let length = channel.bytes.len().checked_add(2).ok_or_else(|| {
                    PpError::InvalidRequest("PSD channel length overflow".to_string())
                })?;
                put_len32(output, length, "PSD channel")?;
            }
            encode_layer_tail(layer, output, Some(bounds), mask.is_some())?;
            Ok(channels)
        }
        SerializedLayerKind::GroupOpen | SerializedLayerKind::GroupEnd => {
            for _ in 0..4 {
                put_i32(output, 0);
            }
            output.extend_from_slice(&0u16.to_be_bytes());
            encode_layer_tail(layer, output, None, false)?;
            Ok(Vec::new())
        }
    }
}

fn encode_layer_tail(
    layer: &SerializedLayer<'_>,
    output: &mut Vec<u8>,
    bounds: Option<(i32, i32, i32, i32)>,
    has_mask: bool,
) -> PpResult<()> {
    output.extend_from_slice(b"8BIM");
    let blend = match layer.kind {
        SerializedLayerKind::GroupEnd => *b"norm",
        _ => psd_blend_key(layer.common.blend),
    };
    output.extend_from_slice(&blend);
    output.push(match layer.kind {
        SerializedLayerKind::GroupEnd => 255,
        _ => layer.common.opacity,
    });
    output.push(0); // clipping
    let hidden = !layer.common.visible || matches!(layer.kind, SerializedLayerKind::GroupEnd);
    output.push(if hidden { 0x02 } else { 0x00 });
    output.push(0);

    let mut extra = Vec::new();
    if has_mask {
        let (top, left, bottom, right) = bounds.expect("pixel mask has bounds");
        extra.extend_from_slice(&20u32.to_be_bytes());
        put_i32(&mut extra, top);
        put_i32(&mut extra, left);
        put_i32(&mut extra, bottom);
        put_i32(&mut extra, right);
        extra.push(0); // default color
        extra.push(0); // flags
        extra.extend_from_slice(&[0, 0]);
    } else {
        extra.extend_from_slice(&0u32.to_be_bytes());
    }
    extra.extend_from_slice(&0u32.to_be_bytes()); // blending ranges
    encode_pascal_name(&mut extra, layer_name(layer));
    encode_unicode_name(&mut extra, layer_name(layer))?;
    match layer.kind {
        SerializedLayerKind::GroupOpen => encode_section_divider(&mut extra, 1)?,
        SerializedLayerKind::GroupEnd => encode_section_divider(&mut extra, 3)?,
        SerializedLayerKind::Pixel { .. } => {}
    }
    put_len32(output, extra.len(), "PSD layer extra data")?;
    output.extend_from_slice(&extra);
    Ok(())
}

fn layer_name<'a>(layer: &'a SerializedLayer<'_>) -> &'a str {
    match layer.kind {
        SerializedLayerKind::GroupEnd => "</Layer group>",
        _ if layer.common.name.is_empty() => &layer.common.id,
        _ => &layer.common.name,
    }
}

fn layer_bounds(common: &LayerCommon, raster: &Raster) -> PpResult<(i32, i32, i32, i32)> {
    let top = i64::from(common.offset_y);
    let left = i64::from(common.offset_x);
    let bottom = top + i64::from(raster.height());
    let right = left + i64::from(raster.width());
    Ok((
        i32::try_from(top)
            .map_err(|_| PpError::InvalidRequest("PSD layer top overflow".to_string()))?,
        i32::try_from(left)
            .map_err(|_| PpError::InvalidRequest("PSD layer left overflow".to_string()))?,
        i32::try_from(bottom)
            .map_err(|_| PpError::InvalidRequest("PSD layer bottom overflow".to_string()))?,
        i32::try_from(right)
            .map_err(|_| PpError::InvalidRequest("PSD layer right overflow".to_string()))?,
    ))
}

fn pixel_planes(raster: &Raster) -> Vec<ChannelPlane> {
    let pixels = raster.pixels();
    let mut red = Vec::with_capacity(pixels.len() / 4);
    let mut green = Vec::with_capacity(pixels.len() / 4);
    let mut blue = Vec::with_capacity(pixels.len() / 4);
    let mut alpha = Vec::with_capacity(pixels.len() / 4);
    for pixel in pixels.as_chunks::<4>().0 {
        red.push(pixel[0]);
        green.push(pixel[1]);
        blue.push(pixel[2]);
        alpha.push(pixel[3]);
    }
    vec![
        ChannelPlane { id: 0, bytes: red },
        ChannelPlane {
            id: 1,
            bytes: green,
        },
        ChannelPlane { id: 2, bytes: blue },
        ChannelPlane {
            id: -1,
            bytes: alpha,
        },
    ]
}

fn alpha_plane(raster: &Raster) -> Vec<u8> {
    raster
        .pixels()
        .as_chunks::<4>()
        .0
        .iter()
        .map(|pixel| pixel[3])
        .collect()
}

fn encode_pascal_name(output: &mut Vec<u8>, name: &str) {
    let bytes = name.as_bytes();
    let count = bytes.len().min(255);
    let start = output.len();
    output.push(count as u8);
    output.extend_from_slice(&bytes[..count]);
    while !(output.len() - start).is_multiple_of(4) {
        output.push(0);
    }
}

fn encode_unicode_name(output: &mut Vec<u8>, name: &str) -> PpResult<()> {
    let utf16 = name.encode_utf16().collect::<Vec<_>>();
    let mut data = Vec::with_capacity(4 + utf16.len() * 2);
    let count = u32::try_from(utf16.len())
        .map_err(|_| PpError::InvalidRequest("PSD unicode layer name is too long".to_string()))?;
    data.extend_from_slice(&count.to_be_bytes());
    for unit in utf16 {
        data.extend_from_slice(&unit.to_be_bytes());
    }
    encode_tagged_block(output, *b"luni", &data)
}

fn encode_section_divider(output: &mut Vec<u8>, kind: u32) -> PpResult<()> {
    encode_tagged_block(output, *b"lsct", &kind.to_be_bytes())
}

fn encode_tagged_block(output: &mut Vec<u8>, key: [u8; 4], data: &[u8]) -> PpResult<()> {
    output.extend_from_slice(b"8BIM");
    output.extend_from_slice(&key);
    put_len32(output, data.len(), "PSD tagged block")?;
    output.extend_from_slice(data);
    if !data.len().is_multiple_of(2) {
        output.push(0);
    }
    Ok(())
}

fn encode_header(document: &Document, output: &mut Vec<u8>) -> PpResult<()> {
    if document.canvas.width > 30_000 || document.canvas.height > 30_000 {
        return Err(PpError::InvalidRequest(
            "PSD v1 dimensions must not exceed 30000 pixels".to_string(),
        ));
    }
    output.extend_from_slice(b"8BPS");
    output.extend_from_slice(&1u16.to_be_bytes());
    output.extend_from_slice(&[0u8; 6]);
    output.extend_from_slice(&4u16.to_be_bytes());
    output.extend_from_slice(&document.canvas.height.to_be_bytes());
    output.extend_from_slice(&document.canvas.width.to_be_bytes());
    output.extend_from_slice(&8u16.to_be_bytes());
    output.extend_from_slice(&3u16.to_be_bytes()); // RGB
    Ok(())
}

fn encode_merged(raster: &Raster, output: &mut Vec<u8>) {
    output.extend_from_slice(&0u16.to_be_bytes()); // raw compression
    for channel in 0..4 {
        output.extend(
            raster
                .pixels()
                .as_chunks::<4>()
                .0
                .iter()
                .map(|pixel| pixel[channel]),
        );
    }
}

fn put_i32(output: &mut Vec<u8>, value: i32) {
    output.extend_from_slice(&value.to_be_bytes());
}

fn put_len32(output: &mut Vec<u8>, value: usize, label: &str) -> PpResult<()> {
    let value = u32::try_from(value)
        .map_err(|_| PpError::InvalidRequest(format!("{label} length exceeds PSD v1 limit")))?;
    output.extend_from_slice(&value.to_be_bytes());
    Ok(())
}

// -------------------------------------------------------------------------------------------------
// Independent structural readback. This intentionally does not reuse serializer records/helpers.
// -------------------------------------------------------------------------------------------------

pub fn inspect_layered_psd(bytes: &[u8]) -> PpResult<PsdStructureReport> {
    let mut cursor = Cursor::new(bytes);
    if cursor.take(4)? != b"8BPS" || cursor.u16()? != 1 {
        return Err(PpError::InvalidRequest(
            "invalid PSD signature/version".to_string(),
        ));
    }
    cursor.skip(6)?;
    let channels = cursor.u16()?;
    let height = cursor.u32()?;
    let width = cursor.u32()?;
    if cursor.u16()? != 8 || cursor.u16()? != 3 {
        return Err(PpError::InvalidRequest(
            "layered PSD must be 8-bit RGB".to_string(),
        ));
    }
    cursor.skip_section32()?; // color mode
    cursor.skip_section32()?; // resources
    let layer_mask_len = cursor.u32()? as usize;
    let layer_mask_end = cursor.checked_end(layer_mask_len)?;
    let layer_info_len = cursor.u32()? as usize;
    let layer_info_end = cursor.checked_end(layer_info_len)?;
    let count_raw = cursor.i16()?;
    let count = usize::from(count_raw.unsigned_abs());
    let mut channel_layouts = Vec::with_capacity(count);
    let mut pixel_layers = 0usize;
    let mut group_openers = 0usize;
    let mut group_dividers = 0usize;
    let mut masks = 0usize;
    for _ in 0..count {
        let top = cursor.i32()?;
        let left = cursor.i32()?;
        let bottom = cursor.i32()?;
        let right = cursor.i32()?;
        let channel_count = cursor.u16()? as usize;
        let mut channels_for_layer = Vec::with_capacity(channel_count);
        for _ in 0..channel_count {
            let id = cursor.i16()?;
            let length = cursor.u32()? as usize;
            if length < 2 {
                return Err(PpError::InvalidRequest(
                    "PSD channel length is invalid".to_string(),
                ));
            }
            channels_for_layer.push((id, length));
        }
        if cursor.take(4)? != b"8BIM" {
            return Err(PpError::InvalidRequest(
                "PSD layer blend signature is invalid".to_string(),
            ));
        }
        cursor.skip(4 + 4)?; // key + opacity/clipping/flags/filler
        let extra_len = cursor.u32()? as usize;
        let extra_end = cursor.checked_end(extra_len)?;
        let mask_len = cursor.u32()? as usize;
        if mask_len != 0 {
            masks += 1;
            cursor.skip(mask_len)?;
        }
        cursor.skip_section32()?; // blending ranges
        cursor.skip_pascal4()?;
        let mut divider = None;
        while cursor.position < extra_end {
            if extra_end - cursor.position < 12 {
                cursor.position = extra_end;
                break;
            }
            let signature = cursor.take(4)?;
            if signature != b"8BIM" && signature != b"8B64" {
                return Err(PpError::InvalidRequest(
                    "PSD tagged block signature is invalid".to_string(),
                ));
            }
            let key = cursor.take(4)?;
            let length = cursor.u32()? as usize;
            let data = cursor.take(length)?;
            if key == b"lsct" && data.len() >= 4 {
                divider = Some(u32::from_be_bytes(data[0..4].try_into().unwrap()));
            }
            if !length.is_multiple_of(2) {
                cursor.skip(1)?;
            }
        }
        if cursor.position != extra_end {
            return Err(PpError::InvalidRequest(
                "PSD layer extra-data length mismatch".to_string(),
            ));
        }
        match divider {
            Some(1 | 2) => group_openers += 1,
            Some(3) => group_dividers += 1,
            _ if channel_count > 0 => pixel_layers += 1,
            _ => {}
        }
        if channel_count > 0 && (bottom < top || right < left) {
            return Err(PpError::InvalidRequest(
                "PSD layer bounds are inverted".to_string(),
            ));
        }
        channel_layouts.push((channels_for_layer, top, left, bottom, right));
    }
    for (channels_for_layer, top, left, bottom, right) in channel_layouts {
        let width = usize::try_from((right - left).max(0))
            .map_err(|_| PpError::InvalidRequest("PSD layer width overflow".to_string()))?;
        let height = usize::try_from((bottom - top).max(0))
            .map_err(|_| PpError::InvalidRequest("PSD layer height overflow".to_string()))?;
        for (id, length) in channels_for_layer {
            if cursor.u16()? != 0 {
                return Err(PpError::InvalidRequest(
                    "PSD v2 layer channel must use raw compression".to_string(),
                ));
            }
            let payload = length - 2;
            let expected = width * height;
            if payload != expected {
                return Err(PpError::InvalidRequest(format!(
                    "PSD channel {id} payload length mismatch: {payload} != {expected}"
                )));
            }
            cursor.skip(payload)?;
        }
    }
    if cursor.position < layer_info_end {
        cursor.position = layer_info_end; // optional even padding
    }
    if cursor.position > layer_info_end {
        return Err(PpError::InvalidRequest(
            "PSD layer-info overrun".to_string(),
        ));
    }
    let global_mask_len = cursor.u32()? as usize;
    cursor.skip(global_mask_len)?;
    if cursor.position < layer_mask_end {
        cursor.position = layer_mask_end;
    }
    if cursor.position != layer_mask_end {
        return Err(PpError::InvalidRequest(
            "PSD layer/mask section overrun".to_string(),
        ));
    }
    if cursor.u16()? != 0 {
        return Err(PpError::InvalidRequest(
            "PSD merged image must use raw compression".to_string(),
        ));
    }
    let plane_len = usize::try_from(u64::from(width) * u64::from(height))
        .map_err(|_| PpError::InvalidRequest("PSD merged plane length overflow".to_string()))?;
    if channels != 4 {
        return Err(PpError::InvalidRequest(
            "layered PSD must contain RGBA merged channels".to_string(),
        ));
    }
    let red = cursor.take(plane_len)?;
    let green = cursor.take(plane_len)?;
    let blue = cursor.take(plane_len)?;
    let alpha = cursor.take(plane_len)?;
    if cursor.position != bytes.len() {
        return Err(PpError::InvalidRequest(
            "PSD has trailing bytes after merged image".to_string(),
        ));
    }
    let mut merged = Vec::with_capacity(plane_len * 4);
    for index in 0..plane_len {
        merged.extend_from_slice(&[red[index], green[index], blue[index], alpha[index]]);
    }
    Ok(PsdStructureReport {
        width,
        height,
        channels,
        layer_records: count,
        pixel_layers,
        group_openers,
        group_dividers,
        masks,
        merged_sha256: Sha256Digest::from_bytes(&merged),
    })
}

struct Cursor<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    fn checked_end(&self, length: usize) -> PpResult<usize> {
        self.position
            .checked_add(length)
            .filter(|end| *end <= self.bytes.len())
            .ok_or_else(|| PpError::InvalidRequest("PSD section exceeds file length".to_string()))
    }

    fn take(&mut self, length: usize) -> PpResult<&'a [u8]> {
        let end = self.checked_end(length)?;
        let slice = &self.bytes[self.position..end];
        self.position = end;
        Ok(slice)
    }

    fn skip(&mut self, length: usize) -> PpResult<()> {
        self.take(length).map(|_| ())
    }

    fn u16(&mut self) -> PpResult<u16> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }

    fn i16(&mut self) -> PpResult<i16> {
        Ok(i16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }

    fn u32(&mut self) -> PpResult<u32> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn i32(&mut self) -> PpResult<i32> {
        Ok(i32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn skip_section32(&mut self) -> PpResult<()> {
        let length = self.u32()? as usize;
        self.skip(length)
    }

    fn skip_pascal4(&mut self) -> PpResult<()> {
        let start = self.position;
        let length = usize::from(*self.take(1)?.first().unwrap());
        self.skip(length)?;
        let consumed = self.position - start;
        let padding = (4 - consumed % 4) % 4;
        self.skip(padding)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AlphaMode, BlendMode, CanvasSpec, ColorSpec, GroupLayer, LayerCommon, PixelFormat,
        PixelLayer, PixelSpec,
    };

    fn srgb() -> PixelSpec {
        PixelSpec::new(PixelFormat::Rgba8, AlphaMode::Straight, ColorSpec::Srgb)
    }

    fn common(id: &str) -> LayerCommon {
        LayerCommon {
            id: id.to_string(),
            name: id.to_string(),
            visible: true,
            opacity: 255,
            blend: BlendMode::Normal,
            offset_x: 0,
            offset_y: 0,
        }
    }

    #[test]
    fn layered_psd_roundtrips_structure_and_merged_pixels() -> PpResult<()> {
        let pixel_bytes = b"pixel-source";
        let artifact = ArtifactRef::from_bytes("image/png", pixel_bytes)?;
        let document = Document::new(
            CanvasSpec {
                width: 2,
                height: 1,
                pixel: srgb(),
            },
            vec![Layer::Group(GroupLayer {
                common: common("group"),
                children: vec![Layer::Pixel(PixelLayer {
                    common: common("pixel"),
                    artifact: artifact.clone(),
                    raster_mask: None,
                })],
            })],
        )?;
        let resolved = vec![ResolvedDocumentRaster {
            artifact,
            raster: Raster::new(2, 1, vec![255, 0, 0, 255, 0, 0, 255, 128])?,
            pixel: srgb(),
        }];
        let encoded = encode_layered_psd(&document, &resolved)?;
        let report = inspect_layered_psd(encoded.bytes())?;
        assert_eq!(encoded.structure(), &report);
        assert_eq!(report.layer_records, 3);
        assert_eq!(report.pixel_layers, 1);
        assert_eq!(report.group_openers, 1);
        assert_eq!(report.group_dividers, 1);
        assert_eq!(
            report.merged_sha256,
            Sha256Digest::from_bytes(encoded.merged().pixels())
        );
        Ok(())
    }
}
