use super::{PpError, PpResult};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point2 {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect2 {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect2 {
    pub fn new(x: f64, y: f64, width: f64, height: f64) -> PpResult<Self> {
        if ![x, y, width, height].into_iter().all(f64::is_finite) || width < 0.0 || height < 0.0 {
            return Err(PpError::InvalidRequest(
                "geometry rectangle must be finite with non-negative size".to_string(),
            ));
        }
        Ok(Self {
            x,
            y,
            width,
            height,
        })
    }

    pub fn right(self) -> f64 {
        self.x + self.width
    }

    pub fn bottom(self) -> f64 {
        self.y + self.height
    }

    pub fn intersection(self, other: Self) -> Option<Self> {
        let x0 = self.x.max(other.x);
        let y0 = self.y.max(other.y);
        let x1 = self.right().min(other.right());
        let y1 = self.bottom().min(other.bottom());
        if x1 < x0 || y1 < y0 {
            return None;
        }
        Some(Self {
            x: x0,
            y: y0,
            width: x1 - x0,
            height: y1 - y0,
        })
    }

    pub fn union(self, other: Self) -> Self {
        let x0 = self.x.min(other.x);
        let y0 = self.y.min(other.y);
        let x1 = self.right().max(other.right());
        let y1 = self.bottom().max(other.bottom());
        Self {
            x: x0,
            y: y0,
            width: x1 - x0,
            height: y1 - y0,
        }
    }
}

/// Row-major homogeneous 3x3 transform. The matrix is immutable and validated on construction.
/// Geometry uses f64 only for vector-space metadata. Raster sampling must define its own explicit
/// quantization/filter policy rather than inheriting floating-point rounding from this type.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transform3 {
    values: [f64; 9],
}

impl Transform3 {
    pub fn new(values: [f64; 9]) -> PpResult<Self> {
        if !values.into_iter().all(f64::is_finite) {
            return Err(PpError::InvalidRequest(
                "geometry transform values must be finite".to_string(),
            ));
        }
        Ok(Self { values })
    }

    pub const fn identity() -> Self {
        Self {
            values: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
        }
    }

    pub fn translation(tx: f64, ty: f64) -> PpResult<Self> {
        Self::new([1.0, 0.0, tx, 0.0, 1.0, ty, 0.0, 0.0, 1.0])
    }

    pub fn scale(sx: f64, sy: f64) -> PpResult<Self> {
        Self::new([sx, 0.0, 0.0, 0.0, sy, 0.0, 0.0, 0.0, 1.0])
    }

    pub fn rotation_radians(radians: f64) -> PpResult<Self> {
        if !radians.is_finite() {
            return Err(PpError::InvalidRequest(
                "rotation must be finite".to_string(),
            ));
        }
        let cosine = radians.cos();
        let sine = radians.sin();
        Self::new([cosine, -sine, 0.0, sine, cosine, 0.0, 0.0, 0.0, 1.0])
    }

    pub fn affine(a: f64, b: f64, c: f64, d: f64, tx: f64, ty: f64) -> PpResult<Self> {
        Self::new([a, c, tx, b, d, ty, 0.0, 0.0, 1.0])
    }

    pub fn projective(values: [f64; 9]) -> PpResult<Self> {
        Self::new(values)
    }

    pub fn values(self) -> [f64; 9] {
        self.values
    }

    /// Composition in application order: `self.then(next)` applies `self` first, then `next`.
    pub fn then(self, next: Self) -> Self {
        Self {
            values: multiply(next.values, self.values),
        }
    }

    pub fn apply(self, point: Point2) -> PpResult<Point2> {
        if !point.x.is_finite() || !point.y.is_finite() {
            return Err(PpError::InvalidRequest(
                "geometry point must be finite".to_string(),
            ));
        }
        let m = self.values;
        let denominator = m[6] * point.x + m[7] * point.y + m[8];
        if !denominator.is_finite() || denominator.abs() <= f64::EPSILON {
            return Err(PpError::InvalidRequest(
                "projective transform maps point to infinity".to_string(),
            ));
        }
        let x = (m[0] * point.x + m[1] * point.y + m[2]) / denominator;
        let y = (m[3] * point.x + m[4] * point.y + m[5]) / denominator;
        if !x.is_finite() || !y.is_finite() {
            return Err(PpError::InvalidRequest(
                "geometry transform produced a non-finite point".to_string(),
            ));
        }
        Ok(Point2 { x, y })
    }

    pub fn bounds(self, rect: Rect2) -> PpResult<Rect2> {
        let corners = [
            Point2 {
                x: rect.x,
                y: rect.y,
            },
            Point2 {
                x: rect.right(),
                y: rect.y,
            },
            Point2 {
                x: rect.x,
                y: rect.bottom(),
            },
            Point2 {
                x: rect.right(),
                y: rect.bottom(),
            },
        ];
        let mut transformed = [Point2 { x: 0.0, y: 0.0 }; 4];
        for (slot, point) in transformed.iter_mut().zip(corners) {
            *slot = self.apply(point)?;
        }
        let min_x = transformed
            .iter()
            .map(|point| point.x)
            .fold(f64::INFINITY, f64::min);
        let max_x = transformed
            .iter()
            .map(|point| point.x)
            .fold(f64::NEG_INFINITY, f64::max);
        let min_y = transformed
            .iter()
            .map(|point| point.y)
            .fold(f64::INFINITY, f64::min);
        let max_y = transformed
            .iter()
            .map(|point| point.y)
            .fold(f64::NEG_INFINITY, f64::max);
        Rect2::new(min_x, min_y, max_x - min_x, max_y - min_y)
    }

    pub fn inverse(self) -> PpResult<Self> {
        let m = self.values;
        let determinant = m[0] * (m[4] * m[8] - m[5] * m[7]) - m[1] * (m[3] * m[8] - m[5] * m[6])
            + m[2] * (m[3] * m[7] - m[4] * m[6]);
        if !determinant.is_finite() || determinant.abs() <= f64::EPSILON {
            return Err(PpError::InvalidRequest(
                "geometry transform is singular".to_string(),
            ));
        }
        let inverse = 1.0 / determinant;
        Self::new([
            (m[4] * m[8] - m[5] * m[7]) * inverse,
            (m[2] * m[7] - m[1] * m[8]) * inverse,
            (m[1] * m[5] - m[2] * m[4]) * inverse,
            (m[5] * m[6] - m[3] * m[8]) * inverse,
            (m[0] * m[8] - m[2] * m[6]) * inverse,
            (m[2] * m[3] - m[0] * m[5]) * inverse,
            (m[3] * m[7] - m[4] * m[6]) * inverse,
            (m[1] * m[6] - m[0] * m[7]) * inverse,
            (m[0] * m[4] - m[1] * m[3]) * inverse,
        ])
    }
}

fn multiply(left: [f64; 9], right: [f64; 9]) -> [f64; 9] {
    let mut out = [0.0; 9];
    for row in 0..3 {
        for column in 0..3 {
            out[row * 3 + column] = (0..3)
                .map(|index| left[row * 3 + index] * right[index * 3 + column])
                .sum();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composition_order_is_explicit() -> PpResult<()> {
        let transform = Transform3::translation(10.0, 5.0)?.then(Transform3::scale(2.0, 3.0)?);
        let point = transform.apply(Point2 { x: 1.0, y: 1.0 })?;
        assert_eq!(point, Point2 { x: 22.0, y: 18.0 });
        Ok(())
    }

    #[test]
    fn inverse_round_trips_affine_point() -> PpResult<()> {
        let transform = Transform3::affine(2.0, 0.0, 0.0, 3.0, 4.0, -7.0)?;
        let source = Point2 { x: 3.0, y: 5.0 };
        let mapped = transform.apply(source)?;
        let restored = transform.inverse()?.apply(mapped)?;
        assert!((restored.x - source.x).abs() < 1e-12);
        assert!((restored.y - source.y).abs() < 1e-12);
        Ok(())
    }
}
