use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{
    AlphaMode, ArtifactRef, AtomicFileWriter, ColorSpec, Document, FilePrecondition, ImageCodec,
    PixelFormat, PixelSpec, PpError, PpResult, ResolvedDocumentRaster, Sha256Digest,
    PSD_EXPORT_SCHEMA,
};

use super::super::{
    path::{reject_same_path, validate_file_extension, validate_raster_input_path},
    shared::{
        decode_raster_snapshot, read_bytes_limited, read_json_request_snapshot, serialize_json,
        MAX_RASTER_READ_BYTES,
    },
};

const DOCUMENT_PSD_COMPILE_SCHEMA: &str = "perfectpixel.document-psd-compile/2";
const DOCUMENT_PSD_OPERATION: &str = "document.compile_psd";
const MAX_DOCUMENT_ARTIFACTS: usize = 4096;

/// Compatibility v1 export. This remains intentionally flattened while the canonical layered
/// compiler is expressed separately through DocumentIR.
pub(crate) fn export_flattened_psd(request_path: PathBuf) -> PpResult<String> {
    validate_file_extension(&request_path, &["json"], "PSD export request")?;
    let request_guard = FilePrecondition::capture(&request_path)?;
    let (request, _request_snapshot): (PsdExportRequest, _) =
        read_json_request_snapshot(&request_path)?;
    if request.schema_version != 1 || request.operation != "export_psd" {
        return Err(PpError::InvalidRequest(
            "PSD export request schemaVersion must be 1 and operation must be 'export_psd'"
                .to_string(),
        ));
    }

    let base_dir = request_path.parent().unwrap_or_else(|| Path::new("."));
    let input = resolve_psd_path(base_dir, &request.input, "input")?;
    let output = resolve_psd_path(base_dir, &request.output, "output")?;
    validate_raster_input_path(&input)?;
    validate_file_extension(&output, &["psd"], "PSD output")?;
    reject_same_path(
        &request_path,
        &output,
        "PSD output must not overwrite its request",
    )?;
    reject_same_path(&input, &output, "PSD input and output must not collide")?;

    let input_guard = FilePrecondition::capture(&input)?;
    let output_precondition = FilePrecondition::capture(&output)?;
    let (source, _input_snapshot) = decode_raster_snapshot(&input)?;
    let encoded = crate::encode_psd(
        &source,
        crate::PsdPathOptions {
            alpha_threshold: request.path.alpha_threshold,
            max_knots: request.path.max_knots,
        },
    )?;
    verify_guard(
        &request_path,
        &request_guard,
        "PSD request",
        "document.export_psd",
    )?;
    verify_guard(&input, &input_guard, "PSD input", "document.export_psd")?;
    let output_bytes = encoded.bytes();
    let output_artifact = ArtifactRef::from_bytes("image/vnd.adobe.photoshop", output_bytes)?;
    let output_sha256 = output_artifact.sha256().as_str().to_owned();
    let output_byte_count = output_bytes.len();
    let contour_count = encoded.contour_count();
    let knot_count = encoded.knot_count();
    AtomicFileWriter::write_bytes_checked(&output, &output_precondition, output_bytes)?;

    serialize_json(
        &PsdExportSummary {
            schema: PSD_EXPORT_SCHEMA,
            schema_version: 1,
            ok: true,
            operation: "export_psd",
            input: input.display().to_string(),
            output: output.display().to_string(),
            width: source.width(),
            height: source.height(),
            channels: 4,
            depth: 8,
            color_mode: "RGB",
            alpha_threshold: request.path.alpha_threshold,
            max_knots: request.path.max_knots,
            contour_count,
            knot_count,
            output_artifact,
            output_sha256,
            output_byte_count,
            flattened: true,
            photoshop_native_open: false,
        },
        "<psd-export-summary>",
    )
}

