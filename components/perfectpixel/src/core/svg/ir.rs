use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

#[derive(Debug, Clone, Copy)]
pub struct SvgLimits {
    pub max_bytes: usize,
    pub max_depth: usize,
    pub max_elements: usize,
    pub max_attributes: usize,
    pub max_tokens: usize,
    pub max_coordinates: usize,
    pub max_path_segments: usize,
    /// Maximum UTF-8 byte length of an SVG `id` attribute.
    pub max_id_bytes: usize,
}
impl SvgLimits {
    pub(crate) fn clamped_to_hard_maxima(self) -> Self {
        let hard_maxima = Self::default();
        Self {
            max_bytes: self.max_bytes.min(hard_maxima.max_bytes),
            max_depth: self.max_depth.min(hard_maxima.max_depth),
            max_elements: self.max_elements.min(hard_maxima.max_elements),
            max_attributes: self.max_attributes.min(hard_maxima.max_attributes),
            max_tokens: self.max_tokens.min(hard_maxima.max_tokens),
            max_coordinates: self.max_coordinates.min(hard_maxima.max_coordinates),
            max_path_segments: self.max_path_segments.min(hard_maxima.max_path_segments),
            max_id_bytes: self.max_id_bytes.min(hard_maxima.max_id_bytes),
        }
    }
}

impl Default for SvgLimits {
    fn default() -> Self {
        Self {
            max_bytes: 1_048_576,
            max_depth: 64,
            max_elements: 16_384,
            max_attributes: 65_536,
            max_tokens: 262_144,
            max_coordinates: 262_144,
            max_path_segments: 131_072,
            max_id_bytes: 256,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SvgIr {
    pub source: String,
    pub root: SvgRoot,
    pub elements: Vec<SvgElement>,
    pub ids: BTreeSet<String>,
    pub references: Vec<String>,
    pub token_count: usize,
    pub coordinate_count: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SvgRoot {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub view_box: Option<[f64; 4]>,
    pub closing_tag: Range<usize>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SvgElement {
    pub local_name: String,
    pub range: Range<usize>,
    pub opening_tag: Range<usize>,
    pub attributes: BTreeMap<String, String>,
    pub attribute_ranges: BTreeMap<String, SvgAttributeRange>,
    pub inherited_opacity: f64,
    pub transform: Transform,
    pub paint: Option<PaintFacts>,
    pub path: Option<PathFacts>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SvgAttributeRange {
    pub attribute: Range<usize>,
    pub value: Range<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transform {
    pub matrix: [f64; 6],
    pub translation_only: bool,
}

impl Transform {
    pub const IDENTITY: Self = Self {
        matrix: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
        translation_only: true,
    };
}

#[derive(Debug, Clone, PartialEq)]
pub struct PaintFacts {
    pub fill: Option<String>,
    pub stroke: Option<String>,
    pub opacity: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PathFacts {
    pub commands: Vec<PathCommand>,
    pub segments: Vec<svgtypes::PathSegment>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathCommand {
    Move,
    Line,
    Curve,
    Close,
}
#[cfg(test)]
mod tests {
    use super::SvgLimits;

    #[test]
    fn caller_limits_cannot_relax_hard_maxima() {
        let limits = SvgLimits {
            max_bytes: usize::MAX,
            max_depth: usize::MAX,
            max_elements: usize::MAX,
            max_attributes: usize::MAX,
            max_tokens: usize::MAX,
            max_coordinates: usize::MAX,
            max_path_segments: usize::MAX,
            max_id_bytes: usize::MAX,
        }
        .clamped_to_hard_maxima();

        assert_eq!(limits.max_bytes, SvgLimits::default().max_bytes);
        assert_eq!(limits.max_depth, SvgLimits::default().max_depth);
        assert_eq!(limits.max_elements, SvgLimits::default().max_elements);
        assert_eq!(limits.max_attributes, SvgLimits::default().max_attributes);
        assert_eq!(limits.max_tokens, SvgLimits::default().max_tokens);
        assert_eq!(limits.max_coordinates, SvgLimits::default().max_coordinates);
        assert_eq!(
            limits.max_path_segments,
            SvgLimits::default().max_path_segments
        );
        assert_eq!(limits.max_id_bytes, SvgLimits::default().max_id_bytes);
    }
}
