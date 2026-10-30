use super::*;

use super::pipeline::median_f64;

pub(super) fn normalize_quality_error(message: String) -> PpError {
    PpError::InvalidOption(message)
}

pub(super) fn state_quality_gates(
    request: &NormalizeRequest,
    report: &NormalizeStateReport,
) -> Vec<NormalizeGateReport> {
    let mut gates = Vec::new();
    let heights = report
        .frame_records
        .iter()
        .filter_map(|record| record.content_box.map(|rect| f64::from(rect.h)))
        .collect::<Vec<_>>();
    if !heights.is_empty() {
        let median_height = median_f64(&heights);
        let height_limit = request
            .quality
            .max_content_height_variance_px
            .max(median_height * request.quality.max_content_height_variance_ratio);
        gates.push(NormalizeGateReport {
            name: format!("{}.contentHeightVariance", report.name),
            ok: report.content_height_range <= height_limit,
            observed: report.content_height_range,
            limit: height_limit,
            unit: "px".to_string(),
        });
    }
    gates.push(NormalizeGateReport {
        name: format!("{}.groundYVariance", report.name),
        ok: report.ground_y_range <= request.quality.max_ground_y_variance_px,
        observed: report.ground_y_range,
        limit: request.quality.max_ground_y_variance_px,
        unit: "px".to_string(),
    });
    gates.push(NormalizeGateReport {
        name: format!("{}.registrationDrift", report.name),
        ok: report.center_x_range <= request.quality.max_registration_drift_px,
        observed: report.center_x_range,
        limit: request.quality.max_registration_drift_px,
        unit: "px".to_string(),
    });
    if let Some(chroma) = &request.chroma {
        let observed = report
            .frame_records
            .iter()
            .map(|record| record.chroma_adjacent_pixels)
            .max()
            .unwrap_or(0) as f64;
        gates.push(NormalizeGateReport {
            name: format!("{}.chromaAdjacentPixels", report.name),
            ok: observed <= f64::from(chroma.adjacent_pixel_threshold),
            observed,
            limit: f64::from(chroma.adjacent_pixel_threshold),
            unit: "px".to_string(),
        });
    }
    gates
}

pub(super) fn default_content_height_variance_px() -> f64 {
    1.0
}

pub(super) fn default_content_height_variance_ratio() -> f64 {
    0.02
}
