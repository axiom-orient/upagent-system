use super::*;

use std::path::{Component, Path};

use super::pipeline::{is_safe_file_name, is_safe_state_name};

const MAX_PALETTE_SIZE: usize = 256;
const MAX_CHROMA_UNMIX_REACH: u8 = 32;

pub(super) fn normalize_contract_error(message: String) -> PpError {
    PpError::InvalidOption(message)
}

pub(super) fn validate_normalize_request(
    request: &NormalizeRequest,
    sources: &[NormalizeStateImages],
) -> Vec<String> {
    let mut errors = Vec::new();
    if request.character.trim().is_empty() {
        errors.push("character is required".to_string());
    }
    if request.cell_width == 0 || request.cell_height == 0 {
        errors.push("cellWidth and cellHeight must be greater than zero".to_string());
    }
    if request.safe_margin_x.saturating_mul(2) >= request.cell_width && request.cell_width > 0 {
        errors.push("safeMarginX leaves no usable cell width".to_string());
    }
    if request.safe_margin_y.saturating_mul(2) >= request.cell_height && request.cell_height > 0 {
        errors.push("safeMarginY leaves no usable cell height".to_string());
    }
    if !is_safe_file_name(&request.sheet_image)
        || !request.sheet_image.to_ascii_lowercase().ends_with(".png")
    {
        errors.push("sheetImage must be a safe .png file name".to_string());
    }
    if request.states.is_empty() {
        errors.push("at least one state is required".to_string());
    }
    if request.states.len() != sources.len() {
        errors.push(format!(
            "loaded source count {} does not match request state count {}",
            sources.len(),
            request.states.len()
        ));
    }
    if !matches!(
        request.fit.resample.as_str(),
        "nearest" | "kcentroid" | "lanczos"
    ) {
        errors.push("fit.resample must be nearest, kcentroid, or lanczos".to_string());
    }
    if !matches!(
        request.fit.align_x.as_str(),
        "foot-centroid" | "centroid" | "bbox-center"
    ) {
        errors.push("fit.alignX must be foot-centroid, centroid, or bbox-center".to_string());
    }
    if request.fit.align_y != "bottom" {
        errors.push("fit.alignY must be bottom".to_string());
    }
    if !(1..=MAX_PALETTE_SIZE).contains(&request.fit.palette_size) {
        errors.push(format!(
            "fit.paletteSize must be from 1 through {MAX_PALETTE_SIZE}"
        ));
    }
    if !request.fit.pixel_perfect && request.fit.logical_height.is_some() {
        errors.push("fit.logicalHeight requires fit.pixelPerfect=true".to_string());
    }
    if !request.fit.pixel_perfect && request.fit.pitch_hint.is_some() {
        errors.push("fit.pitchHint requires fit.pixelPerfect=true".to_string());
    }
    if request.fit.pitch_hint.is_some_and(|pitch| pitch < 2) {
        errors.push("fit.pitchHint must be at least 2".to_string());
    }
    if request.fit.pixel_perfect && request.safe_margin_x > 0 {
        errors.push("safeMarginX is unsupported when fit.pixelPerfect=true".to_string());
    }
    if !request.fit.outline.strength.is_finite()
        || !(0.0..=1.0).contains(&request.fit.outline.strength)
    {
        errors.push("fit.outline.strength must be between 0 and 1".to_string());
    }
    if let Some(chroma) = &request.chroma {
        let chroma_distances = [
            chroma.threshold,
            chroma.fringe_threshold,
            chroma.fringe_delta,
            chroma.adjacent_threshold,
        ];
        if chroma_distances
            .iter()
            .any(|value| !value.is_finite() || *value < 0.0)
        {
            errors.push("chroma thresholds must be finite and zero or positive".to_string());
        } else if chroma.fringe_threshold < chroma.threshold {
            errors.push(
                "chroma.fringeThreshold must be greater than or equal to chroma.threshold"
                    .to_string(),
            );
        }
        if chroma.unmix_reach > MAX_CHROMA_UNMIX_REACH {
            errors.push(format!(
                "chroma.unmixReach must be at most {MAX_CHROMA_UNMIX_REACH}"
            ));
        }
        if !chroma.spill_max_fraction.is_finite()
            || !(0.0..=1.0).contains(&chroma.spill_max_fraction)
        {
            errors.push("chroma.spillMaxFraction must be between 0 and 1".to_string());
        }
    }
    for (name, value) in [
        (
            "quality.maxContentHeightVariancePx",
            request.quality.max_content_height_variance_px,
        ),
        (
            "quality.maxContentHeightVarianceRatio",
            request.quality.max_content_height_variance_ratio,
        ),
        (
            "quality.maxGroundYVariancePx",
            request.quality.max_ground_y_variance_px,
        ),
        (
            "quality.maxRegistrationDriftPx",
            request.quality.max_registration_drift_px,
        ),
    ] {
        if !value.is_finite() || value < 0.0 {
            errors.push(format!("{name} must be finite and zero or positive"));
        }
    }
    let mut seen_states = BTreeSet::new();
    for (index, state) in request.states.iter().enumerate() {
        if !is_safe_state_name(&state.name) {
            errors.push(format!(
                "state '{}' is not safe for output paths",
                state.name
            ));
        }
        if !seen_states.insert(state.name.as_str()) {
            errors.push(format!("duplicate state name '{}'", state.name));
        }
        if !(1..=1000).contains(&state.fps) {
            errors.push(format!(
                "state '{}' fps must be from 1 through 1000",
                state.name
            ));
        }
        let has_frames = !state.frames.is_empty();
        let has_strip = state.strip.is_some();
        if has_frames == has_strip {
            errors.push(format!(
                "state '{}' must use exactly one source: frames or strip",
                state.name
            ));
        }
        if has_strip && state.frame_count.unwrap_or(0) == 0 {
            errors.push(format!(
                "state '{}' using strip must set frameCount greater than zero",
                state.name
            ));
        }
        for frame in &state.frames {
            validate_source_path(frame, &mut errors);
        }
        if let Some(strip) = state.strip.as_deref() {
            validate_source_path(strip, &mut errors);
        }
        if let Some(source) = sources.get(index) {
            if source.name != state.name {
                errors.push(format!(
                    "loaded source '{}' does not match request state '{}'",
                    source.name, state.name
                ));
            }
            match (&source.source, has_strip) {
                (NormalizeStateSource::Frames(frames), false) if frames.is_empty() => {
                    errors.push(format!("state '{}' has no loaded frames", state.name));
                }
                (NormalizeStateSource::Strip { frame_count, .. }, true)
                    if *frame_count != state.frame_count.unwrap_or(0) =>
                {
                    errors.push(format!("state '{}' loaded frameCount mismatch", state.name));
                }
                (NormalizeStateSource::Frames(_), true)
                | (NormalizeStateSource::Strip { .. }, false) => {
                    errors.push(format!(
                        "state '{}' loaded source kind mismatch",
                        state.name
                    ));
                }
                _ => {}
            }
        }
    }
    errors
}

fn validate_source_path(value: &str, errors: &mut Vec<String>) {
    if value.trim().is_empty()
        || value != value.trim()
        || value.contains('\0')
        || value.contains('\\')
    {
        errors.push(format!(
            "frame path '{}' is not a safe relative path",
            value
        ));
        return;
    }
    let path = Path::new(value);
    if path.is_absolute() {
        errors.push(format!(
            "frame path '{}' must be relative to the request file",
            value
        ));
    } else if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        errors.push(format!("frame path '{}' must not contain '..'", value));
    }
}
