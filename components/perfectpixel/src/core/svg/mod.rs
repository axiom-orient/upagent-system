mod ir;
mod metrics;
mod parse;

use serde::{Deserialize, Serialize};

use super::PpResult;
pub(crate) use ir::{SvgIr, SvgLimits};
pub(crate) use parse::parse_bounded;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SvgReport {
    pub path_count: usize,
    pub color_count: usize,
    pub node_count: usize,
    pub curve_segment_count: usize,
    pub closed_path_count: usize,
    pub width: u32,
    pub height: u32,
    pub contains_raster_payload: bool,
}

pub struct SvgContract;

impl SvgContract {
    pub fn validate(svg: &str) -> PpResult<SvgReport> {
        Self::parse(svg).map(|(report, _)| report)
    }

    pub(crate) fn parse(svg: &str) -> PpResult<(SvgReport, SvgIr)> {
        let ir = parse_bounded(svg, SvgLimits::default())?;
        let metrics = metrics::collect(&ir);
        let report = SvgReport {
            path_count: metrics.path_count,
            color_count: ir
                .elements
                .iter()
                .filter(|element| element.local_name == "path")
                .filter_map(|element| element.paint.as_ref()?.fill.as_ref())
                .map(|fill| fill.trim().to_ascii_lowercase())
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            node_count: metrics.node_count,
            curve_segment_count: metrics.curve_segment_count,
            closed_path_count: metrics.closed_path_count,
            width: ir.root.width.unwrap_or(0),
            height: ir.root.height.unwrap_or(0),
            contains_raster_payload: false,
        };
        Ok((report, ir))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_metrics_count_implicit_repeated_segments() -> PpResult<()> {
        let report = SvgContract::validate(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"12\" height=\"8\" viewBox=\"0 0 12 8\"><path fill=\"#000\" d=\"M0 0 L1 1 2 2 C3 3 4 4 5 5 6 6 7 7 8 8 Z\"/></svg>",
        )?;
        assert_eq!(report.node_count, 4);
        assert_eq!(report.curve_segment_count, 2);
        assert_eq!(report.closed_path_count, 1);
        Ok(())
    }
}
