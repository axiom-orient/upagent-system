use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::core::{PpError, PpResult};

use super::artifact_set::{
    validate_relative_path, ArtifactSetConditionPhase, AtomicArtifactSetOwnedEntry,
    AtomicArtifactSetOwnedPlan, AtomicArtifactSetWriter,
};
use super::capability;

const MAX_TEMP_ATTEMPTS: usize = 64;
const MAX_DIRECTORY_ENTRIES: usize = 64;
const MAX_DIRECTORY_BYTES: usize = 64 * 1024 * 1024;
const MAX_RELATIVE_PATH_DEPTH: usize = 16;
const MAX_DIRECTORY_TREE_ENTRIES: usize = MAX_DIRECTORY_ENTRIES * (MAX_RELATIVE_PATH_DEPTH + 1);
const MAX_PRECONDITION_BYTES: u64 = 512 * 1024 * 1024;

/// Immutable evidence for the destination revision observed before expensive work.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilePrecondition {
    path: PathBuf,
    state: FilePreconditionState,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum FilePreconditionState {
    Absent,
    Present(FileRevision),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FileRevision {
    device: u64,
    inode: u64,
    byte_count: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
    sha256: [u8; 32],
}

impl FilePrecondition {
    pub fn capture(path: impl AsRef<Path>) -> PpResult<Self> {
        let path = path.as_ref();
        if path.as_os_str().is_empty() || path.file_name().is_none() {
            return Err(invalid_destination_error(
                path,
                "file target must name a file",
            ));
        }
        validate_destination_parent(path)?;
        let state = match capability::open_read(path) {
            Ok(file) => FilePreconditionState::Present(measure_revision(path, file)?),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                FilePreconditionState::Absent
            }
            Err(source) if source.raw_os_error() == Some(libc::ELOOP) => {
                return Err(invalid_destination_error(
                    path,
                    "file target must not traverse a symlink",
                ))
            }
            Err(source) => return Err(file_error(path, source)),
        };
        Ok(Self {
            path: path.to_path_buf(),
            state,
        })
    }

    fn verify(&self, path: &Path) -> PpResult<()> {
        if self.path != path {
            return Err(PpError::InvalidRequest(format!(
                "publication precondition belongs to '{}' not '{}'",
                self.path.display(),
                path.display()
            )));
        }
        match (&self.state, capability::open_read(path)) {
            (FilePreconditionState::Absent, Err(source))
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                Ok(())
            }
            (FilePreconditionState::Absent, Ok(_)) => Err(precondition_error(
                path,
                "destination appeared after publication precondition capture",
            )),
            (FilePreconditionState::Absent, Err(source)) => Err(file_error(path, source)),
            (FilePreconditionState::Present(_), Err(source))
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                Err(precondition_error(
                    path,
                    "destination disappeared after publication precondition capture",
                ))
            }
            (FilePreconditionState::Present(expected), Ok(file)) => {
                let actual = measure_revision(path, file)?;
                if &actual == expected {
                    Ok(())
                } else {
                    Err(precondition_error(
                        path,
                        "destination changed after publication precondition capture",
                    ))
                }
            }
            (FilePreconditionState::Present(_), Err(source)) => Err(file_error(path, source)),
        }
    }
}

/// Publishes one file through a same-directory temporary file.
///
/// Every path component is resolved with descriptor-relative no-follow I/O.
/// Temporary bytes are durably synchronized before `renameat`; the containing
/// directory is synchronized as part of the rename commit. `write_bytes_checked`
/// additionally binds the commit to a destination revision captured before
/// expensive work so concurrent/stale output cannot be silently overwritten.
pub struct AtomicFileWriter;

pub struct AtomicDirectoryEntry<'a> {
    pub relative_path: &'a Path,
    pub bytes: &'a [u8],
    pub sha256: [u8; 32],
}

pub struct AtomicDirectoryWriter;

struct DirectoryPublicationGuard {
    observed_paths: Vec<PathBuf>,
    next_paths: Vec<PathBuf>,
}

impl AtomicFileWriter {
    pub fn write_bytes(path: impl AsRef<Path>, bytes: &[u8]) -> PpResult<()> {
        let path = path.as_ref();
        let precondition = FilePrecondition::capture(path)?;
        Self::write_bytes_checked(path, &precondition, bytes)
    }

