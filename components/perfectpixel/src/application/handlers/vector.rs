use std::{
    collections::BTreeSet,
    env, fs,
    num::NonZeroUsize,
    path::{Component, Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{
    AtomicDirectoryEntry, AtomicDirectoryWriter, AtomicFileWriter, DiagnosticsIntent,
    EvaluationDecision, PpError, PpResult, SvgProfile, UnitScore, VectorAnalysis,
    VectorAnalysisRequest, VectorDetail, VectorOutcome, VectorPolicy, VectorPresetSelection,
    VectorRequest, Vectorizer,
};

use super::super::{
    generation_adapter::verify_input_snapshots,
    path::{destination_error, validate_file_extension, validate_raster_input_path},
    shared::{
        decode_raster_snapshot, read_utf8_limited, serialize_json, MAX_ARTIFACT_FILE_BYTES,
        MAX_CONTROL_READ_BYTES,
    },
};

const VECTOR_DIAGNOSTICS_OWNERSHIP_FILE: &str = ".perfectpixel-vector-diagnostics.json";
const VECTOR_DIAGNOSTICS_OWNERSHIP_SCHEMA: &str = "perfectpixel.vector-diagnostics-ownership/1";
const MAX_VECTOR_DIAGNOSTIC_ENTRIES: usize = 64;

#[allow(clippy::too_many_arguments)]
pub(crate) fn compile(
    input: PathBuf,
    output: PathBuf,
    preset: VectorPresetSelection,
    profile: SvgProfile,
    detail: Option<VectorDetail>,
    minimum_quality: Option<UnitScore>,
    maximum_quality_loss: Option<UnitScore>,
    maximum_paths: Option<NonZeroUsize>,
    policy_path: Option<PathBuf>,
    report: Option<PathBuf>,
    diagnostics: Option<PathBuf>,
) -> PpResult<String> {
    validate_raster_input_path(&input)?;
    validate_file_extension(&output, &["svg"], "vector output")?;
    if let Some(path) = report.as_deref() {
        validate_file_extension(path, &["json"], "vector report")?;
    }
    let policy = load_vector_policy(policy_path.as_deref())?;
    reject_vector_destination_collisions(
        &input,
        policy_path.as_deref(),
        Some(output.as_path()),
        report.as_deref(),
        diagnostics.as_deref(),
    )?;
    preflight_vector_static_destinations(Some(&output), report.as_deref(), diagnostics.as_deref())?;

    let request = VectorRequest::new(
        preset,
        profile,
        detail,
        minimum_quality,
        maximum_quality_loss,
        maximum_paths,
        policy,
        if diagnostics.is_some() {
            DiagnosticsIntent::requested(Vec::new())
                .map_err(|error| PpError::InvalidRequest(error.to_string()))?
        } else {
            DiagnosticsIntent::none()
        },
    )
    .map_err(|error| PpError::InvalidRequest(error.to_string()))?;

    let (image, input_snapshot) = decode_raster_snapshot(&input)?;
    let outcome = Vectorizer::new()?.run(&image, &request)?;
    let evaluation = match &outcome {
        VectorOutcome::Approved(output) => output.report(),
        VectorOutcome::Rejected(output) => output.report(),
    };
    let report_json = serialize_json(evaluation, "<vector-report>")?;
    let diagnostic_artifacts = match &outcome {
        VectorOutcome::Approved(output) => output.artifacts(),
        VectorOutcome::Rejected(output) => output.artifacts(),
    };

    let mut transaction = CliTransactionOutcome::new(report.is_some(), diagnostics.is_some());
    let diagnostics_manifest = match diagnostics
        .as_ref()
        .map(|_| diagnostic_ownership_manifest(diagnostic_artifacts))
        .transpose()
    {
        Ok(manifest) => manifest,
        Err(error) => {
            transaction = transaction.reduce(CliTransactionEvent::DiagnosticsFailed)?;
            return Err(vector_transaction_failure(
                evaluation,
                transaction,
                "diagnostics",
                error,
            )?);
        }
    };
    let mut diagnostic_entries = match diagnostics
        .as_ref()
        .map(|_| diagnostic_entries(diagnostic_artifacts))
        .transpose()
    {
        Ok(entries) => entries,
        Err(error) => {
            transaction = transaction.reduce(CliTransactionEvent::DiagnosticsFailed)?;
            return Err(vector_transaction_failure(
                evaluation,
                transaction,
                "diagnostics",
                error,
            )?);
        }
    };
    if let (Some(manifest), Some(entries)) =
        (diagnostics_manifest.as_deref(), diagnostic_entries.as_mut())
    {
        entries.push(AtomicDirectoryEntry {
            relative_path: Path::new(VECTOR_DIAGNOSTICS_OWNERSHIP_FILE),
            bytes: manifest,
            sha256: crate::sha256(manifest),
        });
    }
    if let Some(entries) = diagnostic_entries.as_deref() {
        if let Err(error) = preflight_diagnostic_entries(entries) {
            transaction = transaction.reduce(CliTransactionEvent::DiagnosticsFailed)?;
            return Err(vector_transaction_failure(
                evaluation,
                transaction,
                "diagnostics",
                error,
            )?);
        }
    }

    if let Some(path) = report.as_deref() {
        if let Err(error) = preflight_vector_file_destination(path)
            .and_then(|_| verify_input_snapshots(std::slice::from_ref(&input_snapshot)))
            .and_then(|_| AtomicFileWriter::write_text(path, &report_json))
        {
            transaction = transaction.reduce(CliTransactionEvent::ReportFailed)?;
            return Err(vector_transaction_failure(
                evaluation,
                transaction,
                "report",
                error,
            )?);
        }
        transaction = transaction.reduce(CliTransactionEvent::ReportCommitted)?;
    }

    if let (Some(path), Some(entries)) = (diagnostics.as_deref(), diagnostic_entries.as_deref()) {
        if let Err(error) = preflight_vector_diagnostics_destination(path)
            .and_then(|_| verify_input_snapshots(std::slice::from_ref(&input_snapshot)))
            .and_then(|_| AtomicDirectoryWriter::replace(path, entries))
        {
            transaction = transaction.reduce(CliTransactionEvent::DiagnosticsFailed)?;
            return Err(vector_transaction_failure(
                evaluation,
                transaction,
                "diagnostics",
                error,
            )?);
        }
        transaction = transaction.reduce(CliTransactionEvent::DiagnosticsCommitted)?;
    }

    match &outcome {
        VectorOutcome::Approved(approved) => {
            if let Err(error) = preflight_vector_file_destination(&output)
                .and_then(|_| verify_input_snapshots(std::slice::from_ref(&input_snapshot)))
                .and_then(|_| AtomicFileWriter::write_bytes(&output, approved.exact_svg_bytes()))
            {
                transaction = transaction.reduce(CliTransactionEvent::FinalSvgFailed)?;
                return Err(vector_transaction_failure(
                    evaluation,
                    transaction,
                    "finalSvg",
                    error,
                )?);
            }
            transaction = transaction.reduce(CliTransactionEvent::FinalSvgCommitted)?;
            serialize_json(
                &VectorResult {
                    schema: "perfectpixel.vector-result/1",
                    ok: true,
                    decision: evaluation.actual_decision(),
                    report: evaluation,
                    transaction,
                    failure: None,
                },
                "<vector-result>",
            )
        }
        VectorOutcome::Rejected(_) => {
            let payload = serialize_json(
                &VectorResult {
                    schema: "perfectpixel.vector-result/1",
                    ok: false,
                    decision: evaluation.actual_decision(),
                    report: evaluation,
                    transaction,
                    failure: None,
                },
                "<vector-result>",
            )?;
            Err(PpError::VectorRejected { payload })
        }
    }
}

pub(crate) fn analyze(
    input: PathBuf,
    preset: VectorPresetSelection,
    profile: SvgProfile,
    policy_path: Option<PathBuf>,
    report: Option<PathBuf>,
) -> PpResult<String> {
    validate_raster_input_path(&input)?;
    if let Some(path) = report.as_deref() {
        validate_file_extension(path, &["json"], "vector report")?;
    }
    let policy = load_vector_policy(policy_path.as_deref())?;
    reject_vector_destination_collisions(
        &input,
        policy_path.as_deref(),
        None,
        report.as_deref(),
        None,
    )?;
    preflight_vector_static_destinations(None, report.as_deref(), None)?;

    let request = VectorAnalysisRequest::new(preset, profile, policy)
        .map_err(|error| PpError::InvalidRequest(error.to_string()))?;
    let (image, input_snapshot) = decode_raster_snapshot(&input)?;
    let analysis = Vectorizer::new()?.analyze(&image, &request)?;
    let analysis_json = serialize_json(&analysis, "<vector-analysis>")?;
    let mut transaction = CliTransactionOutcome::analysis(report.is_some());

    if let Some(path) = report.as_deref() {
        if let Err(error) = preflight_vector_file_destination(path)
            .and_then(|_| verify_input_snapshots(std::slice::from_ref(&input_snapshot)))
            .and_then(|_| AtomicFileWriter::write_text(path, &analysis_json))
        {
            transaction = transaction.reduce(CliTransactionEvent::ReportFailed)?;
            return Err(vector_analysis_transaction_failure(
                &analysis,
                transaction,
                "report",
                error,
            )?);
        }
        transaction = transaction.reduce(CliTransactionEvent::ReportCommitted)?;
    }

    serialize_json(
        &VectorAnalysisResult {
            schema: "perfectpixel.vector-analysis-result/1",
            ok: true,
            analysis: &analysis,
            transaction,
            failure: None,
        },
        "<vector-analysis-result>",
    )
}

fn load_vector_policy(path: Option<&Path>) -> PpResult<VectorPolicy> {
    let Some(path) = path else {
        return Ok(VectorPolicy::default());
    };
    if validate_file_extension(path, &["json"], "vector policy").is_err() {
        return Err(PpError::InvalidOption(
            "--policy must reference a .json perfectpixel.vector-policy/1 document".to_string(),
        ));
    }
    let text = read_utf8_limited(path, MAX_CONTROL_READ_BYTES)?;
    let policy: VectorPolicy =
        serde_json::from_str(&text).map_err(|source| PpError::InvalidOptionSource {
            message: "--policy must reference a .json perfectpixel.vector-policy/1 document"
                .to_string(),
            path: path.to_path_buf(),
            original_error: source.to_string(),
        })?;
    if policy.schema() != VectorPolicy::SCHEMA {
        return Err(PpError::InvalidOption(
            "--policy must reference a .json perfectpixel.vector-policy/1 document".to_string(),
        ));
    }
    policy.validate().map_err(|_| {
        PpError::InvalidOption(
            "--policy must reference a .json perfectpixel.vector-policy/1 document".to_string(),
        )
    })?;
    Ok(policy)
}

fn reject_vector_destination_collisions(
    input: &Path,
    policy: Option<&Path>,
    output: Option<&Path>,
    report: Option<&Path>,
    diagnostics: Option<&Path>,
) -> PpResult<()> {
    if let Some(output) = output {
        reject_vector_path_overlap(input, output, "vector input and output must not collide")?;
        if let Some(policy) = policy {
            reject_vector_path_overlap(
                policy,
                output,
                "vector policy and output must not collide",
            )?;
        }
    }
    if let Some(report) = report {
        reject_vector_path_overlap(input, report, "vector input and report must not collide")?;
        if let Some(policy) = policy {
            reject_vector_path_overlap(
                policy,
                report,
                "vector policy and report must not collide",
            )?;
        }
        if let Some(output) = output {
            reject_vector_path_overlap(
                output,
                report,
                "vector output and report must not collide",
            )?;
        }
    }
    if let Some(diagnostics) = diagnostics {
        reject_vector_path_overlap(
            input,
            diagnostics,
            "vector input and diagnostics must not collide",
        )?;
        if let Some(policy) = policy {
            reject_vector_path_overlap(
                policy,
                diagnostics,
                "vector policy and diagnostics must not collide",
            )?;
        }
        if let Some(output) = output {
            reject_vector_path_overlap(
                output,
                diagnostics,
                "vector output and diagnostics must not collide",
            )?;
        }
        if let Some(report) = report {
            reject_vector_path_overlap(
                report,
                diagnostics,
                "vector report and diagnostics must not collide",
            )?;
        }
    }
    Ok(())
}

fn preflight_vector_static_destinations(
    output: Option<&Path>,
    report: Option<&Path>,
    diagnostics: Option<&Path>,
) -> PpResult<()> {
    let mut destinations = Vec::new();
    if let Some(output) = output {
        destinations.push(output);
        preflight_vector_file_destination(output)?;
    }
    if let Some(report) = report {
        destinations.push(report);
        preflight_vector_file_destination(report)?;
    }
    if let Some(diagnostics) = diagnostics {
        destinations.push(diagnostics);
        preflight_vector_diagnostics_destination(diagnostics)?;
    }
    for (index, left) in destinations.iter().enumerate() {
        for right in destinations.iter().skip(index + 1) {
            if vector_paths_equivalent(left, right)? {
                return Err(destination_error(
                    left,
                    "destination collides with another destination",
                ));
            }
        }
    }
    Ok(())
}

fn vector_paths_equivalent(left: &Path, right: &Path) -> PpResult<bool> {
    let left = vector_comparable_path(left)?;
    let right = vector_comparable_path(right)?;
    Ok(left == right
        || same_file_identity(&left, &right)?
        || left.to_string_lossy().to_lowercase() == right.to_string_lossy().to_lowercase())
}

fn preflight_vector_file_destination(path: &Path) -> PpResult<()> {
    preflight_vector_parent(path)?;
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => Err(
            destination_error(path, "file destination must be a regular non-symlink file"),
        ),
        Ok(_) => Ok(()),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(destination_error(path, &source.to_string())),
    }
}

fn preflight_vector_diagnostics_destination(path: &Path) -> PpResult<()> {
    preflight_vector_parent(path)?;
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            Err(destination_error(
                path,
                "diagnostics destination must be a non-symlink directory",
            ))
        }
        Ok(_) if vector_diagnostics_owned(path)? => Ok(()),
        Ok(_) => Err(destination_error(
            path,
            "diagnostics destination is not owned by perfectpixel vector",
        )),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(destination_error(path, &source.to_string())),
    }
}

