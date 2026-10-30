use std::ffi::{CString, OsStr, OsString};
use std::fs::File;
use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path};

const CREATE_MODE: libc::mode_t = 0o600;
const DIRECTORY_MODE: libc::mode_t = 0o755;

pub(crate) struct RenameCommit {
    from_parent: File,
    to_parent: File,
}

impl RenameCommit {
    pub(crate) fn sync(self) -> io::Result<()> {
        sync_directory_handle(&self.from_parent)?;
        sync_directory_handle(&self.to_parent)
    }
}

/// Opens a filesystem object without following any symlink component.
/// Resolution starts from an already-open `/` or cwd directory and every
/// descendant component is resolved with `openat(O_NOFOLLOW)`.
pub(crate) fn open_read(path: &Path) -> io::Result<File> {
    let (parent, name) = open_parent(path, false)?;
    openat_file(
        parent.as_raw_fd(),
        &name,
        libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        0,
    )
}

pub(crate) fn open_directory(path: &Path) -> io::Result<File> {
    let (mut current, components) = anchor_and_components(path)?;
    for component in components {
        current = openat_directory(current.as_raw_fd(), &component)?;
    }
    Ok(current)
}

pub(crate) fn create_dir_all(path: &Path) -> io::Result<File> {
    let (mut current, components) = anchor_and_components(path)?;
    for component in components {
        match openat_directory(current.as_raw_fd(), &component) {
            Ok(next) => current = next,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                mkdirat(current.as_raw_fd(), &component, false)?;
                sync_directory_handle(&current)?;
                current = openat_directory(current.as_raw_fd(), &component)?;
            }
            Err(error) => return Err(error),
        }
    }
    Ok(current)
}

pub(crate) fn create_directory_new(path: &Path) -> io::Result<File> {
    let (parent, name) = open_parent(path, false)?;
    mkdirat(parent.as_raw_fd(), &name, true)?;
    sync_directory_handle(&parent)?;
    openat_directory(parent.as_raw_fd(), &name)
}

pub(crate) fn create_new(path: &Path) -> io::Result<File> {
    let (parent, name) = open_parent(path, true)?;
    openat_file(
        parent.as_raw_fd(),
        &name,
        libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        CREATE_MODE,
    )
}

pub(crate) fn open_lock(path: &Path) -> io::Result<File> {
    let (parent, name) = open_parent(path, true)?;
    openat_file(
        parent.as_raw_fd(),
        &name,
        libc::O_RDWR | libc::O_CREAT | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        CREATE_MODE,
    )
}

/// Performs only the visible namespace commit and returns the exact opened
/// parent capabilities that must be synchronized to make that rename durable.
pub(crate) fn rename_visible(from: &Path, to: &Path) -> io::Result<RenameCommit> {
    let (from_parent, from_name) = open_parent(from, false)?;
    let (to_parent, to_name) = open_parent(to, true)?;
    let from_name = c_string(&from_name)?;
    let to_name = c_string(&to_name)?;
    let result = unsafe {
        libc::renameat(
            from_parent.as_raw_fd(),
            from_name.as_ptr(),
            to_parent.as_raw_fd(),
            to_name.as_ptr(),
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(RenameCommit {
        from_parent,
        to_parent,
    })
}

pub(crate) fn rename(from: &Path, to: &Path) -> io::Result<()> {
    rename_visible(from, to)?.sync()
}

pub(crate) fn remove_file(path: &Path) -> io::Result<()> {
    unlinkat(path, 0)
}

pub(crate) fn remove_directory(path: &Path) -> io::Result<()> {
    unlinkat(path, libc::AT_REMOVEDIR)
}

fn unlinkat(path: &Path, flags: libc::c_int) -> io::Result<()> {
    let (parent, name) = open_parent(path, false)?;
    let name = c_string(&name)?;
    let result = unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), flags) };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    sync_directory_handle(&parent)
}

pub(crate) fn read_bounded(path: &Path, limit: usize) -> io::Result<Vec<u8>> {
    let mut file = open_read(path)?;
    let read_limit = u64::try_from(limit)
        .ok()
        .and_then(|value| value.checked_add(1))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "read limit overflow"))?;
    let mut bytes = Vec::new();
    std::io::Read::by_ref(&mut file)
        .take(read_limit)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(io::Error::new(
            io::ErrorKind::FileTooLarge,
            format!("file exceeds {limit}-byte read limit"),
        ));
    }
    Ok(bytes)
}

/// Synchronizes file contents and metadata to the strongest primitive exposed by
/// the supported platform. On macOS/iOS, `fsync` alone may return before the
/// storage device has accepted the data; `F_FULLFSYNC` closes that gap.
pub(crate) fn sync_file_durable(file: &File) -> io::Result<()> {
    file.sync_all()?;
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    {
        let result = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_FULLFSYNC) };
        if result == -1 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

pub(crate) fn sync_directory(path: &Path) -> io::Result<()> {
    let directory = open_directory(path)?;
    sync_directory_handle(&directory)
}

