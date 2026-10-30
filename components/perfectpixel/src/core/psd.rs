//! Deterministic, flattened Photoshop PSD export.
//!
//! The encoder intentionally implements the smallest useful PSD surface for an
//! image asset: an RGB document with one merged RGBA image and two path
//! resources.  It does not pretend to create Photoshop layers or preserve
//! soft alpha in a vector path; the raster alpha channel remains authoritative.

use std::collections::BTreeSet;

use super::{PpError, PpResult, Raster};

pub const PSD_EXPORT_SCHEMA: &str = "perfectpixel.photoshop-export/1";
pub const PSD_MAX_DIMENSION: u32 = 8192;
pub const PSD_MAX_KNOTS: usize = 32_768;
pub const PSD_DEFAULT_ALPHA_THRESHOLD: u8 = 128;
pub const PSD_DEFAULT_MAX_KNOTS: usize = 8192;
pub const PSD_MAX_OUTPUT_BYTES: usize = 64 * 1024 * 1024;
const PSD_MAX_BOUNDARY_EDGES_PER_KNOT: usize = 128;
const PSD_DOCUMENT_FIXED_BYTES: usize = 26 + 4 + 4 + 4 + 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PsdPathOptions {
    pub alpha_threshold: u8,
    pub max_knots: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PsdEncoded {
    bytes: Vec<u8>,
    contour_count: usize,
    knot_count: usize,
}

impl PsdEncoded {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    pub fn contour_count(&self) -> usize {
        self.contour_count
    }

    pub fn knot_count(&self) -> usize {
        self.knot_count
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Vertex {
    x: u32,
    y: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Edge {
    start: Vertex,
    end: Vertex,
}

/// Encode one RGBA raster into a bounded PSD v1 with deterministic paths.
pub fn encode_psd(image: &Raster, options: PsdPathOptions) -> PpResult<PsdEncoded> {
    validate_options(image, options)?;
    validate_raw_output_capacity(image)?;
    let max_edges = options.max_knots * PSD_MAX_BOUNDARY_EDGES_PER_KNOT;
    let contours = extract_contours(image, options.alpha_threshold, max_edges)?;
    if contours.is_empty() {
        return Err(PpError::InvalidRequest(
            "PSD path extraction found no foreground pixels".to_owned(),
        ));
    }
    let knot_count = contours.iter().map(Vec::len).sum::<usize>();
    if knot_count > options.max_knots {
        return Err(PpError::InvalidRequest(format!(
            "PSD path complexity exceeds maxKnots: {knot_count} > {}",
            options.max_knots
        )));
    }

    let path_data = encode_path_data(&contours, image.width(), image.height())?;
    let clipping_data = encode_clipping_path_name();
    let resources = encode_resources(&path_data, &clipping_data)?;
    let bytes = encode_document(image, &resources)?;
    if bytes.len() > PSD_MAX_OUTPUT_BYTES {
        return Err(PpError::InvalidRequest(format!(
            "PSD output exceeds {}-byte limit",
            PSD_MAX_OUTPUT_BYTES
        )));
    }
    validate_encoded_psd(&bytes, image, &path_data)?;
    Ok(PsdEncoded {
        bytes,
        contour_count: contours.len(),
        knot_count,
    })
}

fn validate_options(image: &Raster, options: PsdPathOptions) -> PpResult<()> {
    if image.width() > PSD_MAX_DIMENSION || image.height() > PSD_MAX_DIMENSION {
        return Err(PpError::InvalidRequest(format!(
            "PSD dimensions exceed {PSD_MAX_DIMENSION} pixels per side"
        )));
    }
    if options.alpha_threshold == 0 {
        return Err(PpError::InvalidRequest(
            "PSD alphaThreshold must be from 1 through 255".to_owned(),
        ));
    }
    if options.max_knots == 0 || options.max_knots > PSD_MAX_KNOTS {
        return Err(PpError::InvalidRequest(format!(
            "PSD maxKnots must be from 1 through {PSD_MAX_KNOTS}"
        )));
    }
    Ok(())
}

fn validate_raw_output_capacity(image: &Raster) -> PpResult<()> {
    let pixels = usize::try_from(u64::from(image.width()) * u64::from(image.height()))
        .map_err(|_| PpError::InvalidRequest("PSD pixel count is unrepresentable".to_owned()))?;
    // Header/sections and at least one empty resource block are small relative
    // to the raw planes; reserving this lower bound before contour extraction
    // prevents a large image from allocating an over-limit path workload.
    let lower_bound = pixels
        .checked_mul(4)
        .and_then(|value| value.checked_add(PSD_DOCUMENT_FIXED_BYTES))
        .ok_or_else(|| PpError::InvalidRequest("PSD output size overflow".to_owned()))?;
    if lower_bound > PSD_MAX_OUTPUT_BYTES {
        return Err(PpError::InvalidRequest(format!(
            "PSD output exceeds {}-byte limit",
            PSD_MAX_OUTPUT_BYTES
        )));
    }
    Ok(())
}

fn extract_contours(image: &Raster, threshold: u8, max_edges: usize) -> PpResult<Vec<Vec<Vertex>>> {
    let mut edges = Vec::new();
    let width = image.width();
    let height = image.height();
    for y in 0..height {
        for x in 0..width {
            if image.pixels()[(y as usize * width as usize + x as usize) * 4 + 3] < threshold {
                continue;
            }
            let top = y == 0 || !is_foreground(image, x, y - 1, threshold);
            let right = x + 1 == width || !is_foreground(image, x + 1, y, threshold);
            let bottom = y + 1 == height || !is_foreground(image, x, y + 1, threshold);
            let left = x == 0 || !is_foreground(image, x - 1, y, threshold);
            // Clockwise edges keep the foreground on the right.  At a diagonal
            // touch, the trace's deterministic right-turn rule keeps the two
            // four-connected components as separate contours.
            if top {
                edges.push(Edge {
                    start: Vertex { x, y },
                    end: Vertex { x: x + 1, y },
                });
            }
            if right {
                edges.push(Edge {
                    start: Vertex { x: x + 1, y },
                    end: Vertex { x: x + 1, y: y + 1 },
                });
            }
            if bottom {
                edges.push(Edge {
                    start: Vertex { x: x + 1, y: y + 1 },
                    end: Vertex { x, y: y + 1 },
                });
            }
            if left {
                edges.push(Edge {
                    start: Vertex { x, y: y + 1 },
                    end: Vertex { x, y },
                });
            }
            if edges.len() > max_edges {
                return Err(PpError::InvalidRequest(format!(
                    "PSD path boundary complexity exceeds maxKnots: more than {max_edges} boundary edges"
                )));
            }
        }
    }
    if edges.is_empty() {
        return Ok(Vec::new());
    }
    // Pixel scan order is already deterministic.  Sorting the starts makes
    // the contour start point independent of any future edge generation change.
    edges.sort_by_key(|edge| (edge.start.y, edge.start.x, direction(edge), edge.end));
    let mut used = vec![false; edges.len()];
    let mut contours = Vec::new();
    for index in 0..edges.len() {
        if used[index] {
            continue;
        }
        let first = edges[index];
        let mut contour = vec![first.start];
        used[index] = true;
        let mut current = first.end;
        let mut previous_direction = direction(&first);
        let mut guard = 0usize;
        while current != first.start {
            guard += 1;
            if guard > edges.len() {
                return Err(PpError::InvalidRequest(
                    "PSD path extraction exceeded bounded edge traversal".to_owned(),
                ));
            }
            contour.push(current);
            let next_index = choose_next_edge(&edges, &used, current, previous_direction)
                .ok_or_else(|| {
                    PpError::InvalidRequest(
                        "PSD path extraction produced an open or ambiguous contour".to_owned(),
                    )
                })?;
            used[next_index] = true;
            let next = edges[next_index];
            previous_direction = direction(&next);
            current = next.end;
        }
        simplify_contour(&mut contour);
        if contour.len() < 3 || signed_area_twice(&contour) == 0 {
            return Err(PpError::InvalidRequest(
                "PSD path extraction produced a degenerate contour".to_owned(),
            ));
        }
        contours.push(contour);
    }
    // Even-odd filling makes winding direction immaterial, but stable nesting
    // order is still valuable to downstream path readers and test fixtures.
    contours.sort_by(|left, right| {
        let left_key = contour_key(left);
        let right_key = contour_key(right);
        right_key.cmp(&left_key)
    });
    Ok(contours)
}

fn is_foreground(image: &Raster, x: u32, y: u32, threshold: u8) -> bool {
    image.pixels()[(y as usize * image.width() as usize + x as usize) * 4 + 3] >= threshold
}

fn direction(edge: &Edge) -> u8 {
    match (edge.end.x.cmp(&edge.start.x), edge.end.y.cmp(&edge.start.y)) {
        (std::cmp::Ordering::Greater, _) => 0,
        (_, std::cmp::Ordering::Greater) => 1,
        (std::cmp::Ordering::Less, _) => 2,
        (_, std::cmp::Ordering::Less) => 3,
        _ => unreachable!("contour edges always have nonzero length"),
    }
}

fn choose_next_edge(
    edges: &[Edge],
    used: &[bool],
    start: Vertex,
    previous_direction: u8,
) -> Option<usize> {
    edges
        .iter()
        .enumerate()
        .filter(|(index, edge)| !used[*index] && edge.start == start)
        .min_by_key(|(index, edge)| {
            let turn = (direction(edge) + 4 - previous_direction) % 4;
            // Prefer the clockwise/right turn, then straight, then left, then the
            // reverse direction.  Index is a final deterministic tie-break.
            let rank = match turn {
                1 => 0,
                0 => 1,
                3 => 2,
                _ => 3,
            };
            (rank, edge.end.y, edge.end.x, *index)
        })
        .map(|(index, _)| index)
}

fn simplify_contour(contour: &mut Vec<Vertex>) {
    contour.dedup();
    if contour.len() < 3 {
        return;
    }
    let mut changed = true;
    while changed && contour.len() >= 3 {
        changed = false;
        let count = contour.len();
        let mut keep = vec![true; count];
        for index in 0..count {
            let previous = contour[(index + count - 1) % count];
            let current = contour[index];
            let next = contour[(index + 1) % count];
            let collinear = (previous.x == current.x && current.x == next.x)
                || (previous.y == current.y && current.y == next.y);
            if collinear {
                keep[index] = false;
                changed = true;
            }
        }
        if changed {
            let reduced = contour
                .iter()
                .zip(keep)
                .filter_map(|(vertex, keep)| keep.then_some(*vertex))
                .collect::<Vec<_>>();
            if reduced.len() < 3 {
                break;
            }
            *contour = reduced;
        }
    }
}

fn signed_area_twice(contour: &[Vertex]) -> i64 {
    contour
        .iter()
        .zip(contour.iter().cycle().skip(1))
        .take(contour.len())
        .map(|(left, right)| {
            i64::from(left.x) * i64::from(right.y) - i64::from(right.x) * i64::from(left.y)
        })
        .sum()
}

fn contour_key(contour: &[Vertex]) -> (u64, u32, u32, Vec<Vertex>) {
    let min = contour
        .iter()
        .copied()
        .min_by_key(|vertex| (vertex.y, vertex.x))
        .unwrap();
    (
        signed_area_twice(contour).unsigned_abs(),
        min.y,
        min.x,
        contour.to_vec(),
    )
}

fn encode_path_data(contours: &[Vec<Vertex>], width: u32, height: u32) -> PpResult<Vec<u8>> {
    let record_count = 1usize
        .checked_add(
            contours
                .iter()
                .map(|contour| 1usize.saturating_add(contour.len()))
                .sum::<usize>(),
        )
        .ok_or_else(|| PpError::InvalidRequest("PSD path record count overflow".to_owned()))?;
    let byte_len = record_count
        .checked_mul(26)
        .ok_or_else(|| PpError::InvalidRequest("PSD path data size overflow".to_owned()))?;
    let mut bytes = Vec::with_capacity(byte_len);
    // Adobe's path fill rule record.  The rest of all records is zero-filled.
    bytes.extend_from_slice(&6u16.to_be_bytes());
    bytes.resize(bytes.len() + 24, 0);
    for contour in contours {
        let count = u16::try_from(contour.len()).map_err(|_| {
            PpError::InvalidRequest("PSD contour has too many knots for a path record".to_owned())
        })?;
        bytes.extend_from_slice(&0u16.to_be_bytes());
        bytes.extend_from_slice(&count.to_be_bytes());
        bytes.resize(bytes.len() + 22, 0);
        for vertex in contour {
            bytes.extend_from_slice(&2u16.to_be_bytes());
            let x = fixed_8_24(vertex.x, width)?;
            let y = fixed_8_24(vertex.y, height)?;
            // Photoshop stores vertical then horizontal components.
            for component in [y, x, y, x, y, x] {
                bytes.extend_from_slice(&component.to_be_bytes());
            }
        }
    }
    debug_assert_eq!(bytes.len(), byte_len);
    Ok(bytes)
}

fn fixed_8_24(value: u32, extent: u32) -> PpResult<i32> {
    let numerator = u64::from(value)
        .checked_mul(1u64 << 24)
        .ok_or_else(|| PpError::InvalidRequest("PSD path coordinate overflow".to_owned()))?;
    let normalized = (numerator + u64::from(extent / 2)) / u64::from(extent);
    i32::try_from(normalized)
        .map_err(|_| PpError::InvalidRequest("PSD path coordinate is unrepresentable".to_owned()))
}

fn encode_clipping_path_name() -> Vec<u8> {
    let name = b"Cutout Path";
    let mut data = Vec::with_capacity(18);
    data.push(name.len() as u8);
    data.extend_from_slice(name);
    // Fixed 16.16 flatness = 1.0; fill rule 1 = even/odd.
    data.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    data.extend_from_slice(&1u16.to_be_bytes());
    data
}

fn encode_resources(path_data: &[u8], clipping_data: &[u8]) -> PpResult<Vec<u8>> {
    let mut resources = Vec::new();
    encode_resource(&mut resources, 1025, "Working Path", path_data)?;
    encode_resource(&mut resources, 2000, "Cutout Path", path_data)?;
    encode_resource(&mut resources, 2999, "Cutout Path", clipping_data)?;
    Ok(resources)
}

fn encode_resource(output: &mut Vec<u8>, id: u16, name: &str, data: &[u8]) -> PpResult<()> {
    if name.len() > 255 || !name.is_ascii() {
        return Err(PpError::InvalidRequest(
            "PSD resource name is invalid".to_owned(),
        ));
    }
    output.extend_from_slice(b"8BIM");
    output.extend_from_slice(&id.to_be_bytes());
    output.push(name.len() as u8);
    output.extend_from_slice(name.as_bytes());
    if !(name.len() + 1).is_multiple_of(2) {
        output.push(0);
    }
    let data_len = u32::try_from(data.len())
        .map_err(|_| PpError::InvalidRequest("PSD resource is too large".to_owned()))?;
    output.extend_from_slice(&data_len.to_be_bytes());
    output.extend_from_slice(data);
    if !data.len().is_multiple_of(2) {
        output.push(0);
    }
    Ok(())
}

fn encode_document(image: &Raster, resources: &[u8]) -> PpResult<Vec<u8>> {
    let width = image.width();
    let height = image.height();
    let pixel_bytes = usize::try_from(u64::from(width) * u64::from(height))
        .map_err(|_| PpError::InvalidRequest("PSD pixel count is unrepresentable".to_owned()))?;
    let resources_len = u32::try_from(resources.len())
        .map_err(|_| PpError::InvalidRequest("PSD resources are too large".to_owned()))?;
    let estimated_len = PSD_DOCUMENT_FIXED_BYTES
        .checked_add(resources.len())
        .and_then(|value| value.checked_add(pixel_bytes.checked_mul(4)?))
        .ok_or_else(|| PpError::InvalidRequest("PSD output size overflow".to_owned()))?;
    if estimated_len > PSD_MAX_OUTPUT_BYTES {
        return Err(PpError::InvalidRequest(format!(
            "PSD output exceeds {}-byte limit",
            PSD_MAX_OUTPUT_BYTES
        )));
    }
    let mut bytes = Vec::with_capacity(estimated_len);
    bytes.extend_from_slice(b"8BPS");
    bytes.extend_from_slice(&1u16.to_be_bytes());
    bytes.extend_from_slice(&[0; 6]);
    bytes.extend_from_slice(&4u16.to_be_bytes());
    bytes.extend_from_slice(&height.to_be_bytes());
    bytes.extend_from_slice(&width.to_be_bytes());
    bytes.extend_from_slice(&8u16.to_be_bytes());
    bytes.extend_from_slice(&3u16.to_be_bytes());
    bytes.extend_from_slice(&0u32.to_be_bytes()); // color mode data
    bytes.extend_from_slice(&resources_len.to_be_bytes());
    bytes.extend_from_slice(resources);
    bytes.extend_from_slice(&0u32.to_be_bytes()); // no layers or masks
    bytes.extend_from_slice(&0u16.to_be_bytes()); // raw image compression
    for channel in 0..4 {
        for pixel in image.pixels().as_chunks::<4>().0 {
            bytes.push(pixel[channel]);
        }
    }
    Ok(bytes)
}

/// Validate the serialized PSD at the publication boundary.  Keeping this
/// parser next to the pure encoder catches section-length or resource-layout
/// regressions before `AtomicFileWriter` can make bytes visible.
fn validate_encoded_psd(bytes: &[u8], image: &Raster, expected_path: &[u8]) -> PpResult<()> {
    let mut offset = 0usize;
    if take(bytes, &mut offset, 4)? != b"8BPS" {
        return Err(psd_structure_error("invalid PSD signature"));
    }
    if read_u16(bytes, &mut offset)? != 1 || take(bytes, &mut offset, 6)? != [0; 6] {
        return Err(psd_structure_error("invalid PSD version or reserved bytes"));
    }
    if read_u16(bytes, &mut offset)? != 4
        || read_u32(bytes, &mut offset)? != image.height()
        || read_u32(bytes, &mut offset)? != image.width()
        || read_u16(bytes, &mut offset)? != 8
        || read_u16(bytes, &mut offset)? != 3
    {
        return Err(psd_structure_error(
            "PSD header does not match the source raster",
        ));
    }
    if read_u32(bytes, &mut offset)? != 0 {
        return Err(psd_structure_error("PSD color mode data must be empty"));
    }
    let resources_len = usize::try_from(read_u32(bytes, &mut offset)?)
        .map_err(|_| psd_structure_error("PSD resource length is unrepresentable"))?;
    let resources_end = offset
        .checked_add(resources_len)
        .ok_or_else(|| psd_structure_error("PSD resource section overflows"))?;
    let mut resource_ids = BTreeSet::new();
    let mut path_1025 = None;
    let mut path_2000 = None;
    let mut clipping_2999 = None;
    while offset < resources_end {
        if take(bytes, &mut offset, 4)? != b"8BIM" {
            return Err(psd_structure_error("PSD resource signature is invalid"));
        }
        let id = read_u16(bytes, &mut offset)?;
        if !resource_ids.insert(id) {
            return Err(psd_structure_error(
                "PSD resource identifiers must be unique",
            ));
        }
        let name_len = usize::from(take(bytes, &mut offset, 1)?[0]);
        let name = take(bytes, &mut offset, name_len)?;
        if !(name_len + 1).is_multiple_of(2) {
            take(bytes, &mut offset, 1)?;
        }
        let data_len = usize::try_from(read_u32(bytes, &mut offset)?)
            .map_err(|_| psd_structure_error("PSD resource data length is unrepresentable"))?;
        let data = take(bytes, &mut offset, data_len)?.to_vec();
        if !data_len.is_multiple_of(2) {
            take(bytes, &mut offset, 1)?;
        }
        match id {
            1025 => {
                if name != b"Working Path" {
                    return Err(psd_structure_error(
                        "PSD working-path resource name is invalid",
                    ));
                }
                path_1025 = Some(data);
            }
            2000 => {
                if name != b"Cutout Path" {
                    return Err(psd_structure_error(
                        "PSD saved-path resource name is invalid",
                    ));
                }
                path_2000 = Some(data);
            }
            2999 => {
                if name != b"Cutout Path" {
                    return Err(psd_structure_error("PSD clipping resource name is invalid"));
                }
                clipping_2999 = Some(data);
            }
            _ => {}
        }
    }
    if offset != resources_end {
        return Err(psd_structure_error(
            "PSD resource section length is invalid",
        ));
    }
    if path_1025.as_deref() != Some(expected_path) || path_2000.as_deref() != Some(expected_path) {
        return Err(psd_structure_error(
            "PSD path resources do not match the encoded path",
        ));
    }
    let clipping =
        clipping_2999.ok_or_else(|| psd_structure_error("PSD clipping path is missing"))?;
    validate_clipping_data(&clipping)?;
    validate_path_data(expected_path)?;
    if read_u32(bytes, &mut offset)? != 0 || read_u16(bytes, &mut offset)? != 0 {
        return Err(psd_structure_error(
            "PSD must have no layers and use raw image compression",
        ));
    }
    for channel in 0..4 {
        for pixel in image.pixels().as_chunks::<4>().0 {
            if take(bytes, &mut offset, 1)?[0] != pixel[channel] {
                return Err(psd_structure_error(
                    "PSD raw channel bytes do not match RGBA input",
                ));
            }
        }
    }
    if offset != bytes.len() {
        return Err(psd_structure_error("PSD contains trailing bytes"));
    }
    Ok(())
}

fn validate_path_data(data: &[u8]) -> PpResult<()> {
    if data.len() < 26 || !data.len().is_multiple_of(26) {
        return Err(psd_structure_error("PSD path data is not 26-byte aligned"));
    }
    if u16::from_be_bytes([data[0], data[1]]) != 6 || data[2..26].iter().any(|byte| *byte != 0) {
        return Err(psd_structure_error("PSD path fill-rule record is invalid"));
    }
    let mut offset = 26usize;
    let mut subpaths = 0usize;
    while offset < data.len() {
        if u16::from_be_bytes([data[offset], data[offset + 1]]) != 0
            || data[offset + 4..offset + 26].iter().any(|byte| *byte != 0)
        {
            return Err(psd_structure_error(
                "PSD closed-subpath length record is invalid",
            ));
        }
        let knot_count = usize::from(u16::from_be_bytes([data[offset + 2], data[offset + 3]]));
        if knot_count < 3 {
            return Err(psd_structure_error("PSD closed subpath has too few knots"));
        }
        offset = offset
            .checked_add(26)
            .and_then(|value| value.checked_add(knot_count.checked_mul(26)?))
            .ok_or_else(|| psd_structure_error("PSD path record count overflows"))?;
        if offset > data.len() {
            return Err(psd_structure_error(
                "PSD path record exceeds resource length",
            ));
        }
        let knot_start = offset - knot_count * 26;
        for record in data[knot_start..offset].as_chunks::<26>().0 {
            if u16::from_be_bytes([record[0], record[1]]) != 2
                || record[2..10] != record[10..18]
                || record[10..18] != record[18..26]
            {
                return Err(psd_structure_error("PSD knot record is invalid"));
            }
        }
        subpaths += 1;
    }
    if subpaths == 0 {
        return Err(psd_structure_error("PSD path has no closed subpaths"));
    }
    Ok(())
}

fn validate_clipping_data(data: &[u8]) -> PpResult<()> {
    if data.len() < 7 {
        return Err(psd_structure_error("PSD clipping path data is truncated"));
    }
    let name_len = usize::from(data[0]);
    if name_len != 11 || data.len() != name_len + 7 || &data[1..1 + name_len] != b"Cutout Path" {
        return Err(psd_structure_error("PSD clipping path name is invalid"));
    }
    let flatness_start = 1 + name_len;
    if data[flatness_start..flatness_start + 4] != 0x0001_0000u32.to_be_bytes()
        || data[flatness_start + 4..flatness_start + 6] != 1u16.to_be_bytes()
    {
        return Err(psd_structure_error(
            "PSD clipping flatness or fill rule is invalid",
        ));
    }
    Ok(())
}

fn take<'a>(bytes: &'a [u8], offset: &mut usize, length: usize) -> PpResult<&'a [u8]> {
    let end = offset
        .checked_add(length)
        .ok_or_else(|| psd_structure_error("PSD section offset overflows"))?;
    if end > bytes.len() {
        return Err(psd_structure_error("PSD section is truncated"));
    }
    let value = &bytes[*offset..end];
    *offset = end;
    Ok(value)
}

fn read_u16(bytes: &[u8], offset: &mut usize) -> PpResult<u16> {
    Ok(u16::from_be_bytes(
        take(bytes, offset, 2)?
            .try_into()
            .map_err(|_| psd_structure_error("PSD u16 field is truncated"))?,
    ))
}

fn read_u32(bytes: &[u8], offset: &mut usize) -> PpResult<u32> {
    Ok(u32::from_be_bytes(
        take(bytes, offset, 4)?
            .try_into()
            .map_err(|_| psd_structure_error("PSD u32 field is truncated"))?,
    ))
}

fn psd_structure_error(message: &str) -> PpError {
    PpError::InvalidRequest(message.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(width: u32, height: u32, alpha: &[u8]) -> Raster {
        let mut pixels = vec![0; (width * height * 4) as usize];
        for (index, value) in alpha.iter().copied().enumerate() {
            pixels[index * 4..index * 4 + 4].copy_from_slice(&[20, 40, 60, value]);
        }
        Raster::new(width, height, pixels).unwrap()
    }

    #[test]
    fn rectangle_path_is_deterministic_and_preserves_soft_alpha() {
        let source = image(2, 2, &[255, 255, 255, 128]);
        let options = PsdPathOptions {
            alpha_threshold: 128,
            max_knots: 8192,
        };
        let first = encode_psd(&source, options).unwrap();
        let second = encode_psd(&source, options).unwrap();
        assert_eq!(first.bytes(), second.bytes());
        assert_eq!(first.contour_count(), 1);
        assert_eq!(first.knot_count(), 4);
        assert!(first.bytes().contains(&128));
        let resource_bytes = u32::from_be_bytes(first.bytes()[30..34].try_into().unwrap()) as usize;
        assert_eq!(
            first.bytes().len(),
            PSD_DOCUMENT_FIXED_BYTES + resource_bytes + source.pixels().len()
        );
    }

    #[test]
    fn empty_and_invalid_threshold_fail_closed() {
        let empty = image(2, 2, &[0, 0, 0, 0]);
        assert!(encode_psd(
            &empty,
            PsdPathOptions {
                alpha_threshold: 128,
                max_knots: 8192,
            }
        )
        .is_err());
        assert!(encode_psd(
            &empty,
            PsdPathOptions {
                alpha_threshold: 0,
                max_knots: 8192,
            }
        )
        .is_err());
    }
}
