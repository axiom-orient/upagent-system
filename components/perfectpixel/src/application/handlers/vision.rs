use std::{
    path::{Component, Path, PathBuf},
    time::Duration,
};

use serde::{Deserialize, Serialize};

use crate::effects::{
    run_apple_vision_instances_effect, AppleVisionHelperReceipt, AppleVisionInstancesEffectRequest,
    ExternalToolReceipt,
};
use crate::runtime::PinnedExecutable;
use crate::{
    ArtifactRef, AtomicDirectoryEntry, DirectoryPrecondition, EffectCompletion, EffectIdentity,
    FilePrecondition, ImageCodec, Mask, PngEncoder, PpError, PpResult, Raster, Sha256Digest,
};

use super::super::{
    path::{reject_same_path, validate_file_extension, validate_raster_input_path},
    shared::{
        effect_failure_to_error, read_bytes_limited, read_json_request_snapshot, serialize_json,
        MAX_RASTER_READ_BYTES,
    },
};

const OPERATION: &str = "vision.apple.foreground_instances";
const RESPONSE_SCHEMA: &str = "perfectpixel.vision-foreground-instances/1";
const MANIFEST_SCHEMA: &str = "perfectpixel.vision-foreground-instances-manifest/1";
const MAX_INSTANCES: usize = 63;

