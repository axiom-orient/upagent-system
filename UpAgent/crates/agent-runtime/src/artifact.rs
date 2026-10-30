use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
};

use agent_core::{ArtifactId, ArtifactRef};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

#[cfg(target_os = "macos")]
use rustix::fs::{CWD, RenameFlags, renameat_with};

const MAXIMUM_ARTIFACT_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum ArtifactError {
    #[error("artifact I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("artifact contract failed: {0}")]
    Contract(String),
}

#[derive(Debug, Clone)]
pub struct ArtifactStore {
    root: PathBuf,
}

/// Read-only view of the runtime's single content-addressed artifact store.
///
/// Tool modules receive this capability instead of the writer. New durable
/// artifacts must be returned as `ModuleArtifact` values so the runtime can
/// persist and journal them before exposing success to the model.
#[derive(Debug, Clone)]
pub struct ArtifactReader {
    store: ArtifactStore,
}

impl ArtifactReader {
    pub fn read(&self, reference: &ArtifactRef) -> Result<Vec<u8>, ArtifactError> {
        self.store.read(reference)
    }
}

impl ArtifactStore {
    #[must_use]
    pub fn reader(&self) -> ArtifactReader {
        ArtifactReader {
            store: self.clone(),
        }
    }

    /// Open an artifact store without creating or changing any filesystem
    /// state. Inspection and recovery use this path so missing evidence is a
    /// hard failure rather than an implicitly-created empty store.
    pub fn existing(root: impl AsRef<Path>) -> Result<Self, ArtifactError> {
        let root = root.as_ref();
        let metadata = fs::symlink_metadata(root)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(ArtifactError::Contract(
                "artifact root must be a real directory".to_owned(),
            ));
        }
        let root = root.canonicalize()?;
        Ok(Self { root })
    }

    pub fn open(root: impl AsRef<Path>) -> Result<Self, ArtifactError> {
        let root = root.as_ref();
        prepare_private_root(root)?;
        let root = root.canonicalize()?;
        Ok(Self { root })
    }

    pub fn put_text(&self, text: &str, media_type: &str) -> Result<ArtifactRef, ArtifactError> {
        self.put(text.as_bytes(), media_type, "txt")
    }

    pub fn put(
        &self,
        bytes: &[u8],
        media_type: &str,
        extension: &str,
    ) -> Result<ArtifactRef, ArtifactError> {
        if bytes.is_empty() || bytes.len() > MAXIMUM_ARTIFACT_BYTES {
            return Err(ArtifactError::Contract(format!(
                "artifact must be 1..={MAXIMUM_ARTIFACT_BYTES} bytes"
            )));
        }
        if media_type.trim().is_empty()
            || media_type.len() > 256
            || extension.is_empty()
            || extension.len() > 16
            || !extension.bytes().all(|byte| byte.is_ascii_alphanumeric())
        {
            return Err(ArtifactError::Contract(
                "artifact media type or extension is invalid".to_owned(),
            ));
        }
        let content_digest = format!("{:x}", Sha256::digest(bytes));
        let metadata_digest = metadata_digest(&content_digest, media_type, extension)?;
        let shard_name = &content_digest[..2];
        let file_name = format!("{content_digest}-{metadata_digest}.{extension}");
        let relative_path = format!("{shard_name}/{file_name}");
        let relative = PathBuf::from(&relative_path);
        let shard = self.root.join(shard_name);
        prepare_private_shard(&self.root, &shard)?;
        let destination = self.root.join(&relative);
        // Verification is required whenever this call did not produce the file
        // itself: a pre-existing or concurrently linked target may disagree with
        // the content address. Re-reading bytes this call just wrote and synced
        // would only re-derive a digest already known to match.
        let must_verify = match fs::symlink_metadata(&destination) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_file() {
                    return Err(ArtifactError::Contract(
                        "content-addressed artifact target is not a regular file".to_owned(),
                    ));
                }
                true
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                atomic_create(&destination, bytes)? == Creation::Existed
            }
            Err(error) => return Err(error.into()),
        };
        if must_verify && !file_matches(&destination, bytes)? {
            return Err(ArtifactError::Contract(
                "content-addressed artifact collision detected".to_owned(),
            ));
        }
        Ok(ArtifactRef {
            id: ArtifactId::parse(format!("art-{content_digest}-{metadata_digest}"))
                .map_err(|error| ArtifactError::Contract(error.to_string()))?,
            sha256: content_digest,
            media_type: media_type.to_owned(),
            byte_length: u64::try_from(bytes.len())
                .map_err(|_| ArtifactError::Contract("artifact length overflow".to_owned()))?,
            relative_path,
        })
    }

    /// Reads one previously returned artifact reference after re-deriving its
    /// complete content and metadata identity. Checkpoint resume uses this
    /// path instead of trusting a journal-provided relative path.
    pub fn read(&self, reference: &ArtifactRef) -> Result<Vec<u8>, ArtifactError> {
        if reference.sha256.len() != 64
            || !reference
                .sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            || reference.media_type.trim().is_empty()
            || reference.media_type.len() > 256
        {
            return Err(ArtifactError::Contract(
                "artifact reference identity is invalid".to_owned(),
            ));
        }
        let relative = Path::new(&reference.relative_path);
        let extension = relative
            .extension()
            .and_then(std::ffi::OsStr::to_str)
            .filter(|value| {
                !value.is_empty()
                    && value.len() <= 16
                    && value.bytes().all(|byte| byte.is_ascii_alphanumeric())
            })
            .ok_or_else(|| {
                ArtifactError::Contract("artifact reference extension is invalid".to_owned())
            })?;
        let metadata_digest = metadata_digest(&reference.sha256, &reference.media_type, extension)?;
        let expected_id = format!("art-{}-{metadata_digest}", reference.sha256);
        let expected_relative = format!(
            "{}/{}-{metadata_digest}.{extension}",
            &reference.sha256[..2],
            reference.sha256
        );
        if reference.id.as_str() != expected_id || reference.relative_path != expected_relative {
            return Err(ArtifactError::Contract(
                "artifact reference does not match its content identity".to_owned(),
            ));
        }
        if reference.byte_length == 0
            || reference.byte_length > u64::try_from(MAXIMUM_ARTIFACT_BYTES).unwrap_or(u64::MAX)
        {
            return Err(ArtifactError::Contract(
                "artifact reference length is invalid".to_owned(),
            ));
        }
        let path = self.root.join(relative);
        let canonical = path.canonicalize()?;
        if !canonical.starts_with(&self.root) {
            return Err(ArtifactError::Contract(
                "artifact reference path escaped its artifact root".to_owned(),
            ));
        }
        let mut file = open_read_no_follow(&path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.len() != reference.byte_length {
            return Err(ArtifactError::Contract(
                "artifact reference target is invalid".to_owned(),
            ));
        }
        let mut bytes = Vec::with_capacity(
            usize::try_from(reference.byte_length)
                .map_err(|_| ArtifactError::Contract("artifact length overflow".to_owned()))?,
        );
        Read::by_ref(&mut file)
            .take(u64::try_from(MAXIMUM_ARTIFACT_BYTES).unwrap_or(u64::MAX) + 1)
            .read_to_end(&mut bytes)?;
        let final_metadata = file.metadata()?;
        if u64::try_from(bytes.len()).ok() != Some(reference.byte_length)
            || final_metadata.len() != reference.byte_length
            || format!("{:x}", Sha256::digest(&bytes)) != reference.sha256
        {
            return Err(ArtifactError::Contract(
                "artifact reference content verification failed".to_owned(),
            ));
        }
        Ok(bytes)
    }
}

