use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
};

use agent_core::{
    ArtifactEvidence, ArtifactId, ArtifactRef, MAX_REVIEW_SUBJECT_BUNDLE_BYTES, ReviewSubjectBundle,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{ReviewError, digest_json, sha256};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SubjectBundle {
    pub media_type: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declared_sha256: Option<String>,
}

impl SubjectBundle {
    pub fn validate(&self) -> Result<String, ReviewError> {
        if self.media_type.trim().is_empty() || self.media_type.len() > 128 {
            return Err(ReviewError::Contract(
                "subject mediaType must be 1..=128 bytes".to_owned(),
            ));
        }
        if self.content.is_empty() || self.content.len() > MAX_REVIEW_SUBJECT_BUNDLE_BYTES {
            return Err(ReviewError::Contract(format!(
                "subject content must be 1..={MAX_REVIEW_SUBJECT_BUNDLE_BYTES} bytes"
            )));
        }
        let digest = sha256(self.content.as_bytes());
        if let Some(declared) = &self.declared_sha256
            && declared != &digest
        {
            return Err(ReviewError::Contract(
                "subject declaredSha256 does not match content".to_owned(),
            ));
        }
        Ok(digest)
    }
}

/// Review input is explicit. Inline subjects are self-contained; artifact
/// bundles retain the exact producer task/composition binding and an explicit
/// allow-list of artifact IDs to expose to reviewers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", deny_unknown_fields, rename_all = "camelCase")]
pub enum ReviewSubjectInput {
    #[serde(rename = "inline")]
    Inline { subject: SubjectBundle },
    #[serde(rename = "artifactBundle")]
    ArtifactBundle {
        #[serde(rename = "taskDigest")]
        task_digest: String,
        #[serde(rename = "compositionDigest")]
        composition_digest: String,
        /// Full content-addressed reference to the producer's bundle file.
        bundle: ArtifactRef,
        #[serde(rename = "includeArtifactIds")]
        include_artifact_ids: Vec<ArtifactId>,
    },
}

impl ReviewSubjectInput {
    pub fn validate(&self) -> Result<String, ReviewError> {
        self.identity_digest()
    }

    fn validate_shape(&self) -> Result<(), ReviewError> {
        match self {
            Self::Inline { subject } => {
                subject.validate()?;
            }
            Self::ArtifactBundle {
                task_digest,
                composition_digest,
                bundle,
                include_artifact_ids,
            } => {
                validate_digest_text(task_digest, "taskDigest")?;
                validate_digest_text(composition_digest, "compositionDigest")?;
                validate_artifact_reference_shape(bundle)?;
                if include_artifact_ids.is_empty() || include_artifact_ids.len() > 2_048 {
                    return Err(ReviewError::Contract(
                        "includeArtifactIds must contain 1..=2048 explicit IDs".to_owned(),
                    ));
                }
                let mut ids = BTreeSet::new();
                for id in include_artifact_ids {
                    if !ids.insert(id) {
                        return Err(ReviewError::Contract(
                            "includeArtifactIds contains a duplicate".to_owned(),
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    pub(super) fn identity_digest(&self) -> Result<String, ReviewError> {
        self.validate_shape()?;
        match self {
            Self::Inline { subject } => subject.validate(),
            Self::ArtifactBundle { .. } => digest_json(self),
        }
    }
}

fn validate_digest_text(value: &str, label: &str) -> Result<(), ReviewError> {
    if value.len() != 64
        || value != value.to_ascii_lowercase()
        || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(ReviewError::Contract(format!(
            "{label} must be a lowercase SHA-256 digest"
        )));
    }
    Ok(())
}

fn validate_artifact_reference_shape(reference: &ArtifactRef) -> Result<(), ReviewError> {
    if reference.sha256.len() != 64
        || reference.sha256 != reference.sha256.to_ascii_lowercase()
        || !reference
            .sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        || reference.media_type.trim().is_empty()
        || reference.media_type.len() > 256
        || reference.byte_length == 0
        || reference.byte_length > MAX_REVIEW_ARTIFACT_BYTES
    {
        return Err(ReviewError::Contract(
            "review artifact reference identity is invalid".to_owned(),
        ));
    }
    Ok(())
}

/// Verify and ingest the immutable review subject metadata emitted by the
/// general agent. This parser intentionally depends only on `agent-core`; it
/// does not open runtime state, create a runtime lane, or execute a campaign.
pub fn verify_review_subject_bundle_bytes(
    bytes: &[u8],
    expected_task_digest: &str,
    expected_composition_digest: &str,
) -> Result<ReviewSubjectBundle, ReviewError> {
    if bytes.is_empty() || bytes.len() > MAX_REVIEW_SUBJECT_BUNDLE_BYTES {
        return Err(ReviewError::Contract(
            "review subject bundle is outside its byte bound".to_owned(),
        ));
    }
    let bundle: ReviewSubjectBundle = serde_json::from_slice(bytes).map_err(|error| {
        ReviewError::Json(format!("review subject bundle JSON failed: {error}"))
    })?;
    bundle
        .validate()
        .map_err(|error| ReviewError::Contract(error.to_string()))?;
    if bundle.task_digest != expected_task_digest
        || bundle.composition_digest != expected_composition_digest
    {
        return Err(ReviewError::Contract(
            "review subject bundle does not match the expected task/composition digest".to_owned(),
        ));
    }
    Ok(bundle)
}

const MAX_REVIEW_ARTIFACT_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct MaterializedArtifact {
    artifact: ArtifactRef,
    uses: Vec<agent_core::ArtifactUse>,
    /// Original artifact bytes, decoded as UTF-8 without truncation.
    content: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct MaterializedSubjectEnvelope {
    schema_version: u32,
    task_digest: String,
    composition_digest: String,
    bundle: ArtifactRef,
    artifacts: Vec<MaterializedArtifact>,
}

/// Resolve the explicit review input before provider setup or campaign state
/// is opened. Bundle mode retains the exact bundle reference, selection, uses,
/// and original UTF-8 bytes in the reviewer-visible untrusted envelope.
pub fn materialize_review_subject(
    input: &ReviewSubjectInput,
    artifact_root: Option<&Path>,
) -> Result<SubjectBundle, ReviewError> {
    input.validate_shape()?;
    match input {
        ReviewSubjectInput::Inline { subject } => {
            if artifact_root.is_some() {
                return Err(ReviewError::Contract(
                    "artifact-root is only valid for an artifactBundle review subject".to_owned(),
                ));
            }
            subject.validate()?;
            Ok(subject.clone())
        }
        ReviewSubjectInput::ArtifactBundle {
            task_digest,
            composition_digest,
            bundle: bundle_ref,
            include_artifact_ids,
        } => {
            let root = artifact_root.ok_or_else(|| {
                ReviewError::Contract(
                    "artifact-root is required for an artifactBundle review subject".to_owned(),
                )
            })?;
            let root = existing_artifact_root(root)?;
            let bundle_bytes = read_artifact_reference(&root, bundle_ref)?;
            let bundle =
                verify_review_subject_bundle_bytes(&bundle_bytes, task_digest, composition_digest)?;
            let mut available = BTreeMap::new();
            available.insert(
                bundle.result_artifact.id.clone(),
                ArtifactEvidence {
                    artifact: bundle.result_artifact.clone(),
                    uses: bundle.result_uses.clone(),
                },
            );
            for evidence in &bundle.evidence {
                if available
                    .insert(evidence.artifact.id.clone(), evidence.clone())
                    .is_some()
                {
                    return Err(ReviewError::Contract(
                        "review subject bundle contains duplicate artifact IDs".to_owned(),
                    ));
                }
            }
            // The result artifact carries its own terminal provenance and is
            // still a valid selectable artifact. Evidence entries carry their
            // exact uses and are copied byte-for-byte.  Account for every
            // selected byte length before opening any selected leaf.  This
            // keeps a declared oversized aggregate from being turned into
            // an observable missing-leaf/open result and prevents overflow.
            let mut selected = Vec::with_capacity(include_artifact_ids.len());
            let mut aggregate_bytes = 0_usize;
            let mut missing_id = None;
            for id in include_artifact_ids {
                match available.get(id) {
                    Some(evidence) => {
                        let byte_length =
                            usize::try_from(evidence.artifact.byte_length).map_err(|_| {
                                ReviewError::Contract(
                                    "selected review artifact byte length exceeds the host bound"
                                        .to_owned(),
                                )
                            })?;
                        aggregate_bytes =
                            aggregate_bytes.checked_add(byte_length).ok_or_else(|| {
                                ReviewError::Contract(
                                    "selected review artifact aggregate byte length overflowed"
                                        .to_owned(),
                                )
                            })?;
                        if aggregate_bytes > MAX_REVIEW_SUBJECT_BUNDLE_BYTES {
                            return Err(ReviewError::Contract(
                                "selected review artifact aggregate exceeds the subject bound; no leaf is opened"
                                    .to_owned(),
                            ));
                        }
                        selected.push((id, evidence));
                    }
                    None => {
                        missing_id.get_or_insert(id);
                    }
                }
            }
            if let Some(id) = missing_id {
                return Err(ReviewError::Contract(format!(
                    "includeArtifactIds contains an ID absent from result/evidence: {id}"
                )));
            }
            let mut artifacts = Vec::with_capacity(selected.len());
            for (id, evidence) in selected {
                let bytes = read_artifact_reference(&root, &evidence.artifact)?;
                let content = String::from_utf8(bytes).map_err(|_| {
                    ReviewError::Contract(format!(
                        "selected artifact {id} is not valid UTF-8; binary review input is refused"
                    ))
                })?;
                artifacts.push(MaterializedArtifact {
                    artifact: evidence.artifact.clone(),
                    uses: evidence.uses.clone(),
                    content,
                });
            }
            let envelope = MaterializedSubjectEnvelope {
                schema_version: 1,
                task_digest: task_digest.clone(),
                composition_digest: composition_digest.clone(),
                bundle: bundle_ref.clone(),
                artifacts,
            };
            let content = serde_json::to_string(&envelope)
                .map_err(|error| ReviewError::Json(error.to_string()))?;
            if content.len() > MAX_REVIEW_SUBJECT_BUNDLE_BYTES {
                return Err(ReviewError::Contract(
                    "selected review artifact envelope exceeds the subject bound; no truncation is applied"
                        .to_owned(),
                ));
            }
            let subject = SubjectBundle {
                media_type: "application/vnd.upagent.review-subject-envelope+json; charset=utf-8"
                    .to_owned(),
                declared_sha256: Some(sha256(content.as_bytes())),
                content,
            };
            subject.validate()?;
            Ok(subject)
        }
    }
}

fn existing_artifact_root(path: &Path) -> Result<PathBuf, ReviewError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        ReviewError::Storage(format!("review artifact root unavailable: {error}"))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ReviewError::Storage(
            "review artifact root must be a regular directory".to_owned(),
        ));
    }
    path.canonicalize().map_err(|error| {
        ReviewError::Storage(format!(
            "review artifact root cannot be canonicalized: {error}"
        ))
    })
}

fn read_artifact_reference(root: &Path, reference: &ArtifactRef) -> Result<Vec<u8>, ReviewError> {
    validate_artifact_reference_shape(reference)?;
    let relative = Path::new(&reference.relative_path);
    let extension = relative
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 16
                && value.bytes().all(|byte| byte.is_ascii_alphanumeric())
        })
        .ok_or_else(|| ReviewError::Contract("review artifact extension is invalid".to_owned()))?;
    if relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                std::path::Component::CurDir
                    | std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
    {
        return Err(ReviewError::Contract(
            "review artifact path must be relative and contained".to_owned(),
        ));
    }
    let metadata_digest =
        artifact_metadata_digest(&reference.sha256, &reference.media_type, extension);
    let expected_id = format!("art-{}-{metadata_digest}", reference.sha256);
    let expected_relative = format!(
        "{}/{}-{metadata_digest}.{extension}",
        &reference.sha256[..2],
        reference.sha256
    );
    if reference.id.as_str() != expected_id || reference.relative_path != expected_relative {
        return Err(ReviewError::Contract(
            "review artifact reference does not match its content identity".to_owned(),
        ));
    }
    let components = relative.components().collect::<Vec<_>>();
    if components.len() != 2
        || !matches!(components[0], std::path::Component::Normal(_))
        || !matches!(components[1], std::path::Component::Normal(_))
    {
        return Err(ReviewError::Contract(
            "review artifact path must contain exactly one shard and one leaf".to_owned(),
        ));
    }
    read_artifact_reference_secure(
        root,
        &reference.sha256[..2],
        &reference.sha256,
        extension,
        reference,
    )
}

#[cfg(unix)]
fn read_artifact_reference_secure(
    root: &Path,
    shard: &str,
    leaf_digest: &str,
    extension: &str,
    reference: &ArtifactRef,
) -> Result<Vec<u8>, ReviewError> {
    use rustix::fs::{Mode, OFlags, fstat, open, openat};

    let metadata_digest =
        artifact_metadata_digest(&reference.sha256, &reference.media_type, extension);
    let leaf = format!("{leaf_digest}-{metadata_digest}.{extension}");
    let root_fd = open(
        root,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .map_err(|error| ReviewError::Storage(format!("review artifact root open failed: {error}")))?;
    let shard_fd = openat(
        &root_fd,
        shard,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .map_err(|error| ReviewError::Storage(format!("review artifact shard open failed: {error}")))?;
    let leaf_fd = openat(
        &shard_fd,
        leaf,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .map_err(|error| ReviewError::Storage(format!("review artifact leaf open failed: {error}")))?;
    let stat = fstat(&leaf_fd)
        .map_err(|error| ReviewError::Storage(format!("review artifact stat failed: {error}")))?;
    if (stat.st_mode & 0o170_000) != 0o100_000 {
        return Err(ReviewError::Contract(
            "review artifact leaf is not a regular file".to_owned(),
        ));
    }
    let file: File = leaf_fd.into();
    let metadata = file.metadata().map_err(|error| {
        ReviewError::Storage(format!("review artifact metadata failed: {error}"))
    })?;
    if !metadata.is_file() || metadata.len() != reference.byte_length {
        return Err(ReviewError::Contract(
            "review artifact byte length or file type does not match its reference".to_owned(),
        ));
    }
    let mut bytes = Vec::with_capacity(
        usize::try_from(reference.byte_length)
            .map_err(|_| ReviewError::Contract("review artifact is too large".to_owned()))?,
    );
    (&file)
        .take(MAX_REVIEW_ARTIFACT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| ReviewError::Storage(format!("review artifact read failed: {error}")))?;
    let final_metadata = file.metadata().map_err(|error| {
        ReviewError::Storage(format!("review artifact final metadata failed: {error}"))
    })?;
    if bytes.len() as u64 != reference.byte_length
        || final_metadata.len() != reference.byte_length
        || sha256(&bytes) != reference.sha256
    {
        return Err(ReviewError::Contract(
            "review artifact content does not match its reference".to_owned(),
        ));
    }
    Ok(bytes)
}

#[cfg(not(unix))]
fn read_artifact_reference_secure(
    _root: &Path,
    _shard: &str,
    _leaf_digest: &str,
    _extension: &str,
    _reference: &ArtifactRef,
) -> Result<Vec<u8>, ReviewError> {
    Err(ReviewError::Contract(
        "artifact bundle ingress is unsupported on non-Unix hosts".to_owned(),
    ))
}

pub(super) fn artifact_metadata_digest(
    content_digest: &str,
    media_type: &str,
    extension: &str,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"agent-artifact-v1");
    for value in [
        content_digest.as_bytes(),
        media_type.as_bytes(),
        extension.as_bytes(),
    ] {
        hasher.update((value.len() as u64).to_be_bytes());
        hasher.update(value);
    }
    format!("{:x}", hasher.finalize())
}