/// Canonical v2 DocumentIR compiler. External bytes are resolved and normalized first, core owns
/// merged appearance, the PSD adapter only serializes, and publication is the final checked Effect.
pub(crate) fn compile_layered_psd(request_path: PathBuf) -> PpResult<String> {
    validate_file_extension(&request_path, &["json"], "Document PSD compile request")?;
    let request_guard = FilePrecondition::capture(&request_path)?;
    let (request, _snapshot): (DocumentPsdCompileRequest, _) =
        read_json_request_snapshot(&request_path)?;
    if request.schema_version != 2 || request.operation != DOCUMENT_PSD_OPERATION {
        return Err(PpError::InvalidRequest(format!(
            "document PSD request schemaVersion must be 2 and operation must be '{DOCUMENT_PSD_OPERATION}'"
        )));
    }
    request.document.validate()?;
    if request.artifacts.len() > MAX_DOCUMENT_ARTIFACTS {
        return Err(PpError::InvalidRequest(format!(
            "document artifact binding count exceeds {MAX_DOCUMENT_ARTIFACTS}"
        )));
    }

    let base_dir = request_path.parent().unwrap_or_else(|| Path::new("."));
    let output = resolve_psd_path(base_dir, &request.output, "output")?;
    validate_file_extension(&output, &["psd"], "layered PSD output")?;
    reject_same_path(
        &request_path,
        &output,
        "layered PSD output must not overwrite its request",
    )?;
    let output_precondition = FilePrecondition::capture(&output)?;

    let required = collect_document_artifacts(&request.document);
    if required.len() != request.artifacts.len() {
        return Err(PpError::InvalidRequest(
            "document artifact bindings must match the unique DocumentIR artifact set exactly"
                .to_string(),
        ));
    }

    let mut resolved = Vec::with_capacity(request.artifacts.len());
    let mut input_guards = Vec::with_capacity(request.artifacts.len());
    let mut input_artifacts = Vec::with_capacity(request.artifacts.len());
    for binding in &request.artifacts {
        if !required.contains(&&binding.artifact) {
            return Err(PpError::InvalidRequest(format!(
                "artifact binding {} is not referenced by DocumentIR or does not match its metadata",
                binding.artifact.sha256().as_str()
            )));
        }
        if resolved
            .iter()
            .any(|candidate: &ResolvedDocumentRaster| candidate.artifact == binding.artifact)
        {
            return Err(PpError::InvalidRequest(format!(
                "document repeats artifact binding {}",
                binding.artifact.sha256().as_str()
            )));
        }
        let path = resolve_psd_path(base_dir, &binding.path, "artifact")?;
        validate_raster_input_path(&path)?;
        reject_same_path(
            &path,
            &output,
            "document artifact and output must not collide",
        )?;
        reject_same_path(
            &request_path,
            &path,
            "document request must not be used as a raster artifact",
        )?;
        let guard = FilePrecondition::capture(&path)?;
        let bytes = read_bytes_limited(&path, MAX_RASTER_READ_BYTES)?;
        let observed = ArtifactRef::from_bytes(binding.artifact.media_type(), &bytes)?;
        if observed != binding.artifact {
            return Err(PpError::PreconditionFailed {
                operation: DOCUMENT_PSD_OPERATION.to_string(),
                cause: format!(
                    "artifact '{}' bytes do not match declared ArtifactRef",
                    path.display()
                ),
            });
        }
        let decoded =
            ImageCodec::decode_rgba_bytes_with_metadata(&path, &bytes, Default::default())?;
        let decoded_pixel = decoded.pixel_spec().clone();
        let icc = decoded.icc_profile().map(<[u8]>::to_vec);
        let raster = decoded.into_raster();
        if decoded_pixel.alpha != binding.pixel.alpha {
            return Err(PpError::InvalidRequest(format!(
                "artifact '{}' alpha semantics do not match explicit PixelSpec",
                path.display()
            )));
        }
        if binding.pixel.pixel_format != PixelFormat::Rgba8
            || binding.pixel.alpha == AlphaMode::Premultiplied
        {
            return Err(PpError::InvalidRequest(
                "DocumentIR artifact bindings must be straight/opaque RGBA8".to_string(),
            ));
        }
        let (raster, pixel) = match (&decoded_pixel.color, icc.as_deref()) {
            (ColorSpec::Icc { digest }, Some(profile)) => {
                if binding.pixel.color != decoded_pixel.color
                    || digest != &Sha256Digest::from_bytes(profile)
                {
                    return Err(PpError::InvalidRequest(format!(
                        "artifact '{}' embedded ICC does not match explicit PixelSpec",
                        path.display()
                    )));
                }
                return Err(PpError::Unsupported {
                    operation: DOCUMENT_PSD_OPERATION.to_string(),
                    cause: format!(
                        "artifact '{}' embeds an ICC profile, but DocumentIR ICC conversion is not implemented; provide an explicitly declared unprofiled sRGB artifact",
                        path.display()
                    ),
                });
            }
            (ColorSpec::Unknown, None) if binding.pixel.color == ColorSpec::Srgb => {
                // This is an explicit caller declaration, not an implicit codec fallback.
                (raster, binding.pixel.clone())
            }
            (ColorSpec::Unknown, None) => {
                return Err(PpError::InvalidRequest(format!(
                    "artifact '{}' has no embedded color profile; binding must explicitly declare sRGB",
                    path.display()
                )));
            }
            _ => {
                return Err(PpError::InvalidRequest(format!(
                    "artifact '{}' color provenance is inconsistent",
                    path.display()
                )));
            }
        };
        input_artifacts.push(observed);
        input_guards.push((path, guard));
        resolved.push(ResolvedDocumentRaster {
            artifact: binding.artifact.clone(),
            raster,
            pixel,
        });
    }

    let canonical_request = serde_json::to_vec(&request).map_err(|source| PpError::Json {
        path: request_path.clone(),
        message: source.to_string(),
    })?;
    let operation_digest = Sha256Digest::from_bytes(&canonical_request);
    let encoded = crate::adapters::psd::encode_layered_psd(&request.document, &resolved)?;
    let structure = encoded.structure().clone();

    verify_guard(
        &request_path,
        &request_guard,
        "Document PSD request",
        DOCUMENT_PSD_OPERATION,
    )?;
    for (path, guard) in &input_guards {
        verify_guard(path, guard, "Document PSD artifact", DOCUMENT_PSD_OPERATION)?;
    }

    let output_artifact = ArtifactRef::from_bytes("image/vnd.adobe.photoshop", encoded.bytes())?;
    AtomicFileWriter::write_bytes_checked(&output, &output_precondition, encoded.bytes())?;
    serialize_json(
        &DocumentPsdCompileSummary {
            schema: DOCUMENT_PSD_COMPILE_SCHEMA,
            schema_version: 2,
            ok: true,
            operation: DOCUMENT_PSD_OPERATION,
            output: output.display().to_string(),
            input_artifacts,
            output_artifact,
            operation_digest,
            bytes_written: u64::try_from(encoded.bytes().len()).map_err(|_| {
                PpError::ResourceLimit {
                    operation: DOCUMENT_PSD_OPERATION.to_string(),
                    cause: "layered PSD byte count overflow".to_string(),
                }
            })?,
            merged_rgba_sha256: Sha256Digest::from_bytes(encoded.merged().pixels()),
            layer_records: encoded.layer_records(),
            pixel_layers: encoded.pixel_layers(),
            groups: encoded.groups(),
            masks: encoded.masks(),
            structural_readback: structure,
            photoshop_native_open: false,
        },
        "<document-psd-compile-summary>",
    )
}

