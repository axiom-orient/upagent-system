use std::collections::{BTreeMap, BTreeSet};

use binpack2d::{maxrects, BinPacker, Dimension};

use super::{
    aseprite::{aseprite_json_name, build_aseprite_jsons},
    AnimationEntry, FrameEntry, Manifest, PackingInfo, PackingRequest, SheetInfo, StateFrames,
};
use crate::core::{lossless_content_bbox, FrameRect, Point, PpError, PpResult, Raster, Size};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundlePlan {
    pub sheets: Vec<SheetOutput>,
    pub manifest: Manifest,
    pub frame_outputs: Vec<FrameOutput>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SheetOutput {
    pub relative_path: String,
    pub image: Raster,
    pub aseprite_json_path: String,
    pub aseprite_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameOutput {
    pub relative_path: String,
    pub image: Raster,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AtlasInputFrame<'a> {
    id: usize,
    state_name: String,
    frame_index: u32,
    image: &'a Raster,
    source_size: Size,
    trim: FrameRect,
    full_width: u32,
    full_height: u32,
    output_path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PackingItem {
    id: usize,
    width: u32,
    height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PlacedRect {
    item_id: usize,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    rotated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AtlasPage {
    placements: Vec<PlacedRect>,
    width: u32,
    height: u32,
}

pub fn compose_bundle(
    character: &str,
    sheet_image: &str,
    states: Vec<StateFrames>,
    cell_width: u32,
    cell_height: u32,
) -> PpResult<BundlePlan> {
    compose_bundle_with_packing(
        character,
        sheet_image,
        states,
        cell_width,
        cell_height,
        PackingRequest::default(),
    )
}

pub fn compose_bundle_with_packing(
    character: &str,
    sheet_image: &str,
    states: Vec<StateFrames>,
    cell_width: u32,
    cell_height: u32,
    packing: PackingRequest,
) -> PpResult<BundlePlan> {
    validate_bundle_input(
        character,
        sheet_image,
        &states,
        cell_width,
        cell_height,
        &packing,
    )?;

    let inputs = build_atlas_inputs(&states, cell_width, cell_height, &packing)?;
    let pages = pack_pages(&inputs, &packing)?;
    let sheet_names = sheet_image_names(sheet_image, pages.len())?;
    let mut frame_entries_by_state: BTreeMap<String, Vec<FrameEntry>> = BTreeMap::new();

    let indexed_pages = pages.iter().enumerate().collect::<Vec<_>>();
    let composed_sheets = crate::io::parallel_map(&indexed_pages, |&(page_index, page)| {
        let mut sheet = Raster::blank(page.width, page.height)?;
        for placement in &page.placements {
            let input = &inputs[placement.item_id];
            let content_x = placement.x + packing.padding;
            let content_y = placement.y + packing.padding;
            if input.trim.w == 0 || input.trim.h == 0 {
                continue;
            }
            if placement.rotated {
                sheet.copy_region_rotated_cw(input.image, input.trim, content_x, content_y)?;
            } else {
                sheet.copy_region(input.image, input.trim, content_x, content_y)?;
            }
        }
        let relative_path = sheet_names[page_index].clone();
        let sheet_info = SheetInfo {
            index: page_index as u32,
            image: relative_path.clone(),
            width: page.width,
            height: page.height,
        };
        let sheet_output = SheetOutput {
            aseprite_json_path: aseprite_json_name(&relative_path),
            relative_path,
            image: sheet,
            aseprite_json: String::new(),
        };
        Ok((sheet_info, sheet_output))
    })?;
    let (sheet_infos, mut atlas_sheets): (Vec<_>, Vec<_>) = composed_sheets.into_iter().unzip();

    for (page_index, page) in pages.iter().enumerate() {
        for placement in &page.placements {
            let input = &inputs[placement.item_id];
            let rect = FrameRect {
                x: placement.x + packing.padding,
                y: placement.y + packing.padding,
                w: placement.w.saturating_sub(packing.padding * 2),
                h: placement.h.saturating_sub(packing.padding * 2),
            };
            frame_entries_by_state
                .entry(input.state_name.clone())
                .or_default()
                .push(FrameEntry {
                    index: input.frame_index,
                    sheet: page_index as u32,
                    rect,
                    source_size: input.source_size,
                    sprite_source_size: input.trim,
                    rotated: placement.rotated,
                    output: input.output_path.clone(),
                });
        }
    }

    let mut animations = BTreeMap::new();
    for (state_order, state) in states.iter().enumerate() {
        let fps = state.fps;
        let mut items = frame_entries_by_state.remove(&state.name).ok_or_else(|| {
            PpError::InvalidRequest(format!("state '{}' has no packed frames", state.name))
        })?;
        items.sort_by_key(|item| item.index);
        let pivot = animation_pivot(&items, cell_width, cell_height);
        animations.insert(
            state.name.clone(),
            AnimationEntry {
                order: state_order as u32,
                frames: items.len() as u32,
                fps,
                looped: state.looped,
                duration_ms: 1000 / fps,
                pivot,
                items,
            },
        );
    }

    let manifest = Manifest {
        app: env!("CARGO_PKG_NAME").to_string(),
        generator: format!("{}/maxrects-atlas", env!("CARGO_PKG_NAME")),
        schema: super::SPRITE_SCHEMA.to_string(),
        version: 3,
        character: character.to_string(),
        packing: PackingInfo {
            algorithm: "binpack2d/maxrects".to_string(),
            trim: packing.trim,
            padding: packing.padding,
            allow_rotation: packing.allow_rotation,
            multipack: packing.multipack,
            max_width: packing.max_width,
            max_height: packing.max_height,
        },
        sheets: sheet_infos,
        animations,
    };

    let aseprite_jsons = build_aseprite_jsons(&manifest)?;
    for sheet in &mut atlas_sheets {
        let json = aseprite_jsons
            .iter()
            .find(|entry| entry.relative_path == sheet.aseprite_json_path)
            .ok_or_else(|| {
                PpError::InvalidRequest(format!(
                    "missing Aseprite JSON for sheet '{}'",
                    sheet.relative_path
                ))
            })?;
        sheet.aseprite_json.clone_from(&json.json);
    }

    Ok(BundlePlan {
        sheets: atlas_sheets,
        manifest,
        frame_outputs: build_frame_outputs(&inputs),
    })
}

const MAX_SHEET_DIMENSION: u32 = 8192;
const MAX_SHEET_PIXELS: u64 = 8192 * 8192;
const MAX_PADDING: u32 = 256;

fn validate_sheet_budget(width: u32, height: u32) -> PpResult<()> {
    let pixels = u64::from(width) * u64::from(height);
    if width > MAX_SHEET_DIMENSION || height > MAX_SHEET_DIMENSION || pixels > MAX_SHEET_PIXELS {
        return Err(PpError::InvalidRequest(format!(
            "sheet {}x{} exceeds max dimension {} or max pixels {}",
            width, height, MAX_SHEET_DIMENSION, MAX_SHEET_PIXELS
        )));
    }
    Ok(())
}

fn validate_bundle_input(
    character: &str,
    sheet_image: &str,
    states: &[StateFrames],
    cell_width: u32,
    cell_height: u32,
    packing: &PackingRequest,
) -> PpResult<()> {
    if character.trim().is_empty() {
        return Err(PpError::InvalidRequest("character is required".to_string()));
    }
    validate_sheet_image(sheet_image)?;
    if cell_width == 0 || cell_height == 0 {
        return Err(PpError::InvalidRequest(
            "cellWidth and cellHeight must be greater than zero".to_string(),
        ));
    }
    if packing.max_width == 0 || packing.max_height == 0 {
        return Err(PpError::InvalidRequest(
            "packing.maxWidth and packing.maxHeight must be greater than zero".to_string(),
        ));
    }
    if packing.padding > MAX_PADDING {
        return Err(PpError::InvalidRequest(format!(
            "packing.padding must be <= {MAX_PADDING}"
        )));
    }
    validate_sheet_budget(packing.max_width, packing.max_height)?;
    if states.is_empty() {
        return Err(PpError::InvalidRequest(
            "at least one animation state is required".to_string(),
        ));
    }
    let mut seen_states = BTreeSet::new();
    for state in states {
        validate_state_name(&state.name)?;
        if !seen_states.insert(state.name.as_str()) {
            return Err(PpError::InvalidRequest(format!(
                "duplicate state name '{}' would overwrite animation output",
                state.name
            )));
        }
        if state.frames.is_empty() {
            return Err(PpError::InvalidRequest(format!(
                "state '{}' must contain at least one frame",
                state.name
            )));
        }
        if !(1..=1000).contains(&state.fps) {
            return Err(PpError::InvalidRequest(format!(
                "state '{}' fps must be from 1 through 1000",
                state.name
            )));
        }
    }
    Ok(())
}

fn build_atlas_inputs<'a>(
    states: &'a [StateFrames],
    cell_width: u32,
    cell_height: u32,
    packing: &PackingRequest,
) -> PpResult<Vec<AtlasInputFrame<'a>>> {
    let flat_frames = states
        .iter()
        .flat_map(|state| {
            state
                .frames
                .iter()
                .enumerate()
                .map(move |(frame_index, image)| (state, frame_index, image))
        })
        .collect::<Vec<_>>();

    let inputs = crate::io::parallel_map(&flat_frames, |&(state, frame_index, image)| {
        if image.width() > cell_width || image.height() > cell_height {
            return Err(PpError::InvalidRequest(format!(
                "frame {} for state '{}' is {}x{}, larger than cell {}x{}",
                frame_index,
                state.name,
                image.width(),
                image.height(),
                cell_width,
                cell_height
            )));
        }
        let trim = if packing.trim {
            lossless_content_bbox(image)
        } else {
            FrameRect {
                x: 0,
                y: 0,
                w: image.width(),
                h: image.height(),
            }
        };
        let packed_width = trim.w.max(1);
        let packed_height = trim.h.max(1);
        let full_width = padded_dimension(packed_width, packing.padding)?;
        let full_height = padded_dimension(packed_height, packing.padding)?;
        if !fits_page(
            full_width,
            full_height,
            packing.max_width,
            packing.max_height,
            packing.allow_rotation,
        ) {
            return Err(PpError::InvalidRequest(format!(
                "frame {} for state '{}' needs packed size {}x{} plus padding, exceeding atlas max {}x{}",
                frame_index, state.name, packed_width, packed_height, packing.max_width, packing.max_height
            )));
        }
        Ok(AtlasInputFrame {
            id: 0,
            state_name: state.name.clone(),
            frame_index: frame_index as u32,
            image,
            source_size: Size {
                w: cell_width,
                h: cell_height,
            },
            trim,
            full_width,
            full_height,
            output_path: format!("frames/{}/frame-{frame_index:02}.png", state.name),
        })
    })?;
    Ok(inputs
        .into_iter()
        .enumerate()
        .map(|(id, mut input)| {
            input.id = id;
            input
        })
        .collect())
}

fn padded_dimension(value: u32, padding: u32) -> PpResult<u32> {
    value
        .checked_add(
            padding
                .checked_mul(2)
                .ok_or_else(|| PpError::InvalidRequest("packing padding overflow".to_string()))?,
        )
        .ok_or_else(|| PpError::InvalidRequest("packed frame dimension overflow".to_string()))
}

fn fits_page(
    width: u32,
    height: u32,
    max_width: u32,
    max_height: u32,
    allow_rotation: bool,
) -> bool {
    (width <= max_width && height <= max_height)
        || (allow_rotation && height <= max_width && width <= max_height)
}

fn pack_pages(
    inputs: &[AtlasInputFrame<'_>],
    packing: &PackingRequest,
) -> PpResult<Vec<AtlasPage>> {
    let items = build_packing_items(inputs);
    let mut remaining = items.iter().map(|item| item.id).collect::<Vec<_>>();
    let mut pages = Vec::new();

    while !remaining.is_empty() {
        let (page, not_packed) = pack_single_page(&items, &remaining, packing)?;
        if page.placements.is_empty() {
            return Err(PpError::InvalidRequest(
                "binpack2d produced no atlas page".to_string(),
            ));
        }
        pages.push(page);

        if not_packed.is_empty() {
            break;
        }
        if !packing.multipack {
            return Err(PpError::InvalidRequest(format!(
                "{} frame(s) do not fit one atlas page and packing.multipack is false",
                not_packed.len()
            )));
        }
        remaining = not_packed;
    }

    Ok(pages)
}

fn pack_single_page(
    items: &[PackingItem],
    item_ids: &[usize],
    packing: &PackingRequest,
) -> PpResult<(AtlasPage, Vec<usize>)> {
    let mut ordered = item_ids.to_vec();
    ordered.sort_by(|left, right| item_sort_key(items[*right]).cmp(&item_sort_key(items[*left])));

    let (placements, not_packed) = if packing.allow_rotation {
        let without_rotation = pack_page_without_rotation(items, &ordered, packing)?;
        let with_rotation = pack_page_with_rotation(items, &ordered, packing)?;
        choose_page_pack(without_rotation, with_rotation)
    } else {
        pack_page_without_rotation(items, &ordered, packing)?
    };

    let width = placements
        .iter()
        .map(|placement| placement.x + placement.w)
        .max()
        .unwrap_or(1)
        .max(1);
    let height = placements
        .iter()
        .map(|placement| placement.y + placement.h)
        .max()
        .unwrap_or(1)
        .max(1);
    validate_page_placements(&placements, packing.max_width, packing.max_height)?;
    validate_sheet_budget(width, height)?;

    Ok((
        AtlasPage {
            placements: sort_placements(placements),
            width,
            height,
        },
        not_packed,
    ))
}

fn pack_page_without_rotation(
    items: &[PackingItem],
    item_ids: &[usize],
    packing: &PackingRequest,
) -> PpResult<(Vec<PlacedRect>, Vec<usize>)> {
    let dimensions = item_ids
        .iter()
        .map(|item_id| dimension_for(items[*item_id], false))
        .collect::<PpResult<Vec<_>>>()?;
    let mut bin = maxrects::MaxRectsBin::new(
        to_i32(packing.max_width, "packing.maxWidth")?,
        to_i32(packing.max_height, "packing.maxHeight")?,
    );
    let (inserted, rejected) = bin.insert_list(&dimensions, maxrects::Heuristic::BestShortSideFit);
    let placements = inserted
        .into_iter()
        .map(|rect| placed_rect_from_binpack_rect(rect, false))
        .collect::<PpResult<Vec<_>>>()?;
    let not_packed = rejected
        .into_iter()
        .map(|dim| usize_from_dimension_id(dim.id()))
        .collect::<PpResult<Vec<_>>>()?;

    Ok((placements, not_packed))
}

fn pack_page_with_rotation(
    items: &[PackingItem],
    item_ids: &[usize],
    packing: &PackingRequest,
) -> PpResult<(Vec<PlacedRect>, Vec<usize>)> {
    let mut bin = maxrects::MaxRectsBin::new(
        to_i32(packing.max_width, "packing.maxWidth")?,
        to_i32(packing.max_height, "packing.maxHeight")?,
    );
    let mut placements = Vec::new();
    let mut not_packed = Vec::new();

    for item_id in item_ids {
        let item = items[*item_id];
        let normal = placement_candidate(&bin, item, false)?;
        let rotated = if item.width == item.height {
            None
        } else {
            placement_candidate(&bin, item, true)?
        };
        let Some(candidate) = choose_candidate(normal, rotated) else {
            not_packed.push(*item_id);
            continue;
        };
        bin = candidate.bin;
        placements.push(candidate.placement);
    }

    Ok((placements, not_packed))
}

fn choose_page_pack(
    without_rotation: (Vec<PlacedRect>, Vec<usize>),
    with_rotation: (Vec<PlacedRect>, Vec<usize>),
) -> (Vec<PlacedRect>, Vec<usize>) {
    let without_score = page_pack_score(&without_rotation);
    let with_score = page_pack_score(&with_rotation);
    if with_score < without_score {
        with_rotation
    } else {
        without_rotation
    }
}

fn page_pack_score(candidate: &(Vec<PlacedRect>, Vec<usize>)) -> (usize, u64, u32, u32, usize) {
    let (placements, not_packed) = candidate;
    let used_width = placements
        .iter()
        .map(|placement| placement.x + placement.w)
        .max()
        .unwrap_or(1);
    let used_height = placements
        .iter()
        .map(|placement| placement.y + placement.h)
        .max()
        .unwrap_or(1);
    let rotated_count = placements
        .iter()
        .filter(|placement| placement.rotated)
        .count();
    (
        not_packed.len(),
        u64::from(used_width) * u64::from(used_height),
        used_height,
        used_width,
        rotated_count,
    )
}

#[derive(Debug, Clone)]
struct PlacementCandidate {
    bin: maxrects::MaxRectsBin,
    placement: PlacedRect,
    score: (u64, u32, u32, bool),
}

fn placement_candidate(
    bin: &maxrects::MaxRectsBin,
    item: PackingItem,
    rotated: bool,
) -> PpResult<Option<PlacementCandidate>> {
    let dimension = dimension_for(item, rotated)?;
    let mut candidate_bin = bin.clone();
    let Some(rect) = candidate_bin.insert(&dimension, maxrects::Heuristic::BestShortSideFit) else {
        return Ok(None);
    };
    let placement = placed_rect_from_binpack_rect(rect, rotated)?;
    let (used_width, used_height) = used_bounds(&candidate_bin)?;

    Ok(Some(PlacementCandidate {
        bin: candidate_bin,
        placement,
        score: (
            u64::from(used_width) * u64::from(used_height),
            used_height,
            used_width,
            rotated,
        ),
    }))
}

fn choose_candidate(
    normal: Option<PlacementCandidate>,
    rotated: Option<PlacementCandidate>,
) -> Option<PlacementCandidate> {
    match (normal, rotated) {
        (Some(normal), Some(rotated)) => {
            if rotated.score < normal.score {
                Some(rotated)
            } else {
                Some(normal)
            }
        }
        (Some(normal), None) => Some(normal),
        (None, Some(rotated)) => Some(rotated),
        (None, None) => None,
    }
}

fn build_packing_items(inputs: &[AtlasInputFrame<'_>]) -> Vec<PackingItem> {
    inputs
        .iter()
        .map(|input| PackingItem {
            id: input.id,
            width: input.full_width,
            height: input.full_height,
        })
        .collect()
}

fn dimension_for(item: PackingItem, rotated: bool) -> PpResult<Dimension> {
    let (width, height) = if rotated {
        (item.height, item.width)
    } else {
        (item.width, item.height)
    };
    Ok(Dimension::with_id(
        to_isize(item.id, "frame id")?,
        to_i32(width, "packed frame width")?,
        to_i32(height, "packed frame height")?,
        0,
    ))
}

fn placed_rect_from_binpack_rect(
    rect: binpack2d::Rectangle,
    rotated: bool,
) -> PpResult<PlacedRect> {
    Ok(PlacedRect {
        item_id: usize_from_dimension_id(rect.id())?,
        x: to_u32(rect.x(), "packed x")?,
        y: to_u32(rect.y(), "packed y")?,
        w: to_u32(rect.width(), "packed width")?,
        h: to_u32(rect.height(), "packed height")?,
        rotated,
    })
}

fn used_bounds(bin: &maxrects::MaxRectsBin) -> PpResult<(u32, u32)> {
    let mut width = 1u32;
    let mut height = 1u32;
    for rect in bin.iter() {
        let right = to_u32(
            rect.x().checked_add(rect.width()).ok_or_else(|| {
                PpError::InvalidRequest("packed frame right overflow".to_string())
            })?,
            "packed right",
        )?;
        let bottom = to_u32(
            rect.y().checked_add(rect.height()).ok_or_else(|| {
                PpError::InvalidRequest("packed frame bottom overflow".to_string())
            })?,
            "packed bottom",
        )?;
        width = width.max(right);
        height = height.max(bottom);
    }
    Ok((width, height))
}

fn sort_placements(mut placements: Vec<PlacedRect>) -> Vec<PlacedRect> {
    placements.sort_by_key(|placement| placement.item_id);
    placements
}

fn item_sort_key(item: PackingItem) -> (u32, u32, u32, usize) {
    let area = item.width.saturating_mul(item.height);
    let max_side = item.width.max(item.height);
    let min_side = item.width.min(item.height);
    (area, max_side, min_side, item.id)
}

fn validate_page_placements(
    placements: &[PlacedRect],
    max_width: u32,
    max_height: u32,
) -> PpResult<()> {
    for placement in placements {
        let right = placement.x.checked_add(placement.w).ok_or_else(|| {
            PpError::InvalidRequest(format!(
                "binpack2d returned overflowing x bounds for frame {}",
                placement.item_id
            ))
        })?;
        let bottom = placement.y.checked_add(placement.h).ok_or_else(|| {
            PpError::InvalidRequest(format!(
                "binpack2d returned overflowing y bounds for frame {}",
                placement.item_id
            ))
        })?;
        if right > max_width || bottom > max_height {
            return Err(PpError::InvalidRequest(format!(
                "binpack2d placed frame {} outside page bounds: rect {}x{} at {},{}, max {}x{}",
                placement.item_id,
                placement.w,
                placement.h,
                placement.x,
                placement.y,
                max_width,
                max_height
            )));
        }
    }

    for (left_index, left) in placements.iter().enumerate() {
        for right in placements.iter().skip(left_index + 1) {
            if placements_overlap(*left, *right) {
                return Err(PpError::InvalidRequest(format!(
                    "binpack2d returned overlapping placements: frame {} overlaps frame {}",
                    left.item_id, right.item_id
                )));
            }
        }
    }
    Ok(())
}

fn placements_overlap(left: PlacedRect, right: PlacedRect) -> bool {
    left.x < right.x + right.w
        && left.x + left.w > right.x
        && left.y < right.y + right.h
        && left.y + left.h > right.y
}

fn to_i32(value: u32, label: &str) -> PpResult<i32> {
    i32::try_from(value).map_err(|_| {
        PpError::InvalidRequest(format!("{label} exceeds binpack2d i32 range: {value}"))
    })
}

fn to_isize(value: usize, label: &str) -> PpResult<isize> {
    isize::try_from(value).map_err(|_| {
        PpError::InvalidRequest(format!("{label} exceeds binpack2d isize range: {value}"))
    })
}

fn to_u32(value: i32, label: &str) -> PpResult<u32> {
    u32::try_from(value).map_err(|_| {
        PpError::InvalidRequest(format!("binpack2d returned negative {label}: {value}"))
    })
}

fn usize_from_dimension_id(value: isize) -> PpResult<usize> {
    usize::try_from(value).map_err(|_| {
        PpError::InvalidRequest(format!("binpack2d returned invalid frame id {value}"))
    })
}

fn animation_pivot(items: &[FrameEntry], cell_width: u32, cell_height: u32) -> Point {
    let ground_y = items
        .iter()
        .map(|item| item.sprite_source_size.y + item.sprite_source_size.h)
        .max()
        .unwrap_or(cell_height);
    Point {
        x: cell_width / 2,
        y: ground_y,
    }
}

fn build_frame_outputs(inputs: &[AtlasInputFrame<'_>]) -> Vec<FrameOutput> {
    inputs
        .iter()
        .map(|input| FrameOutput {
            relative_path: input.output_path.clone(),
            image: (*input.image).clone(),
        })
        .collect()
}

fn sheet_image_names(sheet_image: &str, page_count: usize) -> PpResult<Vec<String>> {
    if page_count == 1 {
        return Ok(vec![sheet_image.to_string()]);
    }
    let (stem, extension) = split_png_name(sheet_image)?;
    Ok((0..page_count)
        .map(|index| format!("{stem}-{index:02}.{extension}"))
        .collect())
}

fn split_png_name(sheet_image: &str) -> PpResult<(&str, &str)> {
    let Some(index) = sheet_image.rfind('.') else {
        return Err(PpError::InvalidRequest(
            "sheetImage must include a .png extension".to_string(),
        ));
    };
    Ok((&sheet_image[..index], &sheet_image[index + 1..]))
}

fn validate_sheet_image(sheet_image: &str) -> PpResult<()> {
    if !is_safe_file_name(sheet_image) {
        return Err(PpError::InvalidRequest(
            "sheetImage must be a file name, not a path".to_string(),
        ));
    }
    if !sheet_image.to_ascii_lowercase().ends_with(".png") {
        return Err(PpError::InvalidRequest(
            "sheetImage must be a .png file name".to_string(),
        ));
    }
    if reserved_bundle_output_name(sheet_image) || derived_bundle_output_conflicts(sheet_image) {
        return Err(PpError::InvalidRequest(format!(
            "sheetImage '{}' conflicts with reserved bundle output",
            sheet_image
        )));
    }
    Ok(())
}

fn derived_bundle_output_conflicts(sheet_image: &str) -> bool {
    let stem = sheet_image
        .strip_suffix(".png")
        .or_else(|| sheet_image.strip_suffix(".PNG"))
        .unwrap_or(sheet_image)
        .to_ascii_lowercase();
    let multipack_stem = stem.rsplit_once('-').and_then(|(base, page)| {
        (page.len() >= 2 && page.bytes().all(|byte| byte.is_ascii_digit())).then_some(base)
    });
    stem == "manifest" || stem == "frames" || matches!(multipack_stem, Some("manifest" | "frames"))
}

fn reserved_bundle_output_name(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "manifest.json" | "frames" | "sprite-sheet.json"
    )
}

fn validate_state_name(state_name: &str) -> PpResult<()> {
    if state_name.trim().is_empty() {
        return Err(PpError::InvalidRequest(
            "state name is required".to_string(),
        ));
    }
    if state_name != state_name.trim() {
        return Err(PpError::InvalidRequest(format!(
            "state name '{}' is not safe for output paths",
            state_name
        )));
    }
    if state_name.contains('/')
        || state_name.contains('\\')
        || state_name.contains("..")
        || state_name.contains('\0')
    {
        return Err(PpError::InvalidRequest(format!(
            "state name '{}' is not safe for output paths",
            state_name
        )));
    }
    Ok(())
}

fn is_safe_file_name(value: &str) -> bool {
    !value.is_empty()
        && value == value.trim()
        && value != "."
        && value != ".."
        && !value.contains('/')
        && !value.contains('\\')
        && !value.contains('\0')
}
