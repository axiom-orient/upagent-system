use std::collections::VecDeque;

use super::{FrameRect, PpError, PpResult, Raster};

const MAX_KERNEL_RADIUS: u32 = 1024;

/// Canonical single-channel alpha/mask storage. Values are straight coverage in 0..=255.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mask {
    width: u32,
    height: u32,
    values: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AlphaHistogram {
    pub bins: [u64; 256],
    pub transparent: u64,
    pub opaque: u64,
    pub partial: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectedComponent {
    pub bounds: FrameRect,
    pub pixels: u64,
}

impl Mask {
    pub fn new(width: u32, height: u32, values: Vec<u8>) -> PpResult<Self> {
        let len = mask_len(width, height)?;
        if values.len() != len {
            return Err(PpError::InvalidRequest(format!(
                "mask buffer length mismatch: got {}, expected {} for {}x{}",
                values.len(),
                len,
                width,
                height
            )));
        }
        Ok(Self {
            width,
            height,
            values,
        })
    }

    pub fn blank(width: u32, height: u32) -> PpResult<Self> {
        let len = mask_len(width, height)?;
        Ok(Self {
            width,
            height,
            values: vec![0; len],
        })
    }

    pub fn from_raster_alpha(raster: &Raster) -> PpResult<Self> {
        Self::new(
            raster.width(),
            raster.height(),
            raster
                .pixels()
                .as_chunks::<4>()
                .0
                .iter()
                .map(|pixel| pixel[3])
                .collect(),
        )
    }

    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
    pub fn values(&self) -> &[u8] {
        &self.values
    }

    pub fn threshold(&self, threshold: u8) -> Self {
        Self {
            width: self.width,
            height: self.height,
            values: self
                .values
                .iter()
                .map(|&value| if value >= threshold { 255 } else { 0 })
                .collect(),
        }
    }

    pub fn erode(&self, radius: u32) -> PpResult<Self> {
        morphology(self, radius, Morphology::Minimum)
    }
    pub fn dilate(&self, radius: u32) -> PpResult<Self> {
        morphology(self, radius, Morphology::Maximum)
    }
    pub fn open(&self, radius: u32) -> PpResult<Self> {
        self.erode(radius)?.dilate(radius)
    }
    pub fn close(&self, radius: u32) -> PpResult<Self> {
        self.dilate(radius)?.erode(radius)
    }

    /// Deterministic separable box feather. Each axis is O(pixels), independent of radius.
    pub fn feather(&self, radius: u32) -> PpResult<Self> {
        validate_radius(radius, "feather")?;
        if radius == 0 {
            return Ok(self.clone());
        }
        let horizontal = box_blur_axis(self, radius, Axis::Horizontal)?;
        box_blur_axis(&horizontal, radius, Axis::Vertical)
    }

    pub fn bounding_box(&self, threshold: u8) -> FrameRect {
        let mut min_x = self.width;
        let mut min_y = self.height;
        let mut max_x = None;
        let mut max_y = None;
        for y in 0..self.height {
            for x in 0..self.width {
                if self.get(x, y) < threshold {
                    continue;
                }
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = Some(max_x.map_or(x, |value: u32| value.max(x)));
                max_y = Some(max_y.map_or(y, |value: u32| value.max(y)));
            }
        }
        let (Some(max_x), Some(max_y)) = (max_x, max_y) else {
            return FrameRect::default();
        };
        FrameRect {
            x: min_x,
            y: min_y,
            w: max_x - min_x + 1,
            h: max_y - min_y + 1,
        }
    }

    pub fn histogram(&self) -> AlphaHistogram {
        let mut bins = [0u64; 256];
        for &value in &self.values {
            bins[value as usize] += 1;
        }
        let transparent = bins[0];
        let opaque = bins[255];
        let total = self.values.len() as u64;
        AlphaHistogram {
            bins,
            transparent,
            opaque,
            partial: total - transparent - opaque,
        }
    }

    pub fn coverage_basis_points(&self, threshold: u8) -> u16 {
        let covered = self
            .values
            .iter()
            .filter(|&&value| value >= threshold)
            .count() as u64;
        let total = self.values.len() as u64;
        ((covered * 10_000 + total / 2) / total).min(10_000) as u16
    }

    /// Four-connected components, deterministic by decreasing area then top-left bounds.
    pub fn connected_components(&self, threshold: u8) -> Vec<ConnectedComponent> {
        let mut visited = vec![false; self.values.len()];
        let mut components = Vec::new();
        for y in 0..self.height {
            for x in 0..self.width {
                let start = self.index(x, y);
                if visited[start] || self.values[start] < threshold {
                    continue;
                }
                visited[start] = true;
                let mut queue = VecDeque::from([(x, y)]);
                let (mut min_x, mut min_y, mut max_x, mut max_y) = (x, y, x, y);
                let mut pixels = 0u64;
                while let Some((cx, cy)) = queue.pop_front() {
                    pixels += 1;
                    min_x = min_x.min(cx);
                    min_y = min_y.min(cy);
                    max_x = max_x.max(cx);
                    max_y = max_y.max(cy);
                    for (nx, ny) in neighbors4(cx, cy, self.width, self.height) {
                        let index = self.index(nx, ny);
                        if !visited[index] && self.values[index] >= threshold {
                            visited[index] = true;
                            queue.push_back((nx, ny));
                        }
                    }
                }
                components.push(ConnectedComponent {
                    bounds: FrameRect {
                        x: min_x,
                        y: min_y,
                        w: max_x - min_x + 1,
                        h: max_y - min_y + 1,
                    },
                    pixels,
                });
            }
        }
        components.sort_by_key(|component| {
            (
                std::cmp::Reverse(component.pixels),
                component.bounds.y,
                component.bounds.x,
                component.bounds.h,
                component.bounds.w,
            )
        });
        components
    }

    /// Fills zero-valued holes not four-connected to an image edge.
    pub fn fill_holes(&self) -> Self {
        let mut exterior = vec![false; self.values.len()];
        let mut queue = VecDeque::new();
        for x in 0..self.width {
            enqueue_zero(self, x, 0, &mut exterior, &mut queue);
            if self.height > 1 {
                enqueue_zero(self, x, self.height - 1, &mut exterior, &mut queue);
            }
        }
        for y in 0..self.height {
            enqueue_zero(self, 0, y, &mut exterior, &mut queue);
            if self.width > 1 {
                enqueue_zero(self, self.width - 1, y, &mut exterior, &mut queue);
            }
        }
        while let Some((x, y)) = queue.pop_front() {
            for (nx, ny) in neighbors4(x, y, self.width, self.height) {
                enqueue_zero(self, nx, ny, &mut exterior, &mut queue);
            }
        }
        Self {
            width: self.width,
            height: self.height,
            values: self
                .values
                .iter()
                .enumerate()
                .map(|(index, &value)| {
                    if value == 0 && !exterior[index] {
                        255
                    } else {
                        value
                    }
                })
                .collect(),
        }
    }

    fn get(&self, x: u32, y: u32) -> u8 {
        self.values[self.index(x, y)]
    }
    fn index(&self, x: u32, y: u32) -> usize {
        y as usize * self.width as usize + x as usize
    }
}

/// Removes a known solid matte from observed straight RGB and replaces alpha with `matte`.
pub fn decontaminate_known_background(
    foreground: &Raster,
    matte: &Mask,
    background: [u8; 3],
) -> PpResult<Raster> {
    if foreground.width() != matte.width() || foreground.height() != matte.height() {
        return Err(PpError::InvalidRequest(
            "foreground and matte dimensions must match".to_string(),
        ));
    }
    let mut pixels = foreground.pixels().to_vec();
    for (index, pixel) in pixels.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let alpha = u32::from(matte.values[index]);
        pixel[3] = matte.values[index];
        if alpha == 0 {
            pixel[..3].fill(0);
            continue;
        }
        for channel in 0..3 {
            let observed = i64::from(pixel[channel]) * 255;
            let contamination = i64::from(background[channel]) * i64::from(255 - alpha);
            pixel[channel] =
                div_round_nearest(observed - contamination, i64::from(alpha)).clamp(0, 255) as u8;
        }
    }
    Raster::new(foreground.width(), foreground.height(), pixels)
}

