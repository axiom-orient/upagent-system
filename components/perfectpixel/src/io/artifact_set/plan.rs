use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use crate::PpResult;

use super::{
    file_error, AtomicArtifactSetEntry, MAX_ARTIFACT_BYTES, MAX_ARTIFACT_ENTRIES,
    MAX_RELATIVE_PATH_BYTES, MAX_RELATIVE_PATH_DEPTH,
};

#[derive(Clone, Debug)]
pub(super) struct Plan {
    pub(super) ordered_entry_indexes: Vec<usize>,
    pub(super) writes: Vec<PathBuf>,
    pub(super) removals: Vec<PathBuf>,
    pub(super) touched: Vec<PathBuf>,
}

pub(super) fn validate_plan(
    entries: &[AtomicArtifactSetEntry<'_>],
    removals: &[PathBuf],
) -> PpResult<Plan> {
    let count = entries
        .len()
        .checked_add(removals.len())
        .ok_or_else(|| file_error(Path::new("<artifact-set>"), "artifact count overflow"))?;
    if count > MAX_ARTIFACT_ENTRIES {
        return Err(file_error(
            Path::new("<artifact-set>"),
            "artifact set exceeds entry limit",
        ));
    }

    let mut writes = BTreeSet::new();
    let mut total = 0usize;
    for entry in entries {
        validate_relative_path(entry.relative_path)?;
        if !writes.insert(entry.relative_path.to_path_buf()) {
            return Err(file_error(entry.relative_path, "duplicate artifact entry"));
        }
        total = total
            .checked_add(entry.bytes.len())
            .ok_or_else(|| file_error(entry.relative_path, "artifact bytes overflow"))?;
        if total > MAX_ARTIFACT_BYTES {
            return Err(file_error(
                entry.relative_path,
                "artifact set exceeds byte limit",
            ));
        }
    }

    let mut removal_set = BTreeSet::new();
    for path in removals {
        validate_relative_path(path)?;
        if writes.contains(path) || !removal_set.insert(path.clone()) {
            return Err(file_error(
                path,
                "duplicate or overlapping artifact removal",
            ));
        }
    }

    let touched = writes.union(&removal_set).cloned().collect::<Vec<_>>();
    for path in &touched {
        if path
            .ancestors()
            .skip(1)
            .any(|ancestor| writes.contains(ancestor) || removal_set.contains(ancestor))
        {
            return Err(file_error(
                path,
                "managed artifact path collides with another artifact",
            ));
        }
    }

    let mut ordered_entry_indexes = (0..entries.len()).collect::<Vec<_>>();
    ordered_entry_indexes.sort_by(|left, right| {
        entries[*left]
            .relative_path
            .cmp(entries[*right].relative_path)
    });

    Ok(Plan {
        ordered_entry_indexes,
        writes: writes.into_iter().collect(),
        removals: removal_set.into_iter().collect(),
        touched,
    })
}

pub(crate) fn validate_relative_path(path: &Path) -> PpResult<()> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path.as_os_str().len() > MAX_RELATIVE_PATH_BYTES
    {
        return Err(file_error(
            path,
            "artifact path must be a bounded relative child path",
        ));
    }
    let mut depth = 0usize;
    for component in path.components() {
        if !matches!(component, Component::Normal(_)) {
            return Err(file_error(
                path,
                "artifact path must not contain '.', '..', root, or prefix components",
            ));
        }
        depth += 1;
        if depth > MAX_RELATIVE_PATH_DEPTH {
            return Err(file_error(path, "artifact path exceeds depth limit"));
        }
    }
    Ok(())
}
