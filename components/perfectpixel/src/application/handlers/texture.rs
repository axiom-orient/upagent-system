use std::{
    path::{Component, Path, PathBuf},
    time::Duration,
};

use serde::{Deserialize, Serialize};

use crate::runtime::PinnedExecutable;
use crate::{
    inspect_ktx2, verify_ktx2_contract, verify_raster, AlphaMode, ArtifactRef, AtomicFileWriter,
    ColorSpec, DeltaEThresholds, EffectCompletion, EffectIdentity, ExactAssertion,
    FilePrecondition, ImageCodec, Ktx2EffectRequest, Ktx2ExtractRequest, KtxEncoding,
    PerceptualAssertion, PixelFormat, PixelSpec, PngEncoder, PpError, PpResult, Sha256Digest,
    TextureSemantic, VerificationSpec,
};

use super::super::{
    path::{reject_same_path, validate_file_extension, validate_raster_input_path},
    shared::{
        effect_failure_to_error, read_bytes_limited, read_json_request_snapshot, serialize_json,
        MAX_RASTER_READ_BYTES,
    },
};

const OPERATION: &str = "texture.compile";
const SCHEMA: &str = "perfectpixel.texture-compile/1";

pub(crate) fn compile(request_path: PathBuf, effect_timeout: Duration) -> PpResult<String> {
    validate_file_extension(&request_path, &["json"], "texture compile request")?;
    let request_guard = FilePrecondition::capture(&request_path)?;
    let (request, _snapshot): (TextureCompileRequest, _) =
        read_json_request_snapshot(&request_path)?;
    request.validate()?;

    let base = request_path.parent().unwrap_or_else(|| Path::new("."));
    let input = resolve_path(base, &request.input.path, "texture input")?;
    let output = resolve_path(base, &request.output, "texture output")?;
    validate_raster_input_path(&input)?;
    validate_file_extension(&output, &["ktx2"], "texture output")?;
    reject_same_path(
        &request_path,
        &output,
        "texture output must not overwrite its request",
    )?;
    reject_same_path(&input, &output, "texture input and output must not collide")?;

    let input_guard = FilePrecondition::capture(&input)?;
    let output_guard = FilePrecondition::capture(&output)?;
    let input_bytes = read_bytes_limited(&input, MAX_RASTER_READ_BYTES)?;
    let observed_artifact =
        ArtifactRef::from_bytes(request.input.artifact.media_type(), &input_bytes)?;
    if observed_artifact != request.input.artifact {
        return Err(PpError::PreconditionFailed {
            operation: OPERATION.to_string(),
            cause: format!(
                "input bytes no longer match declared ArtifactRef for '{}'",
                input.display()
            ),
        });
    }

    let decoded =
        ImageCodec::decode_rgba_bytes_with_metadata(&input, &input_bytes, Default::default())?;
    let decoded_pixel = decoded.pixel_spec().clone();
    let icc = decoded.icc_profile().map(<[u8]>::to_vec);
    let source = decoded.into_raster();
    let (source, source_pixel) = normalize_source(
        source,
        decoded_pixel,
        icc.as_deref(),
        &request.input.pixel,
        request.semantic,
    )?;

    let canonical_request = serde_json::to_vec(&request).map_err(|source| PpError::Json {
        path: request_path.clone(),
        message: source.to_string(),
    })?;
    let operation_digest = Sha256Digest::from_bytes(&canonical_request);
    let identity = EffectIdentity::new(
        request.generation,
        operation_digest.clone(),
        observed_artifact.sha256().clone(),
    );
    let executable =
        PinnedExecutable::new(&request.ktx.path, request.ktx.sha256.clone()).map_err(|error| {
            PpError::PreconditionFailed {
                operation: OPERATION.to_string(),
                cause: error.to_string(),
            }
        })?;

    let temp = TempSet::new(&output, request.generation, &operation_digest)?;
    let result = (|| {
        let source_png = PngEncoder::encode_rgba(&source)?;
        AtomicFileWriter::write_bytes(&temp.source_png, &source_png)?;

        let encode = crate::run_ktx2_effect(&Ktx2EffectRequest {
            identity: identity.clone(),
            executable: executable.clone(),
            input: absolute(&temp.source_png)?,
            staging_output: absolute(&temp.ktx_candidate)?,
            encoding: request.encoding.into_effect(),
            generate_mipmaps: request.generate_mipmaps,
            srgb: request.semantic.is_srgb(),
            timeout: effect_timeout,
        });
        let encoded_candidate = accept_effect(encode, &identity)?;
        let info = inspect_ktx2(&encoded_candidate.bytes)?;
        verify_ktx2_contract(
            &info,
            source.width(),
            source.height(),
            request.semantic,
            request.generate_mipmaps,
            request.encoding == TextureEncoding::BasisLz,
        )?;

        let extracted = crate::run_ktx2_extract_effect(&Ktx2ExtractRequest {
            identity: identity.clone(),
            executable: executable.clone(),
            input: absolute(&temp.ktx_candidate)?,
            staging_output: absolute(&temp.roundtrip_png)?,
            timeout: effect_timeout,
        });
        let extracted_candidate = accept_effect(extracted, &identity)?;
        let roundtrip = ImageCodec::decode_rgba_bytes(
            &temp.roundtrip_png,
            &extracted_candidate.bytes,
            Default::default(),
        )?;
        let verification = verify_roundtrip(&source, &roundtrip, &source_pixel, &request)?;
        if !verification.ok {
            return Err(PpError::VerificationFailed {
                operation: OPERATION.to_string(),
                cause: "KTX2 decode roundtrip failed the requested verification contract"
                    .to_string(),
            });
        }

        verify_guard(&request_path, &request_guard, "texture request")?;
        verify_guard(&input, &input_guard, "texture input")?;
        AtomicFileWriter::write_bytes_checked(&output, &output_guard, &encoded_candidate.bytes)?;

        serialize_json(
            &TextureCompileSummary {
                schema: SCHEMA,
                ok: true,
                operation: OPERATION,
                generation: request.generation,
                input_artifact: observed_artifact,
                output_artifact: encoded_candidate.artifact,
                operation_digest,
                bytes_written: u64::try_from(encoded_candidate.bytes.len()).map_err(|_| {
                    PpError::ResourceLimit {
                        operation: OPERATION.to_string(),
                        cause: "KTX2 output byte count overflow".to_string(),
                    }
                })?,
                semantic: request.semantic,
                encoding: request.encoding,
                ktx2: info,
                verification,
                encode_receipt: encoded_candidate.receipt,
                decode_receipt: extracted_candidate.receipt,
            },
            "<texture-compile-summary>",
        )
    })();
    temp.finish(result)
}

