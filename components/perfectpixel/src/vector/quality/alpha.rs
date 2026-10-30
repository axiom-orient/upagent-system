use crate::core::Raster;

/// Protected alpha is a zero-budget invariant: any alpha mismatch rejects the candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AlphaGateEvidence {
    pub compared_pixels: usize,
    pub mismatched_pixels: usize,
    pub first_mismatch: Option<AlphaMismatch>,
    pub dimension_mismatch: Option<DimensionMismatch>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AlphaMismatch {
    pub x: u32,
    pub y: u32,
    pub source_alpha: u8,
    pub candidate_alpha: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DimensionMismatch {
    pub source: (u32, u32),
    pub candidate: (u32, u32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AlphaGateResult {
    Passed(AlphaGateEvidence),
    Failed(AlphaGateEvidence),
}

impl AlphaGateResult {
    pub(crate) fn passed(&self) -> bool {
        matches!(self, Self::Passed(_))
    }

    #[cfg(test)]
    pub(crate) fn evidence(&self) -> &AlphaGateEvidence {
        match self {
            Self::Passed(evidence) | Self::Failed(evidence) => evidence,
        }
    }
}

pub(crate) fn protected_alpha_gate(source: &Raster, candidate: &Raster) -> AlphaGateResult {
    if source.width() != candidate.width() || source.height() != candidate.height() {
        return AlphaGateResult::Failed(AlphaGateEvidence {
            compared_pixels: 0,
            mismatched_pixels: 0,
            first_mismatch: None,
            dimension_mismatch: Some(DimensionMismatch {
                source: (source.width(), source.height()),
                candidate: (candidate.width(), candidate.height()),
            }),
        });
    }
    let mut mismatched_pixels = 0;
    let mut first_mismatch = None;
    for y in 0..source.height() {
        for x in 0..source.width() {
            let source_alpha = source.premultiplied_pixel(x, y)[3];
            let candidate_alpha = candidate.premultiplied_pixel(x, y)[3];
            if source_alpha != candidate_alpha {
                mismatched_pixels += 1;
                first_mismatch.get_or_insert(AlphaMismatch {
                    x,
                    y,
                    source_alpha,
                    candidate_alpha,
                });
            }
        }
    }
    let evidence = AlphaGateEvidence {
        compared_pixels: source.width().saturating_mul(source.height()) as usize,
        mismatched_pixels,
        first_mismatch,
        dimension_mismatch: None,
    };
    if evidence.mismatched_pixels == 0 {
        AlphaGateResult::Passed(evidence)
    } else {
        AlphaGateResult::Failed(evidence)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::PpResult;

    #[test]
    fn protected_alpha_is_zero_budget() -> PpResult<()> {
        let source = Raster::new(2, 1, vec![255, 0, 0, 255, 0, 0, 0, 0])?;
        assert!(protected_alpha_gate(&source, &source).passed());

        let changed = Raster::new(2, 1, vec![255, 0, 0, 254, 0, 0, 0, 0])?;
        let result = protected_alpha_gate(&source, &changed);
        assert!(!result.passed());
        assert_eq!(result.evidence().mismatched_pixels, 1);
        Ok(())
    }

    #[test]
    fn dimension_mismatch_is_reported_without_fabricated_pixels() -> PpResult<()> {
        let source = Raster::new(2, 1, vec![0; 8])?;
        let candidate = Raster::new(1, 1, vec![0; 4])?;
        let result = protected_alpha_gate(&source, &candidate);
        assert!(!result.passed());
        let evidence = result.evidence();
        assert_eq!(evidence.compared_pixels, 0);
        assert_eq!(evidence.mismatched_pixels, 0);
        assert_eq!(evidence.first_mismatch, None);
        assert_eq!(
            evidence.dimension_mismatch,
            Some(DimensionMismatch {
                source: (2, 1),
                candidate: (1, 1),
            })
        );
        Ok(())
    }
}
