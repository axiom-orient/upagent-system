use serde::{Deserialize, Serialize};

use super::Sha256Digest;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PixelFormat {
    Rgba8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlphaMode {
    Opaque,
    Straight,
    Premultiplied,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ColorSpec {
    Srgb,
    LinearSrgb,
    DisplayP3,
    Icc { digest: Sha256Digest },
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PixelSpec {
    pub pixel_format: PixelFormat,
    pub alpha: AlphaMode,
    pub color: ColorSpec,
}

impl PixelSpec {
    pub fn new(pixel_format: PixelFormat, alpha: AlphaMode, color: ColorSpec) -> Self {
        Self {
            pixel_format,
            alpha,
            color,
        }
    }
}

/// Parses the one transport-neutral textual RGB literal accepted by PerfectPixel.
/// The returned bytes are explicitly sRGB8 channel codes; no color-space inference occurs.
pub fn parse_srgb8_hex(raw: &str) -> Option<[u8; 3]> {
    let hex = raw.strip_prefix('#')?;
    if hex.len() != 6 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let channel = |range| u8::from_str_radix(&hex[range], 16).ok();
    Some([channel(0..2)?, channel(2..4)?, channel(4..6)?])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixel_spec_requires_explicit_color_semantics() {
        let spec = PixelSpec::new(PixelFormat::Rgba8, AlphaMode::Straight, ColorSpec::Unknown);
        assert_eq!(spec.pixel_format, PixelFormat::Rgba8);
        assert_eq!(spec.alpha, AlphaMode::Straight);
        assert_eq!(spec.color, ColorSpec::Unknown);
    }

    #[test]
    fn srgb8_hex_parser_is_strict_and_case_insensitive() {
        assert_eq!(parse_srgb8_hex("#00aAFF"), Some([0, 170, 255]));
        assert_eq!(parse_srgb8_hex("00aaff"), None);
        assert_eq!(parse_srgb8_hex("#fff"), None);
        assert_eq!(parse_srgb8_hex("#zz0000"), None);
    }
}
