use std::io::Cursor;
use std::path::Path;

use image::{DynamicImage, ExtendedColorType, ImageDecoder, ImageReader, Limits as ImageLimits};

use crate::core::{
    AlphaMode, ColorSpec, PixelFormat, PixelSpec, PpError, PpResult, Raster, Sha256Digest,
};

const MAX_ICC_PROFILE_BYTES: usize = 16 * 1024 * 1024;
const MAX_ENCODED_IMAGE_BYTES: usize = 320 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeLimits {
    pub max_dimension: u32,
    pub max_pixels: u64,
}

impl DecodeLimits {
    pub const PRODUCTION: Self = Self {
        max_dimension: 8192,
        max_pixels: 8192 * 8192,
    };

    pub fn validate(&self, width: u32, height: u32) -> PpResult<()> {
        self.validate_path(Path::new("<unknown>"), width, height)
    }

    fn validate_path(&self, path: &Path, width: u32, height: u32) -> PpResult<()> {
        let pixels = u64::from(width) * u64::from(height);
        if width > self.max_dimension || height > self.max_dimension || pixels > self.max_pixels {
            return Err(PpError::ImageTooLarge {
                path: path.to_path_buf(),
                width,
                height,
                max_dimension: self.max_dimension,
                max_pixels: self.max_pixels,
            });
        }
        Ok(())
    }
}

impl Default for DecodeLimits {
    fn default() -> Self {
        Self::PRODUCTION
    }
}

/// Decoded pixels plus the semantics that were actually observed at the codec boundary.
/// Absence of an embedded profile remains `ColorSpec::Unknown`; it is never silently promoted to
/// sRGB. `icc_profile` retains exact bytes so a color-management effect can consume the same
/// profile whose digest appears in `pixel_spec`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedRaster {
    raster: Raster,
    pixel_spec: PixelSpec,
    icc_profile: Option<Vec<u8>>,
}

impl DecodedRaster {
    pub fn raster(&self) -> &Raster {
        &self.raster
    }

    pub fn into_raster(self) -> Raster {
        self.raster
    }

    pub fn pixel_spec(&self) -> &PixelSpec {
        &self.pixel_spec
    }

    pub fn icc_profile(&self) -> Option<&[u8]> {
        self.icc_profile.as_deref()
    }
}

pub struct ImageCodec;

impl ImageCodec {
    /// Compatibility decoder for callers that only need canonical RGBA pixels. New correctness
    /// sensitive paths should use `decode_rgba_with_metadata` so color provenance is not lost.
    pub fn decode_rgba(path: impl AsRef<Path>, limits: DecodeLimits) -> PpResult<Raster> {
        Self::decode_rgba_with_metadata(path, limits).map(DecodedRaster::into_raster)
    }

    pub fn decode_rgba_with_metadata(
        path: impl AsRef<Path>,
        limits: DecodeLimits,
    ) -> PpResult<DecodedRaster> {
        let path = path.as_ref();
        // Path decoding is just a convenience over the same immutable byte-snapshot
        // decoder used by generation workflows. No image decoder is allowed to reopen
        // an ambient pathname after validation.
        let bytes =
            super::capability::read_bounded(path, MAX_ENCODED_IMAGE_BYTES).map_err(|source| {
                PpError::FileIo {
                    path: path.to_path_buf(),
                    message: source.to_string(),
                }
            })?;
        Self::decode_rgba_bytes_with_metadata(path, &bytes, limits)
    }

    /// Decodes the exact immutable byte snapshot supplied by an I/O adapter.
    /// This is used by multi-file generation workflows so computation and the publication
    /// precondition refer to the same source revision.
    pub fn decode_rgba_bytes(
        path: impl AsRef<Path>,
        bytes: &[u8],
        limits: DecodeLimits,
    ) -> PpResult<Raster> {
        Self::decode_rgba_bytes_with_metadata(path, bytes, limits).map(DecodedRaster::into_raster)
    }

    pub fn decode_rgba_bytes_with_metadata(
        path: impl AsRef<Path>,
        bytes: &[u8],
        limits: DecodeLimits,
    ) -> PpResult<DecodedRaster> {
        let path = path.as_ref();
        decode_reader(path, ImageReader::new(Cursor::new(bytes)), limits)
    }
}

fn decode_reader<R: std::io::BufRead + std::io::Seek>(
    path: &Path,
    reader: ImageReader<R>,
    limits: DecodeLimits,
) -> PpResult<DecodedRaster> {
    let mut reader = reader
        .with_guessed_format()
        .map_err(|source| PpError::ImageDecode {
            path: path.to_path_buf(),
            message: source.to_string(),
        })?;
    reader.limits(to_image_limits(limits));

    let mut decoder = reader
        .into_decoder()
        .map_err(|source| PpError::ImageDecode {
            path: path.to_path_buf(),
            message: source.to_string(),
        })?;
    // Check the encoded dimensions before asking the decoder to allocate. This keeps
    // limit failures in the product error taxonomy and preserves the logical source
    // path for both pathname and immutable byte-snapshot callers.
    let (encoded_width, encoded_height) = decoder.dimensions();
    limits.validate_path(path, encoded_width, encoded_height)?;
    let original_color_type = decoder.original_color_type();
    let icc_profile = decoder
        .icc_profile()
        .map_err(|source| PpError::ImageDecode {
            path: path.to_path_buf(),
            message: format!("ICC profile read failed: {source}"),
        })?;
    if icc_profile
        .as_ref()
        .is_some_and(|profile| profile.len() > MAX_ICC_PROFILE_BYTES)
    {
        return Err(PpError::InvalidRequest(format!(
            "ICC profile exceeds {MAX_ICC_PROFILE_BYTES}-byte limit"
        )));
    }
    let color = icc_profile
        .as_ref()
        .map(|profile| ColorSpec::Icc {
            digest: Sha256Digest::from_bytes(profile),
        })
        .unwrap_or(ColorSpec::Unknown);
    let alpha = if encoded_type_has_alpha(original_color_type) {
        AlphaMode::Straight
    } else {
        AlphaMode::Opaque
    };

    let image = DynamicImage::from_decoder(decoder)
        .map_err(|source| PpError::ImageDecode {
            path: path.to_path_buf(),
            message: source.to_string(),
        })?
        .into_rgba8();
    limits.validate_path(path, image.width(), image.height())?;
    let raster = Raster::new(image.width(), image.height(), image.into_raw())?;

    Ok(DecodedRaster {
        raster,
        pixel_spec: PixelSpec::new(PixelFormat::Rgba8, alpha, color),
        icc_profile,
    })
}

fn encoded_type_has_alpha(color: ExtendedColorType) -> bool {
    if let Some(color) = color.color_type() {
        return color.has_alpha();
    }
    matches!(
        color,
        ExtendedColorType::A8
            | ExtendedColorType::La1
            | ExtendedColorType::Rgba1
            | ExtendedColorType::La2
            | ExtendedColorType::Rgba2
            | ExtendedColorType::La4
            | ExtendedColorType::Rgba4
            | ExtendedColorType::Bgra8
    )
}

fn to_image_limits(limits: DecodeLimits) -> ImageLimits {
    let mut image_limits = ImageLimits::default();
    image_limits.max_image_width = Some(limits.max_dimension);
    image_limits.max_image_height = Some(limits.max_dimension);
    image_limits.max_alloc = Some(limits.max_pixels.saturating_mul(4));
    image_limits
}