fn normalize_source(
    raster: crate::Raster,
    decoded: PixelSpec,
    icc: Option<&[u8]>,
    declared: &PixelSpec,
    semantic: TextureSemantic,
) -> PpResult<(crate::Raster, PixelSpec)> {
    if declared.pixel_format != PixelFormat::Rgba8 || declared.alpha == AlphaMode::Premultiplied {
        return Err(PpError::InvalidRequest(
            "texture input must explicitly declare straight/opaque RGBA8 pixels".to_string(),
        ));
    }
    if decoded.alpha != declared.alpha {
        return Err(PpError::InvalidRequest(
            "texture input alpha semantics do not match the encoded source".to_string(),
        ));
    }
    match (&decoded.color, icc) {
        (ColorSpec::Icc { digest }, Some(profile)) => {
            if !semantic.is_srgb() {
                return Err(PpError::InvalidRequest(
                    "ICC-tagged input is accepted only for color_srgb texture semantics"
                        .to_string(),
                ));
            }
            if declared.color != decoded.color || digest != &Sha256Digest::from_bytes(profile) {
                return Err(PpError::PreconditionFailed {
                    operation: OPERATION.to_string(),
                    cause: "texture input ICC provenance does not match declared PixelSpec"
                        .to_string(),
                });
            }
            Err(PpError::Unsupported {
                operation: OPERATION.to_string(),
                cause: "embedded ICC conversion is not implemented; provide an explicitly declared unprofiled sRGB source"
                    .to_string(),
            })
        }
        (ColorSpec::Unknown, None) => {
            let required = if semantic.is_srgb() {
                ColorSpec::Srgb
            } else {
                ColorSpec::LinearSrgb
            };
            if declared.color != required {
                return Err(PpError::InvalidRequest(format!(
                    "texture semantic {semantic:?} requires an explicit {required:?} declaration when no ICC profile is embedded"
                )));
            }
            Ok((raster, declared.clone()))
        }
        _ => Err(PpError::InvalidRequest(
            "texture input color provenance is inconsistent".to_string(),
        )),
    }
}

