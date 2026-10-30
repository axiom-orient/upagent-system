use std::path::PathBuf;

use crate::{
    resize_raster, AgentArtifactDependency, AgentArtifactKind, AgentInputArtifact,
    CompareAssertion, PpError, PpResult, Raster, ResampleFilter, MAX_COMPARE_MASK_PIXELS,
};

use super::{read_agent_raster, AgentCompareAssertionRequest};

pub(super) fn prepare_compare_assertion(
    assertion: AgentCompareAssertionRequest,
    masks: &mut Vec<Raster>,
    dependencies: &mut Vec<AgentArtifactDependency>,
    mask_paths: &mut Vec<PathBuf>,
) -> PpResult<CompareAssertion> {
    let mut read_mask = |mask: AgentInputArtifact| -> PpResult<usize> {
        let path = PathBuf::from(&mask.path);
        let (raster, dependency) = read_agent_raster(&mask, AgentArtifactKind::Mask)?;
        let next_pixels = u64::from(raster.width()) * u64::from(raster.height());
        let total_pixels = masks
            .iter()
            .map(|existing| u64::from(existing.width()) * u64::from(existing.height()))
            .try_fold(next_pixels, u64::checked_add)
            .ok_or_else(|| {
                PpError::InvalidRequest("compare mask pixel budget overflowed".to_owned())
            })?;
        if total_pixels > MAX_COMPARE_MASK_PIXELS {
            return Err(PpError::InvalidRequest(format!(
                "compare mask aggregate pixels exceed {MAX_COMPARE_MASK_PIXELS}"
            )));
        }
        let index = masks.len();
        masks.push(raster);
        dependencies.push(dependency);
        mask_paths.push(path);
        Ok(index)
    };
    Ok(match assertion {
        AgentCompareAssertionRequest::ExactEqual { id, severity } => {
            CompareAssertion::ExactEqual { id, severity }
        }
        AgentCompareAssertionRequest::ChangedRatio {
            id,
            severity,
            minimum,
            maximum,
        } => CompareAssertion::ChangedRatio {
            id,
            severity,
            minimum,
            maximum,
        },
        AgentCompareAssertionRequest::OutsideMaskChangedRatio {
            id,
            severity,
            maximum,
            mask,
        } => CompareAssertion::OutsideMaskChangedRatio {
            id,
            severity,
            maximum,
            mask_index: read_mask(mask)?,
        },
        AgentCompareAssertionRequest::InsideMaskChangedRatio {
            id,
            severity,
            minimum,
            maximum,
            mask,
        } => CompareAssertion::InsideMaskChangedRatio {
            id,
            severity,
            minimum,
            maximum,
            mask_index: read_mask(mask)?,
        },
        AgentCompareAssertionRequest::UnchangedRegionExact {
            id,
            severity,
            region,
        } => CompareAssertion::UnchangedRegionExact {
            id,
            severity,
            region,
        },
        AgentCompareAssertionRequest::AlphaIou {
            id,
            severity,
            minimum,
            mask,
        } => CompareAssertion::AlphaIou {
            id,
            severity,
            minimum,
            mask_index: read_mask(mask)?,
        },
        AgentCompareAssertionRequest::MaskLeakageRatio {
            id,
            severity,
            maximum,
            mask,
        } => CompareAssertion::MaskLeakageRatio {
            id,
            severity,
            maximum,
            mask_index: read_mask(mask)?,
        },
        AgentCompareAssertionRequest::ObjectBounds {
            id,
            severity,
            expected,
            tolerance,
        } => CompareAssertion::ObjectBounds {
            id,
            severity,
            expected,
            tolerance,
        },
        AgentCompareAssertionRequest::ObjectCentroid {
            id,
            severity,
            expected,
            tolerance,
        } => CompareAssertion::ObjectCentroid {
            id,
            severity,
            expected,
            tolerance,
        },
        AgentCompareAssertionRequest::ObjectArea {
            id,
            severity,
            expected,
            tolerance,
        } => CompareAssertion::ObjectArea {
            id,
            severity,
            expected,
            tolerance,
        },
        AgentCompareAssertionRequest::MaximumChannelError {
            id,
            severity,
            maximum,
        } => CompareAssertion::MaximumChannelError {
            id,
            severity,
            maximum,
        },
        AgentCompareAssertionRequest::MeanAbsoluteError {
            id,
            severity,
            maximum,
        } => CompareAssertion::MeanAbsoluteError {
            id,
            severity,
            maximum,
        },
    })
}

pub(super) fn bounded_preview(raster: Raster, maximum_edge: u32) -> PpResult<Raster> {
    let largest = raster.width().max(raster.height());
    if largest <= maximum_edge {
        return Ok(raster);
    }
    let width =
        (u64::from(raster.width()) * u64::from(maximum_edge) / u64::from(largest)).max(1) as u32;
    let height =
        (u64::from(raster.height()) * u64::from(maximum_edge) / u64::from(largest)).max(1) as u32;
    resize_raster(&raster, width, height, ResampleFilter::Nearest)
}
