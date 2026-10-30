use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::{ArtifactRef, BlendMode, PixelSpec, PpError, PpResult};

pub const DOCUMENT_SCHEMA: &str = "perfectpixel.document/1";
const MAX_LAYER_DEPTH: usize = 64;
const MAX_LAYER_COUNT: usize = 65_536;
const MAX_LAYER_ID_BYTES: usize = 256;

/// Format-independent layered document semantics. PSD/PSB/OpenRaster are serializers of this
/// structure; they are never the domain authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Document {
    pub schema: String,
    pub canvas: CanvasSpec,
    pub layers: Vec<Layer>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CanvasSpec {
    pub width: u32,
    pub height: u32,
    pub pixel: PixelSpec,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Layer {
    Group(GroupLayer),
    Pixel(PixelLayer),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LayerCommon {
    pub id: String,
    pub name: String,
    pub visible: bool,
    /// Exact 0..=255 layer opacity. No floating-point transport ambiguity.
    pub opacity: u8,
    pub blend: BlendMode,
    /// Version 1 pixel layers use exact integer translation only. Arbitrary affine/projective
    /// transforms belong to a later explicit document schema rather than being silently rounded.
    pub offset_x: i32,
    pub offset_y: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GroupLayer {
    #[serde(flatten)]
    pub common: LayerCommon,
    pub children: Vec<Layer>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PixelLayer {
    #[serde(flatten)]
    pub common: LayerCommon,
    pub artifact: ArtifactRef,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raster_mask: Option<ArtifactRef>,
}

impl Document {
    pub fn new(canvas: CanvasSpec, layers: Vec<Layer>) -> PpResult<Self> {
        let document = Self {
            schema: DOCUMENT_SCHEMA.to_string(),
            canvas,
            layers,
        };
        document.validate()?;
        Ok(document)
    }

    pub fn validate(&self) -> PpResult<()> {
        if self.schema != DOCUMENT_SCHEMA {
            return Err(PpError::InvalidRequest(format!(
                "document schema must be '{DOCUMENT_SCHEMA}'"
            )));
        }
        if self.canvas.width == 0 || self.canvas.height == 0 {
            return Err(PpError::InvalidRequest(
                "document canvas dimensions must be positive".to_string(),
            ));
        }
        let mut ids = BTreeSet::new();
        let mut count = 0usize;
        validate_layers(&self.layers, 1, &mut count, &mut ids)
    }
}

fn validate_layers(
    layers: &[Layer],
    depth: usize,
    count: &mut usize,
    ids: &mut BTreeSet<String>,
) -> PpResult<()> {
    if depth > MAX_LAYER_DEPTH {
        return Err(PpError::InvalidRequest(format!(
            "document layer depth exceeds {MAX_LAYER_DEPTH}"
        )));
    }
    for layer in layers {
        *count = count
            .checked_add(1)
            .ok_or_else(|| PpError::InvalidRequest("document layer count overflow".to_string()))?;
        if *count > MAX_LAYER_COUNT {
            return Err(PpError::InvalidRequest(format!(
                "document layer count exceeds {MAX_LAYER_COUNT}"
            )));
        }
        let common = layer.common();
        validate_common(common)?;
        if !ids.insert(common.id.clone()) {
            return Err(PpError::InvalidRequest(format!(
                "duplicate document layer id '{}'",
                common.id
            )));
        }
        if let Layer::Group(group) = layer {
            validate_layers(&group.children, depth + 1, count, ids)?;
        }
    }
    Ok(())
}

fn validate_common(common: &LayerCommon) -> PpResult<()> {
    if common.id.is_empty()
        || common.id.len() > MAX_LAYER_ID_BYTES
        || common.id.trim() != common.id
        || common.id.chars().any(char::is_control)
    {
        return Err(PpError::InvalidRequest(
            "document layer id must be non-empty, trimmed, printable, and <= 256 bytes".to_string(),
        ));
    }
    if common.name.chars().any(char::is_control) {
        return Err(PpError::InvalidRequest(
            "document layer name must not contain control characters".to_string(),
        ));
    }
    Ok(())
}

impl Layer {
    pub fn common(&self) -> &LayerCommon {
        match self {
            Self::Group(layer) => &layer.common,
            Self::Pixel(layer) => &layer.common,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{AlphaMode, ColorSpec, PixelFormat};

    fn canvas() -> CanvasSpec {
        CanvasSpec {
            width: 64,
            height: 64,
            pixel: PixelSpec::new(PixelFormat::Rgba8, AlphaMode::Straight, ColorSpec::Srgb),
        }
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
    fn document_rejects_duplicate_nested_ids() -> PpResult<()> {
        let artifact = ArtifactRef::from_bytes("image/png", b"pixel")?;
        let layers = vec![
            Layer::Pixel(PixelLayer {
                common: common("same"),
                artifact: artifact.clone(),
                raster_mask: None,
            }),
            Layer::Group(GroupLayer {
                common: common("group"),
                children: vec![Layer::Pixel(PixelLayer {
                    common: common("same"),
                    artifact,
                    raster_mask: None,
                })],
            }),
        ];
        assert!(Document::new(canvas(), layers).is_err());
        Ok(())
    }

    #[test]
    fn document_serialization_is_format_independent() -> PpResult<()> {
        let document = Document::new(canvas(), Vec::new())?;
        let json = serde_json::to_value(document).map_err(|source| PpError::Json {
            path: "<document-test>".into(),
            message: source.to_string(),
        })?;
        assert_eq!(json["schema"], DOCUMENT_SCHEMA);
        assert!(json.get("psd").is_none());
        Ok(())
    }
}