pub(crate) fn foreground_instances(
    request_path: PathBuf,
    effect_timeout: Duration,
) -> PpResult<String> {
    validate_file_extension(&request_path, &["json"], "Apple Vision request")?;
    let request_guard = FilePrecondition::capture(&request_path)?;
    let (request, _snapshot): (VisionRequest, _) = read_json_request_snapshot(&request_path)?;
    request.validate()?;

    let base = request_path.parent().unwrap_or_else(|| Path::new("."));
    let input = resolve_path(base, &request.input.path, "Vision input")?;
    let output_directory =
        resolve_path(base, &request.output_directory, "Vision output directory")?;
    validate_raster_input_path(&input)?;
    reject_same_path(
        &request_path,
        &input,
        "Vision request and input must not collide",
    )?;

    let input_guard = FilePrecondition::capture(&input)?;
    let output_guard = DirectoryPrecondition::capture(&output_directory)?;
    let input_bytes = read_bytes_limited(&input, MAX_RASTER_READ_BYTES)?;
    let observed_input =
        ArtifactRef::from_bytes(request.input.artifact.media_type(), &input_bytes)?;
    if observed_input != request.input.artifact {
        return Err(PpError::PreconditionFailed {
            operation: OPERATION.to_string(),
            cause: format!(
                "input bytes no longer match declared ArtifactRef for '{}'",
                input.display()
            ),
        });
    }
    let source = ImageCodec::decode_rgba_bytes(&input, &input_bytes, Default::default())?;

    let canonical_request = serde_json::to_vec(&request).map_err(|error| PpError::Json {
        path: request_path.clone(),
        message: error.to_string(),
    })?;
    let operation_digest = Sha256Digest::from_bytes(&canonical_request);
    let identity = EffectIdentity::new(
        request.generation,
        operation_digest.clone(),
        observed_input.sha256().clone(),
    );
    let helper = PinnedExecutable::new(&request.helper.path, request.helper.sha256.clone())
        .map_err(|error| PpError::PreconditionFailed {
            operation: OPERATION.to_string(),
            cause: error.to_string(),
        })?;
    let staging_directory =
        staging_directory(&output_directory, request.generation, &operation_digest)?;
    ensure_absent(&staging_directory)?;

    let result = run_apple_vision_instances_effect(&AppleVisionInstancesEffectRequest {
        identity: identity.clone(),
        executable: helper,
        input: absolute(&input)?,
        staging_directory: absolute(&staging_directory)?,
        request_revision: request.request_revision,
        timeout: effect_timeout,
    });
    let candidates = match result.accept_current(&identity) {
        None => {
            cleanup_staging(&staging_directory)?;
            return Err(PpError::Conflict {
                operation: OPERATION.to_string(),
                cause: "stale Apple Vision Effect result rejected".to_string(),
            });
        }
        Some(EffectCompletion::Cancelled) => {
            cleanup_staging(&staging_directory)?;
            return Err(PpError::Cancelled {
                operation: OPERATION.to_string(),
                cause: "Apple Vision Effect was cancelled".to_string(),
            });
        }
        Some(EffectCompletion::Failed { error }) => {
            cleanup_staging(&staging_directory)?;
            return Err(effect_failure_to_error(error));
        }
        Some(EffectCompletion::Succeeded { value }) => value,
    };

    // The Effect result owns exact candidate bytes in memory. Remove all helper-owned filesystem
    // state before deterministic domain verification begins, so every later `?` is leak-free.
    cleanup_staging(&staging_directory)?;

    if candidates.instances.len() > MAX_INSTANCES {
        return Err(PpError::ResourceLimit {
            operation: OPERATION.to_string(),
            cause: format!(
                "Vision returned {} instances; maximum is {MAX_INSTANCES}",
                candidates.instances.len()
            ),
        });
    }

    let mut published = Vec::with_capacity(candidates.instances.len() + 1);
    let mut manifest_instances = Vec::with_capacity(candidates.instances.len());
    for candidate in &candidates.instances {
        let decoded = ImageCodec::decode_rgba_bytes(
            PathBuf::from(format!("<vision-mask-{:08}.png>", candidate.id)),
            &candidate.bytes,
            Default::default(),
        )?;
        if decoded.width() != source.width() || decoded.height() != source.height() {
            return Err(PpError::VerificationFailed {
                operation: OPERATION.to_string(),
                cause: format!(
                    "instance {} mask dimensions {}x{} do not match input {}x{}",
                    candidate.id,
                    decoded.width(),
                    decoded.height(),
                    source.width(),
                    source.height()
                ),
            });
        }

        // Vision helper intentionally emits L8 PNG. ImageCodec expands it to RGB + opaque alpha,
        // therefore candidate coverage is the red channel, never the synthetic alpha channel.
        let candidate_mask = Mask::new(
            decoded.width(),
            decoded.height(),
            decoded
                .pixels()
                .as_chunks::<4>()
                .0
                .iter()
                .map(|pixel| pixel[0])
                .collect(),
        )?;
        let cleaned = cleanup_mask(candidate_mask, &request.cleanup)?;
        let verification = verify_mask(candidate.id, &cleaned, &request.verification)?;
        let canonical_raster = mask_raster(&cleaned)?;
        let bytes = PngEncoder::encode_rgba(&canonical_raster)?;
        let artifact = ArtifactRef::from_bytes("image/png", &bytes)?;
        let file = format!("instance-{:08}.png", candidate.id);
        manifest_instances.push(VisionInstanceManifest {
            id: candidate.id,
            file: file.clone(),
            candidate_artifact: candidate.artifact.clone(),
            artifact,
            bounds: verification.bounds,
            verification: verification.clone(),
        });
        published.push(PublishedFile {
            path: PathBuf::from(file),
            bytes,
        });
    }

    let manifest = VisionManifest {
        schema: MANIFEST_SCHEMA,
        operation: OPERATION,
        generation: request.generation,
        input_artifact: observed_input.clone(),
        operation_digest: operation_digest.clone(),
        provider: candidates.helper_receipt.provider.clone(),
        adapter_version: candidates.helper_receipt.adapter_version.clone(),
        os_version: candidates.helper_receipt.os_version.clone(),
        request_type: candidates.helper_receipt.request_type.clone(),
        request_revision: candidates.helper_receipt.request_revision,
        instances: &manifest_instances,
    };
    let manifest_bytes = serde_json::to_vec_pretty(&manifest).map_err(|error| PpError::Json {
        path: PathBuf::from("<vision-manifest>"),
        message: error.to_string(),
    })?;
    let manifest_artifact = ArtifactRef::from_bytes("application/json", &manifest_bytes)?;
    published.push(PublishedFile {
        path: PathBuf::from("manifest.json"),
        bytes: manifest_bytes,
    });

    verify_file_guard(&request_path, &request_guard, "Vision request")?;
    verify_file_guard(&input, &input_guard, "Vision input")?;

    let entries = published
        .iter()
        .map(|file| AtomicDirectoryEntry {
            relative_path: file.path.as_path(),
            bytes: file.bytes.as_slice(),
            sha256: crate::sha256(file.bytes.as_slice()),
        })
        .collect::<Vec<_>>();
    crate::publish_directory_checked(&output_directory, &output_guard, &entries)?;

    serialize_json(
        &VisionResponse {
            schema: RESPONSE_SCHEMA,
            ok: true,
            operation: OPERATION,
            generation: request.generation,
            input_artifact: observed_input,
            operation_digest,
            manifest_artifact,
            instances: manifest_instances,
            helper_receipt: candidates.helper_receipt,
            effect_receipt: candidates.tool_receipt,
        },
        "<vision-foreground-instances>",
    )
}

fn cleanup_mask(mut mask: Mask, cleanup: &VisionCleanup) -> PpResult<Mask> {
    mask = mask.threshold(cleanup.threshold);
    if cleanup.close_radius > 0 {
        mask = mask.close(cleanup.close_radius)?;
    }
    if cleanup.fill_holes {
        mask = mask.fill_holes();
    }
    if cleanup.feather_radius > 0 {
        mask = mask.feather(cleanup.feather_radius)?;
    }
    Ok(mask)
}

fn verify_mask(
    id: u32,
    mask: &Mask,
    spec: &VisionVerification,
) -> PpResult<VisionMaskVerification> {
    let coverage = mask.coverage_basis_points(spec.alpha_threshold);
    let components = mask.connected_components(spec.alpha_threshold);
    let bounds = mask.bounding_box(spec.alpha_threshold);
    let passed = coverage >= spec.minimum_coverage_basis_points
        && coverage <= spec.maximum_coverage_basis_points
        && !components.is_empty()
        && components.len() <= spec.maximum_components as usize
        && bounds.w > 0
        && bounds.h > 0;
    if !passed {
        return Err(PpError::VerificationFailed {
            operation: OPERATION.to_string(),
            cause: format!(
                "instance {id} mask failed invariants: coverage={coverage}bp components={} bounds={}x{}",
                components.len(), bounds.w, bounds.h
            ),
        });
    }
    Ok(VisionMaskVerification {
        alpha_threshold: spec.alpha_threshold,
        coverage_basis_points: coverage,
        connected_components: u32::try_from(components.len()).unwrap_or(u32::MAX),
        bounds,
    })
}

