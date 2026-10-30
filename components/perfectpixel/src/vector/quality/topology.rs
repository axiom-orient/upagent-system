use crate::core::Raster;

/// Topology is protected with a zero pixel budget. A mask component cannot be added, removed,
/// split, merged, or moved by a publishing candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TopologyGateEvidence {
    pub source_components: usize,
    pub candidate_components: usize,
    pub source_holes: usize,
    pub candidate_holes: usize,
    pub mismatched_mask_pixels: usize,
    pub first_mismatch: Option<TopologyMismatch>,
    pub dimension_mismatch: Option<DimensionMismatch>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TopologyMismatch {
    pub x: u32,
    pub y: u32,
    pub source_opaque: bool,
    pub candidate_opaque: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DimensionMismatch {
    pub source: (u32, u32),
    pub candidate: (u32, u32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TopologyGateResult {
    Passed(TopologyGateEvidence),
    Failed(TopologyGateEvidence),
}

impl TopologyGateResult {
    pub(crate) fn passed(&self) -> bool {
        matches!(self, Self::Passed(_))
    }

    pub(crate) fn evidence(&self) -> &TopologyGateEvidence {
        match self {
            Self::Passed(evidence) | Self::Failed(evidence) => evidence,
        }
    }
}

pub(crate) fn protected_topology_gate(source: &Raster, candidate: &Raster) -> TopologyGateResult {
    if source.width() != candidate.width() || source.height() != candidate.height() {
        return TopologyGateResult::Failed(TopologyGateEvidence {
            source_components: opaque_components(source),
            candidate_components: opaque_components(candidate),
            source_holes: opaque_holes(source),
            candidate_holes: opaque_holes(candidate),
            mismatched_mask_pixels: 0,
            first_mismatch: None,
            dimension_mismatch: Some(DimensionMismatch {
                source: (source.width(), source.height()),
                candidate: (candidate.width(), candidate.height()),
            }),
        });
    }
    let mut mismatched_mask_pixels = 0;
    let mut first_mismatch = None;
    for y in 0..source.height() {
        for x in 0..source.width() {
            let source_opaque = source.premultiplied_pixel(x, y)[3] != 0;
            let candidate_opaque = candidate.premultiplied_pixel(x, y)[3] != 0;
            if source_opaque != candidate_opaque {
                mismatched_mask_pixels += 1;
                first_mismatch.get_or_insert(TopologyMismatch {
                    x,
                    y,
                    source_opaque,
                    candidate_opaque,
                });
            }
        }
    }
    let evidence = TopologyGateEvidence {
        source_components: opaque_components(source),
        candidate_components: opaque_components(candidate),
        source_holes: opaque_holes(source),
        candidate_holes: opaque_holes(candidate),
        mismatched_mask_pixels,
        first_mismatch,
        dimension_mismatch: None,
    };
    if evidence.mismatched_mask_pixels == 0
        && evidence.source_components == evidence.candidate_components
        && evidence.source_holes == evidence.candidate_holes
    {
        TopologyGateResult::Passed(evidence)
    } else {
        TopologyGateResult::Failed(evidence)
    }
}

fn opaque_components(image: &Raster) -> usize {
    let width = image.width() as usize;
    let height = image.height() as usize;
    let mut visited = vec![false; width.saturating_mul(height)];
    let mut components = 0;
    for y in 0..height {
        for x in 0..width {
            let index = y * width + x;
            if visited[index] || image.premultiplied_pixel(x as u32, y as u32)[3] == 0 {
                continue;
            }
            components += 1;
            visited[index] = true;
            let mut stack = vec![(x, y)];
            while let Some((current_x, current_y)) = stack.pop() {
                for (next_x, next_y) in neighbors(current_x, current_y, width, height) {
                    let next_index = next_y * width + next_x;
                    if !visited[next_index]
                        && image.premultiplied_pixel(next_x as u32, next_y as u32)[3] != 0
                    {
                        visited[next_index] = true;
                        stack.push((next_x, next_y));
                    }
                }
            }
        }
    }
    components
}
fn opaque_holes(image: &Raster) -> usize {
    let width = image.width() as usize;
    let height = image.height() as usize;
    let mut visited = vec![false; width.saturating_mul(height)];
    let mut holes = 0;
    for y in 0..height {
        for x in 0..width {
            let index = y * width + x;
            if visited[index] || image.premultiplied_pixel(x as u32, y as u32)[3] != 0 {
                continue;
            }
            visited[index] = true;
            let mut touches_border = x == 0 || y == 0 || x + 1 == width || y + 1 == height;
            let mut stack = vec![(x, y)];
            while let Some((current_x, current_y)) = stack.pop() {
                for next_y in current_y.saturating_sub(1)..=(current_y + 1).min(height - 1) {
                    for next_x in current_x.saturating_sub(1)..=(current_x + 1).min(width - 1) {
                        let next_index = next_y * width + next_x;
                        if !visited[next_index]
                            && image.premultiplied_pixel(next_x as u32, next_y as u32)[3] == 0
                        {
                            visited[next_index] = true;
                            touches_border |= next_x == 0
                                || next_y == 0
                                || next_x + 1 == width
                                || next_y + 1 == height;
                            stack.push((next_x, next_y));
                        }
                    }
                }
            }
            if !touches_border {
                holes += 1;
            }
        }
    }
    holes
}

fn neighbors(
    x: usize,
    y: usize,
    width: usize,
    height: usize,
) -> impl Iterator<Item = (usize, usize)> {
    [
        (x.wrapping_sub(1), y),
        (x + 1, y),
        (x, y.wrapping_sub(1)),
        (x, y + 1),
    ]
    .into_iter()
    .filter(move |(next_x, next_y)| *next_x < width && *next_y < height)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::PpResult;

    #[test]
    fn protected_topology_rejects_component_merge() -> PpResult<()> {
        let source = Raster::new(3, 1, vec![255, 0, 0, 255, 0, 0, 0, 0, 255, 0, 0, 255])?;
        assert!(protected_topology_gate(&source, &source).passed());

        let merged = Raster::new(3, 1, vec![255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255])?;
        let result = protected_topology_gate(&source, &merged);
        assert!(!result.passed());
        assert_eq!(result.evidence().source_components, 2);
        assert_eq!(result.evidence().candidate_components, 1);
        Ok(())
    }
    #[test]
    fn dimension_mismatch_rejects_without_fabricating_pixel_mismatches() -> PpResult<()> {
        let source = Raster::new(2, 1, vec![255, 0, 0, 255, 0, 0, 0, 0])?;
        let candidate = Raster::new(1, 1, vec![255, 0, 0, 255])?;

        let result = protected_topology_gate(&source, &candidate);
        assert!(!result.passed());
        let evidence = result.evidence();
        assert_eq!(
            evidence
                .dimension_mismatch
                .as_ref()
                .map(|mismatch| mismatch.source),
            Some((2, 1))
        );
        assert_eq!(
            evidence
                .dimension_mismatch
                .as_ref()
                .map(|mismatch| mismatch.candidate),
            Some((1, 1))
        );
        assert_eq!(evidence.mismatched_mask_pixels, 0);
        assert_eq!(evidence.first_mismatch, None);
        Ok(())
    }
    #[test]
    fn protected_topology_rejects_hole_fill() -> PpResult<()> {
        let source = Raster::new(
            3,
            3,
            vec![
                1, 1, 1, 255, 1, 1, 1, 255, 1, 1, 1, 255, 1, 1, 1, 255, 0, 0, 0, 0, 1, 1, 1, 255,
                1, 1, 1, 255, 1, 1, 1, 255, 1, 1, 1, 255,
            ],
        )?;
        assert_eq!(opaque_holes(&source), 1);
        let filled = Raster::new(3, 3, [[1, 1, 1, 255]; 9].concat())?;
        let result = protected_topology_gate(&source, &filled);
        assert!(!result.passed());
        assert_eq!(result.evidence().source_holes, 1);
        assert_eq!(result.evidence().candidate_holes, 0);
        Ok(())
    }

    #[test]
    fn diagonal_background_is_not_a_hole() -> PpResult<()> {
        let image = Raster::new(
            3,
            3,
            vec![
                0, 0, 0, 0, 1, 1, 1, 255, 1, 1, 1, 255, 1, 1, 1, 255, 0, 0, 0, 0, 1, 1, 1, 255, 1,
                1, 1, 255, 1, 1, 1, 255, 1, 1, 1, 255,
            ],
        )?;
        assert_eq!(opaque_holes(&image), 0);
        Ok(())
    }
    #[test]
    fn protected_topology_preserves_and_rejects_created_holes() -> PpResult<()> {
        let solid = Raster::new(3, 3, [[1, 1, 1, 255]; 9].concat())?;
        assert!(protected_topology_gate(&solid, &solid).passed());
        let with_hole = Raster::new(
            3,
            3,
            vec![
                1, 1, 1, 255, 1, 1, 1, 255, 1, 1, 1, 255, 1, 1, 1, 255, 0, 0, 0, 0, 1, 1, 1, 255,
                1, 1, 1, 255, 1, 1, 1, 255, 1, 1, 1, 255,
            ],
        )?;
        let result = protected_topology_gate(&solid, &with_hole);
        assert!(!result.passed());
        assert_eq!(result.evidence().source_holes, 0);
        assert_eq!(result.evidence().candidate_holes, 1);
        Ok(())
    }
}