#[cfg(unix)]
fn open_read_no_follow(path: &Path) -> Result<File, ArtifactError> {
    use rustix::fs::{Mode, OFlags, open};

    let descriptor = open(
        path,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .map_err(|error| std::io::Error::from_raw_os_error(error.raw_os_error()))?;
    Ok(descriptor.into())
}

#[cfg(not(unix))]
fn open_read_no_follow(path: &Path) -> Result<File, ArtifactError> {
    Ok(File::open(path)?)
}

fn metadata_digest(
    content_digest: &str,
    media_type: &str,
    extension: &str,
) -> Result<String, ArtifactError> {
    let mut hasher = Sha256::new();
    hasher.update(b"agent-artifact-v1");
    for value in [
        content_digest.as_bytes(),
        media_type.as_bytes(),
        extension.as_bytes(),
    ] {
        let length = u64::try_from(value.len()).map_err(|_| {
            ArtifactError::Contract("artifact metadata length exceeds u64".to_owned())
        })?;
        hasher.update(length.to_be_bytes());
        hasher.update(value);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn file_matches(path: &Path, expected: &[u8]) -> Result<bool, ArtifactError> {
    let path_metadata = fs::symlink_metadata(path)?;
    if path_metadata.file_type().is_symlink() || !path_metadata.is_file() {
        return Err(ArtifactError::Contract(
            "content-addressed artifact target is not a regular file".to_owned(),
        ));
    }
    if path_metadata.len()
        != u64::try_from(expected.len())
            .map_err(|_| ArtifactError::Contract("artifact length overflow".to_owned()))?
    {
        return Ok(false);
    }
    let mut file = fs::File::open(path)?;
    if !file.metadata()?.is_file() {
        return Err(ArtifactError::Contract(
            "content-addressed artifact target is not a regular file".to_owned(),
        ));
    }
    let mut offset = 0_usize;
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            return Ok(offset == expected.len());
        }
        let end = offset.checked_add(read).ok_or_else(|| {
            ArtifactError::Contract("artifact comparison length overflow".to_owned())
        })?;
        if end > expected.len() || buffer[..read] != expected[offset..end] {
            return Ok(false);
        }
        offset = end;
    }
}

/// Whether `atomic_create` produced the destination or found it already linked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Creation {
    Created,
    Existed,
}

