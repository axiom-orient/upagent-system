use serde::Serialize;

use super::{FrameEntry, Manifest};
use crate::core::{FrameRect, PpError, PpResult};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AsepriteJsonOutput {
    pub relative_path: String,
    pub json: String,
}

#[derive(Debug, Serialize)]
struct AsepriteSheet {
    frames: Vec<AsepriteFrame>,
    meta: AsepriteMeta,
}

#[derive(Debug, Serialize)]
struct AsepriteFrame {
    filename: String,
    frame: FrameRect,
    rotated: bool,
    trimmed: bool,
    #[serde(rename = "spriteSourceSize")]
    sprite_source_size: FrameRect,
    #[serde(rename = "sourceSize")]
    source_size: AsepriteSize,
    duration: u32,
}

#[derive(Debug, Serialize)]
struct AsepriteSize {
    w: u32,
    h: u32,
}

#[derive(Debug, Serialize)]
struct AsepriteMeta {
    app: String,
    version: String,
    image: String,
    format: String,
    size: AsepriteSize,
    scale: String,
    #[serde(rename = "frameTags")]
    frame_tags: Vec<AsepriteFrameTag>,
}

#[derive(Debug, Serialize)]
struct AsepriteFrameTag {
    name: String,
    from: u32,
    to: u32,
    direction: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    repeat: Option<String>,
}

pub fn build_aseprite_jsons(manifest: &Manifest) -> PpResult<Vec<AsepriteJsonOutput>> {
    let mut outputs = Vec::with_capacity(manifest.sheets.len());
    let mut animations = manifest.animations.iter().collect::<Vec<_>>();
    animations.sort_by_key(|(_, animation)| animation.order);
    for sheet in &manifest.sheets {
        let mut frames = Vec::new();
        let mut frame_tags = Vec::new();
        let mut index = 0u32;
        for &(name, animation) in &animations {
            let first_index = index;
            for item in animation
                .items
                .iter()
                .filter(|item| item.sheet == sheet.index)
            {
                frames.push(aseprite_frame(name, item, animation.duration_ms));
                index += 1;
            }
            if index > first_index {
                frame_tags.push(AsepriteFrameTag {
                    name: name.clone(),
                    from: first_index,
                    to: index - 1,
                    direction: "forward".to_string(),
                    repeat: (!animation.looped).then(|| "1".to_string()),
                });
            }
        }

        let document = AsepriteSheet {
            frames,
            meta: AsepriteMeta {
                app: env!("CARGO_PKG_NAME").to_string(),
                version: "1.0".to_string(),
                image: sheet.image.clone(),
                format: "RGBA8888".to_string(),
                size: AsepriteSize {
                    w: sheet.width,
                    h: sheet.height,
                },
                scale: "1".to_string(),
                frame_tags,
            },
        };
        outputs.push(AsepriteJsonOutput {
            relative_path: aseprite_json_name(&sheet.image),
            json: serde_json::to_string_pretty(&document).map_err(|source| PpError::Json {
                path: aseprite_json_name(&sheet.image).into(),
                message: source.to_string(),
            })?,
        });
    }
    Ok(outputs)
}

fn aseprite_frame(animation_name: &str, item: &FrameEntry, duration: u32) -> AsepriteFrame {
    AsepriteFrame {
        filename: format!("{} {}", animation_name, item.index),
        frame: item.rect,
        rotated: item.rotated,
        trimmed: item.sprite_source_size.x != 0
            || item.sprite_source_size.y != 0
            || item.sprite_source_size.w != item.source_size.w
            || item.sprite_source_size.h != item.source_size.h,
        sprite_source_size: item.sprite_source_size,
        source_size: AsepriteSize {
            w: item.source_size.w,
            h: item.source_size.h,
        },
        duration,
    }
}

pub(super) fn aseprite_json_name(sheet_image: &str) -> String {
    let stem = sheet_image
        .strip_suffix(".png")
        .or_else(|| sheet_image.strip_suffix(".PNG"))
        .unwrap_or(sheet_image);
    format!("{stem}.json")
}
