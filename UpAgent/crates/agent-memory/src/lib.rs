#![forbid(unsafe_code)]

use std::{
    cmp::Reverse,
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use agent_core::IdentifierError;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
mod private_fs;

use private_fs::{atomic_replace_private, open_private_file, prepare_private_root};

const MEMORY_SCHEMA_VERSION: u32 = 1;
const MAX_MEMORY_FILE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_MEMORY_ENTRIES: usize = 4_096;
const MAX_CONTENT_BYTES: usize = 1024 * 1024;
const MAX_RECALL_RESULTS: usize = 256;
const MAXIMUM_LOCK_WAIT: Duration = Duration::from_secs(30);
const MINIMUM_LOCK_BACKOFF: Duration = Duration::from_millis(2);
const MAXIMUM_LOCK_BACKOFF: Duration = Duration::from_millis(50);

/// `fs2` reports contention with the platform's "would block" error, which is
/// `WouldBlock` on Unix and a raw `ERROR_LOCK_VIOLATION` on Windows.
fn is_lock_contended(error: &std::io::Error) -> bool {
    const ERROR_LOCK_VIOLATION: i32 = 33;
    error.kind() == std::io::ErrorKind::WouldBlock
        || error.raw_os_error() == Some(ERROR_LOCK_VIOLATION)
}

#[derive(Debug, Error)]
pub enum MemoryError {
    #[error("memory I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("memory JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("memory identifier is invalid: {0}")]
    Identifier(#[from] IdentifierError),
    #[error("memory state is invalid: {0}")]
    Invalid(String),
    #[error("memory revision conflict: expected {expected}, actual {actual}")]
    RevisionConflict { expected: u64, actual: u64 },
    #[error("memory entry {0} was not found")]
    NotFound(String),
    #[error("memory scope lock was still held after {waited_ms} ms")]
    LockUnavailable { waited_ms: u64 },
    #[error("memory replacement completed but durability is uncertain: {0}")]
    CommitUncertain(#[source] std::io::Error),
}

impl MemoryError {
    #[must_use]
    pub fn may_have_committed(&self) -> bool {
        matches!(self, Self::CommitUncertain(_))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryKind {
    Fact,
    Preference,
    Decision,
    Goal,
    Routine,
    Relationship,
    Project,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryConsent {
    UserProvided,
    ExplicitlyApproved,
    InferredCandidate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum MemoryRetention {
    Project,
    Persistent,
    Until(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct MemoryEntry {
    pub id: String,
    pub kind: MemoryKind,
    pub content: String,
    pub provenance: String,
    pub confidence_basis_points: u16,
    pub consent: MemoryConsent,
    pub retention: MemoryRetention,
    pub created_at: String,
    pub last_verified_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<String>,
    #[serde(default)]
    pub forgotten: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct NewMemory {
    pub kind: MemoryKind,
    pub content: String,
    pub provenance: String,
    pub confidence_basis_points: u16,
    pub consent: MemoryConsent,
    pub retention: MemoryRetention,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<String>,
}

impl NewMemory {
    fn validate(&self) -> Result<(), MemoryError> {
        if self.content.trim().is_empty() || self.content.len() > MAX_CONTENT_BYTES {
            return Err(MemoryError::Invalid(format!(
                "memory content must be 1..={MAX_CONTENT_BYTES} bytes"
            )));
        }
        if self.provenance.trim().is_empty() || self.provenance.len() > 16 * 1024 {
            return Err(MemoryError::Invalid(
                "memory provenance must be 1..=16384 bytes".to_owned(),
            ));
        }
        if self.confidence_basis_points > 10_000 {
            return Err(MemoryError::Invalid(
                "memory confidence must be in 0..=10000 basis points".to_owned(),
            ));
        }
        if matches!(self.consent, MemoryConsent::InferredCandidate) {
            return Err(MemoryError::Invalid(
                "inferred candidates require explicit promotion before durable storage".to_owned(),
            ));
        }
        if let MemoryRetention::Until(value) = &self.retention {
            let deadline = OffsetDateTime::parse(value, &Rfc3339).map_err(|_| {
                MemoryError::Invalid(
                    "memory retention deadline must be an RFC 3339 timestamp".to_owned(),
                )
            })?;
            if deadline <= OffsetDateTime::now_utc() {
                return Err(MemoryError::Invalid(
                    "memory retention deadline must be in the future".to_owned(),
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct MemorySnapshot {
    pub schema_version: u32,
    pub scope: String,
    pub revision: u64,
    pub entries: BTreeMap<String, MemoryEntry>,
    pub index: BTreeMap<String, BTreeSet<String>>,
}

impl MemorySnapshot {
    fn empty(scope: &str) -> Self {
        Self {
            schema_version: MEMORY_SCHEMA_VERSION,
            scope: scope.to_owned(),
            revision: 0,
            entries: BTreeMap::new(),
            index: BTreeMap::new(),
        }
    }

    fn validate(&self, scope: &str) -> Result<(), MemoryError> {
        if self.schema_version != MEMORY_SCHEMA_VERSION || self.scope != scope {
            return Err(MemoryError::Invalid(
                "memory snapshot schema or scope is inconsistent".to_owned(),
            ));
        }
        if self.entries.len() > MAX_MEMORY_ENTRIES {
            return Err(MemoryError::Invalid(
                "memory snapshot exceeds its entry bound".to_owned(),
            ));
        }
        let mut expected_index = BTreeMap::<String, BTreeSet<String>>::new();
        for (id, entry) in &self.entries {
            if entry.id != *id
                || entry.content.trim().is_empty()
                || entry.content.len() > MAX_CONTENT_BYTES
                || entry.provenance.trim().is_empty()
                || entry.provenance.len() > 16 * 1024
                || entry.confidence_basis_points > 10_000
                || matches!(entry.consent, MemoryConsent::InferredCandidate)
            {
                return Err(MemoryError::Invalid(format!(
                    "memory entry {id} violates its durable contract"
                )));
            }
            OffsetDateTime::parse(&entry.created_at, &Rfc3339).map_err(|_| {
                MemoryError::Invalid(format!("memory entry {id} has an invalid createdAt"))
            })?;
            OffsetDateTime::parse(&entry.last_verified_at, &Rfc3339).map_err(|_| {
                MemoryError::Invalid(format!("memory entry {id} has an invalid lastVerifiedAt"))
            })?;
            if let MemoryRetention::Until(value) = &entry.retention {
                OffsetDateTime::parse(value, &Rfc3339).map_err(|_| {
                    MemoryError::Invalid(format!(
                        "memory entry {id} has an invalid retention deadline"
                    ))
                })?;
            }
            if let Some(previous) = entry.supersedes.as_deref()
                && (previous == id || !self.entries.contains_key(previous))
            {
                return Err(MemoryError::Invalid(format!(
                    "memory entry {id} has an invalid supersedes reference"
                )));
            }
            if !entry.forgotten {
                add_to_index(&mut expected_index, entry);
            }
        }
        if self.index != expected_index {
            return Err(MemoryError::Invalid(
                "memory index is inconsistent with durable entries".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RecallMatch {
    pub entry: MemoryEntry,
    pub term_matches: u32,
}

#[derive(Debug, Clone)]
pub struct MemoryStore {
    root: PathBuf,
    directory: Arc<File>,
    lock_wait: Duration,
}

impl MemoryStore {
    pub fn open(root: impl AsRef<Path>) -> Result<Self, MemoryError> {
        Self::open_with_lock_wait(root, MAXIMUM_LOCK_WAIT)
    }

    /// Open a store with an explicit bound on how long a scope lock may be
    /// contended before the call fails.
    pub fn open_with_lock_wait(
        root: impl AsRef<Path>,
        lock_wait: Duration,
    ) -> Result<Self, MemoryError> {
        if lock_wait.is_zero() || lock_wait > MAXIMUM_LOCK_WAIT {
            return Err(MemoryError::Invalid(format!(
                "memory lock wait must be within 1..={} ms",
                MAXIMUM_LOCK_WAIT.as_millis()
            )));
        }
        let root = root.as_ref().to_path_buf();
        let directory = prepare_private_root(&root)?;
        let canonical = root.canonicalize()?;
        Ok(Self {
            root: canonical,
            directory: Arc::new(directory),
            lock_wait,
        })
    }

    pub fn snapshot(&self, scope: &str) -> Result<MemorySnapshot, MemoryError> {
        validate_scope(scope)?;
        let _guard = self.lock_scope(scope)?;
        self.read_unlocked(scope)
    }

    pub fn append(
        &self,
        scope: &str,
        expected_revision: u64,
        memory: NewMemory,
    ) -> Result<MemorySnapshot, MemoryError> {
        validate_scope(scope)?;
        memory.validate()?;
        let _guard = self.lock_scope(scope)?;
        let mut snapshot = self.read_unlocked(scope)?;
        require_revision(&snapshot, expected_revision)?;
        if snapshot.entries.len() >= MAX_MEMORY_ENTRIES {
            return Err(MemoryError::Invalid(
                "memory store reached its bounded entry limit".to_owned(),
            ));
        }
        if let Some(superseded) = memory.supersedes.as_deref() {
            let previous = snapshot
                .entries
                .get_mut(superseded)
                .ok_or_else(|| MemoryError::NotFound(superseded.to_owned()))?;
            previous.forgotten = true;
            let previous = previous.clone();
            remove_from_index(&mut snapshot.index, &previous);
        }
        let now = now_rfc3339()?;
        let id = stable_memory_id(scope, snapshot.revision, &memory, &now);
        let entry = MemoryEntry {
            id: id.clone(),
            kind: memory.kind,
            content: memory.content,
            provenance: memory.provenance,
            confidence_basis_points: memory.confidence_basis_points,
            consent: memory.consent,
            retention: memory.retention,
            created_at: now.clone(),
            last_verified_at: now,
            supersedes: memory.supersedes,
            forgotten: false,
        };
        add_to_index(&mut snapshot.index, &entry);
        snapshot.entries.insert(id, entry);
        snapshot.revision = snapshot
            .revision
            .checked_add(1)
            .ok_or_else(|| MemoryError::Invalid("memory revision overflow".to_owned()))?;
        self.write_unlocked(scope, &snapshot)?;
        Ok(snapshot)
    }

    pub fn forget(
        &self,
        scope: &str,
        expected_revision: u64,
        id: &str,
    ) -> Result<MemorySnapshot, MemoryError> {
        validate_scope(scope)?;
        let _guard = self.lock_scope(scope)?;
        let mut snapshot = self.read_unlocked(scope)?;
        require_revision(&snapshot, expected_revision)?;
        let entry = snapshot
            .entries
            .remove(id)
            .ok_or_else(|| MemoryError::NotFound(id.to_owned()))?;
        remove_from_index(&mut snapshot.index, &entry);
        for remaining in snapshot.entries.values_mut() {
            if remaining.supersedes.as_deref() == Some(id) {
                remaining.supersedes = None;
            }
        }
        snapshot.revision = snapshot
            .revision
            .checked_add(1)
            .ok_or_else(|| MemoryError::Invalid("memory revision overflow".to_owned()))?;
        self.write_unlocked(scope, &snapshot)?;
        Ok(snapshot)
    }

    pub fn recall(
        &self,
        scope: &str,
        query: &str,
        maximum: usize,
    ) -> Result<Vec<RecallMatch>, MemoryError> {
        if maximum == 0 || maximum > MAX_RECALL_RESULTS {
            return Err(MemoryError::Invalid(format!(
                "recall maximum must be 1..={MAX_RECALL_RESULTS}"
            )));
        }
        let snapshot = self.snapshot(scope)?;
        let terms = tokenize(query);
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        let mut counts = BTreeMap::<String, u32>::new();
        for term in terms {
            if let Some(ids) = snapshot.index.get(&term) {
                for id in ids {
                    let count = counts.entry(id.clone()).or_default();
                    *count = count.saturating_add(1);
                }
            }
        }
        let now = OffsetDateTime::now_utc();
        let mut matches = counts
            .into_iter()
            .filter_map(|(id, term_matches)| {
                snapshot.entries.get(&id).and_then(|entry| {
                    is_recallable(entry, now).then(|| RecallMatch {
                        entry: entry.clone(),
                        term_matches,
                    })
                })
            })
            .collect::<Vec<_>>();
        matches.sort_by_key(|item| {
            (
                Reverse(item.term_matches),
                Reverse(item.entry.confidence_basis_points),
                Reverse(item.entry.last_verified_at.clone()),
                item.entry.id.clone(),
            )
        });
        matches.truncate(maximum);
        Ok(matches)
    }

    fn read_unlocked(&self, scope: &str) -> Result<MemorySnapshot, MemoryError> {
        let path = self.scope_path(scope);
        let file = match open_private_file(&self.directory, &path, false) {
            Ok(file) => file,
            Err(MemoryError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(MemorySnapshot::empty(scope));
            }
            Err(error) => return Err(error),
        };
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(MemoryError::Invalid(
                "memory snapshot must be a regular file".to_owned(),
            ));
        }
        if metadata.len() > MAX_MEMORY_FILE_BYTES {
            return Err(MemoryError::Invalid(
                "memory snapshot exceeds its byte bound".to_owned(),
            ));
        }
        let capacity = usize::try_from(metadata.len()).map_err(|_| {
            MemoryError::Invalid("memory snapshot length exceeds this platform".to_owned())
        })?;
        let mut bytes = Vec::with_capacity(capacity);
        file.take(MAX_MEMORY_FILE_BYTES + 1)
            .read_to_end(&mut bytes)?;
        let byte_length = u64::try_from(bytes.len())
            .map_err(|_| MemoryError::Invalid("memory snapshot length exceeds u64".to_owned()))?;
        if byte_length > MAX_MEMORY_FILE_BYTES {
            return Err(MemoryError::Invalid(
                "memory snapshot exceeds its byte bound".to_owned(),
            ));
        }
        let snapshot: MemorySnapshot = serde_json::from_slice(&bytes)?;
        snapshot.validate(scope)?;
        Ok(snapshot)
    }

    fn write_unlocked(&self, scope: &str, snapshot: &MemorySnapshot) -> Result<(), MemoryError> {
        snapshot.validate(scope)?;
        let bytes = serde_json::to_vec_pretty(snapshot)?;
        let byte_length = u64::try_from(bytes.len())
            .map_err(|_| MemoryError::Invalid("memory snapshot length exceeds u64".to_owned()))?;
        if byte_length > MAX_MEMORY_FILE_BYTES {
            return Err(MemoryError::Invalid(
                "memory snapshot exceeds its byte bound".to_owned(),
            ));
        }
        atomic_replace_private(&self.directory, &self.scope_path(scope), &bytes)
    }

    /// Acquire the per-scope lock without ever parking indefinitely.
    ///
    /// A blocking `flock` observes neither a deadline nor a cancellation
    /// signal, so a stuck peer would suspend the calling task past its own
    /// budget with no way to interrupt it. Polling `try_lock_exclusive` bounds
    /// the wait and turns starvation into a typed, retryable error.
    fn lock_scope(&self, scope: &str) -> Result<ScopeLock, MemoryError> {
        let path = self.root.join(format!("{}.lock", scope_digest(scope)));
        let file = open_private_file(&self.directory, &path, true)?;
        let deadline = Instant::now() + self.lock_wait;
        let mut backoff = MINIMUM_LOCK_BACKOFF;
        loop {
            match file.try_lock_exclusive() {
                Ok(()) => return Ok(ScopeLock { file }),
                Err(error) if is_lock_contended(&error) => {}
                Err(error) => return Err(error.into()),
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(MemoryError::LockUnavailable {
                    waited_ms: u64::try_from(self.lock_wait.as_millis()).unwrap_or(u64::MAX),
                });
            }
            std::thread::sleep(backoff.min(remaining));
            backoff = (backoff * 2).min(MAXIMUM_LOCK_BACKOFF);
        }
    }

    fn scope_path(&self, scope: &str) -> PathBuf {
        self.root.join(format!("{}.json", scope_digest(scope)))
    }
}

#[derive(Debug)]
struct ScopeLock {
    file: File,
}

impl Drop for ScopeLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.file);
    }
}

fn require_revision(snapshot: &MemorySnapshot, expected: u64) -> Result<(), MemoryError> {
    if snapshot.revision != expected {
        return Err(MemoryError::RevisionConflict {
            expected,
            actual: snapshot.revision,
        });
    }
    Ok(())
}

fn validate_scope(scope: &str) -> Result<(), MemoryError> {
    if scope.is_empty()
        || scope.len() > 512
        || scope.contains('\0')
        || scope.chars().any(char::is_control)
    {
        return Err(MemoryError::Invalid(
            "memory scope must be a bounded printable value".to_owned(),
        ));
    }
    Ok(())
}

fn stable_memory_id(scope: &str, revision: u64, memory: &NewMemory, now: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(scope.as_bytes());
    hasher.update(revision.to_le_bytes());
    hasher.update(memory.content.as_bytes());
    hasher.update(memory.provenance.as_bytes());
    hasher.update(now.as_bytes());
    format!("mem-{:x}", hasher.finalize())
}

fn scope_digest(scope: &str) -> String {
    format!("{:x}", Sha256::digest(scope.as_bytes()))
}

fn add_to_index(index: &mut BTreeMap<String, BTreeSet<String>>, entry: &MemoryEntry) {
    for term in tokenize(&entry.content) {
        index.entry(term).or_default().insert(entry.id.clone());
    }
}

fn remove_from_index(index: &mut BTreeMap<String, BTreeSet<String>>, entry: &MemoryEntry) {
    let mut empty = Vec::new();
    for term in tokenize(&entry.content) {
        if let Some(ids) = index.get_mut(&term) {
            ids.remove(&entry.id);
            if ids.is_empty() {
                empty.push(term);
            }
        }
    }
    for term in empty {
        index.remove(&term);
    }
}

fn tokenize(value: &str) -> BTreeSet<String> {
    value
        .split(|character: char| {
            !character.is_alphanumeric() && character != '_' && character != '-'
        })
        .filter_map(|term| {
            let term = term.trim().to_lowercase();
            (term.chars().count() >= 2 && term.len() <= 128).then_some(term)
        })
        .take(4_096)
        .collect()
}

fn is_recallable(entry: &MemoryEntry, now: OffsetDateTime) -> bool {
    if entry.forgotten {
        return false;
    }
    match &entry.retention {
        MemoryRetention::Project | MemoryRetention::Persistent => true,
        MemoryRetention::Until(value) => {
            OffsetDateTime::parse(value, &Rfc3339).is_ok_and(|deadline| deadline > now)
        }
    }
}

fn now_rfc3339() -> Result<String, MemoryError> {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|error| MemoryError::Invalid(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn memory(content: &str) -> NewMemory {
        NewMemory {
            kind: MemoryKind::Preference,
            content: content.to_owned(),
            provenance: "user-message:1".to_owned(),
            confidence_basis_points: 10_000,
            consent: MemoryConsent::UserProvided,
            retention: MemoryRetention::Persistent,
            supersedes: None,
        }
    }

    #[test]
    fn forget_physically_removes_the_entry() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let store = MemoryStore::open(temp.path())?;
        let snapshot = store.append("user:ax", 0, memory("Remove this memory"))?;
        let id = snapshot
            .entries
            .keys()
            .next()
            .ok_or_else(|| MemoryError::Invalid("expected one memory entry".to_owned()))?
            .clone();
        let updated = store.forget("user:ax", snapshot.revision, &id)?;
        assert!(!updated.entries.contains_key(&id));
        assert_eq!(
            store.recall("user:ax", "remove memory", 10)?,
            [] as [RecallMatch; 0]
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn memory_snapshot_symbolic_links_are_rejected() -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir()?;
        let store = MemoryStore::open(temp.path().join("memory"))?;
        let outside = temp.path().join("outside.json");
        fs::write(&outside, b"{}")?;
        symlink(&outside, store.scope_path("user:ax"))?;

        assert!(store.snapshot("user:ax").is_err());
        Ok(())
    }

    #[test]
    fn a_contended_scope_lock_fails_instead_of_parking_forever()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let store = MemoryStore::open_with_lock_wait(temp.path(), Duration::from_millis(200))?;

        // Hold the scope lock the store serializes on. A blocking `flock`
        // observes neither a deadline nor a cancellation signal, so it would
        // suspend the calling task indefinitely.
        let lock_path = temp
            .path()
            .canonicalize()?
            .join(format!("{}.lock", scope_digest("user:ax")));
        let held = open_private_file(&store.directory, &lock_path, true)?;
        FileExt::lock_exclusive(&held)?;

        let started = Instant::now();
        let error = store
            .append("user:ax", 0, memory("Blocked by a peer"))
            .expect_err("a contended scope must fail rather than hang");
        let elapsed = started.elapsed();

        assert!(matches!(error, MemoryError::LockUnavailable { .. }));
        assert!(
            elapsed >= Duration::from_millis(200),
            "returned after {elapsed:?} without contending for the lock"
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "parked for {elapsed:?} instead of honouring its bounded wait"
        );

        FileExt::unlock(&held)?;
        // The scope is usable again once the peer releases the lock.
        assert_eq!(store.append("user:ax", 0, memory("Now free"))?.revision, 1);
        Ok(())
    }

    #[test]
    fn revision_cas_and_indexed_recall_are_explicit() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let store = MemoryStore::open(temp.path())?;
        let snapshot = store.append("user:ax", 0, memory("Prefers simple Rust systems"))?;
        assert_eq!(snapshot.revision, 1);
        let conflict = store.append("user:ax", 0, memory("Uses Swift"));
        assert!(matches!(
            conflict,
            Err(MemoryError::RevisionConflict { .. })
        ));
        let recalled = store.recall("user:ax", "simple rust", 10)?;
        assert_eq!(recalled.len(), 1);
        assert_eq!(recalled[0].term_matches, 2);
        Ok(())
    }
}
