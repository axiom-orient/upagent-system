use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};

use cap_std::{ambient_authority, fs::Dir};
use rax_core::{
    ErrorCode, ErrorPhase, RaxError, WorkspaceBinding, operation::validate_relative_path,
};
use sha2::{Digest, Sha256};

use crate::error::{invalid_path, io_error};

#[derive(Debug)]
pub struct Workspace {
    root: PathBuf,
    display_path: String,
    identity: String,
    dir: Dir,
}

impl Workspace {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, RaxError> {
        let requested = path.as_ref();
        let root = std::fs::canonicalize(requested).map_err(|error| {
            io_error(
                ErrorPhase::Validate,
                "failed to resolve workspace",
                Some(requested),
                error,
            )
        })?;
        let metadata = std::fs::metadata(&root).map_err(|error| {
            io_error(
                ErrorPhase::Validate,
                "failed to inspect workspace",
                Some(&root),
                error,
            )
        })?;
        if !metadata.is_dir() {
            return Err(RaxError::new(
                ErrorCode::NotADirectory,
                ErrorPhase::Validate,
                false,
                "workspace must be an existing directory",
            )
            .with_detail("path", root.to_string_lossy().to_string()));
        }

        let display_path = root
            .to_str()
            .ok_or_else(|| {
                invalid_path(
                    "workspace path must be UTF-8 for the JSON protocol",
                    root.to_string_lossy().to_string(),
                )
            })?
            .to_string();
        let identity = workspace_identity(&root, &metadata);
        let dir = Dir::open_ambient_dir(&root, ambient_authority()).map_err(|error| {
            io_error(
                ErrorPhase::Validate,
                "failed to open workspace capability",
                Some(&root),
                error,
            )
        })?;

        Ok(Self {
            root,
            display_path,
            identity,
            dir,
        })
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub fn identity(&self) -> &str {
        &self.identity
    }

    #[must_use]
    pub fn binding(&self) -> WorkspaceBinding {
        WorkspaceBinding {
            display_path: self.display_path.clone(),
            identity: self.identity.clone(),
        }
    }

    pub fn dir(&self) -> Result<Dir, RaxError> {
        self.dir.try_clone().map_err(|error| {
            io_error(
                ErrorPhase::Apply,
                "failed to clone workspace capability",
                Some(&self.root),
                error,
            )
        })
    }

    pub fn relative(&self, path: &str, allow_empty: bool) -> Result<PathBuf, RaxError> {
        validate_relative_path(path, allow_empty)?;
        Ok(if path.is_empty() {
            PathBuf::from(".")
        } else {
            PathBuf::from(path)
        })
    }

    pub fn absolute_existing_dir(&self, path: &str) -> Result<PathBuf, RaxError> {
        let relative = self.relative(path, true)?;
        self.ensure_no_symlink_components(path, false, ErrorPhase::Validate)?;
        let absolute = std::fs::canonicalize(self.root.join(&relative)).map_err(|error| {
            io_error(
                ErrorPhase::Validate,
                "failed to resolve workspace directory",
                Some(&self.root.join(&relative)),
                error,
            )
        })?;
        if !absolute.starts_with(&self.root) {
            return Err(RaxError::new(
                ErrorCode::PathOutsideWorkspace,
                ErrorPhase::Validate,
                false,
                "resolved directory is outside the workspace",
            )
            .with_detail("path", path.to_string()));
        }
        let metadata = std::fs::metadata(&absolute).map_err(|error| {
            io_error(
                ErrorPhase::Validate,
                "failed to inspect workspace directory",
                Some(&absolute),
                error,
            )
        })?;
        if !metadata.is_dir() {
            return Err(RaxError::new(
                ErrorCode::NotADirectory,
                ErrorPhase::Validate,
                false,
                "path must resolve to a directory",
            )
            .with_detail("path", path.to_string()));
        }
        Ok(absolute)
    }

    pub fn ensure_no_symlink_components(
        &self,
        path: &str,
        allow_leaf_symlink: bool,
        phase: ErrorPhase,
    ) -> Result<(), RaxError> {
        let relative = self.relative(path, true)?;
        if relative == Path::new(".") {
            return Ok(());
        }

        let mut current = PathBuf::new();
        let components: Vec<_> = relative.components().collect();
        for (index, component) in components.iter().enumerate() {
            current.push(component.as_os_str());
            match self.dir.symlink_metadata(&current) {
                Ok(metadata) => {
                    let is_leaf = index + 1 == components.len();
                    if metadata.file_type().is_symlink() && !(is_leaf && allow_leaf_symlink) {
                        return Err(RaxError::new(
                            ErrorCode::SymlinkNotAllowed,
                            phase,
                            false,
                            "symbolic links are not allowed in this path",
                        )
                        .with_detail("path", display_relative(&current)?));
                    }
                    if !is_leaf && !metadata.is_dir() {
                        return Err(RaxError::new(
                            ErrorCode::NotADirectory,
                            phase,
                            false,
                            "a non-directory component appears before the path leaf",
                        )
                        .with_detail("path", display_relative(&current)?));
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
                Err(error) => {
                    return Err(io_error(
                        phase,
                        "failed to inspect path component",
                        Some(&current),
                        error,
                    ));
                }
            }
        }
        Ok(())
    }
}

fn display_relative(path: &Path) -> Result<String, RaxError> {
    path.to_str().map(ToOwned::to_owned).ok_or_else(|| {
        invalid_path(
            "non-UTF-8 filesystem names cannot be represented by the protocol",
            path.to_string_lossy().to_string(),
        )
    })
}

fn workspace_identity(root: &Path, metadata: &std::fs::Metadata) -> String {
    let mut hasher = Sha256::new();
    hash_os_str(&mut hasher, root.as_os_str());

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        hasher.update(metadata.dev().to_le_bytes());
        hasher.update(metadata.ino().to_le_bytes());
    }

    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        hasher.update(metadata.file_attributes().to_le_bytes());
    }

    format!("{:x}", hasher.finalize())
}

fn hash_os_str(hasher: &mut Sha256, value: &OsStr) {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        hasher.update(value.as_bytes());
    }

    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        for unit in value.encode_wide() {
            hasher.update(unit.to_le_bytes());
        }
    }

    #[cfg(not(any(unix, windows)))]
    hasher.update(value.to_string_lossy().as_bytes());
}
