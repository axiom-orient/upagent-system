mod aseprite;
mod atlas;
mod manifest;
mod normalize;

pub use aseprite::*;
pub use atlas::*;
pub use manifest::*;
pub use normalize::*;

/// Schema identity written into every bundle `manifest.json` and required when reading one.
pub const SPRITE_SCHEMA: &str = "perfectpixel.sprite/3";
/// Schema identity written into every `normalize-report.json`.
pub const NORMALIZE_SCHEMA: &str = "perfectpixel.normalize/1";
