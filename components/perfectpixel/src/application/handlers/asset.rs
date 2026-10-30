use std::{
    num::NonZeroU32,
    path::{Path, PathBuf},
};

use serde::Serialize;

use crate::{
    inspect_raster, resize_raster, ArtifactRef, AtomicFileWriter, DecodeLimits, FilePrecondition,
    ImageCodec, JpegQuality, PixelSpec, PpError, PpResult, ResampleFilter, ScaleFactor,
};

use super::super::{
    asset_codec::{
        encode_raster, output_format, AssetEncodeOptions, AssetOutputFormat, DEFAULT_JPEG_QUALITY,
    },
    path::{reject_same_path, validate_destination_parent, validate_raster_input_path},
    shared::{read_bytes_limited, serialize_json, MAX_RASTER_READ_BYTES},
};

const ASSET_INSPECTION_SCHEMA: &str = "perfectpixel.asset-inspection/1";
const ASSET_TRANSFORM_SCHEMA: &str = "perfectpixel.asset-transform/1";

pub(crate) fn inspect(input: PathBuf) -> PpResult<String> {
    validate_raster_input_path(&input)?;
    let bytes = read_bytes_limited(&input, MAX_RASTER_READ_BYTES)?;
    let decoded =
        ImageCodec::decode_rgba_bytes_with_metadata(&input, &bytes, DecodeLimits::default())?;
    let artifact = ArtifactRef::from_bytes(input_media_type(&input)?, &bytes)?;
    let pixel_spec = decoded.pixel_spec().clone();
    let icc_profile_byte_count = decoded
        .icc_profile()
        .map(byte_count)
        .transpose()?
        .unwrap_or(0);
    let inspection = inspect_raster(decoded.raster());
    serialize_json(
        &InspectPayload {
            schema: ASSET_INSPECTION_SCHEMA,
            schema_version: 1,
            ok: true,
            input: input.display().to_string(),
            input_sha256: artifact.sha256().as_str(),
            input_byte_count: artifact.bytes(),
            artifact: &artifact,
            pixel_spec: &pixel_spec,
            icc_profile_byte_count,
            inspection,
        },
        "<inspect>",
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn convert(
    input: PathBuf,
    output: PathBuf,
    width: Option<NonZeroU32>,
    height: Option<NonZeroU32>,
    filter: Option<ResampleFilter>,
    jpeg_quality: Option<JpegQuality>,
    background: Option<[u8; 3]>,
) -> PpResult<String> {
    let operation = AssetOperation::load(
        "convert",
        input,
        output,
        filter.unwrap_or(ResampleFilter::Lanczos3),
        jpeg_quality,
        background,
    )?;
    if width.is_none() && height.is_none() && filter.is_some() {
        return Err(PpError::InvalidOption(
            "--filter requires --width or --height for convert".to_string(),
        ));
    }
    let target = proportional_target(
        operation.source_width,
        operation.source_height,
        width.map(NonZeroU32::get),
        height.map(NonZeroU32::get),
    )?;
    operation.publish(target)
}

pub(crate) fn upscale(
    input: PathBuf,
    output: PathBuf,
    scale: ScaleFactor,
    filter: Option<ResampleFilter>,
    jpeg_quality: Option<JpegQuality>,
    background: Option<[u8; 3]>,
) -> PpResult<String> {
    let operation = AssetOperation::load(
        "upscale",
        input,
        output,
        filter.unwrap_or(ResampleFilter::Nearest),
        jpeg_quality,
        background,
    )?;
    let width = operation
        .source_width
        .checked_mul(scale.get())
        .ok_or_else(|| PpError::InvalidRequest("upscale dimensions overflow".to_string()))?;
    let height = operation
        .source_height
        .checked_mul(scale.get())
        .ok_or_else(|| PpError::InvalidRequest("upscale dimensions overflow".to_string()))?;
    validate_output_dimensions(width, height)?;
    operation.publish((width, height))
}

struct AssetOperation {
    command: &'static str,
    input: PathBuf,
    output: PathBuf,
    output_precondition: FilePrecondition,
    format: AssetOutputFormat,
    filter: ResampleFilter,
    jpeg_quality: u8,
    background: Option<[u8; 3]>,
    input_sha256: String,
    input_byte_count: u64,
    source_width: u32,
    source_height: u32,
    source: crate::Raster,
}

impl AssetOperation {
    fn load(
        command: &'static str,
        input: PathBuf,
        output: PathBuf,
        filter: ResampleFilter,
        jpeg_quality: Option<JpegQuality>,
        background: Option<[u8; 3]>,
    ) -> PpResult<Self> {
        validate_raster_input_path(&input)?;
        let format = output_format(&output)?;
        reject_same_path(&input, &output, "asset input and output must not collide")?;
        validate_destination_parent(&output)?;
        if format != AssetOutputFormat::Jpeg && jpeg_quality.is_some() {
            return Err(PpError::InvalidOption(
                "--jpeg-quality is only valid for JPEG output".to_string(),
            ));
        }
        if format != AssetOutputFormat::Jpeg && background.is_some() {
            return Err(PpError::InvalidOption(
                "--background is only valid for JPEG output".to_string(),
            ));
        }
        let output_precondition = FilePrecondition::capture(&output)?;
        let bytes = read_bytes_limited(&input, MAX_RASTER_READ_BYTES)?;
        let source = ImageCodec::decode_rgba_bytes(&input, &bytes, DecodeLimits::default())?;
        Ok(Self {
            command,
            input,
            output,
            output_precondition,
            format,
            filter,
            jpeg_quality: jpeg_quality
                .map(JpegQuality::get)
                .unwrap_or(DEFAULT_JPEG_QUALITY),
            background,
            input_sha256: crate::sha256_hex(&bytes),
            input_byte_count: byte_count(&bytes)?,
            source_width: source.width(),
            source_height: source.height(),
            source,
        })
    }

    fn publish(&self, target: (u32, u32)) -> PpResult<String> {
        validate_output_dimensions(target.0, target.1)?;
        let resized;
        let image = if target == (self.source_width, self.source_height) {
            &self.source
        } else {
            resized = resize_raster(&self.source, target.0, target.1, self.filter)?;
            &resized
        };
        let bytes = encode_raster(
            image,
            AssetEncodeOptions {
                format: self.format,
                jpeg_quality: self.jpeg_quality,
                background: self.background,
            },
        )?;
        let output_sha256 = crate::sha256_hex(&bytes);
        let output_byte_count = byte_count(&bytes)?;
        AtomicFileWriter::write_bytes_checked(&self.output, &self.output_precondition, &bytes)?;
        serialize_json(
            &AssetTransformSummary {
                schema: ASSET_TRANSFORM_SCHEMA,
                ok: true,
                command: self.command,
                input: self.input.display().to_string(),
                output: self.output.display().to_string(),
                input_sha256: &self.input_sha256,
                input_byte_count: self.input_byte_count,
                output_sha256,
                output_byte_count,
                input_width: self.source_width,
                input_height: self.source_height,
                output_width: image.width(),
                output_height: image.height(),
                format: format_name(self.format),
                filter: filter_name(self.filter),
            },
            "<asset-transform-summary>",
        )
    }
}

fn proportional_target(
    source_width: u32,
    source_height: u32,
    width: Option<u32>,
    height: Option<u32>,
) -> PpResult<(u32, u32)> {
    let result = match (width, height) {
        (None, None) => (source_width, source_height),
        (Some(width), Some(height)) => (width, height),
        (Some(width), None) => (
            width,
            proportional_dimension(source_height, source_width, width)?,
        ),
        (None, Some(height)) => (
            proportional_dimension(source_width, source_height, height)?,
            height,
        ),
    };
    validate_output_dimensions(result.0, result.1)?;
    Ok(result)
}

fn proportional_dimension(
    source_numerator: u32,
    source_denominator: u32,
    target: u32,
) -> PpResult<u32> {
    let numerator = u64::from(source_numerator)
        .checked_mul(u64::from(target))
        .ok_or_else(|| PpError::InvalidRequest("resize dimensions overflow".to_string()))?;
    let denominator = u64::from(source_denominator);
    let rounded = numerator
        .checked_add(denominator / 2)
        .ok_or_else(|| PpError::InvalidRequest("resize dimensions overflow".to_string()))?
        / denominator;
    u32::try_from(rounded.max(1))
        .map_err(|_| PpError::InvalidRequest("resize dimensions overflow".to_string()))
}

fn validate_output_dimensions(width: u32, height: u32) -> PpResult<()> {
    DecodeLimits::default().validate(width, height)
}

fn byte_count(bytes: &[u8]) -> PpResult<u64> {
    u64::try_from(bytes.len())
        .map_err(|_| PpError::InvalidRequest("asset byte count overflow".to_string()))
}

fn input_media_type(path: &Path) -> PpResult<&'static str> {
    match path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase())
        .as_deref()
    {
        Some("png") => Ok("image/png"),
        Some("jpg") | Some("jpeg") => Ok("image/jpeg"),
        Some("webp") => Ok("image/webp"),
        _ => Err(PpError::InvalidRequest(
            "asset input must use one of these extensions: png, jpg, jpeg, webp".to_string(),
        )),
    }
}

fn format_name(format: AssetOutputFormat) -> &'static str {
    match format {
        AssetOutputFormat::Png => "png",
        AssetOutputFormat::Jpeg => "jpeg",
        AssetOutputFormat::Webp => "webp-lossless",
    }
}

fn filter_name(filter: ResampleFilter) -> &'static str {
    match filter {
        ResampleFilter::Nearest => "nearest",
        ResampleFilter::Lanczos3 => "lanczos3",
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct InspectPayload<'a> {
    schema: &'static str,
    schema_version: u32,
    ok: bool,
    input: String,
    input_sha256: &'a str,
    input_byte_count: u64,
    artifact: &'a ArtifactRef,
    pixel_spec: &'a PixelSpec,
    icc_profile_byte_count: u64,
    #[serde(flatten)]
    inspection: crate::RasterInspection,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AssetTransformSummary<'a> {
    schema: &'static str,
    ok: bool,
    command: &'static str,
    input: String,
    output: String,
    input_sha256: &'a str,
    input_byte_count: u64,
    output_sha256: String,
    output_byte_count: u64,
    input_width: u32,
    input_height: u32,
    output_width: u32,
    output_height: u32,
    format: &'static str,
    filter: &'static str,
}