fn vector_diagnostics_owned(path: &Path) -> PpResult<bool> {
    let mut names = BTreeSet::new();
    for entry in fs::read_dir(path).map_err(|source| PpError::FileIo {
        path: path.to_owned(),
        message: source.to_string(),
    })? {
        let entry = entry.map_err(|source| PpError::FileIo {
            path: path.to_owned(),
            message: source.to_string(),
        })?;
        names.insert(entry.file_name());
        if names.len() > MAX_VECTOR_DIAGNOSTIC_ENTRIES + 1 {
            return Ok(false);
        }
    }

    let marker_path = path.join(VECTOR_DIAGNOSTICS_OWNERSHIP_FILE);
    let marker_metadata = match fs::symlink_metadata(&marker_path) {
        Ok(metadata)
            if metadata.is_file()
                && !metadata.file_type().is_symlink()
                && metadata.len() <= MAX_CONTROL_READ_BYTES as u64 =>
        {
            metadata
        }
        Ok(_) => return Ok(false),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(source) => {
            return Err(PpError::FileIo {
                path: marker_path,
                message: source.to_string(),
            })
        }
    };
    let marker = fs::read(&marker_path).map_err(|source| PpError::FileIo {
        path: marker_path.clone(),
        message: source.to_string(),
    })?;
    if marker.len() as u64 != marker_metadata.len() {
        return Ok(false);
    }
    let manifest: VectorDiagnosticsOwnership = match serde_json::from_slice(&marker) {
        Ok(manifest) => manifest,
        Err(_) => return Ok(false),
    };
    if manifest.schema != VECTOR_DIAGNOSTICS_OWNERSHIP_SCHEMA
        || manifest.artifacts.len() > MAX_VECTOR_DIAGNOSTIC_ENTRIES
    {
        return Ok(false);
    }

    let mut expected =
        BTreeSet::from([std::ffi::OsString::from(VECTOR_DIAGNOSTICS_OWNERSHIP_FILE)]);
    for artifact in &manifest.artifacts {
        if !matches!(artifact.path.as_str(), "candidate.svg" | "render-back.png")
            || !crate::is_sha256_hex(&artifact.sha256)
            || !expected.insert(std::ffi::OsString::from(artifact.path.as_str()))
        {
            return Ok(false);
        }
    }
    if names != expected {
        return Ok(false);
    }
    for artifact in manifest.artifacts {
        let artifact_path = path.join(&artifact.path);
        let metadata = match fs::symlink_metadata(&artifact_path) {
            Ok(metadata)
                if metadata.is_file()
                    && !metadata.file_type().is_symlink()
                    && metadata.len() <= MAX_ARTIFACT_FILE_BYTES as u64 =>
            {
                metadata
            }
            Ok(_) => return Ok(false),
            Err(source) => {
                return Err(PpError::FileIo {
                    path: artifact_path,
                    message: source.to_string(),
                })
            }
        };
        let bytes = fs::read(&artifact_path).map_err(|source| PpError::FileIo {
            path: artifact_path,
            message: source.to_string(),
        })?;
        if bytes.len() as u64 != metadata.len() || crate::sha256_hex(&bytes) != artifact.sha256 {
            return Ok(false);
        }
    }
    Ok(true)
}

