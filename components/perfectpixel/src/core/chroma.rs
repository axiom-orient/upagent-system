use std::collections::BTreeSet;

use super::{PpError, PpResult};

/// Stable public contract for controlled chroma planning.
pub const CHROMA_PLAN_SCHEMA: &str = "perfectpixel.chroma-plan/1";
/// Metric used for ranking the fixed candidate palette.
pub const CHROMA_PLAN_METRIC: &str = "oklab-euclidean-maximin/1";
/// Fixed, high-saturation sRGB candidates, in RGB lexicographic order.
pub const CHROMA_CANDIDATE_PALETTE: [[u8; 3]; 8] = [
    [0, 0, 255],
    [0, 255, 0],
    [0, 255, 255],
    [128, 0, 255],
    [255, 0, 0],
    [255, 0, 255],
    [255, 128, 0],
    [255, 255, 0],
];

const MAX_SUBJECT_COLORS: usize = 32;

#[derive(Debug, Clone, PartialEq)]
pub struct ChromaCandidateScore {
    pub rgb: [u8; 3],
    pub min_distance: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChromaPlan {
    pub selected_rgb: [u8; 3],
    pub min_distance: f64,
    pub candidates: Vec<ChromaCandidateScore>,
}

/// Choose a fixed high-saturation candidate that maximizes its minimum OKLab distance
/// from the supplied subject colors. Input validation is deliberately strict because the
/// result is intended to be used as a controlled background color, not as segmentation.
pub fn plan_chroma(subject_colors: &[[u8; 3]]) -> PpResult<ChromaPlan> {
    if subject_colors.is_empty() || subject_colors.len() > MAX_SUBJECT_COLORS {
        return Err(PpError::InvalidRequest(
            "chroma plan subjectRgbColors must contain 1..=32 colors".to_string(),
        ));
    }
    let unique = subject_colors.iter().copied().collect::<BTreeSet<_>>();
    if unique.len() != subject_colors.len() {
        return Err(PpError::InvalidRequest(
            "chroma plan subjectRgbColors must contain unique RGB colors".to_string(),
        ));
    }

    let candidates = CHROMA_CANDIDATE_PALETTE
        .iter()
        .copied()
        .map(|rgb| ChromaCandidateScore {
            rgb,
            min_distance: subject_colors
                .iter()
                .map(|subject| oklab_distance(rgb, *subject))
                .fold(f64::INFINITY, f64::min),
        })
        .collect::<Vec<_>>();
    let selected = choose_best_candidate(&candidates).ok_or_else(|| {
        PpError::InvalidRequest("chroma plan candidate palette is empty".to_string())
    })?;

    Ok(ChromaPlan {
        selected_rgb: selected.rgb,
        min_distance: selected.min_distance,
        candidates,
    })
}

fn choose_best_candidate(candidates: &[ChromaCandidateScore]) -> Option<&ChromaCandidateScore> {
    candidates.iter().min_by(|left, right| {
        right
            .min_distance
            .total_cmp(&left.min_distance)
            .then_with(|| left.rgb.cmp(&right.rgb))
    })
}

/// Euclidean distance in OKLab after explicit IEC sRGB transfer and the standard OKLab
/// linear-RGB matrices. Keeping the conversion here makes the metric independent of image
/// codecs and avoids a hidden color-management dependency.
pub fn oklab_distance(left: [u8; 3], right: [u8; 3]) -> f64 {
    let left = oklab(left);
    let right = oklab(right);
    let dl = left[0] - right[0];
    let da = left[1] - right[1];
    let db = left[2] - right[2];
    (dl * dl + da * da + db * db).sqrt()
}

fn oklab(rgb: [u8; 3]) -> [f64; 3] {
    let [r, g, b] = rgb.map(|channel| srgb_to_linear(f64::from(channel) / 255.0));
    let l = 0.412_221_470_8 * r + 0.536_332_536_3 * g + 0.051_445_992_9 * b;
    let m = 0.211_903_498_2 * r + 0.680_699_545_1 * g + 0.107_396_956_6 * b;
    let s = 0.088_302_461_9 * r + 0.281_718_837_6 * g + 0.629_978_700_5 * b;
    let l = l.cbrt();
    let m = m.cbrt();
    let s = s.cbrt();
    [
        0.210_454_255_3 * l + 0.793_617_785_0 * m - 0.004_072_046_8 * s,
        1.977_998_495_1 * l - 2.428_592_205_0 * m + 0.450_593_709_9 * s,
        0.025_904_037_1 * l + 0.782_771_766_2 * m - 0.808_675_766_0 * s,
    ]
}

fn srgb_to_linear(value: f64) -> f64 {
    if value <= 0.040_45 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oklab_planning_is_deterministic_and_bounded() {
        let subjects = [[255, 255, 255], [32, 32, 32], [180, 80, 20]];
        let first = plan_chroma(&subjects).unwrap();
        let second = plan_chroma(&subjects).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.candidates.len(), 8);
        assert!(first.min_distance.is_finite());
        assert!(first
            .candidates
            .windows(2)
            .all(|pair| pair[0].rgb < pair[1].rgb));
    }

    #[test]
    fn equal_scores_use_rgb_lexicographic_tie_break() {
        let candidates = vec![
            ChromaCandidateScore {
                rgb: [255, 0, 0],
                min_distance: 1.0,
            },
            ChromaCandidateScore {
                rgb: [0, 255, 0],
                min_distance: 1.0,
            },
        ];
        assert_eq!(choose_best_candidate(&candidates).unwrap().rgb, [0, 255, 0]);
    }

    #[test]
    fn empty_duplicate_and_excess_subject_colors_are_rejected() {
        assert!(plan_chroma(&[]).is_err());
        assert!(plan_chroma(&[[1, 2, 3], [1, 2, 3]]).is_err());
        assert!(plan_chroma(&[[0, 0, 0]; 33]).is_err());
    }
}
