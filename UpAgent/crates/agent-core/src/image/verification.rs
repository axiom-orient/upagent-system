use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::ArtifactRef;

use super::{
    IMAGE_VERIFICATION_SPEC_SCHEMA, MAX_IMAGE_COMPARE_ASSERTIONS, validate_image_artifact_ref,
};

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum ImageCompareArtifactKind {
    #[default]
    Raster,
    Object,
    Export,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ImageCompareSeverity {
    Required,
    Advisory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ImageCompareRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ImageCompareAssertionResult {
    pub id: String,
    pub assertion_type: String,
    pub severity: ImageCompareSeverity,
    pub passed: bool,
    pub observed: serde_json::Value,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ImageCompareAssertion {
    ExactEqual {
        id: String,
        severity: ImageCompareSeverity,
    },
    ChangedRatio {
        id: String,
        severity: ImageCompareSeverity,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        minimum: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        maximum: Option<f64>,
    },
    OutsideMaskChangedRatio {
        id: String,
        severity: ImageCompareSeverity,
        maximum: f64,
        mask_artifact: ArtifactRef,
    },
    InsideMaskChangedRatio {
        id: String,
        severity: ImageCompareSeverity,
        minimum: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        maximum: Option<f64>,
        mask_artifact: ArtifactRef,
    },
    UnchangedRegionExact {
        id: String,
        severity: ImageCompareSeverity,
        region: ImageCompareRect,
    },
    AlphaIou {
        id: String,
        severity: ImageCompareSeverity,
        minimum: f64,
        mask_artifact: ArtifactRef,
    },
    MaskLeakageRatio {
        id: String,
        severity: ImageCompareSeverity,
        maximum: f64,
        mask_artifact: ArtifactRef,
    },
    ObjectBounds {
        id: String,
        severity: ImageCompareSeverity,
        expected: ImageCompareRect,
        tolerance: u32,
    },
    ObjectCentroid {
        id: String,
        severity: ImageCompareSeverity,
        expected: [f64; 2],
        tolerance: f64,
    },
    ObjectArea {
        id: String,
        severity: ImageCompareSeverity,
        expected: u64,
        tolerance: u64,
    },
    MaximumChannelError {
        id: String,
        severity: ImageCompareSeverity,
        maximum: u8,
    },
    MeanAbsoluteError {
        id: String,
        severity: ImageCompareSeverity,
        maximum: f64,
    },
}

impl ImageCompareAssertion {
    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            Self::ExactEqual { id, .. }
            | Self::ChangedRatio { id, .. }
            | Self::OutsideMaskChangedRatio { id, .. }
            | Self::InsideMaskChangedRatio { id, .. }
            | Self::UnchangedRegionExact { id, .. }
            | Self::AlphaIou { id, .. }
            | Self::MaskLeakageRatio { id, .. }
            | Self::ObjectBounds { id, .. }
            | Self::ObjectCentroid { id, .. }
            | Self::ObjectArea { id, .. }
            | Self::MaximumChannelError { id, .. }
            | Self::MeanAbsoluteError { id, .. } => id,
        }
    }

    #[must_use]
    pub const fn severity(&self) -> ImageCompareSeverity {
        match self {
            Self::ExactEqual { severity, .. }
            | Self::ChangedRatio { severity, .. }
            | Self::OutsideMaskChangedRatio { severity, .. }
            | Self::InsideMaskChangedRatio { severity, .. }
            | Self::UnchangedRegionExact { severity, .. }
            | Self::AlphaIou { severity, .. }
            | Self::MaskLeakageRatio { severity, .. }
            | Self::ObjectBounds { severity, .. }
            | Self::ObjectCentroid { severity, .. }
            | Self::ObjectArea { severity, .. }
            | Self::MaximumChannelError { severity, .. }
            | Self::MeanAbsoluteError { severity, .. } => *severity,
        }
    }

    #[must_use]
    pub const fn assertion_type(&self) -> &'static str {
        match self {
            Self::ExactEqual { .. } => "exact_equal",
            Self::ChangedRatio { .. } => "changed_ratio",
            Self::OutsideMaskChangedRatio { .. } => "outside_mask_changed_ratio",
            Self::InsideMaskChangedRatio { .. } => "inside_mask_changed_ratio",
            Self::UnchangedRegionExact { .. } => "unchanged_region_exact",
            Self::AlphaIou { .. } => "alpha_iou",
            Self::MaskLeakageRatio { .. } => "mask_leakage_ratio",
            Self::ObjectBounds { .. } => "object_bounds",
            Self::ObjectCentroid { .. } => "object_centroid",
            Self::ObjectArea { .. } => "object_area",
            Self::MaximumChannelError { .. } => "maximum_channel_error",
            Self::MeanAbsoluteError { .. } => "mean_absolute_error",
        }
    }

    fn validate(&self) -> Result<(), String> {
        let id = self.id();
        let mut bytes = id.bytes();
        if !bytes.next().is_some_and(|byte| byte.is_ascii_alphabetic())
            || id.len() > 64
            || !bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return Err("image compare assertion id is invalid".to_owned());
        }
        match self {
            Self::ExactEqual { .. }
            | Self::ObjectArea { .. }
            | Self::MaximumChannelError { .. } => Ok(()),
            Self::ChangedRatio {
                minimum, maximum, ..
            } => validate_ratio_bounds(*minimum, *maximum),
            Self::OutsideMaskChangedRatio {
                maximum,
                mask_artifact,
                ..
            }
            | Self::MaskLeakageRatio {
                maximum,
                mask_artifact,
                ..
            } => {
                validate_unit_interval(*maximum)?;
                validate_image_artifact_ref(mask_artifact)
            }
            Self::InsideMaskChangedRatio {
                minimum,
                maximum,
                mask_artifact,
                ..
            } => {
                validate_ratio_bounds(Some(*minimum), *maximum)?;
                validate_image_artifact_ref(mask_artifact)
            }
            Self::UnchangedRegionExact { region, .. } => validate_compare_rect(*region),
            Self::AlphaIou {
                minimum,
                mask_artifact,
                ..
            } => {
                validate_unit_interval(*minimum)?;
                validate_image_artifact_ref(mask_artifact)
            }
            Self::ObjectBounds {
                expected,
                tolerance,
                ..
            } => {
                validate_compare_rect(*expected)?;
                if *tolerance > 8_192 {
                    return Err("image compare bounds tolerance exceeds 8192".to_owned());
                }
                Ok(())
            }
            Self::ObjectCentroid {
                expected,
                tolerance,
                ..
            } => {
                if expected
                    .iter()
                    .any(|value| !value.is_finite() || *value < 0.0 || *value > 8_192.0)
                    || !tolerance.is_finite()
                    || !(0.0..=8_192.0).contains(tolerance)
                {
                    return Err("image compare centroid assertion is invalid".to_owned());
                }
                Ok(())
            }
            Self::MeanAbsoluteError { maximum, .. } => validate_unit_interval(*maximum),
        }
    }
}