fn div_round_nearest(numerator: i64, denominator: i64) -> i64 {
    debug_assert!(denominator > 0);
    if numerator >= 0 {
        (numerator + denominator / 2) / denominator
    } else {
        (numerator - denominator / 2) / denominator
    }
}

fn mask_len(width: u32, height: u32) -> PpResult<usize> {
    if width == 0 || height == 0 {
        return Err(PpError::InvalidRequest(format!(
            "mask dimensions must be positive: {width}x{height}"
        )));
    }
    (width as usize)
        .checked_mul(height as usize)
        .ok_or_else(|| PpError::InvalidRequest("mask dimensions overflow".to_string()))
}

fn validate_radius(radius: u32, operation: &str) -> PpResult<()> {
    if radius > MAX_KERNEL_RADIUS {
        return Err(PpError::InvalidRequest(format!(
            "{operation} radius {radius} exceeds {MAX_KERNEL_RADIUS}"
        )));
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum Morphology {
    Minimum,
    Maximum,
}
#[derive(Clone, Copy)]
enum Axis {
    Horizontal,
    Vertical,
}

fn morphology(mask: &Mask, radius: u32, operation: Morphology) -> PpResult<Mask> {
    validate_radius(radius, "morphology")?;
    if radius == 0 {
        return Ok(mask.clone());
    }
    let horizontal = morphology_axis(mask, radius, Axis::Horizontal, operation)?;
    morphology_axis(&horizontal, radius, Axis::Vertical, operation)
}

fn morphology_axis(mask: &Mask, radius: u32, axis: Axis, operation: Morphology) -> PpResult<Mask> {
    let mut output = Mask::blank(mask.width, mask.height)?;
    let lines = match axis {
        Axis::Horizontal => mask.height,
        Axis::Vertical => mask.width,
    };
    let line_len = match axis {
        Axis::Horizontal => mask.width as usize,
        Axis::Vertical => mask.height as usize,
    };
    let mut input_line = vec![0u8; line_len];
    let mut output_line = vec![0u8; line_len];
    for line in 0..lines {
        read_line(mask, axis, line, &mut input_line);
        sliding_extreme(&input_line, radius as usize, operation, &mut output_line);
        write_line(&mut output, axis, line, &output_line);
    }
    Ok(output)
}

fn sliding_extreme(input: &[u8], radius: usize, operation: Morphology, output: &mut [u8]) {
    let mut deque = VecDeque::<usize>::new();
    let mut next = 0usize;
    for (center, output_value) in output.iter_mut().enumerate().take(input.len()) {
        let wanted_end = center.saturating_add(radius).min(input.len() - 1);
        while next <= wanted_end {
            while deque
                .back()
                .is_some_and(|&index| dominates(input[next], input[index], operation))
            {
                deque.pop_back();
            }
            deque.push_back(next);
            next += 1;
        }
        let wanted_start = center.saturating_sub(radius);
        while deque.front().is_some_and(|&index| index < wanted_start) {
            deque.pop_front();
        }
        *output_value = input[*deque.front().expect("non-empty clipped morphology window")];
    }
}

fn dominates(candidate: u8, current: u8, operation: Morphology) -> bool {
    match operation {
        Morphology::Minimum => candidate <= current,
        Morphology::Maximum => candidate >= current,
    }
}

fn box_blur_axis(mask: &Mask, radius: u32, axis: Axis) -> PpResult<Mask> {
    let mut output = Mask::blank(mask.width, mask.height)?;
    let lines = match axis {
        Axis::Horizontal => mask.height,
        Axis::Vertical => mask.width,
    };
    let line_len = match axis {
        Axis::Horizontal => mask.width as usize,
        Axis::Vertical => mask.height as usize,
    };
    let radius = radius as usize;
    let mut input_line = vec![0u8; line_len];
    let mut output_line = vec![0u8; line_len];
    let mut prefix = vec![0u64; line_len + 1];
    for line in 0..lines {
        read_line(mask, axis, line, &mut input_line);
        prefix[0] = 0;
        for (index, &value) in input_line.iter().enumerate() {
            prefix[index + 1] = prefix[index] + u64::from(value);
        }
        for (center, output_value) in output_line.iter_mut().enumerate().take(line_len) {
            let start = center.saturating_sub(radius);
            let end = center.saturating_add(radius).min(line_len - 1);
            let sum = prefix[end + 1] - prefix[start];
            let count = (end - start + 1) as u64;
            *output_value = ((sum + count / 2) / count) as u8;
        }
        write_line(&mut output, axis, line, &output_line);
    }
    Ok(output)
}

fn read_line(mask: &Mask, axis: Axis, line: u32, output: &mut [u8]) {
    match axis {
        Axis::Horizontal => {
            let start = line as usize * mask.width as usize;
            output.copy_from_slice(&mask.values[start..start + mask.width as usize]);
        }
        Axis::Vertical => {
            for (y, value) in output.iter_mut().enumerate() {
                *value = mask.values[y * mask.width as usize + line as usize];
            }
        }
    }
}

fn write_line(mask: &mut Mask, axis: Axis, line: u32, input: &[u8]) {
    match axis {
        Axis::Horizontal => {
            let start = line as usize * mask.width as usize;
            mask.values[start..start + mask.width as usize].copy_from_slice(input);
        }
        Axis::Vertical => {
            for (y, &value) in input.iter().enumerate() {
                mask.values[y * mask.width as usize + line as usize] = value;
            }
        }
    }
}

fn neighbors4(x: u32, y: u32, width: u32, height: u32) -> impl Iterator<Item = (u32, u32)> {
    let mut values = [(0, 0); 4];
    let mut count = 0usize;
    if x > 0 {
        values[count] = (x - 1, y);
        count += 1;
    }
    if x + 1 < width {
        values[count] = (x + 1, y);
        count += 1;
    }
    if y > 0 {
        values[count] = (x, y - 1);
        count += 1;
    }
    if y + 1 < height {
        values[count] = (x, y + 1);
        count += 1;
    }
    values.into_iter().take(count)
}

fn enqueue_zero(
    mask: &Mask,
    x: u32,
    y: u32,
    exterior: &mut [bool],
    queue: &mut VecDeque<(u32, u32)>,
) {
    let index = mask.index(x, y);
    if !exterior[index] && mask.values[index] == 0 {
        exterior[index] = true;
        queue.push_back((x, y));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn components_and_hole_fill_are_four_connected() -> PpResult<()> {
        let mask = Mask::new(
            5,
            5,
            vec![
                0, 0, 0, 0, 0, 0, 255, 255, 255, 0, 0, 255, 0, 255, 0, 0, 255, 255, 255, 0, 0, 0,
                0, 0, 255,
            ],
        )?;
        let components = mask.connected_components(1);
        assert_eq!(components.len(), 2);
        assert_eq!(components[0].pixels, 8);
        assert_eq!(mask.fill_holes().get(2, 2), 255);
        Ok(())
    }

    #[test]
    fn optimized_morphology_matches_clipped_window_semantics() -> PpResult<()> {
        let mask = Mask::new(5, 1, vec![5, 4, 3, 2, 1])?;
        assert_eq!(mask.erode(1)?.values(), &[4, 3, 2, 1, 1]);
        assert_eq!(mask.dilate(1)?.values(), &[5, 5, 4, 3, 2]);
        Ok(())
    }

    #[test]
    fn feather_uses_rounded_clipped_box_windows() -> PpResult<()> {
        let mask = Mask::new(3, 1, vec![0, 255, 0])?;
        assert_eq!(mask.feather(1)?.values(), &[128, 85, 128]);
        Ok(())
    }

    #[test]
    fn decontamination_unmixes_known_background() -> PpResult<()> {
        let foreground = Raster::new(1, 1, vec![255, 128, 128, 255])?;
        let matte = Mask::new(1, 1, vec![128])?;
        let result = decontaminate_known_background(&foreground, &matte, [255, 255, 255])?;
        assert_eq!(result.pixels()[3], 128);
        assert!(result.pixels()[1] <= 2 && result.pixels()[2] <= 2);
        Ok(())
    }
}