fn preflight_vector_parent(path: &Path) -> PpResult<()> {
    let mut parent = path.parent();
    while let Some(candidate) = parent {
        match fs::symlink_metadata(candidate) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(destination_error(
                    candidate,
                    "destination parent must be a non-symlink directory",
                ));
            }
            Ok(_) => return Ok(()),
            Err(source)
                if matches!(
                    source.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                parent = candidate.parent();
            }
            Err(source) => return Err(destination_error(candidate, &source.to_string())),
        }
    }
    Ok(())
}

fn preflight_diagnostic_entries(entries: &[AtomicDirectoryEntry<'_>]) -> PpResult<()> {
    if entries.len() > MAX_VECTOR_DIAGNOSTIC_ENTRIES {
        return Err(PpError::FileIo {
            path: PathBuf::from("<diagnostics>"),
            message: "too many directory entries".to_string(),
        });
    }
    let mut paths = BTreeSet::new();
    let mut total = 0usize;
    for entry in entries {
        let path = entry.relative_path;
        if path.as_os_str().is_empty()
            || path.as_os_str().len() > 1024
            || path
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
            || path.components().count() > 16
            || !paths.insert(path.to_path_buf())
        {
            return Err(PpError::FileIo {
                path: path.to_path_buf(),
                message: "invalid or duplicate diagnostic entry path".to_string(),
            });
        }
        total = total
            .checked_add(entry.bytes.len())
            .ok_or_else(|| PpError::FileIo {
                path: path.to_path_buf(),
                message: "diagnostic artifact bytes overflow".to_string(),
            })?;
        if total > MAX_ARTIFACT_FILE_BYTES {
            return Err(PpError::FileIo {
                path: path.to_path_buf(),
                message: "diagnostic artifact bytes exceed limit".to_string(),
            });
        }
    }
    for path in &paths {
        if path
            .ancestors()
            .skip(1)
            .any(|ancestor| paths.contains(ancestor))
        {
            return Err(PpError::FileIo {
                path: path.to_path_buf(),
                message: "diagnostic entry collides with another entry".to_string(),
            });
        }
    }
    Ok(())
}

fn reject_vector_path_overlap(left: &Path, right: &Path, message: &str) -> PpResult<()> {
    let left = vector_comparable_path(left)?;
    let right = vector_comparable_path(right)?;
    if left == right
        || same_file_identity(&left, &right)?
        || left.to_string_lossy().to_lowercase() == right.to_string_lossy().to_lowercase()
        || left.starts_with(&right)
        || right.starts_with(&left)
    {
        return Err(PpError::InvalidRequest(message.to_string()));
    }
    Ok(())
}

fn vector_comparable_path(path: &Path) -> PpResult<PathBuf> {
    match fs::canonicalize(path) {
        Ok(real_path) => return Ok(real_path),
        Err(source)
            if matches!(
                source.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) => {}
        Err(source) => {
            return Err(PpError::FileIo {
                path: path.to_owned(),
                message: source.to_string(),
            })
        }
    }
    let current_dir = env::current_dir().map_err(|source| PpError::FileIo {
        path: PathBuf::from("."),
        message: source.to_string(),
    })?;
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        current_dir.join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(value) => normalized.push(value),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
        }
    }

    let mut existing_prefix = normalized.clone();
    let mut missing_suffix = Vec::new();
    loop {
        match fs::symlink_metadata(&existing_prefix) {
            Ok(_) => break,
            Err(source)
                if matches!(
                    source.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                let Some(file_name) = existing_prefix.file_name().map(ToOwned::to_owned) else {
                    return Err(PpError::FileIo {
                        path: normalized,
                        message: "destination has no resolvable existing parent".to_string(),
                    });
                };
                missing_suffix.push(file_name);
                existing_prefix.pop();
            }
            Err(source) => {
                return Err(PpError::FileIo {
                    path: existing_prefix,
                    message: source.to_string(),
                })
            }
        }
    }
    let mut canonical_prefix =
        fs::canonicalize(&existing_prefix).map_err(|source| PpError::FileIo {
            path: existing_prefix,
            message: source.to_string(),
        })?;
    for component in missing_suffix.into_iter().rev() {
        canonical_prefix.push(component);
    }
    Ok(canonical_prefix)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileIdentity {
    device: u64,
    file: u64,
}

