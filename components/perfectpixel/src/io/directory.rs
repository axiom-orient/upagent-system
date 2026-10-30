use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use crate::core::{sha256::sha256, PpError, PpResult};

use super::{
    artifact_set::{
        ArtifactSetConditionPhase, AtomicArtifactSetOwnedEntry, AtomicArtifactSetOwnedPlan,
        AtomicArtifactSetWriter,
    },
    atomic::AtomicDirectoryEntry,
};

const MAX_ENTRIES: usize = 64;
const MAX_BYTES: usize = 64 * 1024 * 1024;
const MAX_DEPTH: usize = 16;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectoryPrecondition {
    target: PathBuf,
    snapshot: DirectorySnapshot,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DirectorySnapshot {
    files: Vec<DirectoryFileRevision>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DirectoryFileRevision {
    relative_path: PathBuf,
    bytes: u64,
    sha256: [u8; 32],
}

struct PublicationCondition {
    before: DirectorySnapshot,
    after: DirectorySnapshot,
}

impl DirectoryPrecondition {
    pub fn capture(target: impl AsRef<Path>) -> PpResult<Self> {
        let target = target.as_ref();
        validate_target(target)?;
        Ok(Self {
            target: target.to_path_buf(),
            snapshot: snapshot(target)?,
        })
    }
}

/// Publishes a complete managed directory against a revision captured before expensive work.
/// AtomicArtifactSetWriter remains the only commit/rollback authority; this layer owns only the
/// optimistic content precondition for directory-shaped outputs.
pub fn publish_directory_checked(
    target: impl AsRef<Path>,
    precondition: &DirectoryPrecondition,
    entries: &[AtomicDirectoryEntry<'_>],
) -> PpResult<()> {
    let target = target.as_ref();
    if target != precondition.target {
        return Err(PpError::InvalidRequest(format!(
            "directory publication precondition belongs to '{}' not '{}'",
            precondition.target.display(),
            target.display()
        )));
    }
    validate_entries(entries)?;

    let owned_entries = entries
        .iter()
        .map(|entry| AtomicArtifactSetOwnedEntry {
            relative_path: entry.relative_path.to_path_buf(),
            bytes: entry.bytes.to_vec(),
        })
        .collect::<Vec<_>>();
    let after = snapshot_from_entries(entries);
    let desired = after
        .files
        .iter()
        .map(|file| file.relative_path.clone())
        .collect::<BTreeSet<_>>();
    let expected_before = precondition.snapshot.clone();
    let ensure_empty_root = entries.is_empty();

    AtomicArtifactSetWriter::publish_with_planner_checked_exact(
        target,
        move |locked_root| {
            let observed = snapshot(locked_root)?;
            if observed != expected_before {
                return Err(precondition_error(
                    locked_root,
                    "managed directory changed after publication precondition capture",
                ));
            }
            let existing = observed
                .files
                .iter()
                .map(|file| file.relative_path.clone())
                .collect::<BTreeSet<_>>();
            let removals = existing.difference(&desired).cloned().collect();
            Ok((
                AtomicArtifactSetOwnedPlan {
                    entries: owned_entries,
                    removals,
                },
                PublicationCondition {
                    before: observed,
                    after,
                },
            ))
        },
        verify_publication,
        ensure_empty_root,
    )
}

fn verify_publication(
    root: &Path,
    phase: ArtifactSetConditionPhase,
    condition: &PublicationCondition,
) -> PpResult<()> {
    let actual = snapshot(root)?;
    let expected = match phase {
        ArtifactSetConditionPhase::BeforeMutation => &condition.before,
        ArtifactSetConditionPhase::BeforeCommit => &condition.after,
    };
    if &actual == expected {
        Ok(())
    } else {
        Err(precondition_error(
            root,
            match phase {
                ArtifactSetConditionPhase::BeforeMutation => {
                    "managed directory changed before mutation"
                }
                ArtifactSetConditionPhase::BeforeCommit => {
                    "installed managed directory does not match verified publication"
                }
            },
        ))
    }
}

fn snapshot_from_entries(entries: &[AtomicDirectoryEntry<'_>]) -> DirectorySnapshot {
    let mut files = entries
        .iter()
        .map(|entry| DirectoryFileRevision {
            relative_path: entry.relative_path.to_path_buf(),
            bytes: u64::try_from(entry.bytes.len()).unwrap_or(u64::MAX),
            sha256: entry.sha256,
        })
        .collect::<Vec<_>>();
    files.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    DirectorySnapshot { files }
}

fn snapshot(root: &Path) -> PpResult<DirectorySnapshot> {
    match fs::symlink_metadata(root) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(DirectorySnapshot { files: Vec::new() });
        }
        Err(error) => return Err(file_error(root, error)),
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(PpError::InvalidRequest(format!(
                "managed directory '{}' must be a non-symlink directory",
                root.display()
            )));
        }
        Ok(_) => {}
    }

    let mut files = Vec::new();
    let mut directories = Vec::new();
    let mut total = 0usize;
    visit(root, root, 0, &mut files, &mut directories, &mut total)?;
    files.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    directories.sort();
    for directory in directories {
        if !files
            .iter()
            .any(|file| file.relative_path.starts_with(&directory))
        {
            return Err(PpError::InvalidRequest(format!(
                "managed directory '{}' contains unmanaged empty directory '{}'",
                root.display(),
                directory.display()
            )));
        }
    }
    Ok(DirectorySnapshot { files })
}

fn visit(
    root: &Path,
    directory: &Path,
    depth: usize,
    files: &mut Vec<DirectoryFileRevision>,
    directories: &mut Vec<PathBuf>,
    total: &mut usize,
) -> PpResult<()> {
    if depth > MAX_DEPTH {
        return Err(PpError::ResourceLimit {
            operation: "artifact.publish".to_string(),
            cause: format!("managed directory depth exceeds {MAX_DEPTH}"),
        });
    }
    let mut entries = fs::read_dir(directory)
        .map_err(|error| file_error(directory, error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| file_error(directory, error))?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let relative = path
            .strip_prefix(root)
            .map_err(|error| file_error(&path, error))?
            .to_path_buf();
        let file_type = entry
            .file_type()
            .map_err(|error| file_error(&path, error))?;
        if file_type.is_symlink() {
            return Err(PpError::InvalidRequest(format!(
                "managed directory '{}' must not contain symlink '{}'",
                root.display(),
                relative.display()
            )));
        }
        if file_type.is_dir() {
            directories.push(relative);
            visit(root, &path, depth + 1, files, directories, total)?;
            continue;
        }
        if !file_type.is_file() {
            return Err(PpError::InvalidRequest(format!(
                "managed directory '{}' contains non-regular entry '{}'",
                root.display(),
                relative.display()
            )));
        }
        if files.len() >= MAX_ENTRIES {
            return Err(PpError::ResourceLimit {
                operation: "artifact.publish".to_string(),
                cause: format!("managed directory exceeds {MAX_ENTRIES} files"),
            });
        }
        let bytes = crate::io::capability::read_bounded(&path, MAX_BYTES.saturating_sub(*total))
            .map_err(|error| file_error(&path, error))?;
        *total = total
            .checked_add(bytes.len())
            .ok_or_else(|| PpError::ResourceLimit {
                operation: "artifact.publish".to_string(),
                cause: "managed directory byte count overflow".to_string(),
            })?;
        if *total > MAX_BYTES {
            return Err(PpError::ResourceLimit {
                operation: "artifact.publish".to_string(),
                cause: format!("managed directory exceeds {MAX_BYTES} bytes"),
            });
        }
        files.push(DirectoryFileRevision {
            relative_path: relative,
            bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            sha256: sha256(&bytes),
        });
    }
    Ok(())
}

fn validate_target(target: &Path) -> PpResult<()> {
    if target.as_os_str().is_empty() || target.file_name().is_none() {
        return Err(PpError::InvalidRequest(
            "managed directory target must name a directory".to_string(),
        ));
    }
    Ok(())
}

fn validate_entries(entries: &[AtomicDirectoryEntry<'_>]) -> PpResult<()> {
    if entries.len() > MAX_ENTRIES {
        return Err(PpError::ResourceLimit {
            operation: "artifact.publish".to_string(),
            cause: format!("publication contains more than {MAX_ENTRIES} files"),
        });
    }
    let mut total = 0usize;
    let mut paths = BTreeMap::new();
    for entry in entries {
        super::artifact_set::validate_relative_path(entry.relative_path)?;
        if paths
            .insert(entry.relative_path.to_path_buf(), ())
            .is_some()
        {
            return Err(PpError::InvalidRequest(format!(
                "duplicate managed directory entry '{}'",
                entry.relative_path.display()
            )));
        }
        total = total
            .checked_add(entry.bytes.len())
            .ok_or_else(|| PpError::ResourceLimit {
                operation: "artifact.publish".to_string(),
                cause: "publication byte count overflow".to_string(),
            })?;
        if total > MAX_BYTES {
            return Err(PpError::ResourceLimit {
                operation: "artifact.publish".to_string(),
                cause: format!("publication exceeds {MAX_BYTES} bytes"),
            });
        }
        if sha256(entry.bytes) != entry.sha256 {
            return Err(PpError::InvalidRequest(format!(
                "managed directory entry '{}' digest mismatch",
                entry.relative_path.display()
            )));
        }
    }
    Ok(())
}

fn precondition_error(path: &Path, cause: &str) -> PpError {
    PpError::PreconditionFailed {
        operation: "artifact.publish".to_string(),
        cause: format!("{}: {cause}", path.display()),
    }
}

fn file_error(path: &Path, error: impl ToString) -> PpError {
    PpError::FileIo {
        path: path.to_path_buf(),
        message: error.to_string(),
    }
}