fn sync_directory_handle(directory: &File) -> io::Result<()> {
    directory.sync_all()
}

fn open_parent(path: &Path, create_parent: bool) -> io::Result<(File, OsString)> {
    let file_name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path must name an entry"))?
        .to_os_string();
    let parent = path
        .parent()
        .filter(|value| !value.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let directory = if create_parent {
        create_dir_all(parent)?
    } else {
        open_directory(parent)?
    };
    Ok((directory, file_name))
}

fn anchor_and_components(path: &Path) -> io::Result<(File, Vec<OsString>)> {
    let anchor = if path.is_absolute() {
        Path::new("/")
    } else {
        Path::new(".")
    };
    // macOS exposes `/var` and `/tmp` as the system aliases `private/var` and
    // `private/tmp`. The descriptor walk intentionally rejects symlink
    // ancestors, so translate these platform-owned aliases before opening
    // components. Do not generalize this to arbitrary symlinks: callers still
    // get no-follow semantics everywhere else.
    #[cfg(target_os = "macos")]
    let macos_var_alias = path.is_absolute()
        && std::fs::read_link("/var").ok().as_deref() == Some(Path::new("private/var"));
    #[cfg(target_os = "macos")]
    let macos_tmp_alias = path.is_absolute()
        && std::fs::read_link("/tmp").ok().as_deref() == Some(Path::new("private/tmp"));
    let mut components = Vec::new();
    let mut first_normal = true;
    for component in path.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(value) => {
                #[cfg(target_os = "macos")]
                if first_normal
                    && ((macos_var_alias && value == OsStr::new("var"))
                        || (macos_tmp_alias && value == OsStr::new("tmp")))
                {
                    components.push(OsString::from("private"));
                }
                components.push(value.to_os_string());
                first_normal = false;
            }
            // Parent traversal is valid for ambient CLI paths. It remains
            // descriptor-relative and cannot be replaced by a symlink during use.
            Component::ParentDir => components.push(OsString::from("..")),
            Component::Prefix(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "non-Unix path prefix is unsupported",
                ))
            }
        }
    }
    let directory = File::open(anchor)?;
    Ok((directory, components))
}

fn openat_directory(parent: RawFd, name: &OsStr) -> io::Result<File> {
    openat_file(
        parent,
        name,
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        0,
    )
}

fn openat_file(
    parent: RawFd,
    name: &OsStr,
    flags: libc::c_int,
    mode: libc::mode_t,
) -> io::Result<File> {
    let name = c_string(name)?;
    let fd = unsafe { libc::openat(parent, name.as_ptr(), flags, mode as libc::c_int) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn mkdirat(parent: RawFd, name: &OsStr, exclusive: bool) -> io::Result<()> {
    let name = c_string(name)?;
    let result = unsafe { libc::mkdirat(parent, name.as_ptr(), DIRECTORY_MODE) };
    if result != 0 {
        let error = io::Error::last_os_error();
        if !(error.kind() == io::ErrorKind::AlreadyExists && !exclusive) {
            return Err(error);
        }
    }
    Ok(())
}

fn c_string(value: &OsStr) -> io::Result<CString> {
    CString::new(value.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "filesystem path contains NUL"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use std::os::unix::fs::symlink;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn root(label: &str) -> std::path::PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "perfectpixel-capability-{label}-{}-{stamp}",
            std::process::id()
        ))
    }

    #[test]
    fn read_rejects_symlink_ancestor() {
        let base = root("nofollow");
        let outside = root("outside");
        fs::create_dir_all(&base).expect("base");
        fs::create_dir_all(&outside).expect("outside");
        fs::write(outside.join("secret.txt"), b"secret").expect("secret");
        symlink(&outside, base.join("link")).expect("symlink");

        let result = open_read(&base.join("link/secret.txt"));
        assert!(result.is_err());

        let _ = fs::remove_dir_all(base);
        let _ = fs::remove_dir_all(outside);
    }

    #[test]
    fn rename_is_descriptor_relative_and_durable() -> io::Result<()> {
        let base = root("rename");
        create_dir_all(&base)?;
        let from = base.join("from.bin");
        let to = base.join("to.bin");
        let mut file = create_new(&from)?;
        file.write_all(b"payload")?;
        sync_file_durable(&file)?;
        drop(file);
        rename(&from, &to)?;
        assert_eq!(read_bounded(&to, 16)?, b"payload");
        let _ = fs::remove_dir_all(base);
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn descriptor_walk_resolves_platform_private_aliases() -> io::Result<()> {
        let base = Path::new("/tmp").join(format!(
            "perfectpixel-capability-alias-{}",
            std::process::id()
        ));
        fs::create_dir_all(&base)?;
        let from = base.join("from.bin");
        let to = base.join("to.bin");
        let mut file = create_new(&from)?;
        file.write_all(b"alias-payload")?;
        sync_file_durable(&file)?;
        drop(file);
        rename(&from, &to)?;
        assert_eq!(read_bounded(&to, 32)?, b"alias-payload");
        fs::remove_dir_all(&base)?;
        Ok(())
    }
}