fn same_file_identity(left: &Path, right: &Path) -> PpResult<bool> {
    Ok(matches!(
        (file_identity(left)?, file_identity(right)?),
        (Some(left), Some(right)) if left == right
    ))
}

fn file_identity(path: &Path) -> PpResult<Option<FileIdentity>> {
    use std::os::unix::fs::MetadataExt;

    match fs::metadata(path) {
        Ok(metadata) => Ok(Some(FileIdentity {
            device: metadata.dev(),
            file: metadata.ino(),
        })),
        Err(source)
            if matches!(
                source.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(None)
        }
        Err(source) => Err(PpError::FileIo {
            path: path.to_owned(),
            message: source.to_string(),
        }),
    }
}

fn diagnostic_ownership_manifest(artifacts: &crate::DiagnosticArtifactSet) -> PpResult<Vec<u8>> {
    serde_json::to_vec(&VectorDiagnosticsOwnership {
        schema: VECTOR_DIAGNOSTICS_OWNERSHIP_SCHEMA.to_string(),
        artifacts: artifacts
            .artifacts()
            .iter()
            .map(|artifact| VectorDiagnosticsOwnershipArtifact {
                path: artifact.relative_path().to_string(),
                sha256: artifact.digest().to_string(),
            })
            .collect(),
    })
    .map_err(|source| PpError::Json {
        path: PathBuf::from("<vector-diagnostics-ownership>"),
        message: source.to_string(),
    })
}

