#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Lab {
    pub l: f64,
    pub a: f64,
    pub b: f64,
}

/// Deterministic IEC 61966-2-1 sRGB8 -> linear-sRGB u16 transfer.
pub fn srgb8_to_linear16(value: u8) -> u16 {
    SRGB8_TO_LINEAR16[value as usize]
}

/// Deterministic nearest-code linear-sRGB u16 -> sRGB8 transfer.
pub fn linear16_to_srgb8(value: u16) -> u8 {
    match SRGB8_TO_LINEAR16.binary_search(&value) {
        Ok(index) => index as u8,
        Err(0) => 0,
        Err(256) => 255,
        Err(index) => {
            let lower = SRGB8_TO_LINEAR16[index - 1];
            let upper = SRGB8_TO_LINEAR16[index];
            if value - lower <= upper - value {
                (index - 1) as u8
            } else {
                index as u8
            }
        }
    }
}

/// Converts sRGB8 to CIE L*a*b* using D65/2° white. This is a verification metric path, not a
/// publication-byte transform; canonical publication transfer remains the integer LUT above.
pub fn srgb8_to_lab(rgb: [u8; 3]) -> Lab {
    let r = f64::from(srgb8_to_linear16(rgb[0])) / 65_535.0;
    let g = f64::from(srgb8_to_linear16(rgb[1])) / 65_535.0;
    let b = f64::from(srgb8_to_linear16(rgb[2])) / 65_535.0;

    // IEC sRGB primaries, D65 white.
    let x = 0.412_456_4 * r + 0.357_576_1 * g + 0.180_437_5 * b;
    let y = 0.212_672_9 * r + 0.715_152_2 * g + 0.072_175_0 * b;
    let z = 0.019_333_9 * r + 0.119_192_0 * g + 0.950_304_1 * b;

    let fx = lab_f(x / 0.950_47);
    let fy = lab_f(y);
    let fz = lab_f(z / 1.088_83);
    Lab {
        l: 116.0 * fy - 16.0,
        a: 500.0 * (fx - fy),
        b: 200.0 * (fy - fz),
    }
}

fn lab_f(value: f64) -> f64 {
    const DELTA: f64 = 6.0 / 29.0;
    const DELTA_CUBED: f64 = DELTA * DELTA * DELTA;
    if value > DELTA_CUBED {
        value.cbrt()
    } else {
        value / (3.0 * DELTA * DELTA) + 4.0 / 29.0
    }
}

/// CIEDE2000 color difference, kL=kC=kH=1 (reference/viewing-condition defaults).
pub fn delta_e2000(left: Lab, right: Lab) -> f64 {
    use std::f64::consts::{PI, TAU};

    let c1 = left.a.hypot(left.b);
    let c2 = right.a.hypot(right.b);
    let c_bar = (c1 + c2) / 2.0;
    let c7 = c_bar.powi(7);
    let g = 0.5 * (1.0 - (c7 / (c7 + 25.0_f64.powi(7))).sqrt());

    let a1_prime = (1.0 + g) * left.a;
    let a2_prime = (1.0 + g) * right.a;
    let c1_prime = a1_prime.hypot(left.b);
    let c2_prime = a2_prime.hypot(right.b);
    let h1_prime = hue_radians(left.b, a1_prime);
    let h2_prime = hue_radians(right.b, a2_prime);

    let delta_l_prime = right.l - left.l;
    let delta_c_prime = c2_prime - c1_prime;
    let delta_h_angle = if c1_prime == 0.0 || c2_prime == 0.0 {
        0.0
    } else {
        let delta = h2_prime - h1_prime;
        if delta.abs() <= PI {
            delta
        } else if delta > PI {
            delta - TAU
        } else {
            delta + TAU
        }
    };
    let delta_h_prime = 2.0 * (c1_prime * c2_prime).sqrt() * (delta_h_angle / 2.0).sin();

    let l_bar_prime = (left.l + right.l) / 2.0;
    let c_bar_prime = (c1_prime + c2_prime) / 2.0;
    let h_bar_prime = if c1_prime == 0.0 || c2_prime == 0.0 {
        h1_prime + h2_prime
    } else if (h1_prime - h2_prime).abs() <= PI {
        (h1_prime + h2_prime) / 2.0
    } else if h1_prime + h2_prime < TAU {
        (h1_prime + h2_prime + TAU) / 2.0
    } else {
        (h1_prime + h2_prime - TAU) / 2.0
    };

    let t = 1.0 - 0.17 * (h_bar_prime - degrees(30.0)).cos()
        + 0.24 * (2.0 * h_bar_prime).cos()
        + 0.32 * (3.0 * h_bar_prime + degrees(6.0)).cos()
        - 0.20 * (4.0 * h_bar_prime - degrees(63.0)).cos();
    let delta_theta =
        degrees(30.0) * (-(((radians_to_degrees(h_bar_prime) - 275.0) / 25.0).powi(2))).exp();
    let c_bar_prime7 = c_bar_prime.powi(7);
    let r_c = 2.0 * (c_bar_prime7 / (c_bar_prime7 + 25.0_f64.powi(7))).sqrt();
    let l_delta = l_bar_prime - 50.0;
    let s_l = 1.0 + 0.015 * l_delta * l_delta / (20.0 + l_delta * l_delta).sqrt();
    let s_c = 1.0 + 0.045 * c_bar_prime;
    let s_h = 1.0 + 0.015 * c_bar_prime * t;
    let r_t = -r_c * (2.0 * delta_theta).sin();

    let l_term = delta_l_prime / s_l;
    let c_term = delta_c_prime / s_c;
    let h_term = delta_h_prime / s_h;
    (l_term * l_term + c_term * c_term + h_term * h_term + r_t * c_term * h_term).sqrt()
}