fn atomic_create(path: &Path, bytes: &[u8]) -> Result<Creation, ArtifactError> {
    let parent = path
        .parent()
        .ok_or_else(|| ArtifactError::Contract("artifact path has no parent".to_owned()))?;
    let temporary = parent.join(format!(".artifact-{}.tmp", Uuid::now_v7()));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    let creation = install_noclobber(&temporary, path)?;
    #[cfg(unix)]
    fs::File::open(parent)?.sync_all()?;
    Ok(creation)
}

fn install_noclobber(temporary: &Path, destination: &Path) -> Result<Creation, ArtifactError> {
    #[cfg(target_os = "macos")]
    {
        // RENAME_EXCL and RENAME_NOFOLLOW_ANY make the final create atomic:
        // an existing file or a symlink can never be replaced between the
        // metadata check and installation. rustix exposes the native
        // renameatx_np call without requiring unsafe code in this crate.
        const RENAME_NOFOLLOW_ANY: u32 = 0x0000_0010;
        let flags = RenameFlags::NOREPLACE | RenameFlags::from_bits_retain(RENAME_NOFOLLOW_ANY);
        match renameat_with(CWD, temporary, CWD, destination, flags) {
            Ok(()) => Ok(Creation::Created),
            Err(error) if error == rustix::io::Errno::EXIST => {
                fs::remove_file(temporary)?;
                Ok(Creation::Existed)
            }
            Err(error) => {
                let _ = fs::remove_file(temporary);
                Err(std::io::Error::from(error).into())
            }
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        let result = match fs::hard_link(temporary, destination) {
            Ok(()) => Ok(Creation::Created),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                Ok(Creation::Existed)
            }
            Err(error) => Err(error),
        };
        let cleanup = fs::remove_file(temporary);
        match result {
            Ok(creation) => {
                cleanup?;
                Ok(creation)
            }
            Err(error) => {
                let _ = cleanup;
                Err(error.into())
            }
        }
    }
}

fn prepare_private_root(path: &Path) -> Result<(), ArtifactError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(ArtifactError::Contract(
                    "artifact root must be a real directory".to_owned(),
                ));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(path)?;
        }
        Err(error) => return Err(error.into()),
    }
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ArtifactError::Contract(
            "artifact root must be a real directory".to_owned(),
        ));
    }
    set_private_directory_permissions(path)?;
    Ok(())
}

fn prepare_private_shard(root: &Path, shard: &Path) -> Result<(), ArtifactError> {
    if shard.parent() != Some(root) {
        return Err(ArtifactError::Contract(
            "artifact shard is outside the store root".to_owned(),
        ));
    }
    match fs::symlink_metadata(shard) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(ArtifactError::Contract(
                    "artifact shard must be a real directory".to_owned(),
                ));
            }
        }
        // Concurrent tasks that produce artifacts with the same digest prefix
        // race to create the shard. Losing that race is not a failure: the
        // directory the loser needed now exists, and the check below still
        // proves it is a real directory inside the store.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => match fs::create_dir(shard) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        },
        Err(error) => return Err(error.into()),
    }
    let metadata = fs::symlink_metadata(shard)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ArtifactError::Contract(
            "artifact shard must be a real directory".to_owned(),
        ));
    }
    set_private_directory_permissions(shard)?;
    let canonical = shard.canonicalize()?;
    if !canonical.starts_with(root) {
        return Err(ArtifactError::Contract(
            "artifact shard escaped the store root".to_owned(),
        ));
    }
    Ok(())
}