    pub fn write_bytes_checked(
        path: impl AsRef<Path>,
        precondition: &FilePrecondition,
        bytes: &[u8],
    ) -> PpResult<()> {
        let path = prepare_file_path(path.as_ref())?;
        let (tmp, file) = create_temp_file(&path)?;
        let result = (|| {
            write_temp(&tmp, file, bytes)?;
            precondition.verify(&path)?;
            capability::rename(&tmp, &path).map_err(|source| PpError::FileIo {
                path: path.to_path_buf(),
                message: format!("replacement/durability commit failed: {source}"),
            })
        })();
        match result {
            Ok(()) => Ok(()),
            Err(error) => cleanup_temp_after_failure(&tmp, error),
        }
    }

    pub fn write_text(path: impl AsRef<Path>, text: &str) -> PpResult<()> {
        Self::write_bytes(path, text.as_bytes())
    }

    pub fn write_text_checked(
        path: impl AsRef<Path>,
        precondition: &FilePrecondition,
        text: &str,
    ) -> PpResult<()> {
        Self::write_bytes_checked(path, precondition, text.as_bytes())
    }
}

impl AtomicDirectoryWriter {
    pub fn replace(target: impl AsRef<Path>, entries: &[AtomicDirectoryEntry<'_>]) -> PpResult<()> {
        Self::replace_inner(target, entries, false)
    }

    /// Publishes an agent-owned artifact directory only when the requested
    /// directory is absent or empty. Agent operations never adopt or prune
    /// unrelated files left by another operation.
    pub fn replace_new_or_empty(
        target: impl AsRef<Path>,
        entries: &[AtomicDirectoryEntry<'_>],
    ) -> PpResult<()> {
        Self::replace_inner(target, entries, true)
    }

    fn replace_inner(
        target: impl AsRef<Path>,
        entries: &[AtomicDirectoryEntry<'_>],
        require_empty_root: bool,
    ) -> PpResult<()> {
        let requested_target = target.as_ref();
        validate_target(requested_target)?;
        validate_entries(entries)?;
        let owned_entries = entries
            .iter()
            .map(|entry| AtomicArtifactSetOwnedEntry {
                relative_path: entry.relative_path.to_path_buf(),
                bytes: entry.bytes.to_vec(),
            })
            .collect::<Vec<_>>();
        let desired_paths = owned_entries
            .iter()
            .map(|entry| entry.relative_path.clone())
            .collect::<BTreeSet<_>>();

        let next_paths = desired_paths.iter().cloned().collect::<Vec<_>>();
        let ensure_empty_root_flag = entries.is_empty();
        AtomicArtifactSetWriter::publish_with_planner_checked_exact(
            requested_target,
            move |locked_root| {
                if require_empty_root {
                    ensure_empty_root(locked_root)?;
                }
                let existing_paths = collect_existing_directory_files(locked_root, &desired_paths)?;
                let removals = existing_paths
                    .iter()
                    .filter(|path| !desired_paths.contains(*path))
                    .cloned()
                    .collect();
                Ok((
                    AtomicArtifactSetOwnedPlan {
                        entries: owned_entries,
                        removals,
                    },
                    DirectoryPublicationGuard {
                        observed_paths: existing_paths,
                        next_paths,
                    },
                ))
            },
            verify_directory_publication,
            ensure_empty_root_flag,
        )
    }
}

fn ensure_empty_root(root: &Path) -> PpResult<()> {
    let mut entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => return Err(file_error(root, source)),
    };
    if entries
        .next()
        .transpose()
        .map_err(|source| file_error(root, source))?
        .is_some()
    {
        return Err(invalid_destination_error(
            root,
            "directory target must be absent or empty",
        ));
    }
    Ok(())
}

fn measure_revision(path: &Path, mut file: File) -> PpResult<FileRevision> {
    let before = file.metadata().map_err(|source| file_error(path, source))?;
    if !before.is_file() {
        return Err(invalid_destination_error(
            path,
            "destination must be a regular file",
        ));
    }
    if before.len() > MAX_PRECONDITION_BYTES {
        return Err(file_error(
            path,
            format!("destination exceeds {MAX_PRECONDITION_BYTES}-byte precondition limit"),
        ));
    }
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|source| file_error(path, source))?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    let after = file.metadata().map_err(|source| file_error(path, source))?;
    if revision_without_hash(&before) != revision_without_hash(&after) {
        return Err(precondition_error(
            path,
            "destination changed while publication precondition was captured",
        ));
    }
    let base = revision_without_hash(&after);
    Ok(FileRevision {
        device: base.0,
        inode: base.1,
        byte_count: base.2,
        modified_seconds: base.3,
        modified_nanoseconds: base.4,
        changed_seconds: base.5,
        changed_nanoseconds: base.6,
        sha256: hash.finalize().into(),
    })
}

fn revision_without_hash(metadata: &fs::Metadata) -> (u64, u64, u64, i64, i64, i64, i64) {
    (
        metadata.dev(),
        metadata.ino(),
        metadata.len(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec(),
    )
}

fn verify_directory_publication(
    root: &Path,
    phase: ArtifactSetConditionPhase,
    guard: &DirectoryPublicationGuard,
) -> PpResult<()> {
    let desired = guard.next_paths.iter().cloned().collect::<BTreeSet<_>>();
    let actual = collect_existing_directory_files(root, &desired)?;
    let expected = match phase {
        ArtifactSetConditionPhase::BeforeMutation => &guard.observed_paths,
        ArtifactSetConditionPhase::BeforeCommit => &guard.next_paths,
    };
    if actual.as_slice() != expected.as_slice() {
        let message = match phase {
            ArtifactSetConditionPhase::BeforeMutation => {
                "managed directory file set changed before publication mutation"
            }
            ArtifactSetConditionPhase::BeforeCommit => {
                "managed directory file set changed before publication commit"
            }
        };
        return Err(precondition_error(root, message));
    }
    Ok(())
}

fn validate_target(target: &Path) -> PpResult<()> {
    if target.as_os_str().is_empty() || target.file_name().is_none() {
        return Err(file_error(target, "directory target must name a directory"));
    }
    validate_destination_parent(target)?;
    match fs::symlink_metadata(target) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(invalid_destination_error(
                target,
                "directory target must be a non-symlink directory",
            ));
        }
        Ok(_) => {}
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => return Err(file_error(target, source)),
    }
    Ok(())
}