fn verify_roundtrip(
    source: &crate::Raster,
    roundtrip: &crate::Raster,
    source_pixel: &PixelSpec,
    request: &TextureCompileRequest,
) -> PpResult<crate::VerificationReport> {
    if request.semantic.is_srgb() {
        let thresholds = request.verification.delta_e2000.ok_or_else(|| {
            PpError::InvalidRequest(
                "color_srgb texture verification requires deltaE2000 thresholds".to_string(),
            )
        })?;
        return verify_raster(
            &VerificationSpec {
                exact: vec![ExactAssertion::Dimensions {
                    width: source.width(),
                    height: source.height(),
                }],
                perceptual: vec![PerceptualAssertion::DeltaE2000 { thresholds }],
                regions: Vec::new(),
            },
            roundtrip,
            source_pixel,
            None,
            Some(source),
        );
    }

    let maximum = request
        .verification
        .maximum_absolute_channel_error
        .ok_or_else(|| {
            PpError::InvalidRequest(
                "linear/normal_map/mask texture verification requires maximumAbsoluteChannelError"
                    .to_string(),
            )
        })?;
    if source.width() != roundtrip.width() || source.height() != roundtrip.height() {
        return Err(PpError::VerificationFailed {
            operation: OPERATION.to_string(),
            cause: "KTX2 roundtrip dimensions changed".to_string(),
        });
    }
    let max_error = source
        .pixels()
        .iter()
        .zip(roundtrip.pixels())
        .map(|(a, b)| a.abs_diff(*b))
        .max()
        .unwrap_or(0);
    if max_error > maximum {
        return Err(PpError::VerificationFailed {
            operation: OPERATION.to_string(),
            cause: format!("KTX2 roundtrip maximum channel error {max_error} exceeds {maximum}"),
        });
    }
    verify_raster(
        &VerificationSpec {
            exact: vec![ExactAssertion::Dimensions {
                width: source.width(),
                height: source.height(),
            }],
            perceptual: Vec::new(),
            regions: Vec::new(),
        },
        roundtrip,
        source_pixel,
        None,
        Some(source),
    )
}

fn accept_effect<T>(result: crate::EffectResult<T>, identity: &EffectIdentity) -> PpResult<T> {
    match result.accept_current(identity) {
        None => Err(PpError::Conflict {
            operation: OPERATION.to_string(),
            cause: "stale Effect result rejected".to_string(),
        }),
        Some(EffectCompletion::Cancelled) => Err(PpError::Cancelled {
            operation: OPERATION.to_string(),
            cause: "external texture Effect was cancelled".to_string(),
        }),
        Some(EffectCompletion::Failed { error }) => Err(effect_failure_to_error(error)),
        Some(EffectCompletion::Succeeded { value }) => Ok(value),
    }
}

fn verify_guard(path: &Path, expected: &FilePrecondition, label: &str) -> PpResult<()> {
    if FilePrecondition::capture(path)? != *expected {
        return Err(PpError::PreconditionFailed {
            operation: OPERATION.to_string(),
            cause: format!("{label} changed while operation was running"),
        });
    }
    Ok(())
}

fn resolve_path(base: &Path, value: &str, label: &str) -> PpResult<PathBuf> {
    if value.is_empty()
        || value != value.trim()
        || value.contains('\0')
        || value.contains('\\')
        || value.len() > 4096
    {
        return Err(PpError::InvalidRequest(format!(
            "{label} must be a bounded printable path"
        )));
    }
    let path = Path::new(value);
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(PpError::InvalidRequest(format!(
            "{label} must not contain '..'"
        )));
    }
    Ok(if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    })
}

fn absolute(path: &Path) -> PpResult<PathBuf> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    std::env::current_dir()
        .map(|root| root.join(path))
        .map_err(|error| PpError::FileIo {
            path: path.to_path_buf(),
            message: error.to_string(),
        })
}

struct TempSet {
    source_png: PathBuf,
    ktx_candidate: PathBuf,
    roundtrip_png: PathBuf,
}

