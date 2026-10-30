use std::fmt;
use std::num::NonZeroUsize;

use serde::{Deserialize, Serialize};

/// A vector family selection. `Auto` may still require an explicit preset after analysis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum VectorPresetSelection {
    Auto,
    PixelArt,
    ExactGrid,
    FlatIcon,
    LineArt,
    BoundedIllustration,
}

/// The output-profile predicates requested for a vector document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SvgProfile {
    Compact,
    MotionStructureReady,
}

/// A validated candidate-detail preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct VectorDetail(u8);

impl VectorDetail {
    pub const MIN: u8 = 1;
    pub const MAX: u8 = 5;

    pub fn new(value: u8) -> Result<Self, RequestValidationError> {
        if (Self::MIN..=Self::MAX).contains(&value) {
            Ok(Self(value))
        } else {
            Err(RequestValidationError::DetailOutOfRange(value))
        }
    }

    pub const fn get(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for VectorDetail {
    type Error = RequestValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl<'de> Deserialize<'de> for VectorDetail {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Self::new(u8::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// A finite score in the inclusive unit interval.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct UnitScore(f64);

impl UnitScore {
    pub fn new(value: f64) -> Result<Self, RequestValidationError> {
        if !value.is_finite() {
            return Err(RequestValidationError::ScoreNotFinite);
        }
        if !(0.0..=1.0).contains(&value) {
            return Err(RequestValidationError::ScoreOutOfRange(value));
        }
        Ok(Self(value))
    }

    pub const fn get(self) -> f64 {
        self.0
    }
}

impl TryFrom<f64> for UnitScore {
    type Error = RequestValidationError;

    fn try_from(value: f64) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl<'de> Deserialize<'de> for UnitScore {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Self::new(f64::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// Validated request-input errors. These errors never expose or alter embedded authority.
#[derive(Debug, Clone, PartialEq)]
pub enum RequestValidationError {
    DetailOutOfRange(u8),
    ScoreNotFinite,
    ScoreOutOfRange(f64),
    InvalidPolicySchema(String),
    EmptyPolicyVersion,
    EmptyPaletteColor,
    MalformedPaletteColor(String),
    DuplicatePaletteColor(String),
    RequiredPaletteNotAllowed(String),
    DiagnosticsNotRequested,
    TooManyDiagnosticArtifacts,
    InvalidDiagnosticArtifactKind(String),
    DuplicateDiagnosticArtifactKind(String),
}

impl fmt::Display for RequestValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DetailOutOfRange(value) => {
                write!(formatter, "vector detail {value} is outside 1..=5")
            }
            Self::ScoreNotFinite => formatter.write_str("unit score must be finite"),
            Self::ScoreOutOfRange(value) => {
                write!(formatter, "unit score {value} is outside 0..=1")
            }
            Self::InvalidPolicySchema(schema) => {
                write!(
                    formatter,
                    "policy schema must be '{}', got '{schema}'",
                    VectorPolicy::SCHEMA
                )
            }
            Self::EmptyPolicyVersion => formatter.write_str("policy version must not be empty"),
            Self::EmptyPaletteColor => formatter.write_str("palette colors must not be empty"),
            Self::MalformedPaletteColor(color) => write!(
                formatter,
                "palette color {color} must use canonical #RRGGBB hexadecimal syntax"
            ),
            Self::DuplicatePaletteColor(color) => {
                write!(formatter, "duplicate palette color {color}")
            }
            Self::RequiredPaletteNotAllowed(color) => {
                write!(formatter, "required palette color {color} is not allowed")
            }
            Self::DiagnosticsNotRequested => {
                formatter.write_str("diagnostic artifact kinds require diagnostics to be requested")
            }
            Self::TooManyDiagnosticArtifacts => {
                formatter.write_str("too many diagnostic artifact kinds")
            }
            Self::InvalidDiagnosticArtifactKind(kind) => {
                write!(formatter, "invalid diagnostic artifact kind {kind}")
            }
            Self::DuplicateDiagnosticArtifactKind(kind) => {
                write!(formatter, "duplicate diagnostic artifact kind {kind}")
            }
        }
    }
}

impl std::error::Error for RequestValidationError {}

/// Selection and tightening policy input. It intentionally contains no routes, thresholds,
/// registry identities, backend identities, or authority digests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VectorPolicy {
    schema: String,
    version: String,
    allowed_palette: Vec<String>,
    required_palette: Vec<String>,
    reject_unmapped: bool,
    allow_drop_noise: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RawVectorPolicy {
    schema: String,
    version: String,
    allowed_palette: Vec<String>,
    required_palette: Vec<String>,
    reject_unmapped: bool,
    allow_drop_noise: bool,
}

impl<'de> Deserialize<'de> for VectorPolicy {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = RawVectorPolicy::deserialize(deserializer)?;
        let policy = Self {
            schema: raw.schema,
            version: raw.version,
            allowed_palette: raw.allowed_palette,
            required_palette: raw.required_palette,
            reject_unmapped: raw.reject_unmapped,
            allow_drop_noise: raw.allow_drop_noise,
        };
        policy.validate().map_err(serde::de::Error::custom)?;
        Ok(policy)
    }
}

impl VectorPolicy {
    pub const SCHEMA: &'static str = "perfectpixel.vector-policy/1";

    pub fn new(
        version: impl Into<String>,
        allowed_palette: Vec<String>,
        required_palette: Vec<String>,
        reject_unmapped: bool,
        allow_drop_noise: bool,
    ) -> Result<Self, RequestValidationError> {
        let policy = Self {
            schema: Self::SCHEMA.to_owned(),
            version: version.into(),
            allowed_palette,
            required_palette,
            reject_unmapped,
            allow_drop_noise,
        };
        policy.validate()?;
        Ok(policy)
    }

    pub fn default_selection() -> Self {
        Self {
            schema: Self::SCHEMA.to_owned(),
            version: "default".to_owned(),
            allowed_palette: Vec::new(),
            required_palette: Vec::new(),
            reject_unmapped: false,
            allow_drop_noise: false,
        }
    }

    pub fn validate(&self) -> Result<(), RequestValidationError> {
        if self.schema != Self::SCHEMA {
            return Err(RequestValidationError::InvalidPolicySchema(
                self.schema.clone(),
            ));
        }
        if self.version.trim().is_empty() {
            return Err(RequestValidationError::EmptyPolicyVersion);
        }
        let allowed = validate_palette(&self.allowed_palette)?;
        let required = validate_palette(&self.required_palette)?;
        for color in &required {
            if !allowed.is_empty() && !allowed.contains(color) {
                return Err(RequestValidationError::RequiredPaletteNotAllowed(
                    color.clone(),
                ));
            }
        }
        Ok(())
    }

    pub fn schema(&self) -> &str {
        &self.schema
    }
    pub fn version(&self) -> &str {
        &self.version
    }
    pub fn allowed_palette(&self) -> &[String] {
        &self.allowed_palette
    }
    pub fn required_palette(&self) -> &[String] {
        &self.required_palette
    }
    pub const fn reject_unmapped(&self) -> bool {
        self.reject_unmapped
    }
    pub const fn allow_drop_noise(&self) -> bool {
        self.allow_drop_noise
    }
}

impl Default for VectorPolicy {
    fn default() -> Self {
        Self::default_selection()
    }
}

fn validate_palette(
    colors: &[String],
) -> Result<std::collections::BTreeSet<String>, RequestValidationError> {
    let mut seen = std::collections::BTreeSet::new();
    for color in colors {
        if color.trim().is_empty() {
            return Err(RequestValidationError::EmptyPaletteColor);
        }
        let canonical = canonical_palette_color(color)
            .ok_or_else(|| RequestValidationError::MalformedPaletteColor(color.clone()))?;
        if !seen.insert(canonical.clone()) {
            return Err(RequestValidationError::DuplicatePaletteColor(canonical));
        }
    }
    Ok(seen)
}

/// Returns the lowercase ASCII representation used for policy and SVG paint comparisons.
pub(super) fn canonical_palette_color(color: &str) -> Option<String> {
    if color.len() != 7
        || !color.starts_with('#')
        || !color.as_bytes()[1..].iter().all(u8::is_ascii_hexdigit)
    {
        return None;
    }
    Some(color.to_ascii_lowercase())
}

/// Whether immutable diagnostic evidence should be requested for a generation outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiagnosticsIntent {
    requested: bool,
    artifact_kinds: Vec<String>,
}

impl DiagnosticsIntent {
    const MAX_ARTIFACT_KINDS: usize = 16;
    pub const CANDIDATE_SVG: &'static str = "candidate-svg";
    pub const RENDER_BACK: &'static str = "render-back";

    pub fn none() -> Self {
        Self {
            requested: false,
            artifact_kinds: Vec::new(),
        }
    }

    pub fn requested(artifact_kinds: Vec<String>) -> Result<Self, RequestValidationError> {
        let intent = Self {
            requested: true,
            artifact_kinds,
        };
        intent.validate()?;
        Ok(intent)
    }

    fn validate(&self) -> Result<(), RequestValidationError> {
        if !self.requested && !self.artifact_kinds.is_empty() {
            return Err(RequestValidationError::DiagnosticsNotRequested);
        }
        if self.artifact_kinds.len() > Self::MAX_ARTIFACT_KINDS {
            return Err(RequestValidationError::TooManyDiagnosticArtifacts);
        }
        let mut kinds = std::collections::BTreeSet::new();
        for kind in &self.artifact_kinds {
            if !matches!(kind.as_str(), Self::CANDIDATE_SVG | Self::RENDER_BACK) {
                return Err(RequestValidationError::InvalidDiagnosticArtifactKind(
                    kind.clone(),
                ));
            }
            if !kinds.insert(kind.as_str()) {
                return Err(RequestValidationError::DuplicateDiagnosticArtifactKind(
                    kind.clone(),
                ));
            }
        }
        Ok(())
    }

    pub const fn is_requested(&self) -> bool {
        self.requested
    }
    pub fn artifact_kinds(&self) -> &[String] {
        &self.artifact_kinds
    }
}

impl<'de> Deserialize<'de> for DiagnosticsIntent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct RawDiagnosticsIntent {
            requested: bool,
            artifact_kinds: Vec<String>,
        }

        let raw = RawDiagnosticsIntent::deserialize(deserializer)?;
        let intent = Self {
            requested: raw.requested,
            artifact_kinds: raw.artifact_kinds,
        };
        intent.validate().map_err(serde::de::Error::custom)?;
        Ok(intent)
    }
}

impl Default for DiagnosticsIntent {
    fn default() -> Self {
        Self::none()
    }
}

/// Analysis accepts only controls that cannot cause candidate generation or publication.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VectorAnalysisRequest {
    preset: VectorPresetSelection,
    profile: SvgProfile,
    policy: VectorPolicy,
}