fn prepare_file_path(requested: &Path) -> PpResult<PathBuf> {
    if requested.as_os_str().is_empty() || requested.file_name().is_none() {
        return Err(invalid_destination_error(
            requested,
            "file target must name a file",
        ));
    }
    let parent = requested
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    validate_destination_parent(requested)?;
    capability::create_dir_all(parent).map_err(|source| file_error(parent, source))?;
    match capability::open_read(requested) {
        Ok(file)
            if !file
                .metadata()
                .map_err(|source| file_error(requested, source))?
                .is_file() =>
        {
            Err(invalid_destination_error(
                requested,
                "file target must be a regular non-symlink file",
            ))
        }
        Ok(_) => Ok(requested.to_path_buf()),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(requested.to_path_buf()),
        Err(source) if source.raw_os_error() == Some(libc::ELOOP) => Err(
            invalid_destination_error(requested, "file target must be a regular non-symlink file"),
        ),
        Err(source) => Err(file_error(requested, source)),
    }
}

/// Validate every existing destination ancestor before descriptor-relative I/O
/// starts. Missing ancestors are allowed because the capability layer creates
/// them without following links. macOS exposes `/tmp` and `/var` as known
/// platform aliases; these two links are translated by the capability layer and
/// are the only symlink ancestors accepted here.
fn validate_destination_parent(path: &Path) -> PpResult<()> {
    let mut parent = path
        .parent()
        .filter(|value| !value.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    loop {
        let candidate = parent;
        match fs::symlink_metadata(candidate) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                if is_known_platform_alias(candidate) {
                    return Ok(());
                }
                return Err(invalid_destination_error(
                    candidate,
                    "destination parent must be a non-symlink directory",
                ));
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(invalid_destination_error(
                    candidate,
                    "destination parent must be a non-symlink directory",
                ));
            }
            Ok(_) => return Ok(()),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                parent = candidate.parent().unwrap_or_else(|| Path::new("."));
                if candidate == Path::new(".") {
                    return Ok(());
                }
            }
            Err(source) if source.kind() == std::io::ErrorKind::NotADirectory => {
                return Err(invalid_destination_error(
                    candidate,
                    "destination parent must be a non-symlink directory",
                ));
            }
            Err(source) => return Err(file_error(candidate, source)),
        }
    }
}

