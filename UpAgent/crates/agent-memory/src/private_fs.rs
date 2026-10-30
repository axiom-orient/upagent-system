use std::{
    fs::{self, File},
    io::Write,
    path::Path,
};

use rustix::fs::{AtFlags, Mode, OFlags, fstat, open, openat, renameat, statat, unlinkat};
use uuid::Uuid;

use crate::MemoryError;

pub(super) fn prepare_private_root(path: &Path) -> Result<File, MemoryError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(MemoryError::Invalid(
                    "memory root must be a real directory".to_owned(),
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
        return Err(MemoryError::Invalid(
            "memory root must be a real directory".to_owned(),
        ));
    }
    let directory = File::from(
        open(
            path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(std::io::Error::from)?,
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let opened = directory.metadata()?;
        if opened.dev() != metadata.dev() || opened.ino() != metadata.ino() {
            return Err(MemoryError::Invalid(
                "memory root changed while opening".to_owned(),
            ));
        }
        directory.set_permissions(fs::Permissions::from_mode(0o700))?;
    }
    Ok(directory)
}

pub(super) fn open_private_file(
    directory: &File,
    path: &Path,
    create: bool,
) -> Result<File, MemoryError> {
    open_private_file_with(directory, path, create, || {}, || {})
}

fn open_private_file_with(
    directory: &File,
    path: &Path,
    create: bool,
    before_open: impl FnOnce(),
    after_open: impl FnOnce(),
) -> Result<File, MemoryError> {
    let name = file_name(path)?;
    validate_file_type(directory, name, "memory state file must be a regular file")?;
    let mut flags = OFlags::RDWR | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK;
    if create {
        flags |= OFlags::CREATE;
    }
    before_open();
    let opened = openat(directory, name, flags, Mode::from_raw_mode(0o600));
    after_open();
    let file = File::from(opened.map_err(std::io::Error::from)?);
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(MemoryError::Invalid(
            "memory state file must be a regular file".to_owned(),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let opened = fstat(&file).map_err(std::io::Error::from)?;
        let current =
            statat(directory, name, AtFlags::SYMLINK_NOFOLLOW).map_err(std::io::Error::from)?;
        if opened.st_dev != current.st_dev || opened.st_ino != current.st_ino {
            return Err(MemoryError::Invalid(
                "memory state file changed while opening".to_owned(),
            ));
        }
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    Ok(file)
}

fn create_private_file(directory: &File, path: &Path) -> Result<File, MemoryError> {
    let file = File::from(
        openat(
            directory,
            file_name(path)?,
            OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )
        .map_err(std::io::Error::from)?,
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    Ok(file)
}

pub(super) fn atomic_replace_private(
    directory: &File,
    path: &Path,
    bytes: &[u8],
) -> Result<(), MemoryError> {
    atomic_replace_private_with_sync(directory, path, bytes, File::sync_all)
}

fn atomic_replace_private_with_sync(
    directory: &File,
    path: &Path,
    bytes: &[u8],
    sync_directory: impl FnOnce(&File) -> std::io::Result<()>,
) -> Result<(), MemoryError> {
    let name = file_name(path)?;
    validate_file_type(
        directory,
        name,
        "memory snapshot destination must be a regular file",
    )?;
    let temporary = format!(".memory-{}.tmp", Uuid::new_v4());
    let mut file = create_private_file(directory, Path::new(&temporary))?;
    let prepared = (|| -> std::io::Result<()> {
        file.write_all(bytes)?;
        file.sync_all()?;
        renameat(directory, temporary.as_str(), directory, name).map_err(Into::into)
    })();
    if let Err(error) = prepared {
        let _ = unlinkat(directory, temporary.as_str(), AtFlags::empty());
        return Err(error.into());
    }
    // Rename has already made the new snapshot visible. A failed directory
    // flush cannot be reported as an operation with no persistent effect.
    sync_directory(directory).map_err(MemoryError::CommitUncertain)?;
    Ok(())
}

fn file_name(path: &Path) -> Result<&std::ffi::OsStr, MemoryError> {
    path.file_name()
        .ok_or_else(|| MemoryError::Invalid("memory file path has no file name".to_owned()))
}

fn validate_file_type(
    directory: &File,
    name: &std::ffi::OsStr,
    message: &str,
) -> Result<(), MemoryError> {
    match statat(directory, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(metadata)
            if rustix::fs::FileType::from_raw_mode(metadata.st_mode)
                != rustix::fs::FileType::RegularFile =>
        {
            Err(MemoryError::Invalid(message.to_owned()))
        }
        Ok(_) | Err(rustix::io::Errno::NOENT) => Ok(()),
        Err(error) => Err(std::io::Error::from(error).into()),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::Read;
    use std::os::unix::fs::{PermissionsExt, symlink};

    #[test]
    fn failure_after_rename_reports_a_possible_commit() {
        let temp = tempfile::tempdir().unwrap();
        let directory = prepare_private_root(temp.path()).unwrap();
        let path = temp.path().join("state");
        fs::write(&path, b"old").unwrap();
        let error = atomic_replace_private_with_sync(&directory, &path, b"new", |_| {
            // Exercise a real fsync error after the real replacement; no
            // filesystem failure or mutation is simulated as a success.
            File::open("/dev/null")?.sync_all()
        })
        .unwrap_err();
        assert_eq!(fs::read(&path).unwrap(), b"new");
        assert!(
            error.may_have_committed(),
            "post-rename failure lost its effect: {error}"
        );
    }

    #[test]
    fn root_replacement_cannot_redirect_an_open_to_an_outside_directory() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        let parked = temp.path().join("parked");
        let outside = temp.path().join("outside");
        let directory = prepare_private_root(&root).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::write(root.join("state"), b"inside").unwrap();
        fs::write(outside.join("state"), b"outside").unwrap();
        fs::set_permissions(outside.join("state"), fs::Permissions::from_mode(0o644)).unwrap();
        let mut file = open_private_file_with(
            &directory,
            &root.join("state"),
            false,
            || {
                fs::rename(&root, &parked).unwrap();
                symlink(&outside, &root).unwrap();
            },
            || {
                fs::remove_file(&root).unwrap();
                fs::rename(&parked, &root).unwrap();
            },
        )
        .unwrap();
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"inside");
        fs::rename(&root, &parked).unwrap();
        symlink(&outside, &root).unwrap();
        atomic_replace_private(&directory, &root.join("state"), b"updated").unwrap();
        assert_eq!(fs::read(parked.join("state")).unwrap(), b"updated");
        assert_eq!(fs::read(outside.join("state")).unwrap(), b"outside");
        fs::remove_file(&root).unwrap();
        fs::rename(&parked, &root).unwrap();
        assert_eq!(
            fs::metadata(outside.join("state"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o644
        );
    }

    #[test]
    fn a_replaced_regular_file_is_rejected_before_its_permissions_change() {
        let temp = tempfile::tempdir().unwrap();
        let directory = prepare_private_root(temp.path()).unwrap();
        let path = temp.path().join("state");
        let parked = temp.path().join("parked");
        fs::write(&path, b"original").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        let result = open_private_file_with(
            &directory,
            &path,
            false,
            || {},
            || {
                fs::rename(&path, &parked).unwrap();
                fs::write(&path, b"replacement").unwrap();
            },
        );
        assert!(result.is_err());
        assert_eq!(
            fs::metadata(&parked).unwrap().permissions().mode() & 0o777,
            0o644
        );
    }

    #[test]
    fn temporary_symlink_swap_cannot_open_or_chmod_an_outside_file() {
        let temp = tempfile::tempdir().unwrap();
        let directory = prepare_private_root(temp.path()).unwrap();
        let path = temp.path().join("state");
        let parked = temp.path().join("parked");
        let outside = temp.path().join("outside");
        fs::write(&path, b"inside").unwrap();
        fs::write(&outside, b"outside").unwrap();
        fs::set_permissions(&outside, fs::Permissions::from_mode(0o644)).unwrap();
        let result = open_private_file_with(
            &directory,
            &path,
            false,
            || {
                fs::rename(&path, &parked).unwrap();
                symlink(&outside, &path).unwrap();
            },
            || {
                fs::remove_file(&path).unwrap();
                fs::rename(&parked, &path).unwrap();
            },
        );
        assert!(
            result.is_err(),
            "outside descriptor escaped path validation"
        );
        assert_eq!(
            fs::metadata(&outside).unwrap().permissions().mode() & 0o777,
            0o644
        );
    }
}
