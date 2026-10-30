use serde::{Deserialize, Serialize};

use super::{
    delta_e2000, srgb8_to_lab, ArtifactRef, FrameRect, Mask, PixelSpec, PpError, PpResult, Raster,
    Sha256Digest,
};

pub const VERIFICATION_REPORT_SCHEMA: &str = "perfectpixel.verification-report/4";
const MAX_SRGB_DELTA_E_MILLI: usize = 200_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VerificationSpec {
    #[serde(default)]
    pub exact: Vec<ExactAssertion>,
    #[serde(default)]
    pub perceptual: Vec<PerceptualAssertion>,
    #[serde(default)]
    pub regions: Vec<RegionAssertion>,
}

impl VerificationSpec {
    pub fn validate(&self) -> PpResult<()> {
        if self.exact.is_empty() && self.perceptual.is_empty() && self.regions.is_empty() {
            return Err(PpError::InvalidRequest(
                "verification must contain at least one assertion".to_string(),
            ));
        }
        for assertion in &self.exact {
            match assertion {
                ExactAssertion::Dimensions { width, height } if *width == 0 || *height == 0 => {
                    return Err(PpError::InvalidRequest(
                        "verification dimensions must be positive".to_string(),
                    ));
                }
                ExactAssertion::AlphaBounds { minimum, maximum } if minimum > maximum => {
                    return Err(PpError::InvalidRequest(
                        "verification alpha minimum must not exceed maximum".to_string(),
                    ));
                }
                ExactAssertion::ConnectedComponentCount {
                    minimum, maximum, ..
                } if minimum > maximum => {
                    return Err(PpError::InvalidRequest(
                        "component-count minimum must not exceed maximum".to_string(),
                    ));
                }
                ExactAssertion::ContentBounds { expected, .. }
                    if expected.w == 0 || expected.h == 0 =>
                {
                    return Err(PpError::InvalidRequest(
                        "expected content bounds must be positive".to_string(),
                    ));
                }
                _ => {}
            }
        }
        for assertion in &self.perceptual {
            match assertion {
                PerceptualAssertion::DeltaE2000 { thresholds } => {
                    validate_delta_thresholds(*thresholds)?;
                }
            }
        }
        for assertion in &self.regions {
            let rect = assertion.rect();
            if rect.w == 0 || rect.h == 0 {
                return Err(PpError::InvalidRequest(
                    "verification region dimensions must be positive".to_string(),
                ));
            }
            match assertion {
                RegionAssertion::AlphaBounds {
                    minimum, maximum, ..
                } if minimum > maximum => {
                    return Err(PpError::InvalidRequest(
                        "region alpha minimum must not exceed maximum".to_string(),
                    ));
                }
                RegionAssertion::DeltaE2000 { thresholds, .. } => {
                    validate_delta_thresholds(*thresholds)?;
                }
                _ => {}
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExactAssertion {
    Dimensions {
        width: u32,
        height: u32,
    },
    PixelSpec {
        expected: PixelSpec,
    },
    AlphaBounds {
        minimum: u8,
        maximum: u8,
    },
    ArtifactSha256 {
        expected: Sha256Digest,
    },
    ConnectedComponentCount {
        threshold: u8,
        minimum: u32,
        maximum: u32,
    },
    ContentBounds {
        threshold: u8,
        expected: FrameRect,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeltaEThresholds {
    /// CIEDE2000 ΔE00 multiplied by 1000. `1500` means ΔE00 <= 1.5.
    pub mean_milli_max: u32,
    pub p95_milli_max: u32,
    pub max_milli_max: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PerceptualAssertion {
    DeltaE2000 {
        #[serde(flatten)]
        thresholds: DeltaEThresholds,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RegionAssertion {
    ExactRgba {
        rect: FrameRect,
    },
    AlphaBounds {
        rect: FrameRect,
        minimum: u8,
        maximum: u8,
    },
    DeltaE2000 {
        rect: FrameRect,
        #[serde(flatten)]
        thresholds: DeltaEThresholds,
    },
}

impl RegionAssertion {
    fn rect(&self) -> FrameRect {
        match self {
            Self::ExactRgba { rect }
            | Self::AlphaBounds { rect, .. }
            | Self::DeltaE2000 { rect, .. } => *rect,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationReport {
    pub schema: &'static str,
    pub ok: bool,
    pub exact: Vec<ExactCheck>,
    pub perceptual: Vec<PerceptualCheck>,
    pub regions: Vec<RegionCheck>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExactCheck {
    pub assertion: ExactAssertion,
    pub passed: bool,
    pub evidence: ExactEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExactEvidence {
    Dimensions { width: u32, height: u32 },
    PixelSpec { actual: PixelSpec },
    AlphaBounds { minimum: u8, maximum: u8 },
    ArtifactSha256 { actual: Option<Sha256Digest> },
    ConnectedComponentCount { actual: u32 },
    ContentBounds { actual: FrameRect },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PerceptualCheck {
    pub assertion: PerceptualAssertion,
    pub passed: bool,
    pub evidence: PerceptualEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PerceptualEvidence {
    MissingReference,
    DimensionMismatch {
        actual_width: u32,
        actual_height: u32,
        reference_width: u32,
        reference_height: u32,
    },
    DeltaE2000 {
        mean_milli: u32,
        p95_milli: u32,
        max_milli: u32,
        compared_pixels: u64,
        alpha_support_mismatch_pixels: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RegionCheck {
    pub assertion: RegionAssertion,
    pub passed: bool,
    pub evidence: RegionEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RegionEvidence {
    OutOfBounds {
        raster_width: u32,
        raster_height: u32,
    },
    MissingReference,
    ExactRgba {
        compared_pixels: u64,
        mismatch_pixels: u64,
    },
    AlphaBounds {
        minimum: u8,
        maximum: u8,
    },
    DeltaE2000 {
        mean_milli: u32,
        p95_milli: u32,
        max_milli: u32,
        compared_pixels: u64,
        alpha_support_mismatch_pixels: u64,
    },
}

pub fn verify_raster_exact(
    spec: &VerificationSpec,
    raster: &Raster,
    pixel_spec: &PixelSpec,
    artifact: Option<&ArtifactRef>,
) -> PpResult<VerificationReport> {
    if !spec.perceptual.is_empty() || !spec.regions.is_empty() {
        return Err(PpError::InvalidRequest(
            "verify_raster_exact accepts exact assertions only; use verify_raster for reference/region assertions"
                .to_string(),
        ));
    }
    verify_raster(spec, raster, pixel_spec, artifact, None)
}

pub fn verify_raster(
    spec: &VerificationSpec,
    raster: &Raster,
    pixel_spec: &PixelSpec,
    artifact: Option<&ArtifactRef>,
    reference: Option<&Raster>,
) -> PpResult<VerificationReport> {
    spec.validate()?;
    let mask = Mask::from_raster_alpha(raster)?;
    let observed_alpha = alpha_bounds(raster, None)?;
    let mut exact = Vec::with_capacity(spec.exact.len());
    for assertion in &spec.exact {
        let (passed, evidence) = match assertion {
            ExactAssertion::Dimensions { width, height } => (
                raster.width() == *width && raster.height() == *height,
                ExactEvidence::Dimensions {
                    width: raster.width(),
                    height: raster.height(),
                },
            ),
            ExactAssertion::PixelSpec { expected } => (
                pixel_spec == expected,
                ExactEvidence::PixelSpec {
                    actual: pixel_spec.clone(),
                },
            ),
            ExactAssertion::AlphaBounds { minimum, maximum } => (
                observed_alpha.0 >= *minimum && observed_alpha.1 <= *maximum,
                ExactEvidence::AlphaBounds {
                    minimum: observed_alpha.0,
                    maximum: observed_alpha.1,
                },
            ),
            ExactAssertion::ArtifactSha256 { expected } => {
                let actual = artifact.map(|artifact| artifact.sha256().clone());
                (
                    actual.as_ref() == Some(expected),
                    ExactEvidence::ArtifactSha256 { actual },
                )
            }
            ExactAssertion::ConnectedComponentCount {
                threshold,
                minimum,
                maximum,
            } => {
                let count =
                    u32::try_from(mask.connected_components(*threshold).len()).unwrap_or(u32::MAX);
                (
                    count >= *minimum && count <= *maximum,
                    ExactEvidence::ConnectedComponentCount { actual: count },
                )
            }
            ExactAssertion::ContentBounds {
                threshold,
                expected,
            } => {
                let actual = mask.bounding_box(*threshold);
                (actual == *expected, ExactEvidence::ContentBounds { actual })
            }
        };
        exact.push(ExactCheck {
            assertion: assertion.clone(),
            passed,
            evidence,
        });
    }

    let perceptual = spec
        .perceptual
        .iter()
        .map(|assertion| evaluate_perceptual(assertion, raster, reference))
        .collect::<PpResult<Vec<_>>>()?;
    let regions = spec
        .regions
        .iter()
        .map(|assertion| evaluate_region(assertion, raster, reference))
        .collect::<PpResult<Vec<_>>>()?;
    let ok = exact.iter().all(|check| check.passed)
        && perceptual.iter().all(|check| check.passed)
        && regions.iter().all(|check| check.passed);
    Ok(VerificationReport {
        schema: VERIFICATION_REPORT_SCHEMA,
        ok,
        exact,
        perceptual,
        regions,
    })
}

fn evaluate_perceptual(
    assertion: &PerceptualAssertion,
    actual: &Raster,
    reference: Option<&Raster>,
) -> PpResult<PerceptualCheck> {
    let Some(reference) = reference else {
        return Ok(PerceptualCheck {
            assertion: assertion.clone(),
            passed: false,
            evidence: PerceptualEvidence::MissingReference,
        });
    };
    if actual.width() != reference.width() || actual.height() != reference.height() {
        return Ok(PerceptualCheck {
            assertion: assertion.clone(),
            passed: false,
            evidence: PerceptualEvidence::DimensionMismatch {
                actual_width: actual.width(),
                actual_height: actual.height(),
                reference_width: reference.width(),
                reference_height: reference.height(),
            },
        });
    }
    match assertion {
        PerceptualAssertion::DeltaE2000 { thresholds } => {
            let metrics = delta_e_metrics(actual, reference, None)?;
            Ok(PerceptualCheck {
                assertion: assertion.clone(),
                passed: delta_e_passes(&metrics, *thresholds),
                evidence: PerceptualEvidence::DeltaE2000 {
                    mean_milli: metrics.mean_milli,
                    p95_milli: metrics.p95_milli,
                    max_milli: metrics.max_milli,
                    compared_pixels: metrics.compared_pixels,
                    alpha_support_mismatch_pixels: metrics.alpha_support_mismatch_pixels,
                },
            })
        }
    }
}

fn evaluate_region(
    assertion: &RegionAssertion,
    actual: &Raster,
    reference: Option<&Raster>,
) -> PpResult<RegionCheck> {
    let rect = assertion.rect();
    if !rect_fits(actual, rect) {
        return Ok(RegionCheck {
            assertion: assertion.clone(),
            passed: false,
            evidence: RegionEvidence::OutOfBounds {
                raster_width: actual.width(),
                raster_height: actual.height(),
            },
        });
    }
    match assertion {
        RegionAssertion::AlphaBounds {
            minimum, maximum, ..
        } => {
            let (actual_minimum, actual_maximum) = alpha_bounds(actual, Some(rect))?;
            Ok(RegionCheck {
                assertion: assertion.clone(),
                passed: actual_minimum >= *minimum && actual_maximum <= *maximum,
                evidence: RegionEvidence::AlphaBounds {
                    minimum: actual_minimum,
                    maximum: actual_maximum,
                },
            })
        }
        RegionAssertion::ExactRgba { .. } => {
            let Some(reference) = reference else {
                return Ok(RegionCheck {
                    assertion: assertion.clone(),
                    passed: false,
                    evidence: RegionEvidence::MissingReference,
                });
            };
            if !rect_fits(reference, rect) {
                return Ok(RegionCheck {
                    assertion: assertion.clone(),
                    passed: false,
                    evidence: RegionEvidence::OutOfBounds {
                        raster_width: reference.width(),
                        raster_height: reference.height(),
                    },
                });
            }
            let (compared_pixels, mismatch_pixels) =
                exact_region_mismatches(actual, reference, rect);
            Ok(RegionCheck {
                assertion: assertion.clone(),
                passed: mismatch_pixels == 0,
                evidence: RegionEvidence::ExactRgba {
                    compared_pixels,
                    mismatch_pixels,
                },
            })
        }
        RegionAssertion::DeltaE2000 { thresholds, .. } => {
            let Some(reference) = reference else {
                return Ok(RegionCheck {
                    assertion: assertion.clone(),
                    passed: false,
                    evidence: RegionEvidence::MissingReference,
                });
            };
            if !rect_fits(reference, rect) {
                return Ok(RegionCheck {
                    assertion: assertion.clone(),
                    passed: false,
                    evidence: RegionEvidence::OutOfBounds {
                        raster_width: reference.width(),
                        raster_height: reference.height(),
                    },
                });
            }
            let metrics = delta_e_metrics(actual, reference, Some(rect))?;
            Ok(RegionCheck {
                assertion: assertion.clone(),
                passed: delta_e_passes(&metrics, *thresholds),
                evidence: RegionEvidence::DeltaE2000 {
                    mean_milli: metrics.mean_milli,
                    p95_milli: metrics.p95_milli,
                    max_milli: metrics.max_milli,
                    compared_pixels: metrics.compared_pixels,
                    alpha_support_mismatch_pixels: metrics.alpha_support_mismatch_pixels,
                },
            })
        }
    }
}

fn validate_delta_thresholds(thresholds: DeltaEThresholds) -> PpResult<()> {
    if thresholds.mean_milli_max > thresholds.p95_milli_max
        || thresholds.p95_milli_max > thresholds.max_milli_max
    {
        return Err(PpError::InvalidRequest(
            "DeltaE2000 thresholds must satisfy mean <= p95 <= max".to_string(),
        ));
    }
    Ok(())
}

struct DeltaEMetrics {
    mean_milli: u32,
    p95_milli: u32,
    max_milli: u32,
    compared_pixels: u64,
    alpha_support_mismatch_pixels: u64,
}

fn delta_e_passes(metrics: &DeltaEMetrics, thresholds: DeltaEThresholds) -> bool {
    metrics.alpha_support_mismatch_pixels == 0
        && metrics.mean_milli <= thresholds.mean_milli_max
        && metrics.p95_milli <= thresholds.p95_milli_max
        && metrics.max_milli <= thresholds.max_milli_max
}

fn delta_e_metrics(
    actual: &Raster,
    reference: &Raster,
    rect: Option<FrameRect>,
) -> PpResult<DeltaEMetrics> {
    let rect = rect.unwrap_or(FrameRect {
        x: 0,
        y: 0,
        w: actual.width(),
        h: actual.height(),
    });
    if !rect_fits(actual, rect) || !rect_fits(reference, rect) {
        return Err(PpError::InvalidRequest(
            "DeltaE2000 comparison region is out of bounds".to_string(),
        ));
    }
    let mut histogram = vec![0u64; MAX_SRGB_DELTA_E_MILLI + 1];
    let mut sum = 0u64;
    let mut max = 0u32;
    let mut compared = 0u64;
    let mut alpha_support_mismatch = 0u64;
    for y in rect.y..rect.y + rect.h {
        for x in rect.x..rect.x + rect.w {
            let actual_pixel = rgba_at(actual, x, y);
            let reference_pixel = rgba_at(reference, x, y);
            let actual_present = actual_pixel[3] != 0;
            let reference_present = reference_pixel[3] != 0;
            if actual_present != reference_present {
                alpha_support_mismatch = alpha_support_mismatch.saturating_add(1);
            }
            if !actual_present || !reference_present {
                continue;
            }
            let difference = delta_e2000(
                srgb8_to_lab([actual_pixel[0], actual_pixel[1], actual_pixel[2]]),
                srgb8_to_lab([reference_pixel[0], reference_pixel[1], reference_pixel[2]]),
            );
            if !difference.is_finite() || difference < 0.0 {
                return Err(PpError::InvalidRequest(
                    "DeltaE2000 produced a non-finite metric".to_string(),
                ));
            }
            let milli = (difference * 1000.0).round().clamp(0.0, u32::MAX as f64) as u32;
            histogram[(milli as usize).min(MAX_SRGB_DELTA_E_MILLI)] += 1;
            sum = sum.saturating_add(u64::from(milli));
            max = max.max(milli);
            compared += 1;
        }
    }
    let mean = sum
        .saturating_add(compared / 2)
        .checked_div(compared)
        .unwrap_or(0)
        .min(u64::from(u32::MAX)) as u32;
    Ok(DeltaEMetrics {
        mean_milli: mean,
        p95_milli: percentile95(&histogram, compared),
        max_milli: max,
        compared_pixels: compared,
        alpha_support_mismatch_pixels: alpha_support_mismatch,
    })
}

fn exact_region_mismatches(actual: &Raster, reference: &Raster, rect: FrameRect) -> (u64, u64) {
    let mut compared = 0u64;
    let mut mismatched = 0u64;
    for y in rect.y..rect.y + rect.h {
        for x in rect.x..rect.x + rect.w {
            compared += 1;
            if rgba_at(actual, x, y) != rgba_at(reference, x, y) {
                mismatched += 1;
            }
        }
    }
    (compared, mismatched)
}

fn alpha_bounds(raster: &Raster, rect: Option<FrameRect>) -> PpResult<(u8, u8)> {
    let rect = rect.unwrap_or(FrameRect {
        x: 0,
        y: 0,
        w: raster.width(),
        h: raster.height(),
    });
    if !rect_fits(raster, rect) {
        return Err(PpError::InvalidRequest(
            "alpha comparison region is out of bounds".to_string(),
        ));
    }
    let mut minimum = u8::MAX;
    let mut maximum = u8::MIN;
    for y in rect.y..rect.y + rect.h {
        for x in rect.x..rect.x + rect.w {
            let alpha = rgba_at(raster, x, y)[3];
            minimum = minimum.min(alpha);
            maximum = maximum.max(alpha);
        }
    }
    Ok((minimum, maximum))
}

fn rect_fits(raster: &Raster, rect: FrameRect) -> bool {
    rect.w != 0
        && rect.h != 0
        && rect
            .x
            .checked_add(rect.w)
            .is_some_and(|right| right <= raster.width())
        && rect
            .y
            .checked_add(rect.h)
            .is_some_and(|bottom| bottom <= raster.height())
}

fn rgba_at(raster: &Raster, x: u32, y: u32) -> [u8; 4] {
    let index = (y as usize * raster.width() as usize + x as usize) * 4;
    let pixels = &raster.pixels()[index..index + 4];
    [pixels[0], pixels[1], pixels[2], pixels[3]]
}

fn percentile95(histogram: &[u64], samples: u64) -> u32 {
    if samples == 0 {
        return 0;
    }
    let target = (samples.saturating_mul(95).saturating_add(99)) / 100;
    let mut cumulative = 0u64;
    for (index, &count) in histogram.iter().enumerate() {
        cumulative = cumulative.saturating_add(count);
        if cumulative >= target {
            return index as u32;
        }
    }
    (histogram.len() - 1) as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{AlphaMode, ColorSpec, PixelFormat};

    fn exact_spec(assertions: Vec<ExactAssertion>) -> VerificationSpec {
        VerificationSpec {
            exact: assertions,
            perceptual: Vec::new(),
            regions: Vec::new(),
        }
    }

    #[test]
    fn exact_verification_reports_each_invariant() -> crate::PpResult<()> {
        let raster = Raster::new(2, 1, vec![255, 0, 0, 255, 0, 0, 0, 0])?;
        let pixel_spec = PixelSpec::new(PixelFormat::Rgba8, AlphaMode::Straight, ColorSpec::Srgb);
        let artifact = ArtifactRef::from_bytes("image/png", b"encoded")?;
        let spec = exact_spec(vec![
            ExactAssertion::Dimensions {
                width: 2,
                height: 1,
            },
            ExactAssertion::PixelSpec {
                expected: pixel_spec.clone(),
            },
            ExactAssertion::AlphaBounds {
                minimum: 0,
                maximum: 255,
            },
            ExactAssertion::ArtifactSha256 {
                expected: artifact.sha256().clone(),
            },
        ]);
        let report = verify_raster_exact(&spec, &raster, &pixel_spec, Some(&artifact))?;
        assert!(report.ok);
        assert_eq!(report.exact.len(), 4);
        Ok(())
    }

    #[test]
    fn verification_rejects_vacuous_success() -> crate::PpResult<()> {
        let raster = Raster::blank(1, 1)?;
        let pixel_spec =
            PixelSpec::new(PixelFormat::Rgba8, AlphaMode::Straight, ColorSpec::Unknown);
        assert!(verify_raster(
            &VerificationSpec::default(),
            &raster,
            &pixel_spec,
            None,
            None
        )
        .is_err());
        Ok(())
    }

    #[test]
    fn delta_e_identical_is_machine_readable() -> crate::PpResult<()> {
        let raster = Raster::new(2, 1, vec![10, 20, 30, 255, 40, 50, 60, 255])?;
        let pixel_spec = PixelSpec::new(PixelFormat::Rgba8, AlphaMode::Opaque, ColorSpec::Srgb);
        let spec = VerificationSpec {
            exact: vec![ExactAssertion::Dimensions {
                width: 2,
                height: 1,
            }],
            perceptual: vec![PerceptualAssertion::DeltaE2000 {
                thresholds: DeltaEThresholds {
                    mean_milli_max: 0,
                    p95_milli_max: 0,
                    max_milli_max: 0,
                },
            }],
            regions: Vec::new(),
        };
        let report = verify_raster(&spec, &raster, &pixel_spec, None, Some(&raster))?;
        assert!(report.ok);
        assert!(matches!(
            report.perceptual[0].evidence,
            PerceptualEvidence::DeltaE2000 { max_milli: 0, .. }
        ));
        Ok(())
    }

    #[test]
    fn delta_e_requires_alpha_support_to_match() -> crate::PpResult<()> {
        let actual = Raster::new(1, 1, vec![255, 0, 0, 0])?;
        let reference = Raster::new(1, 1, vec![255, 0, 0, 255])?;
        let pixel_spec = PixelSpec::new(PixelFormat::Rgba8, AlphaMode::Straight, ColorSpec::Srgb);
        let spec = VerificationSpec {
            exact: Vec::new(),
            perceptual: vec![PerceptualAssertion::DeltaE2000 {
                thresholds: DeltaEThresholds {
                    mean_milli_max: 0,
                    p95_milli_max: 0,
                    max_milli_max: 0,
                },
            }],
            regions: Vec::new(),
        };
        let report = verify_raster(&spec, &actual, &pixel_spec, None, Some(&reference))?;
        assert!(!report.ok);
        Ok(())
    }

    #[test]
    fn region_exact_and_alpha_are_independent_gates() -> crate::PpResult<()> {
        let reference = Raster::new(2, 1, vec![1, 2, 3, 255, 4, 5, 6, 0])?;
        let actual = Raster::new(2, 1, vec![1, 2, 3, 255, 9, 9, 9, 0])?;
        let pixel_spec = PixelSpec::new(PixelFormat::Rgba8, AlphaMode::Straight, ColorSpec::Srgb);
        let spec = VerificationSpec {
            exact: Vec::new(),
            perceptual: Vec::new(),
            regions: vec![
                RegionAssertion::ExactRgba {
                    rect: FrameRect {
                        x: 0,
                        y: 0,
                        w: 1,
                        h: 1,
                    },
                },
                RegionAssertion::AlphaBounds {
                    rect: FrameRect {
                        x: 1,
                        y: 0,
                        w: 1,
                        h: 1,
                    },
                    minimum: 0,
                    maximum: 0,
                },
            ],
        };
        let report = verify_raster(&spec, &actual, &pixel_spec, None, Some(&reference))?;
        assert!(report.ok);
        Ok(())
    }
}
