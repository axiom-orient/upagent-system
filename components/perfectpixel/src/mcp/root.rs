use std::{
    fs,
    io::Read,
    path::{Component, Path, PathBuf},
};

pub(crate) const MAX_MCP_PATH_BYTES: usize = 1_024;

#[derive(Clone, Debug, PartialEq, Eq)]
struct FileRevision {
    device: u64,
    inode: u64,
    byte_count: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

fn file_revision(metadata: &fs::Metadata) -> FileRevision {
    use std::os::unix::fs::MetadataExt;

    FileRevision {
        device: metadata.dev(),
        inode: metadata.ino(),
        byte_count: metadata.len(),
        modified_seconds: metadata.mtime(),
        modified_nanoseconds: metadata.mtime_nsec(),
        changed_seconds: metadata.ctime(),
        changed_nanoseconds: metadata.ctime_nsec(),
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Root {
    canonical: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RootPathError {
    message: String,
}

impl RootPathError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub(crate) fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for RootPathError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for RootPathError {}

impl Root {
    pub(crate) fn new(path: PathBuf) -> Result<Self, RootPathError> {
        if !path.is_absolute() {
            return Err(RootPathError::new(
                "root must be an absolute existing directory",
            ));
        }
        let metadata = fs::symlink_metadata(&path)
            .map_err(|_| RootPathError::new("root must be an absolute existing directory"))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(RootPathError::new(
                "root must be an absolute existing directory",
            ));
        }
        let canonical = fs::canonicalize(&path)
            .map_err(|_| RootPathError::new("root must be an absolute existing directory"))?;
        Ok(Self { canonical })
    }

    pub(crate) fn canonical(&self) -> &Path {
        &self.canonical
    }

    pub(crate) fn input_file(&self, value: &str) -> Result<PathBuf, RootPathError> {
        let path = self.resolve_relative(value)?;
        self.check_existing_components(&path)?;
        let metadata = fs::symlink_metadata(&path)
            .map_err(|_| RootPathError::new(format!("input path does not exist: {value}")))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(RootPathError::new(format!(
                "input path must be a regular file: {value}"
            )));
        }
        self.ensure_canonical_inside(&path)?;
        Ok(path)
    }

    pub(crate) fn output_file(&self, value: &str) -> Result<PathBuf, RootPathError> {
        let path = self.resolve_relative(value)?;
        self.check_existing_components(&path)?;
        if let Ok(metadata) = fs::symlink_metadata(&path) {
            if metadata.file_type().is_symlink() {
                return Err(RootPathError::new(format!(
                    "output path must not contain a symlink: {value}"
                )));
            }
            self.ensure_canonical_inside(&path)?;
        } else if let Some(parent) = path.parent() {
            self.ensure_canonical_within(parent)?;
        }
        Ok(path)
    }

    pub(crate) fn output_dir(&self, value: &str) -> Result<PathBuf, RootPathError> {
        let path = self.resolve_relative(value)?;
        if path == self.canonical {
            return Err(RootPathError::new(
                "output directory must be a strict child of root",
            ));
        }
        self.check_existing_components(&path)?;
        if let Ok(metadata) = fs::symlink_metadata(&path) {
            if metadata.file_type().is_symlink() {
                return Err(RootPathError::new(format!(
                    "output directory must not be a symlink: {value}"
                )));
            }
            self.ensure_canonical_inside(&path)?;
        } else if let Some(parent) = path.parent() {
            self.ensure_canonical_within(parent)?;
        }
        Ok(path)
    }

    pub(crate) fn nested_input(
        &self,
        request_path: &Path,
        value: &str,
    ) -> Result<PathBuf, RootPathError> {
        let nested = validate_relative(value)?;
        let parent = request_path
            .parent()
            .ok_or_else(|| RootPathError::new("request path has no parent directory"))?;
        let path = parent.join(nested);
        self.check_existing_components(&path)?;
        let metadata = fs::symlink_metadata(&path).map_err(|_| {
            RootPathError::new(format!("nested input path does not exist: {value}"))
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(RootPathError::new(format!(
                "nested input path must be a regular file: {value}"
            )));
        }
        self.ensure_canonical_inside(&path)?;
        Ok(path)
    }

    pub(crate) fn read_bounded_regular_file(
        &self,
        path: &Path,
        limit: usize,
    ) -> Result<Vec<u8>, RootPathError> {
        self.revalidate_input_file(path)?;
        let canonical_before = fs::canonicalize(path).map_err(|error| {
            RootPathError::new(format!("cannot resolve request file before open: {error}"))
        })?;
        if !canonical_before.starts_with(&self.canonical) || canonical_before == self.canonical {
            return Err(RootPathError::new(
                "request file resolves outside the configured root",
            ));
        }
        let path_before = fs::symlink_metadata(path).map_err(|error| {
            RootPathError::new(format!("cannot inspect request file before open: {error}"))
        })?;
        if path_before.file_type().is_symlink() || !path_before.is_file() {
            return Err(RootPathError::new(
                "request preparse input must be a regular non-symlink file",
            ));
        }
        let revision = file_revision(&path_before);

        let mut file = fs::File::open(path).map_err(|error| {
            RootPathError::new(format!("cannot open request file for preparse: {error}"))
        })?;
        self.verify_open_file(path, &file, &canonical_before, &revision)?;

        let read_limit = u64::try_from(limit)
            .ok()
            .and_then(|value| value.checked_add(1))
            .ok_or_else(|| RootPathError::new("request preparse read limit overflow"))?;
        let mut bytes = Vec::new();
        file.by_ref()
            .take(read_limit)
            .read_to_end(&mut bytes)
            .map_err(|error| {
                RootPathError::new(format!("cannot read request file for preparse: {error}"))
            })?;

        self.verify_open_file(path, &file, &canonical_before, &revision)?;
        if bytes.len() > limit {
            return Err(RootPathError::new(format!(
                "request file exceeds the {limit}-byte preparse limit"
            )));
        }
        if u64::try_from(bytes.len()).ok() != Some(revision.byte_count) {
            return Err(RootPathError::new(
                "request file changed while its bounded bytes were read",
            ));
        }
        Ok(bytes)
    }

    pub(crate) fn revalidate_input_file(&self, path: &Path) -> Result<(), RootPathError> {
        self.check_existing_components(path)?;
        let metadata = fs::symlink_metadata(path).map_err(|_| {
            RootPathError::new(format!("input path no longer exists: {}", path.display()))
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(RootPathError::new(format!(
                "input path must remain a regular non-symlink file: {}",
                path.display()
            )));
        }
        self.ensure_canonical_inside(path)
    }

    pub(crate) fn revalidate_output_file(&self, path: &Path) -> Result<(), RootPathError> {
        self.check_existing_components(path)?;
        if let Ok(metadata) = fs::symlink_metadata(path) {
            if metadata.file_type().is_symlink() {
                return Err(RootPathError::new(format!(
                    "output path must not contain a symlink: {}",
                    path.display()
                )));
            }
            self.ensure_canonical_inside(path)?;
        } else if let Some(parent) = path.parent() {
            self.ensure_canonical_within(parent)?;
        }
        Ok(())
    }

    pub(crate) fn revalidate_output_dir(&self, path: &Path) -> Result<(), RootPathError> {
        if path == self.canonical {
            return Err(RootPathError::new(
                "output directory must be a strict child of root",
            ));
        }
        self.check_existing_components(path)?;
        if let Ok(metadata) = fs::symlink_metadata(path) {
            if metadata.file_type().is_symlink() {
                return Err(RootPathError::new(format!(
                    "output directory must not be a symlink: {}",
                    path.display()
                )));
            }
            self.ensure_canonical_inside(path)?;
        } else if let Some(parent) = path.parent() {
            self.ensure_canonical_within(parent)?;
        }
        Ok(())
    }

    fn resolve_relative(&self, value: &str) -> Result<PathBuf, RootPathError> {
        let relative = validate_relative(value)?;
        let path = self.canonical.join(relative);
        self.check_existing_components(&path)?;
        Ok(path)
    }

    fn check_existing_components(&self, path: &Path) -> Result<(), RootPathError> {
        let relative = path
            .strip_prefix(&self.canonical)
            .map_err(|_| RootPathError::new("path resolves outside the configured root"))?;
        let mut current = self.canonical.clone();
        for component in relative.components() {
            let Component::Normal(value) = component else {
                return Err(RootPathError::new(
                    "path must be root-relative without '.', '..', or absolute components",
                ));
            };
            current.push(value);
            match fs::symlink_metadata(&current) {
                Ok(metadata) => {
                    if metadata.file_type().is_symlink() {
                        return Err(RootPathError::new(format!(
                            "path contains a symlink component: {}",
                            current.display()
                        )));
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
                Err(_) => {
                    return Err(RootPathError::new(format!(
                        "cannot inspect path component: {}",
                        current.display()
                    )));
                }
            }
        }
        Ok(())
    }

    fn ensure_canonical_inside(&self, path: &Path) -> Result<(), RootPathError> {
        self.ensure_canonical_within(path)?;
        if fs::canonicalize(path)
            .map_err(|_| RootPathError::new(format!("cannot resolve path: {}", path.display())))?
            == self.canonical
        {
            return Err(RootPathError::new("path resolves to the configured root"));
        }
        Ok(())
    }

    fn ensure_canonical_within(&self, path: &Path) -> Result<(), RootPathError> {
        let canonical = fs::canonicalize(path)
            .map_err(|_| RootPathError::new(format!("cannot resolve path: {}", path.display())))?;
        if !canonical.starts_with(&self.canonical) {
            return Err(RootPathError::new(
                "path resolves outside the configured root",
            ));
        }
        Ok(())
    }

    fn verify_open_file(
        &self,
        path: &Path,
        file: &fs::File,
        expected_canonical: &Path,
        expected_revision: &FileRevision,
    ) -> Result<(), RootPathError> {
        self.check_existing_components(path)?;
        let path_metadata = fs::symlink_metadata(path).map_err(|error| {
            RootPathError::new(format!("cannot inspect request file after open: {error}"))
        })?;
        let handle_metadata = file.metadata().map_err(|error| {
            RootPathError::new(format!("cannot inspect open request file: {error}"))
        })?;
        let canonical = fs::canonicalize(path).map_err(|error| {
            RootPathError::new(format!("cannot resolve request file after open: {error}"))
        })?;
        if path_metadata.file_type().is_symlink()
            || !path_metadata.is_file()
            || !handle_metadata.is_file()
            || canonical != expected_canonical
            || !canonical.starts_with(&self.canonical)
            || canonical == self.canonical
            || file_revision(&path_metadata) != *expected_revision
            || file_revision(&handle_metadata) != *expected_revision
        {
            return Err(RootPathError::new(
                "request file changed while its bounded bytes were captured",
            ));
        }
        Ok(())
    }
}

fn validate_relative(value: &str) -> Result<PathBuf, RootPathError> {
    if value.is_empty() {
        return Err(RootPathError::new("path must not be empty"));
    }
    if value.len() > MAX_MCP_PATH_BYTES {
        return Err(RootPathError::new(format!(
            "path exceeds the {MAX_MCP_PATH_BYTES}-byte limit"
        )));
    }
    if value.as_bytes().contains(&0) {
        return Err(RootPathError::new("path must not contain NUL"));
    }
    if value.contains('\\') {
        return Err(RootPathError::new("path must use UTF-8 slash separators"));
    }
    if value.starts_with('/')
        || value
            .split('/')
            .next()
            .is_some_and(|part| part.contains(':'))
    {
        return Err(RootPathError::new("path must be root-relative"));
    }
    let path = Path::new(value);
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => normalized.push(value),
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => {
                return Err(RootPathError::new(
                    "path must be root-relative without '.', '..', or absolute components",
                ));
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        return Err(RootPathError::new("path must not be empty"));
    }
    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root(label: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "perfectpixel-mcp-root-{label}-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create root");
        path
    }

    #[test]
    fn relative_path_contract_rejects_unsafe_forms() {
        for value in [
            "",
            ".",
            "..",
            "../input.png",
            "/tmp/input.png",
            r"nested\input.png",
            "C:/input.png",
            "input\0.png",
        ] {
            assert!(validate_relative(value).is_err(), "{value:?}");
        }
        assert!(validate_relative(&"a".repeat(MAX_MCP_PATH_BYTES + 1)).is_err());
    }

    #[test]
    fn output_targets_must_be_strict_children_but_direct_files_are_allowed() {
        let root_path = temp_root("output-targets");
        let root = Root::new(root_path.clone()).expect("root");
        assert!(root.output_file("output.png").is_ok());
        assert!(root.output_dir("generated").is_ok());
        assert!(root.output_dir(".").is_err());
        let _ = fs::remove_dir_all(root_path);
    }

    #[test]
    fn input_must_be_a_regular_file_without_symlink_components() {
        let root_path = temp_root("input");
        let input = root_path.join("input.png");
        fs::write(&input, b"not-an-image").expect("input");
        let root = Root::new(root_path.clone()).expect("root");
        assert_eq!(
            root.input_file("input.png").expect("regular input"),
            root.canonical().join("input.png")
        );

        {
            use std::os::unix::fs::symlink;
            let outside = temp_root("outside");
            let outside_file = outside.join("outside.png");
            fs::write(&outside_file, b"outside").expect("outside input");
            symlink(&outside_file, root_path.join("escape.png")).expect("symlink");
            assert!(root.input_file("escape.png").is_err());
            let _ = fs::remove_dir_all(outside);
        }

        let _ = fs::remove_dir_all(root_path);
    }

    #[test]
    fn immediate_revalidation_rejects_a_retargeted_input_path() {
        use std::os::unix::fs::symlink;

        let root_path = temp_root("revalidate-retarget");
        let input = root_path.join("input.png");
        fs::write(&input, b"original").expect("input");
        let root = Root::new(root_path.clone()).expect("root");
        let prepared = root.input_file("input.png").expect("initial input");

        let outside = temp_root("revalidate-retarget-outside");
        let outside_file = outside.join("outside.png");
        fs::write(&outside_file, b"outside").expect("outside input");
        fs::remove_file(&input).expect("remove original input");
        symlink(&outside_file, &input).expect("retarget input");

        assert!(root.revalidate_input_file(&prepared).is_err());
        let _ = fs::remove_dir_all(root_path);
        let _ = fs::remove_dir_all(outside);
    }
}
