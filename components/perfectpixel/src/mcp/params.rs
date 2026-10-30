use rmcp::schemars::JsonSchema;
use serde::Deserialize;

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum AssetFilter {
    Nearest,
    Lanczos3,
}

impl AssetFilter {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Nearest => "nearest",
            Self::Lanczos3 => "lanczos3",
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct SchemaParams {}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct InspectParams {
    /// Root-relative UTF-8 slash path to an existing regular PNG, JPEG, or WebP file.
    pub(crate) input_path: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ConvertParams {
    /// Root-relative UTF-8 slash path to an existing regular PNG, JPEG, or WebP file.
    pub(crate) input_path: String,
    /// Root-relative UTF-8 slash path to the output image file.
    pub(crate) output_path: String,
    /// Optional output width. Canonical Operation validation owns positivity.
    pub(crate) width: Option<u32>,
    /// Optional output height. Canonical Operation validation owns positivity.
    pub(crate) height: Option<u32>,
    /// Resampling filter transport enum.
    pub(crate) filter: Option<AssetFilter>,
    /// JPEG quality. Canonical Operation validation owns the accepted range.
    pub(crate) jpeg_quality: Option<u8>,
    /// JPEG matte as #RRGGBB; semantic validity belongs to the Operation/application handler.
    pub(crate) background: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct UpscaleParams {
    pub(crate) input_path: String,
    pub(crate) output_path: String,
    /// Integer scale; canonical Operation validation owns the accepted range.
    pub(crate) scale: u32,
    pub(crate) filter: Option<AssetFilter>,
    pub(crate) jpeg_quality: Option<u8>,
    pub(crate) background: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RequestDirectoryParams {
    pub(crate) request_path: String,
    pub(crate) output_dir: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum VectorPreset {
    Auto,
    PixelArt,
    ExactGrid,
    FlatIcon,
    LineArt,
    BoundedIllustration,
}

impl VectorPreset {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::PixelArt => "pixel-art",
            Self::ExactGrid => "exact-grid",
            Self::FlatIcon => "flat-icon",
            Self::LineArt => "line-art",
            Self::BoundedIllustration => "bounded-illustration",
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum VectorProfile {
    Compact,
    MotionStructureReady,
}

impl VectorProfile {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Compact => "compact",
            Self::MotionStructureReady => "motion-structure-ready",
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) enum VectorDetail {
    #[serde(rename = "auto")]
    #[schemars(rename = "auto")]
    Auto,
    #[serde(rename = "1")]
    #[schemars(rename = "1")]
    One,
    #[serde(rename = "2")]
    #[schemars(rename = "2")]
    Two,
    #[serde(rename = "3")]
    #[schemars(rename = "3")]
    Three,
    #[serde(rename = "4")]
    #[schemars(rename = "4")]
    Four,
    #[serde(rename = "5")]
    #[schemars(rename = "5")]
    Five,
}

impl VectorDetail {
    pub(crate) fn as_cli_value(&self) -> Option<&'static str> {
        match self {
            Self::Auto => None,
            Self::One => Some("1"),
            Self::Two => Some("2"),
            Self::Three => Some("3"),
            Self::Four => Some("4"),
            Self::Five => Some("5"),
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct VectorParams {
    pub(crate) input_path: String,
    pub(crate) output_path: String,
    pub(crate) preset: Option<VectorPreset>,
    pub(crate) profile: Option<VectorProfile>,
    pub(crate) detail: Option<VectorDetail>,
    pub(crate) min_quality: Option<f64>,
    pub(crate) max_quality_loss: Option<f64>,
    pub(crate) max_paths: Option<usize>,
    pub(crate) policy_path: Option<String>,
    pub(crate) report_path: Option<String>,
    pub(crate) diagnostics_dir: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct VectorAnalyzeParams {
    pub(crate) input_path: String,
    pub(crate) preset: Option<VectorPreset>,
    pub(crate) profile: Option<VectorProfile>,
    pub(crate) policy_path: Option<String>,
    pub(crate) report_path: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct MotionScaffoldParams {
    pub(crate) input_path: String,
    pub(crate) output_dir: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct MotionBuildParams {
    pub(crate) request_path: String,
    pub(crate) output_dir: String,
}