impl VectorAnalysisRequest {
    pub fn new(
        preset: VectorPresetSelection,
        profile: SvgProfile,
        policy: VectorPolicy,
    ) -> Result<Self, RequestValidationError> {
        policy.validate()?;
        Ok(Self {
            preset,
            profile,
            policy,
        })
    }

    pub const fn preset(&self) -> VectorPresetSelection {
        self.preset
    }
    pub const fn profile(&self) -> SvgProfile {
        self.profile
    }
    pub fn policy(&self) -> &VectorPolicy {
        &self.policy
    }
}

/// A validated generation request. Numeric controls can only tighten embedded requirements.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VectorRequest {
    pub preset: VectorPresetSelection,
    pub profile: SvgProfile,
    pub detail: Option<VectorDetail>,
    pub minimum_quality: Option<UnitScore>,
    pub maximum_quality_loss: Option<UnitScore>,
    pub maximum_paths: Option<NonZeroUsize>,
    pub policy: VectorPolicy,
    pub diagnostics: DiagnosticsIntent,
}

impl VectorRequest {
    // Parameters are exactly this value type's fields and the body is a pure move, so
    // clippy's argument-count heuristic does not apply: grouping them would mean a second
    // struct with identical fields.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        preset: VectorPresetSelection,
        profile: SvgProfile,
        detail: Option<VectorDetail>,
        minimum_quality: Option<UnitScore>,
        maximum_quality_loss: Option<UnitScore>,
        maximum_paths: Option<NonZeroUsize>,
        policy: VectorPolicy,
        diagnostics: DiagnosticsIntent,
    ) -> Result<Self, RequestValidationError> {
        policy.validate()?;
        diagnostics.validate()?;
        Ok(Self {
            preset,
            profile,
            detail,
            minimum_quality,
            maximum_quality_loss,
            maximum_paths,
            policy,
            diagnostics,
        })
    }

    pub const fn preset(&self) -> VectorPresetSelection {
        self.preset
    }
    pub const fn profile(&self) -> SvgProfile {
        self.profile
    }
    pub const fn detail(&self) -> Option<VectorDetail> {
        self.detail
    }
    pub const fn minimum_quality(&self) -> Option<UnitScore> {
        self.minimum_quality
    }
    pub const fn maximum_quality_loss(&self) -> Option<UnitScore> {
        self.maximum_quality_loss
    }
    pub const fn maximum_paths(&self) -> Option<NonZeroUsize> {
        self.maximum_paths
    }
    pub fn policy(&self) -> &VectorPolicy {
        &self.policy
    }
    pub fn diagnostics(&self) -> &DiagnosticsIntent {
        &self.diagnostics
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_accepts_case_insensitive_canonical_hex() {
        assert!(
            VectorPolicy::new("test", vec!["#A0B1C2".to_owned()], Vec::new(), false, false,)
                .is_ok()
        );
        assert!(
            VectorPolicy::new("test", vec!["#a0b1c2".to_owned()], Vec::new(), false, false,)
                .is_ok()
        );
        assert!(matches!(
            VectorPolicy::new("test", vec!["#a0b1cg".to_owned()], Vec::new(), false, false,),
            Err(RequestValidationError::MalformedPaletteColor(_))
        ));
    }

    #[test]
    fn diagnostics_intent_rejects_unrequested_artifacts_on_deserialization() {
        assert!(serde_json::from_str::<DiagnosticsIntent>(
            r#"{"requested":false,"artifactKinds":["candidate-svg"]}"#
        )
        .is_err());
    }
}