fn is_known_platform_alias(path: &Path) -> bool {
    #[cfg(target_os = "macos")]
    {
        (path == Path::new("/tmp")
            && fs::read_link("/tmp").ok().as_deref() == Some(Path::new("private/tmp")))
            || (path == Path::new("/var")
                && fs::read_link("/var").ok().as_deref() == Some(Path::new("private/var")))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        false
    }
}

fn validate_entries(entries: &[AtomicDirectoryEntry<'_>]) -> PpResult<()> {
    if entries.len() > MAX_DIRECTORY_ENTRIES {
        return Err(file_error(
            Path::new("<diagnostics>"),
            "too many directory entries",
        ));
    }
    let mut total_bytes = 0usize;
    let mut paths = BTreeSet::new();
    for entry in entries {
        validate_relative_path(entry.relative_path)?;
        if !paths.insert(entry.relative_path.to_path_buf()) {
            return Err(file_error(entry.relative_path, "duplicate directory entry"));
        }
        total_bytes = total_bytes
            .checked_add(entry.bytes.len())
            .ok_or_else(|| file_error(entry.relative_path, "directory artifact bytes overflow"))?;
        if total_bytes > MAX_DIRECTORY_BYTES {
            return Err(file_error(
                entry.relative_path,
                "directory artifact bytes exceed limit",
            ));
        }
        if crate::core::sha256::sha256(entry.bytes) != entry.sha256 {
            return Err(file_error(
                entry.relative_path,
                "directory artifact digest mismatch",
            ));
        }
    }
    for path in &paths {
        for ancestor in path.ancestors().skip(1) {
            if paths.contains(ancestor) {
                return Err(file_error(
                    path,
                    "directory entry collides with another entry",
                ));
            }
        }
    }
    Ok(())
}

fn collect_existing_directory_files(
    root: &Path,
    desired_paths: &BTreeSet<PathBuf>,
) -> PpResult<Vec<PathBuf>> {
    match fs::symlink_metadata(root) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(invalid_destination_error(
                root,
                "managed directory root must be a non-symlink directory",
            ));
        }
        Ok(_) => {}
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(file_error(root, source)),
    }

    let mut files = Vec::new();
    let mut directories = Vec::new();
    let mut tree_entries = 0usize;
    let mut total_bytes = 0u64;
    collect_directory_tree(
        root,
        root,
        &mut files,
        &mut directories,
        &mut tree_entries,
        &mut total_bytes,
    )?;

    files.sort();
    directories.sort();
    for directory in directories {
        let represented_by_file = files.iter().any(|path| path.starts_with(&directory));
        let represented_by_next_set = desired_paths
            .iter()
            .any(|path| path.starts_with(&directory));
        if !represented_by_file && !represented_by_next_set {
            return Err(file_error(
                &root.join(&directory),
                "directory replacement cannot own an empty directory without a managed file",
            ));
        }
    }
    Ok(files)
}

fn collect_directory_tree(
    root: &Path,
    directory: &Path,
    files: &mut Vec<PathBuf>,
    directories: &mut Vec<PathBuf>,
    tree_entries: &mut usize,
    total_bytes: &mut u64,
) -> PpResult<()> {
    let mut entries = fs::read_dir(directory)
        .map_err(|source| file_error(directory, source))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| file_error(directory, source))?;
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        *tree_entries = (*tree_entries)
            .checked_add(1)
            .ok_or_else(|| file_error(directory, "directory tree entry count overflow"))?;
        if *tree_entries > MAX_DIRECTORY_TREE_ENTRIES {
            return Err(file_error(
                directory,
                "existing directory tree exceeds entry limit",
            ));
        }

        let path = entry.path();
        let relative = path
            .strip_prefix(root)
            .map_err(|source| file_error(&path, source))?
            .to_path_buf();
        validate_relative_path(&relative)?;
        let file_type = entry
            .file_type()
            .map_err(|source| file_error(&path, source))?;
        if file_type.is_symlink() {
            return Err(invalid_destination_error(
                &path,
                "managed directory tree must not contain symlinks",
            ));
        }
        if file_type.is_dir() {
            directories.push(relative);
            collect_directory_tree(root, &path, files, directories, tree_entries, total_bytes)?;
            continue;
        }
        if !file_type.is_file() {
            return Err(invalid_destination_error(
                &path,
                "managed directory tree must contain only regular files and directories",
            ));
        }

        let metadata = entry
            .metadata()
            .map_err(|source| file_error(&path, source))?;
        *total_bytes = (*total_bytes)
            .checked_add(metadata.len())
            .ok_or_else(|| file_error(&path, "existing directory byte count overflow"))?;
        if *total_bytes > MAX_DIRECTORY_BYTES as u64 {
            return Err(file_error(
                &path,
                "existing directory artifacts exceed byte limit",
            ));
        }
        files.push(relative);
        if files.len() > MAX_DIRECTORY_ENTRIES {
            return Err(file_error(
                &path,
                "existing directory artifacts exceed entry limit",
            ));
        }
    }
    Ok(())
}