fn hue_radians(b: f64, a_prime: f64) -> f64 {
    use std::f64::consts::TAU;
    let value = b.atan2(a_prime);
    if value < 0.0 {
        value + TAU
    } else {
        value
    }
}

const fn degrees(value: f64) -> f64 {
    value * std::f64::consts::PI / 180.0
}

fn radians_to_degrees(value: f64) -> f64 {
    value * 180.0 / std::f64::consts::PI
}

// IEC 61966-2-1 sRGB transfer sampled at every 8-bit code value, rounded to u16 linear light.
const SRGB8_TO_LINEAR16: [u16; 256] = [
    0, 20, 40, 60, 80, 99, 119, 139, 159, 179, 199, 219, 241, 264, 288, 313, 340, 367, 396, 427,
    458, 491, 526, 562, 599, 637, 677, 718, 761, 805, 851, 898, 947, 997, 1048, 1101, 1156, 1212,
    1270, 1330, 1391, 1453, 1517, 1583, 1651, 1720, 1790, 1863, 1937, 2013, 2090, 2170, 2250, 2333,
    2418, 2504, 2592, 2681, 2773, 2866, 2961, 3058, 3157, 3258, 3360, 3464, 3570, 3678, 3788, 3900,
    4014, 4129, 4247, 4366, 4488, 4611, 4736, 4864, 4993, 5124, 5257, 5392, 5530, 5669, 5810, 5953,
    6099, 6246, 6395, 6547, 6700, 6856, 7014, 7174, 7335, 7500, 7666, 7834, 8004, 8177, 8352, 8528,
    8708, 8889, 9072, 9258, 9445, 9635, 9828, 10022, 10219, 10417, 10619, 10822, 11028, 11235,
    11446, 11658, 11873, 12090, 12309, 12530, 12754, 12980, 13209, 13440, 13673, 13909, 14146,
    14387, 14629, 14874, 15122, 15371, 15623, 15878, 16135, 16394, 16656, 16920, 17187, 17456,
    17727, 18001, 18277, 18556, 18837, 19121, 19407, 19696, 19987, 20281, 20577, 20876, 21177,
    21481, 21787, 22096, 22407, 22721, 23038, 23357, 23678, 24002, 24329, 24658, 24990, 25325,
    25662, 26001, 26344, 26688, 27036, 27386, 27739, 28094, 28452, 28813, 29176, 29542, 29911,
    30282, 30656, 31033, 31412, 31794, 32179, 32567, 32957, 33350, 33745, 34143, 34544, 34948,
    35355, 35764, 36176, 36591, 37008, 37429, 37852, 38278, 38706, 39138, 39572, 40009, 40449,
    40891, 41337, 41785, 42236, 42690, 43147, 43606, 44069, 44534, 45002, 45473, 45947, 46423,
    46903, 47385, 47871, 48359, 48850, 49344, 49841, 50341, 50844, 51349, 51858, 52369, 52884,
    53401, 53921, 54445, 54971, 55500, 56032, 56567, 57105, 57646, 58190, 58737, 59287, 59840,
    60396, 60955, 61517, 62082, 62650, 63221, 63795, 64372, 64952, 65535,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transfer_table_round_trips_all_codes() {
        for code in 0u8..=255 {
            assert_eq!(linear16_to_srgb8(srgb8_to_linear16(code)), code);
        }
    }

    #[test]
    fn ciede2000_matches_sharma_reference_pair() {
        let left = Lab {
            l: 50.0,
            a: 2.6772,
            b: -79.7751,
        };
        let right = Lab {
            l: 50.0,
            a: 0.0,
            b: -82.7485,
        };
        let difference = delta_e2000(left, right);
        assert!((difference - 2.0425).abs() < 0.0001, "{difference}");
    }
}