fn collect_document_artifacts(document: &Document) -> Vec<&ArtifactRef> {
    fn visit<'a>(layers: &'a [crate::Layer], output: &mut Vec<&'a ArtifactRef>) {
        for layer in layers {
            match layer {
                crate::Layer::Pixel(pixel) => {
                    if !output.contains(&&pixel.artifact) {
                        output.push(&pixel.artifact);
                    }
                    if let Some(mask) = &pixel.raster_mask {
                        if !output.contains(&mask) {
                            output.push(mask);
                        }
                    }
                }
                crate::Layer::Group(group) => visit(&group.children, output),
            }
        }
    }
    let mut output = Vec::new();
    visit(&document.layers, &mut output);
    output
}

fn verify_guard(
    path: &Path,
    expected: &FilePrecondition,
    label: &str,
    operation: &str,
) -> PpResult<()> {
    let actual = FilePrecondition::capture(path)?;
    if &actual != expected {
        return Err(PpError::PreconditionFailed {
            operation: operation.to_string(),
            cause: format!(
                "{label} changed while operation was running: {}",
                path.display()
            ),
        });
    }
    Ok(())
}

fn resolve_psd_path(base_dir: &Path, value: &str, label: &str) -> PpResult<PathBuf> {
    if value.trim().is_empty()
        || value != value.trim()
        || value.contains('\0')
        || value.contains('\\')
    {
        return Err(PpError::InvalidRequest(format!(
            "PSD {label} path must be a non-empty printable path"
        )));
    }
    if value.len() > 4096 {
        return Err(PpError::InvalidRequest(format!(
            "PSD {label} path exceeds 4096 bytes"
        )));
    }
    let path = Path::new(value);
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(PpError::InvalidRequest(format!(
            "PSD {label} path must not contain '..'"
        )));
    }
    Ok(if path.is_absolute() {
        path.to_path_buf()
    } else {
        base_dir.join(path)
    })
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DocumentPsdCompileRequest {
    schema_version: u32,
    operation: String,
    document: Document,
    artifacts: Vec<DocumentArtifactBinding>,
    output: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DocumentArtifactBinding {
    artifact: ArtifactRef,
    path: String,
    pixel: PixelSpec,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DocumentPsdCompileSummary {
    schema: &'static str,
    schema_version: u32,
    ok: bool,
    operation: &'static str,
    output: String,
    input_artifacts: Vec<ArtifactRef>,
    output_artifact: ArtifactRef,
    operation_digest: Sha256Digest,
    bytes_written: u64,
    merged_rgba_sha256: Sha256Digest,
    layer_records: usize,
    pixel_layers: usize,
    groups: usize,
    masks: usize,
    structural_readback: crate::adapters::psd::PsdStructureReport,
    photoshop_native_open: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PsdExportRequest {
    schema_version: u32,
    operation: String,
    input: String,
    output: String,
    path: PsdPathRequest,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PsdPathRequest {
    alpha_threshold: u8,
    max_knots: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PsdExportSummary {
    schema: &'static str,
    schema_version: u32,
    ok: bool,
    operation: &'static str,
    input: String,
    output: String,
    width: u32,
    height: u32,
    channels: u16,
    depth: u16,
    color_mode: &'static str,
    alpha_threshold: u8,
    max_knots: usize,
    contour_count: usize,
    knot_count: usize,
    output_artifact: ArtifactRef,
    output_sha256: String,
    output_byte_count: usize,
    flattened: bool,
    photoshop_native_open: bool,
}