fn mask_raster(mask: &Mask) -> PpResult<Raster> {
    let mut pixels = Vec::with_capacity(mask.values().len() * 4);
    for &alpha in mask.values() {
        pixels.extend_from_slice(&[255, 255, 255, alpha]);
    }
    Raster::new(mask.width(), mask.height(), pixels)
}

fn verify_file_guard(path: &Path, expected: &FilePrecondition, label: &str) -> PpResult<()> {
    if FilePrecondition::capture(path)? != *expected {
        return Err(PpError::PreconditionFailed {
            operation: OPERATION.to_string(),
            cause: format!("{label} changed while the Effect was running"),
        });
    }
    Ok(())
}

fn staging_directory(output: &Path, generation: u64, digest: &Sha256Digest) -> PpResult<PathBuf> {
    let parent = output.parent().ok_or_else(|| {
        PpError::InvalidRequest("Vision output directory has no parent".to_string())
    })?;
    let name = output
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("vision");
    Ok(parent.join(format!(
        ".{name}.pp-{generation}-{}.vision-stage",
        &digest.as_str()[..12]
    )))
}

fn ensure_absent(path: &Path) -> PpResult<()> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Ok(_) => Err(PpError::Conflict {
            operation: OPERATION.to_string(),
            cause: format!("Vision staging path '{}' already exists", path.display()),
        }),
        Err(error) => Err(PpError::FileIo {
            path: path.to_path_buf(),
            message: error.to_string(),
        }),
    }
}

fn cleanup_staging(path: &Path) -> PpResult<()> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(PpError::DependencyFailed {
            operation: OPERATION.to_string(),
            dependency: "filesystem".to_string(),
            cause: format!(
                "failed to clean Vision staging directory '{}': {error}",
                path.display()
            ),
            retryable: false,
        }),
    }
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct VisionRequest {
    schema_version: u32,
    operation: String,
    generation: u64,
    input: VisionInput,
    output_directory: String,
    helper: PinnedTool,
    request_revision: u32,
    cleanup: VisionCleanup,
    verification: VisionVerification,
}

impl VisionRequest {
    fn validate(&self) -> PpResult<()> {
        if self.schema_version != 1 || self.operation != OPERATION {
            return Err(PpError::InvalidRequest(format!(
                "Vision request schemaVersion must be 1 and operation must be '{OPERATION}'"
            )));
        }
        if self.request_revision != 1 {
            return Err(PpError::Unsupported {
                operation: OPERATION.to_string(),
                cause: format!("requestRevision {} is unsupported", self.request_revision),
            });
        }
        if self.verification.minimum_coverage_basis_points
            > self.verification.maximum_coverage_basis_points
            || self.verification.maximum_coverage_basis_points > 10_000
            || self.verification.maximum_components == 0
        {
            return Err(PpError::InvalidRequest(
                "Vision verification bounds are invalid".to_string(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct VisionInput {
    path: String,
    artifact: ArtifactRef,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PinnedTool {
    path: PathBuf,
    sha256: Sha256Digest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct VisionCleanup {
    threshold: u8,
    close_radius: u32,
    fill_holes: bool,
    feather_radius: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct VisionVerification {
    alpha_threshold: u8,
    minimum_coverage_basis_points: u16,
    maximum_coverage_basis_points: u16,
    maximum_components: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct VisionMaskVerification {
    alpha_threshold: u8,
    coverage_basis_points: u16,
    connected_components: u32,
    bounds: crate::FrameRect,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct VisionInstanceManifest {
    id: u32,
    file: String,
    candidate_artifact: ArtifactRef,
    artifact: ArtifactRef,
    bounds: crate::FrameRect,
    verification: VisionMaskVerification,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct VisionManifest<'a> {
    schema: &'static str,
    operation: &'static str,
    generation: u64,
    input_artifact: ArtifactRef,
    operation_digest: Sha256Digest,
    provider: String,
    adapter_version: String,
    os_version: String,
    request_type: String,
    request_revision: u32,
    instances: &'a [VisionInstanceManifest],
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct VisionResponse {
    schema: &'static str,
    ok: bool,
    operation: &'static str,
    generation: u64,
    input_artifact: ArtifactRef,
    operation_digest: Sha256Digest,
    manifest_artifact: ArtifactRef,
    instances: Vec<VisionInstanceManifest>,
    helper_receipt: AppleVisionHelperReceipt,
    effect_receipt: ExternalToolReceipt,
}

struct PublishedFile {
    path: PathBuf,
    bytes: Vec<u8>,
}