fn set_private_directory_permissions(path: &Path) -> Result<(), std::io::Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_identity_includes_content_metadata() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let store = ArtifactStore::open(temp.path())?;

        let markdown = store.put(b"same bytes", "text/markdown", "md")?;
        let plain = store.put(b"same bytes", "text/plain", "txt")?;
        let markdown_again = store.put(b"same bytes", "text/markdown", "md")?;

        assert_ne!(markdown.id, plain.id);
        assert_ne!(markdown.relative_path, plain.relative_path);
        assert_eq!(markdown, markdown_again);
        assert_eq!(markdown.sha256, plain.sha256);
        Ok(())
    }

    #[test]
    fn concurrent_writers_of_one_shard_all_succeed() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let store = ArtifactStore::open(temp.path())?;

        // Identical content lands in one shard, so every writer races to create
        // that directory. Losing the race must not fail the task.
        let stored = std::thread::scope(|scope| {
            let handles = (0..8)
                .map(|_| scope.spawn(|| store.put(b"shared artifact body", "text/plain", "txt")))
                .collect::<Vec<_>>();
            handles
                .into_iter()
                .map(|handle| handle.join().expect("writer thread"))
                .collect::<Vec<_>>()
        });

        let first = stored
            .first()
            .expect("at least one writer")
            .as_ref()
            .map_err(ToString::to_string)?
            .clone();
        for result in &stored {
            let reference = result.as_ref().map_err(ToString::to_string)?;
            assert_eq!(*reference, first, "content addressing must be stable");
        }
        Ok(())
    }

    #[test]
    fn atomic_create_is_exclusive_and_preserves_the_first_file()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        let destination = root.join("destination");

        assert_eq!(atomic_create(&destination, b"first")?, Creation::Created);
        assert_eq!(atomic_create(&destination, b"second")?, Creation::Existed);
        assert_eq!(fs::read(&destination)?, b"first");
        Ok(())
    }

    #[test]
    fn corrupted_content_addressed_targets_are_rejected() -> Result<(), Box<dyn std::error::Error>>
    {
        let temp = tempfile::tempdir()?;
        let store = ArtifactStore::open(temp.path())?;
        let artifact = store.put(b"expected", "text/plain", "txt")?;
        fs::write(temp.path().join(&artifact.relative_path), b"corrupt!")?;

        let error = store
            .put(b"expected", "text/plain", "txt")
            .expect_err("a corrupted content-addressed target must fail");
        assert!(matches!(error, ArtifactError::Contract(message) if message.contains("collision")));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn symlink_content_addressed_targets_are_rejected_without_following()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir()?;
        let store = ArtifactStore::open(temp.path())?;
        let artifact = store.put(b"expected", "text/plain", "txt")?;
        let destination = temp.path().join(&artifact.relative_path);
        fs::remove_file(&destination)?;
        let outside = temp.path().join("outside");
        fs::write(&outside, b"outside")?;
        symlink(&outside, &destination)?;

        assert!(store.put(b"expected", "text/plain", "txt").is_err());
        assert_eq!(fs::read(&outside)?, b"outside");
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn artifact_reads_refuse_a_replaced_symlink_target() -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir()?;
        let store = ArtifactStore::open(temp.path())?;
        let reference = store.put_text("trusted", "text/plain")?;
        let target = temp.path().join(&reference.relative_path);
        fs::remove_file(&target)?;
        let outside = temp.path().join("outside");
        fs::write(&outside, "trusted")?;
        symlink(&outside, &target)?;

        assert!(matches!(store.read(&reference), Err(ArtifactError::Io(_))));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn artifact_reads_refuse_a_symlinked_shard_escape() -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir()?;
        let outside = tempfile::tempdir()?;
        let store = ArtifactStore::open(temp.path())?;
        let reference = store.put_text("trusted", "text/plain")?;
        let shard = temp.path().join(&reference.relative_path);
        let shard = shard.parent().ok_or("artifact shard missing")?;
        let outside_shard = outside.path().join(&reference.sha256[..2]);
        fs::create_dir_all(&outside_shard)?;
        fs::copy(
            temp.path().join(&reference.relative_path),
            outside_shard.join(
                std::path::Path::new(&reference.relative_path)
                    .file_name()
                    .ok_or("artifact filename missing")?,
            ),
        )?;
        fs::remove_file(temp.path().join(&reference.relative_path))?;
        fs::remove_dir(shard)?;
        symlink(&outside_shard, shard)?;

        assert!(matches!(
            store.read(&reference),
            Err(ArtifactError::Contract(message)) if message.contains("escaped")
        ));
        Ok(())
    }
}