fn diagnostic_entries(
    artifacts: &crate::DiagnosticArtifactSet,
) -> PpResult<Vec<AtomicDirectoryEntry<'_>>> {
    artifacts
        .artifacts()
        .iter()
        .map(|artifact| {
            let digest = artifact.digest();
            let mut sha256 = [0u8; 32];
            if digest.len() != 64 {
                return Err(PpError::Vectorizer(
                    "diagnostic artifact digest is not SHA-256".to_string(),
                ));
            }
            for (index, byte) in sha256.iter_mut().enumerate() {
                *byte =
                    u8::from_str_radix(&digest[index * 2..index * 2 + 2], 16).map_err(|_| {
                        PpError::Vectorizer(
                            "diagnostic artifact digest is not hexadecimal".to_string(),
                        )
                    })?;
            }
            Ok(AtomicDirectoryEntry {
                relative_path: Path::new(artifact.relative_path()),
                bytes: artifact.exact_bytes(),
                sha256,
            })
        })
        .collect()
}

fn vector_transaction_failure(
    report: &crate::EvaluationReport,
    transaction: CliTransactionOutcome,
    phase: &'static str,
    error: PpError,
) -> PpResult<PpError> {
    let exit_code = transaction_exit_code(&error);
    let payload = serialize_json(
        &VectorResult {
            schema: "perfectpixel.vector-result/1",
            ok: false,
            decision: report.actual_decision(),
            report,
            transaction,
            failure: Some(CliFailure::from_error(phase, error)),
        },
        "<vector-result>",
    )?;
    Ok(PpError::CliTransactionFailed { exit_code, payload })
}