fn write_temp(tmp: &Path, mut file: File, bytes: &[u8]) -> PpResult<()> {
    file.write_all(bytes).map_err(|source| PpError::FileIo {
        path: tmp.to_path_buf(),
        message: source.to_string(),
    })?;
    capability::sync_file_durable(&file).map_err(|source| PpError::FileIo {
        path: tmp.to_path_buf(),
        message: source.to_string(),
    })
}

fn create_temp_file(path: &Path) -> PpResult<(PathBuf, File)> {
    for attempt in 0..MAX_TEMP_ATTEMPTS {
        let tmp = temp_path(path, "tmp", attempt);
        match capability::create_new(&tmp) {
            Ok(file) => return Ok((tmp, file)),
            Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(source) => return Err(file_error(&tmp, source)),
        }
    }
    Err(file_error(
        path,
        format!("could not create unique temp file after {MAX_TEMP_ATTEMPTS} attempts"),
    ))
}

fn cleanup_temp_after_failure(tmp: &Path, primary: PpError) -> PpResult<()> {
    match capability::remove_file(tmp) {
        Ok(()) => Err(primary),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Err(primary),
        Err(source) => Err(PpError::FileIo {
            path: tmp.to_path_buf(),
            message: format!("{primary}; failed to durably remove temporary file: {source}"),
        }),
    }
}

fn temp_path(path: &Path, kind: &str, attempt: usize) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("output");
    path.with_file_name(format!(
        ".{file_name}.{kind}.{}.{}",
        std::process::id(),
        attempt
    ))
}

fn file_error(path: &Path, source: impl ToString) -> PpError {
    PpError::FileIo {
        path: path.to_path_buf(),
        message: source.to_string(),
    }
}

fn precondition_error(path: &Path, message: &str) -> PpError {
    PpError::InvalidRequestSource {
        message: format!("publication precondition failed: {message}"),
        path: path.to_path_buf(),
        original_error: message.to_string(),
    }
}

fn invalid_destination_error(path: &Path, message: &str) -> PpError {
    PpError::InvalidRequest(format!("destination '{}': {message}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_target_rejects_an_existing_directory() {
        let root = std::env::temp_dir().join(format!(
            "perfectpixel-file-target-directory-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("directory target");

        let error = AtomicFileWriter::write_bytes(&root, b"bytes")
            .expect_err("directory must not be replaced as a file");

        assert!(matches!(error, PpError::InvalidRequest(_)));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn checked_write_rejects_stale_destination() {
        let root = std::env::temp_dir().join(format!(
            "perfectpixel-file-precondition-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("root");
        let target = root.join("asset.bin");
        fs::write(&target, b"old").expect("old");
        let precondition = FilePrecondition::capture(&target).expect("capture");
        fs::write(&target, b"external").expect("external");

        let error = AtomicFileWriter::write_bytes_checked(&target, &precondition, b"new")
            .expect_err("stale destination must fail");
        assert!(error.to_string().contains("precondition"));
        assert_eq!(fs::read(&target).expect("current"), b"external");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn directory_replacement_rejects_unrepresented_empty_subdirectories() {
        let root = std::env::temp_dir().join(format!(
            "perfectpixel-empty-directory-{}",
            std::process::id()
        ));
        let target = root.join("diagnostics");
        let empty = target.join("unmanaged-empty");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&empty).expect("empty directory");

        let error = AtomicDirectoryWriter::replace(&target, &[])
            .expect_err("empty directory ownership must fail closed");

        assert!(error.to_string().contains("cannot own an empty directory"));
        assert!(empty.is_dir());
        let _ = fs::remove_dir_all(root);
    }
}
