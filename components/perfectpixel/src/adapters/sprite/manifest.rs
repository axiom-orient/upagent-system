use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::core::{FrameRect, Point, Size};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SpriteBundleRequest {
    pub character: String,
    #[serde(default = "default_sheet_image")]
    pub sheet_image: String,
    pub cell_width: u32,
    pub cell_height: u32,
    #[serde(default)]
    pub packing: PackingRequest,
    pub states: Vec<StateRequest>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PackingRequest {
    #[serde(default = "default_max_width")]
    pub max_width: u32,
    #[serde(default = "default_max_height")]
    pub max_height: u32,
    #[serde(default = "default_padding")]
    pub padding: u32,
    #[serde(default = "default_trim")]
    pub trim: bool,
    #[serde(default)]
    pub allow_rotation: bool,
    #[serde(default = "default_multipack")]
    pub multipack: bool,
}

impl Default for PackingRequest {
    fn default() -> Self {
        Self {
            max_width: default_max_width(),
            max_height: default_max_height(),
            padding: default_padding(),
            trim: default_trim(),
            allow_rotation: false,
            multipack: default_multipack(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StateRequest {
    pub name: String,
    pub fps: u32,
    #[serde(rename = "loop")]
    pub looped: bool,
    pub frames: Vec<String>,
}

fn default_sheet_image() -> String {
    "sprite-sheet.png".to_string()
}

fn default_max_width() -> u32 {
    2048
}

fn default_max_height() -> u32 {
    2048
}

fn default_padding() -> u32 {
    2
}

fn default_trim() -> bool {
    true
}

fn default_multipack() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateFrames {
    pub name: String,
    pub fps: u32,
    pub looped: bool,
    pub frames: Vec<crate::core::Raster>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub app: String,
    pub generator: String,
    pub schema: String,
    pub version: u32,
    pub character: String,
    pub packing: PackingInfo,
    pub sheets: Vec<SheetInfo>,
    pub animations: BTreeMap<String, AnimationEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackingInfo {
    pub algorithm: String,
    pub trim: bool,
    pub padding: u32,
    pub allow_rotation: bool,
    pub multipack: bool,
    pub max_width: u32,
    pub max_height: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SheetInfo {
    pub index: u32,
    pub image: String,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnimationEntry {
    pub order: u32,
    pub frames: u32,
    pub fps: u32,
    #[serde(rename = "loop")]
    pub looped: bool,
    pub duration_ms: u32,
    pub pivot: Point,
    pub items: Vec<FrameEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameEntry {
    pub index: u32,
    pub sheet: u32,
    pub rect: FrameRect,
    pub source_size: Size,
    pub sprite_source_size: FrameRect,
    pub rotated: bool,
    pub output: String,
}