fn vector_analysis_transaction_failure(
    analysis: &VectorAnalysis,
    transaction: CliTransactionOutcome,
    phase: &'static str,
    error: PpError,
) -> PpResult<PpError> {
    let exit_code = transaction_exit_code(&error);
    let payload = serialize_json(
        &VectorAnalysisResult {
            schema: "perfectpixel.vector-analysis-result/1",
            ok: false,
            analysis,
            transaction,
            failure: Some(CliFailure::from_error(phase, error)),
        },
        "<vector-analysis-result>",
    )?;
    Ok(PpError::CliTransactionFailed { exit_code, payload })
}

fn transaction_exit_code(error: &PpError) -> i32 {
    match error {
        PpError::InvalidOption(_)
        | PpError::InvalidOptionSource { .. }
        | PpError::InvalidRequest(_)
        | PpError::InvalidRequestSource { .. }
        | PpError::SvgContract(_) => 2,
        _ => 3,
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct VectorDiagnosticsOwnership {
    schema: String,
    artifacts: Vec<VectorDiagnosticsOwnershipArtifact>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct VectorDiagnosticsOwnershipArtifact {
    path: String,
    sha256: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CliFailure {
    phase: &'static str,
    path: Option<String>,
    original_error: String,
}

impl CliFailure {
    fn from_error(phase: &'static str, error: PpError) -> Self {
        Self {
            phase,
            path: super::super::shared::error_path(&error),
            original_error: super::super::shared::original_error(&error),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
enum CommitState {
    NotRequested,
    NotAttempted,
    Committed,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct CliTransactionOutcome {
    report: CommitState,
    diagnostics: CommitState,
    final_commit: CommitState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CliTransactionEvent {
    ReportCommitted,
    ReportFailed,
    DiagnosticsCommitted,
    DiagnosticsFailed,
    FinalSvgCommitted,
    FinalSvgFailed,
}

impl CliTransactionOutcome {
    fn new(report_requested: bool, diagnostics_requested: bool) -> Self {
        Self {
            report: if report_requested {
                CommitState::NotAttempted
            } else {
                CommitState::NotRequested
            },
            diagnostics: if diagnostics_requested {
                CommitState::NotAttempted
            } else {
                CommitState::NotRequested
            },
            final_commit: CommitState::NotAttempted,
        }
    }

    fn analysis(report_requested: bool) -> Self {
        Self {
            report: if report_requested {
                CommitState::NotAttempted
            } else {
                CommitState::NotRequested
            },
            diagnostics: CommitState::NotRequested,
            final_commit: CommitState::NotRequested,
        }
    }

    fn reduce(self, event: CliTransactionEvent) -> PpResult<Self> {
        let mut next = self;
        let (slot, result, artifact) = match event {
            CliTransactionEvent::ReportCommitted => {
                (&mut next.report, CommitState::Committed, "report")
            }
            CliTransactionEvent::ReportFailed => (&mut next.report, CommitState::Failed, "report"),
            CliTransactionEvent::DiagnosticsCommitted => {
                (&mut next.diagnostics, CommitState::Committed, "diagnostics")
            }
            CliTransactionEvent::DiagnosticsFailed => {
                (&mut next.diagnostics, CommitState::Failed, "diagnostics")
            }
            CliTransactionEvent::FinalSvgCommitted => {
                (&mut next.final_commit, CommitState::Committed, "finalSvg")
            }
            CliTransactionEvent::FinalSvgFailed => {
                (&mut next.final_commit, CommitState::Failed, "finalSvg")
            }
        };
        if *slot != CommitState::NotAttempted {
            return Err(PpError::Vectorizer(format!(
                "invalid vector transaction event {event:?}: {artifact} is already {slot:?}"
            )));
        }
        *slot = result;
        Ok(next)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct VectorResult<'a> {
    schema: &'static str,
    ok: bool,
    decision: EvaluationDecision,
    report: &'a crate::EvaluationReport,
    transaction: CliTransactionOutcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    failure: Option<CliFailure>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct VectorAnalysisResult<'a> {
    schema: &'static str,
    ok: bool,
    analysis: &'a VectorAnalysis,
    transaction: CliTransactionOutcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    failure: Option<CliFailure>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn analysis_transaction_rejects_unrequested_effects() {
        let transaction = CliTransactionOutcome::analysis(false);
        assert!(transaction
            .reduce(CliTransactionEvent::ReportCommitted)
            .is_err());
        assert!(transaction
            .reduce(CliTransactionEvent::DiagnosticsFailed)
            .is_err());
        assert!(transaction
            .reduce(CliTransactionEvent::FinalSvgFailed)
            .is_err());
    }
}
