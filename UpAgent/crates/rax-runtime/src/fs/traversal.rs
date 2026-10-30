use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
};

use cap_std::fs::{Dir, Metadata};
use rax_core::{ErrorCode, ErrorPhase, FsEntryKind, RaxError};

pub(super) const HARD_ENTRY_LIMIT: usize = 100_000;

use crate::error::{invalid_path, io_error, operation_error};

#[derive(Debug)]
pub(super) struct WalkEntry {
    pub(super) path: PathBuf,
    pub(super) kind: FsEntryKind,
    pub(super) size: u64,
}

#[derive(Debug, Clone, Copy, Default)]
pub(super) struct WalkSummary {
    pub(super) hard_limit_reached: bool,
}

#[derive(Debug)]
pub(super) struct WalkResult {
    pub(super) entries: Vec<WalkEntry>,
    pub(super) truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WalkControl {
    Continue,
    Stop,
}

pub(super) fn collect(
    dir: &Dir,
    root: &Path,
    recursive: bool,
    max_depth: usize,
    max_entries: usize,
    phase: ErrorPhase,
) -> Result<WalkResult, RaxError> {
    let mut entries = Vec::new();
    let summary = visit(
        dir,
        root,
        recursive,
        max_depth,
        max_entries,
        phase,
        |entry| {
            entries.push(entry);
            Ok(WalkControl::Continue)
        },
    )?;
    Ok(WalkResult {
        entries,
        truncated: summary.hard_limit_reached,
    })
}

pub(super) fn visit<F>(
    dir: &Dir,
    root: &Path,
    recursive: bool,
    max_depth: usize,
    max_entries: usize,
    phase: ErrorPhase,
    mut visitor: F,
) -> Result<WalkSummary, RaxError>
where
    F: FnMut(WalkEntry) -> Result<WalkControl, RaxError>,
{
    let metadata = dir
        .symlink_metadata(root)
        .map_err(|error| io_error(phase, "failed to inspect traversal root", Some(root), error))?;
    if !metadata.is_dir() {
        return Err(operation_error(
            ErrorCode::NotADirectory,
            phase,
            "traversal root must be a directory",
        )
        .with_detail("path", path_to_protocol(root)));
    }

    let mut queue = VecDeque::from([(root.to_path_buf(), 0_usize)]);
    let mut visited = 0_usize;
    while let Some((directory, depth)) = queue.pop_front() {
        let children = read_sorted_children(dir, &directory, phase)?;
        for entry in children {
            if visited == max_entries {
                return Ok(WalkSummary {
                    hard_limit_reached: true,
                });
            }
            visited = visited.checked_add(1).ok_or_else(|| {
                operation_error(
                    ErrorCode::LimitExceeded,
                    phase,
                    "filesystem traversal entry count overflowed",
                )
            })?;
            let should_recurse =
                recursive && entry.kind == FsEntryKind::Directory && depth < max_depth;
            let child_directory = should_recurse.then(|| entry.path.clone());
            if visitor(entry)? == WalkControl::Stop {
                return Ok(WalkSummary {
                    hard_limit_reached: false,
                });
            }
            if let Some(child_directory) = child_directory {
                queue.push_back((child_directory, depth + 1));
            }
        }
    }
    Ok(WalkSummary::default())
}

fn read_sorted_children(
    dir: &Dir,
    directory: &Path,
    phase: ErrorPhase,
) -> Result<Vec<WalkEntry>, RaxError> {
    let iterator = dir
        .read_dir(directory)
        .map_err(|error| io_error(phase, "failed to read directory", Some(directory), error))?;
    let mut children = Vec::new();
    for item in iterator {
        let entry = item.map_err(|error| {
            io_error(
                phase,
                "failed to read directory entry",
                Some(directory),
                error,
            )
        })?;
        let name = entry.file_name();
        let name_text = name.to_str().ok_or_else(|| {
            invalid_path(
                "non-UTF-8 filesystem names cannot be represented by the protocol",
                name.to_string_lossy().to_string(),
            )
        })?;
        let path = directory.join(name_text);
        let metadata = dir.symlink_metadata(&path).map_err(|error| {
            io_error(
                phase,
                "failed to inspect directory entry",
                Some(&path),
                error,
            )
        })?;
        children.push(WalkEntry {
            path,
            kind: entry_kind(&metadata),
            size: metadata.len(),
        });
    }
    children.sort_unstable_by(|left, right| left.path.cmp(&right.path));
    Ok(children)
}

pub(super) fn entry_kind(metadata: &Metadata) -> FsEntryKind {
    let file_type = metadata.file_type();
    if file_type.is_file() {
        FsEntryKind::File
    } else if file_type.is_dir() {
        FsEntryKind::Directory
    } else if file_type.is_symlink() {
        FsEntryKind::Symlink
    } else {
        FsEntryKind::Other
    }
}

pub(super) fn path_to_protocol(path: &Path) -> String {
    let mut output = String::new();
    for component in path.components() {
        let value = match component {
            std::path::Component::Normal(value) => value.to_string_lossy(),
            std::path::Component::CurDir => continue,
            _ => component.as_os_str().to_string_lossy(),
        };
        if !output.is_empty() {
            output.push('/');
        }
        output.push_str(&value);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use cap_std::ambient_authority;
    use tempfile::TempDir;

    #[test]
    fn visitor_stops_without_collecting_remaining_entries() {
        let root = TempDir::new().unwrap();
        std::fs::write(root.path().join("a"), b"a").unwrap();
        std::fs::write(root.path().join("b"), b"b").unwrap();
        let dir = Dir::open_ambient_dir(root.path(), ambient_authority()).unwrap();
        let mut visited = Vec::new();
        let summary = visit(
            &dir,
            Path::new("."),
            true,
            usize::MAX,
            100,
            ErrorPhase::Apply,
            |entry| {
                visited.push(path_to_protocol(&entry.path));
                Ok(WalkControl::Stop)
            },
        )
        .unwrap();
        assert_eq!(visited, vec!["a"]);
        assert!(!summary.hard_limit_reached);
    }

    #[test]
    fn exact_entry_limit_is_not_truncated_without_an_extra_entry() {
        let root = TempDir::new().unwrap();
        std::fs::write(root.path().join("a"), b"a").unwrap();
        let dir = Dir::open_ambient_dir(root.path(), ambient_authority()).unwrap();
        let result = collect(&dir, Path::new("."), true, usize::MAX, 1, ErrorPhase::Apply).unwrap();
        assert_eq!(result.entries.len(), 1);
        assert!(!result.truncated);
    }
}