fn validate_unit_interval(value: f64) -> Result<(), String> {
    if !value.is_finite() || !(0.0..=1.0).contains(&value) {
        return Err("image compare ratio must be finite and within 0..=1".to_owned());
    }
    Ok(())
}

fn validate_ratio_bounds(minimum: Option<f64>, maximum: Option<f64>) -> Result<(), String> {
    if minimum.is_none() && maximum.is_none() {
        return Err("image compare ratio requires minimum or maximum".to_owned());
    }
    if let Some(value) = minimum {
        validate_unit_interval(value)?;
    }
    if let Some(value) = maximum {
        validate_unit_interval(value)?;
    }
    if minimum
        .zip(maximum)
        .is_some_and(|(left, right)| left > right)
    {
        return Err("image compare ratio minimum exceeds maximum".to_owned());
    }
    Ok(())
}

fn validate_compare_rect(value: ImageCompareRect) -> Result<(), String> {
    if value.width == 0
        || value.height == 0
        || value
            .x
            .checked_add(value.width)
            .is_none_or(|edge| edge > 8_192)
        || value
            .y
            .checked_add(value.height)
            .is_none_or(|edge| edge > 8_192)
    {
        return Err("image compare rectangle is outside the supported bounds".to_owned());
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ImageVerificationSpec {
    pub schema: String,
    pub before_artifact: ArtifactRef,
    pub after_kind: ImageCompareArtifactKind,
    pub assertions: Vec<ImageCompareAssertion>,
}

impl ImageVerificationSpec {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != IMAGE_VERIFICATION_SPEC_SCHEMA {
            return Err("image verification specification schema is unsupported".to_owned());
        }
        validate_image_artifact_ref(&self.before_artifact)?;
        if self.assertions.is_empty() || self.assertions.len() > MAX_IMAGE_COMPARE_ASSERTIONS {
            return Err(format!(
                "image verification assertions must contain 1..={MAX_IMAGE_COMPARE_ASSERTIONS} entries"
            ));
        }
        let mut ids = std::collections::BTreeSet::new();
        let mut has_required = false;
        for assertion in &self.assertions {
            assertion.validate()?;
            if !ids.insert(assertion.id()) {
                return Err("image verification assertion ids must be unique".to_owned());
            }
            has_required |= assertion.severity() == ImageCompareSeverity::Required;
        }
        if !has_required {
            return Err(
                "deterministic image verification requires a required assertion".to_owned(),
            );
        }
        let encoded = serde_json::to_vec(self)
            .map_err(|error| format!("image verification specification is invalid: {error}"))?;
        if encoded.len() > 256 * 1024 {
            return Err("image verification specification exceeds 256 KiB".to_owned());
        }
        Ok(())
    }

    pub fn digest(&self) -> Result<String, String> {
        self.validate()?;
        let encoded = serde_json::to_vec(self).map_err(|error| {
            format!("image verification specification could not be encoded: {error}")
        })?;
        let mut hasher = Sha256::new();
        hasher.update(b"perfectpixel-agent-image-verification-spec-v1");
        hasher.update([0]);
        hasher.update(encoded);
        Ok(format!("{:x}", hasher.finalize()))
    }

    pub fn validate_result(
        &self,
        all_required_passed: bool,
        results: &[ImageCompareAssertionResult],
    ) -> Result<Vec<String>, String> {
        self.validate()?;
        if results.len() != self.assertions.len() {
            return Err("image compare result does not cover the exact assertion set".to_owned());
        }
        let expected = self
            .assertions
            .iter()
            .map(|assertion| {
                (
                    assertion.id(),
                    (assertion.severity(), assertion.assertion_type()),
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        let mut seen = std::collections::BTreeSet::new();
        let mut failed = Vec::new();
        for result in results {
            if expected.get(result.id.as_str())
                != Some(&(result.severity, result.assertion_type.as_str()))
                || !seen.insert(result.id.as_str())
            {
                return Err("image compare result assertion identity does not match".to_owned());
            }
            if !result.passed && result.severity == ImageCompareSeverity::Required {
                failed.push(result.id.clone());
            }
        }
        if seen.len() != expected.len() || all_required_passed != failed.is_empty() {
            return Err("image compare aggregate result is inconsistent".to_owned());
        }
        Ok(failed)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ImageVerificationContract {
    pub visual_review: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deterministic: Option<ImageVerificationSpec>,
}

impl ImageVerificationContract {
    pub fn validate(&self) -> Result<(), String> {
        if !self.visual_review {
            return Err("image task requires visual review".to_owned());
        }
        if let Some(specification) = &self.deterministic {
            specification.validate()?;
        }
        Ok(())
    }
}