impl TempSet {
    fn new(output: &Path, generation: u64, digest: &Sha256Digest) -> PpResult<Self> {
        let parent = output
            .parent()
            .ok_or_else(|| PpError::InvalidRequest("texture output has no parent".to_string()))?;
        let name = output
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("texture.ktx2");
        let prefix = &digest.as_str()[..12];
        Ok(Self {
            source_png: parent.join(format!(".{name}.pp-{generation}-{prefix}.source.png")),
            ktx_candidate: parent.join(format!(".{name}.pp-{generation}-{prefix}.candidate.ktx2")),
            roundtrip_png: parent.join(format!(".{name}.pp-{generation}-{prefix}.roundtrip.png")),
        })
    }

    fn finish<T>(self, result: PpResult<T>) -> PpResult<T> {
        let mut cleanup_error = None;
        for path in [&self.source_png, &self.ktx_candidate, &self.roundtrip_png] {
            match crate::io::capability::remove_file(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    cleanup_error = Some(format!("failed to remove '{}': {error}", path.display()))
                }
            }
        }
        match (result, cleanup_error) {
            (Ok(value), None) => Ok(value),
            (Ok(_), Some(cause)) => Err(PpError::DependencyFailed {
                operation: OPERATION.to_string(),
                dependency: "filesystem".to_string(),
                cause,
                retryable: false,
            }),
            (Err(error), None) => Err(error),
            (Err(error), Some(cleanup)) => Err(PpError::DependencyFailed {
                operation: OPERATION.to_string(),
                dependency: "filesystem".to_string(),
                cause: format!("{error}; cleanup also failed: {cleanup}"),
                retryable: false,
            }),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TextureCompileRequest {
    schema_version: u32,
    operation: String,
    generation: u64,
    input: TextureInputBinding,
    output: String,
    semantic: TextureSemantic,
    encoding: TextureEncoding,
    generate_mipmaps: bool,
    ktx: PinnedTool,
    verification: TextureVerification,
}

impl TextureCompileRequest {
    fn validate(&self) -> PpResult<()> {
        if self.schema_version != 1 || self.operation != OPERATION {
            return Err(PpError::InvalidRequest(format!(
                "texture request schemaVersion must be 1 and operation must be '{OPERATION}'"
            )));
        }
        if self.semantic == TextureSemantic::NormalMap && self.encoding != TextureEncoding::Uastc {
            return Err(PpError::InvalidRequest(
                "normal_map textures require UASTC encoding".to_string(),
            ));
        }
        if self.semantic.is_srgb() {
            if self.verification.delta_e2000.is_none()
                || self.verification.maximum_absolute_channel_error.is_some()
            {
                return Err(PpError::InvalidRequest(
                    "color_srgb verification must provide only deltaE2000 thresholds".to_string(),
                ));
            }
        } else if self.verification.maximum_absolute_channel_error.is_none()
            || self.verification.delta_e2000.is_some()
        {
            return Err(PpError::InvalidRequest(
                "non-sRGB verification must provide only maximumAbsoluteChannelError".to_string(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TextureInputBinding {
    path: String,
    artifact: ArtifactRef,
    pixel: PixelSpec,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum TextureEncoding {
    BasisLz,
    Uastc,
}

impl TextureEncoding {
    fn into_effect(self) -> KtxEncoding {
        match self {
            Self::BasisLz => KtxEncoding::BasisLz,
            Self::Uastc => KtxEncoding::Uastc,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PinnedTool {
    path: PathBuf,
    sha256: Sha256Digest,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TextureVerification {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    delta_e2000: Option<DeltaEThresholds>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    maximum_absolute_channel_error: Option<u8>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TextureCompileSummary {
    schema: &'static str,
    ok: bool,
    operation: &'static str,
    generation: u64,
    input_artifact: ArtifactRef,
    output_artifact: ArtifactRef,
    operation_digest: Sha256Digest,
    bytes_written: u64,
    semantic: TextureSemantic,
    encoding: TextureEncoding,
    ktx2: crate::Ktx2Info,
    verification: crate::VerificationReport,
    encode_receipt: crate::ExternalToolReceipt,
    decode_receipt: crate::ExternalToolReceipt,
}
