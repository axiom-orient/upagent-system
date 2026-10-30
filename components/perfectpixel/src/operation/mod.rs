use std::{
    num::{NonZeroU32, NonZeroUsize},
    path::PathBuf,
    time::Duration,
};

use serde::{Deserialize, Serialize};

use crate::{ResampleFilter, SvgProfile, UnitScore, VectorDetail, VectorPresetSelection};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SideEffectClass {
    None,
    Read,
    Publish,
    ExternalProcess,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationRisk {
    None,
    LocalRead,
    LocalMutation,
    ExternalExecution,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OperationSpec {
    pub name: &'static str,
    pub summary: &'static str,
    pub side_effect: SideEffectClass,
    pub risk: OperationRisk,
    pub timeout: Option<Duration>,
    pub capabilities: &'static [&'static str],
}

const CAP_NONE: &[&str] = &[];
const CAP_READ: &[&str] = &["filesystem.read"];
const CAP_PUBLISH: &[&str] = &["filesystem.read", "filesystem.publish"];
const CAP_EXTERNAL_PUBLISH: &[&str] = &["filesystem.read", "filesystem.publish", "process.spawn"];

const SCHEMA_SPEC: OperationSpec = spec(
    "system.schema",
    "Return the product schema",
    SideEffectClass::None,
    OperationRisk::None,
    None,
    CAP_NONE,
);
const INSPECT_SPEC: OperationSpec = spec(
    "image.inspect",
    "Inspect one raster artifact",
    SideEffectClass::Read,
    OperationRisk::LocalRead,
    None,
    CAP_READ,
);
const CONVERT_SPEC: OperationSpec = spec(
    "image.convert",
    "Convert or resize one raster artifact",
    SideEffectClass::Publish,
    OperationRisk::LocalMutation,
    None,
    CAP_PUBLISH,
);
const UPSCALE_SPEC: OperationSpec = spec(
    "image.upscale",
    "Integer-upscale one raster artifact",
    SideEffectClass::Publish,
    OperationRisk::LocalMutation,
    None,
    CAP_PUBLISH,
);
const EDIT_SPEC: OperationSpec = spec(
    "image.edit",
    "Apply deterministic raster edits",
    SideEffectClass::Publish,
    OperationRisk::LocalMutation,
    None,
    CAP_PUBLISH,
);
const EXPORT_PSD_SPEC: OperationSpec = spec(
    "document.export_psd",
    "Export the compatibility flattened PSD contract",
    SideEffectClass::Publish,
    OperationRisk::LocalMutation,
    None,
    CAP_PUBLISH,
);
const COMPILE_DOCUMENT_PSD_SPEC: OperationSpec = spec(
    "document.compile_psd",
    "Compile DocumentIR to deterministic layered PSD",
    SideEffectClass::Publish,
    OperationRisk::LocalMutation,
    None,
    CAP_PUBLISH,
);
const CHROMA_PLAN_SPEC: OperationSpec = spec(
    "image.chroma_plan",
    "Plan a controlled chroma background",
    SideEffectClass::Read,
    OperationRisk::LocalRead,
    None,
    CAP_READ,
);
const NORMALIZE_SPRITE_SPEC: OperationSpec = spec(
    "sprite.normalize",
    "Normalize sprite frames",
    SideEffectClass::Publish,
    OperationRisk::LocalMutation,
    None,
    CAP_PUBLISH,
);
const COMPILE_SPRITE_SPEC: OperationSpec = spec(
    "sprite.compile",
    "Compile a sprite atlas bundle",
    SideEffectClass::Publish,
    OperationRisk::LocalMutation,
    None,
    CAP_PUBLISH,
);
const COMPILE_TEXTURE_SPEC: OperationSpec = spec(
    "texture.compile",
    "Compile and verify a KTX2 texture artifact",
    SideEffectClass::ExternalProcess,
    OperationRisk::ExternalExecution,
    Some(Duration::from_secs(120)),
    CAP_EXTERNAL_PUBLISH,
);
const COMPILE_VECTOR_SPEC: OperationSpec = spec(
    "vector.compile",
    "Compile a quality-gated SVG",
    SideEffectClass::Publish,
    OperationRisk::LocalMutation,
    None,
    CAP_PUBLISH,
);
// `vector-analyze` is read-only only when no report path is requested. Operation metadata is
// deliberately conservative because the registry is the capability/risk authority for the whole
// operation, not a transport-specific guess about one invocation.
const ANALYZE_VECTOR_SPEC: OperationSpec = spec(
    "vector.analyze",
    "Analyze vectorization and optionally publish a report",
    SideEffectClass::Publish,
    OperationRisk::LocalMutation,
    None,
    CAP_PUBLISH,
);
const APPLE_VISION_FOREGROUND_SPEC: OperationSpec = spec(
    "vision.apple.foreground_instances",
    "Generate Apple Vision foreground mask candidates and verify publication",
    SideEffectClass::ExternalProcess,
    OperationRisk::ExternalExecution,
    Some(Duration::from_secs(120)),
    CAP_EXTERNAL_PUBLISH,
);
const SCAFFOLD_MOTION_SPEC: OperationSpec = spec(
    "motion.scaffold",
    "Scaffold motion metadata from SVG",
    SideEffectClass::Publish,
    OperationRisk::LocalMutation,
    None,
    CAP_PUBLISH,
);
const COMPILE_MOTION_SPEC: OperationSpec = spec(
    "motion.compile",
    "Compile deterministic motion artifacts",
    SideEffectClass::Publish,
    OperationRisk::LocalMutation,
    None,
    CAP_PUBLISH,
);

const OPERATION_SPECS: &[OperationSpec] = &[
    SCHEMA_SPEC,
    INSPECT_SPEC,
    CONVERT_SPEC,
    UPSCALE_SPEC,
    EDIT_SPEC,
    EXPORT_PSD_SPEC,
    COMPILE_DOCUMENT_PSD_SPEC,
    CHROMA_PLAN_SPEC,
    NORMALIZE_SPRITE_SPEC,
    COMPILE_SPRITE_SPEC,
    COMPILE_TEXTURE_SPEC,
    COMPILE_VECTOR_SPEC,
    ANALYZE_VECTOR_SPEC,
    APPLE_VISION_FOREGROUND_SPEC,
    SCAFFOLD_MOTION_SPEC,
    COMPILE_MOTION_SPEC,
];

pub fn operation_specs() -> &'static [OperationSpec] {
    OPERATION_SPECS
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JpegQuality(u8);

impl JpegQuality {
    pub fn new(value: u8) -> Result<Self, OperationInputError> {
        if (1..=100).contains(&value) {
            Ok(Self(value))
        } else {
            Err(OperationInputError::new("jpeg quality must be in 1..=100"))
        }
    }

    pub fn get(self) -> u8 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScaleFactor(NonZeroU32);

impl ScaleFactor {
    pub fn new(value: u32) -> Result<Self, OperationInputError> {
        let value = NonZeroU32::new(value)
            .filter(|value| value.get() >= 2)
            .ok_or_else(|| OperationInputError::new("upscale factor must be >= 2"))?;
        Ok(Self(value))
    }

    pub fn get(self) -> u32 {
        self.0.get()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationInputError {
    message: String,
}

impl OperationInputError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for OperationInputError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for OperationInputError {}

pub fn parse_resample_filter(value: &str) -> Result<ResampleFilter, OperationInputError> {
    match value {
        "nearest" => Ok(ResampleFilter::Nearest),
        "lanczos3" => Ok(ResampleFilter::Lanczos3),
        _ => Err(OperationInputError::new(
            "filter must be nearest or lanczos3",
        )),
    }
}

pub fn parse_vector_preset(value: &str) -> Result<VectorPresetSelection, OperationInputError> {
    match value {
        "auto" => Ok(VectorPresetSelection::Auto),
        "pixel-art" => Ok(VectorPresetSelection::PixelArt),
        "exact-grid" => Ok(VectorPresetSelection::ExactGrid),
        "flat-icon" => Ok(VectorPresetSelection::FlatIcon),
        "line-art" => Ok(VectorPresetSelection::LineArt),
        "bounded-illustration" => Ok(VectorPresetSelection::BoundedIllustration),
        _ => Err(OperationInputError::new(
            "preset must be auto, pixel-art, exact-grid, flat-icon, line-art, or bounded-illustration",
        )),
    }
}

pub fn parse_vector_profile(value: &str) -> Result<SvgProfile, OperationInputError> {
    match value {
        "compact" => Ok(SvgProfile::Compact),
        "motion-structure-ready" => Ok(SvgProfile::MotionStructureReady),
        _ => Err(OperationInputError::new(
            "profile must be compact or motion-structure-ready",
        )),
    }
}

pub fn parse_vector_detail(value: &str) -> Result<Option<VectorDetail>, OperationInputError> {
    if value == "auto" {
        return Ok(None);
    }
    let value = value.parse::<u8>().map_err(|_| {
        OperationInputError::new("--detail must be auto or an integer from 1 through 5")
    })?;
    VectorDetail::new(value).map(Some).map_err(|_| {
        OperationInputError::new("--detail must be auto or an integer from 1 through 5")
    })
}

pub fn parse_unit_score(value: &str) -> Result<UnitScore, OperationInputError> {
    let parsed = value.parse::<f64>().map_err(|_| {
        OperationInputError::new("quality threshold must be a finite number from 0 through 1")
    })?;
    UnitScore::new(parsed).map_err(|error| OperationInputError::new(error.to_string()))
}

pub enum Operation {
    Schema,
    Inspect {
        input: PathBuf,
    },
    Convert {
        input: PathBuf,
        output: PathBuf,
        width: Option<NonZeroU32>,
        height: Option<NonZeroU32>,
        filter: Option<ResampleFilter>,
        jpeg_quality: Option<JpegQuality>,
        background: Option<[u8; 3]>,
    },
    Upscale {
        input: PathBuf,
        output: PathBuf,
        scale: ScaleFactor,
        filter: Option<ResampleFilter>,
        jpeg_quality: Option<JpegQuality>,
        background: Option<[u8; 3]>,
    },
    Edit {
        request: PathBuf,
    },
    ExportPsd {
        request: PathBuf,
    },
    CompileDocumentPsd {
        request: PathBuf,
    },
    ChromaPlan {
        request: PathBuf,
    },
    NormalizeSprite {
        request: PathBuf,
        output_dir: PathBuf,
    },
    CompileSprite {
        request: PathBuf,
        output_dir: PathBuf,
    },
    CompileTexture {
        request: PathBuf,
    },
    CompileVector {
        input: PathBuf,
        output: PathBuf,
        preset: VectorPresetSelection,
        profile: SvgProfile,
        detail: Option<VectorDetail>,
        minimum_quality: Option<UnitScore>,
        maximum_quality_loss: Option<UnitScore>,
        maximum_paths: Option<NonZeroUsize>,
        policy: Option<PathBuf>,
        report: Option<PathBuf>,
        diagnostics: Option<PathBuf>,
    },
    AnalyzeVector {
        input: PathBuf,
        preset: VectorPresetSelection,
        profile: SvgProfile,
        policy: Option<PathBuf>,
        report: Option<PathBuf>,
    },
    AppleVisionForegroundInstances {
        request: PathBuf,
    },
    ScaffoldMotion {
        input: PathBuf,
        output_dir: PathBuf,
    },
    CompileMotion {
        request: PathBuf,
        output_dir: PathBuf,
    },
}

impl Operation {
    pub const fn spec(&self) -> OperationSpec {
        match self {
            Self::Schema => SCHEMA_SPEC,
            Self::Inspect { .. } => INSPECT_SPEC,
            Self::Convert { .. } => CONVERT_SPEC,
            Self::Upscale { .. } => UPSCALE_SPEC,
            Self::Edit { .. } => EDIT_SPEC,
            Self::ExportPsd { .. } => EXPORT_PSD_SPEC,
            Self::CompileDocumentPsd { .. } => COMPILE_DOCUMENT_PSD_SPEC,
            Self::ChromaPlan { .. } => CHROMA_PLAN_SPEC,
            Self::NormalizeSprite { .. } => NORMALIZE_SPRITE_SPEC,
            Self::CompileSprite { .. } => COMPILE_SPRITE_SPEC,
            Self::CompileTexture { .. } => COMPILE_TEXTURE_SPEC,
            Self::CompileVector { .. } => COMPILE_VECTOR_SPEC,
            Self::AnalyzeVector { .. } => ANALYZE_VECTOR_SPEC,
            Self::AppleVisionForegroundInstances { .. } => APPLE_VISION_FOREGROUND_SPEC,
            Self::ScaffoldMotion { .. } => SCAFFOLD_MOTION_SPEC,
            Self::CompileMotion { .. } => COMPILE_MOTION_SPEC,
        }
    }
}

const fn spec(
    name: &'static str,
    summary: &'static str,
    side_effect: SideEffectClass,
    risk: OperationRisk,
    timeout: Option<Duration>,
    capabilities: &'static [&'static str],
) -> OperationSpec {
    OperationSpec {
        name,
        summary,
        side_effect,
        risk,
        timeout,
        capabilities,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationErrorCode {
    InvalidArgument,
    Unsupported,
    NotFound,
    Conflict,
    PreconditionFailed,
    ResourceLimit,
    Timeout,
    Cancelled,
    DependencyFailed,
    VerificationFailed,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperationFailure {
    pub code: OperationErrorCode,
    pub operation: String,
    pub cause: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub context: Vec<FailureContext>,
    pub retryable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FailureContext {
    pub key: String,
    pub value: String,
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn bounded_scalars_reject_invalid_state() {
        assert!(JpegQuality::new(0).is_err());
        assert!(JpegQuality::new(101).is_err());
        assert_eq!(JpegQuality::new(85).expect("valid").get(), 85);
        assert!(ScaleFactor::new(1).is_err());
        assert_eq!(ScaleFactor::new(2).expect("valid").get(), 2);
    }

    #[test]
    fn operation_registry_has_one_unique_owner() {
        let specs = operation_specs();
        let names = specs.iter().map(|spec| spec.name).collect::<BTreeSet<_>>();
        assert_eq!(names.len(), specs.len());
        assert_eq!(specs.len(), 16);
        assert!(specs.contains(&COMPILE_DOCUMENT_PSD_SPEC));
        assert!(specs.contains(&COMPILE_TEXTURE_SPEC));
        assert!(specs.contains(&APPLE_VISION_FOREGROUND_SPEC));
    }

    #[test]
    fn operation_metadata_matches_reachable_effects() {
        let inspect = Operation::Inspect {
            input: "a.png".into(),
        }
        .spec();
        assert_eq!(inspect.side_effect, SideEffectClass::Read);
        assert_eq!(inspect.risk, OperationRisk::LocalRead);
        assert_eq!(inspect.capabilities, &["filesystem.read"]);

        let analyze = Operation::AnalyzeVector {
            input: "a.png".into(),
            preset: VectorPresetSelection::Auto,
            profile: SvgProfile::Compact,
            policy: None,
            report: None,
        }
        .spec();
        assert_eq!(analyze.side_effect, SideEffectClass::Publish);
        assert_eq!(analyze.risk, OperationRisk::LocalMutation);
        assert_eq!(
            analyze.capabilities,
            &["filesystem.read", "filesystem.publish"]
        );
    }

    #[test]
    fn canonical_parsers_reject_invalid_values() {
        assert!(parse_resample_filter("bilinear").is_err());
        assert!(parse_vector_preset("unknown").is_err());
        assert!(parse_vector_profile("unknown").is_err());
        assert!(parse_vector_detail("6").is_err());
        assert!(parse_unit_score("nan").is_err());
    }
}
