use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{PpError, PpResult};

use super::capability;

mod plan;

pub(crate) use plan::validate_relative_path;
use plan::{validate_plan, Plan};

const JOURNAL_SCHEMA: &str = "perfectpixel.artifact-set-transaction/2";
const PREPARED_MARKER: &str = "prepared.json";
const BACKUPS_READY_MARKER: &str = "backups-ready.json";
const INSTALLED_MARKER: &str = "installed.json";
const ABORTED_MARKER: &str = "aborted.json";
const MAX_TEMP_ATTEMPTS: usize = 64;
const MAX_ARTIFACT_ENTRIES: usize = 4_096;
const MAX_ARTIFACT_BYTES: usize = 512 * 1024 * 1024;
const MAX_RELATIVE_PATH_BYTES: usize = 1_024;
const MAX_RELATIVE_PATH_DEPTH: usize = 16;
const MAX_JOURNAL_BYTES: usize = 1024 * 1024;

pub struct AtomicArtifactSetEntry<'a> {
    pub relative_path: &'a Path,
    pub bytes: &'a [u8],
}

#[derive(Debug)]
pub struct AtomicArtifactSetOwnedEntry {
    pub relative_path: PathBuf,
    pub bytes: Vec<u8>,
}

#[derive(Debug)]
pub struct AtomicArtifactSetOwnedPlan {
    pub entries: Vec<AtomicArtifactSetOwnedEntry>,
    pub removals: Vec<PathBuf>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArtifactSetConditionPhase {
    BeforeMutation,
    BeforeCommit,
}

/// Canonical multi-artifact publication authority.
///
/// `validate -> durable stage -> durable backup -> precondition -> install ->
/// verify -> durable terminal marker -> cleanup`
///
/// Every mutation is descriptor-relative and no-follow. The `/2` journal schema,
/// lock name, and transaction names intentionally remain compatible with the
/// previous publisher so stale on-disk recovery state survives this clean break.
pub struct AtomicArtifactSetWriter;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TransactionState {
    Prepared,
    BackupsReady,
    Installed,
    Aborted,
    Restored,
    RecoveryRequired,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TransactionEvent {
    BackupsRecorded,
    InstallRecorded,
    PublicationAborted,
    RollbackRestored,
    RecoveryRequired,
}

fn reduce_transaction(
    state: TransactionState,
    event: TransactionEvent,
) -> Result<TransactionState, &'static str> {
    match (state, event) {
        (TransactionState::Prepared, TransactionEvent::BackupsRecorded) => {
            Ok(TransactionState::BackupsReady)
        }
        (TransactionState::BackupsReady, TransactionEvent::InstallRecorded) => {
            Ok(TransactionState::Installed)
        }
        (
            TransactionState::Prepared | TransactionState::BackupsReady,
            TransactionEvent::PublicationAborted,
        ) => Ok(TransactionState::Aborted),
        (TransactionState::BackupsReady, TransactionEvent::RollbackRestored) => {
            Ok(TransactionState::Restored)
        }
        (_, TransactionEvent::RecoveryRequired) => Ok(TransactionState::RecoveryRequired),
        _ => Err("event is not valid for the current artifact transaction state"),
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Journal {
    schema: String,
    root_name: String,
    root_existed: bool,
    writes: Vec<String>,
    removals: Vec<String>,
    backups: Vec<BackupRecord>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BackupRecord {
    path: String,
    byte_count: u64,
    sha256: String,
}

#[derive(Clone, Debug)]
struct RecoveryPlan {
    root_existed: bool,
    writes: BTreeSet<PathBuf>,
    backups: BTreeMap<PathBuf, BackupRecord>,
    touched: Vec<PathBuf>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TerminalMarkerOutcome {
    Durable,
    VisibleDurabilityUnconfirmed,
}

struct PublicationLock {
    _file: File,
}

struct Transaction {
    state: TransactionState,
    root: PathBuf,
    transaction_dir: PathBuf,
    stage_dir: PathBuf,
    backup_dir: PathBuf,
    plan: Plan,
    journal: Journal,
}

impl AtomicArtifactSetWriter {
    pub fn publish(
        root: impl AsRef<Path>,
        entries: &[AtomicArtifactSetEntry<'_>],
        removals: &[PathBuf],
    ) -> PpResult<()> {
        let root = prepare_root(root.as_ref())?;
        let _lock = acquire_lock(&root)?;
        recover_stale_transactions(&root)?;
        let mut condition = |_root: &Path, _phase: ArtifactSetConditionPhase, _context: &()| Ok(());
        publish_locked(&root, entries, removals, &(), &mut condition, false)
    }

    pub fn publish_with_planner<F>(root: impl AsRef<Path>, planner: F) -> PpResult<()>
    where
        F: FnOnce(&Path) -> PpResult<AtomicArtifactSetOwnedPlan>,
    {
        Self::publish_with_planner_checked(
            root,
            |locked_root| planner(locked_root).map(|plan| (plan, ())),
            |_root, _phase, _context| Ok(()),
        )
    }

    pub fn publish_with_planner_checked<F, V, C>(
        root: impl AsRef<Path>,
        planner: F,
        condition: V,
    ) -> PpResult<()>
    where
        F: FnOnce(&Path) -> PpResult<(AtomicArtifactSetOwnedPlan, C)>,
        V: FnMut(&Path, ArtifactSetConditionPhase, &C) -> PpResult<()>,
    {
        publish_planned(root.as_ref(), planner, condition, false)
    }

    pub(super) fn publish_with_planner_checked_exact<F, V, C>(
        root: impl AsRef<Path>,
        planner: F,
        condition: V,
        ensure_empty_root: bool,
    ) -> PpResult<()>
    where
        F: FnOnce(&Path) -> PpResult<(AtomicArtifactSetOwnedPlan, C)>,
        V: FnMut(&Path, ArtifactSetConditionPhase, &C) -> PpResult<()>,
    {
        publish_planned(root.as_ref(), planner, condition, ensure_empty_root)
    }
}

fn publish_planned<F, V, C>(
    requested_root: &Path,
    planner: F,
    mut condition: V,
    ensure_empty_root: bool,
) -> PpResult<()>
where
    F: FnOnce(&Path) -> PpResult<(AtomicArtifactSetOwnedPlan, C)>,
    V: FnMut(&Path, ArtifactSetConditionPhase, &C) -> PpResult<()>,
{
    let root = prepare_root(requested_root)?;
    let _lock = acquire_lock(&root)?;
    recover_stale_transactions(&root)?;
    preflight_root(&root)?;

    let (owned, context) = planner(&root)?;
    let entries = owned
        .entries
        .iter()
        .map(|entry| AtomicArtifactSetEntry {
            relative_path: entry.relative_path.as_path(),
            bytes: entry.bytes.as_slice(),
        })
        .collect::<Vec<_>>();

    if entries.is_empty() && owned.removals.is_empty() {
        condition(&root, ArtifactSetConditionPhase::BeforeMutation, &context)?;
        let created = if ensure_empty_root {
            ensure_root_exists(&root)?
        } else {
            false
        };
        if let Err(primary) = condition(&root, ArtifactSetConditionPhase::BeforeCommit, &context) {
            if created {
                rollback_empty_root_creation(&root).map_err(|rollback| {
                    transaction_error(
                        &root,
                        TransactionState::RecoveryRequired,
                        format!("{primary}; failed to roll back empty root: {rollback}"),
                    )
                })?;
            }
            return Err(primary);
        }
        return Ok(());
    }

    publish_locked(
        &root,
        &entries,
        &owned.removals,
        &context,
        &mut condition,
        ensure_empty_root,
    )
}

fn publish_locked<V, C>(
    root: &Path,
    entries: &[AtomicArtifactSetEntry<'_>],
    removals: &[PathBuf],
    context: &C,
    condition: &mut V,
    ensure_empty_root: bool,
) -> PpResult<()>
where
    V: FnMut(&Path, ArtifactSetConditionPhase, &C) -> PpResult<()>,
{
    let plan = validate_plan(entries, removals)?;
    preflight_targets(root, &plan.touched)?;
    if plan.touched.is_empty() {
        if ensure_empty_root {
            ensure_root_exists(root)?;
        }
        return Ok(());
    }

    let root_existed = existing_directory(root)?;
    let transaction_dir = create_transaction_dir(root)?;
    let mut transaction = match Transaction::prepare(
        root.to_path_buf(),
        transaction_dir.clone(),
        root_existed,
        plan,
    ) {
        Ok(transaction) => transaction,
        Err(primary) => {
            return match cleanup_transaction(&transaction_dir) {
                Ok(()) => Err(primary),
                Err(cleanup) => Err(transaction_error(
                    root,
                    TransactionState::RecoveryRequired,
                    format!("{primary}; failed to durably clean incomplete transaction: {cleanup}"),
                )),
            }
        }
    };

    if let Err(error) = transaction.stage(entries) {
        return Err(transaction.abort_without_mutation(error));
    }
    if let Err(error) = transaction.snapshot_existing() {
        return Err(transaction.abort_without_mutation(error));
    }
    if let Err(error) = condition(root, ArtifactSetConditionPhase::BeforeMutation, context) {
        return Err(transaction.abort_before_install(error));
    }
    if let Err(error) = transaction.verify_snapshot_unchanged() {
        return Err(transaction.abort_before_install(error));
    }
    if let Err(error) = transaction.install(entries) {
        return Err(transaction.rollback(error));
    }
    if let Err(error) = condition(root, ArtifactSetConditionPhase::BeforeCommit, context) {
        return Err(transaction.rollback(error));
    }
    if let Err(error) = transaction.verify_installed(entries) {
        return Err(transaction.rollback(error));
    }

    match transaction.record_install() {
        Ok(TerminalMarkerOutcome::Durable) => transaction.commit(),
        Ok(TerminalMarkerOutcome::VisibleDurabilityUnconfirmed) => {
            Err(transaction.retain_visible_commit())
        }
        Err(error) => Err(transaction.rollback(error)),
    }
}

impl Transaction {
    fn prepare(
        root: PathBuf,
        transaction_dir: PathBuf,
        root_existed: bool,
        plan: Plan,
    ) -> PpResult<Self> {
        let stage_dir = transaction_dir.join("stage");
        let backup_dir = transaction_dir.join("backup");
        capability::create_directory_new(&stage_dir)
            .map_err(|source| file_error(&stage_dir, source))?;
        capability::create_directory_new(&backup_dir)
            .map_err(|source| file_error(&backup_dir, source))?;
        let journal = Journal {
            schema: JOURNAL_SCHEMA.to_string(),
            root_name: root_file_name(&root)?,
            root_existed,
            writes: plan
                .writes
                .iter()
                .map(|path| journal_path(path))
                .collect::<PpResult<_>>()?,
            removals: plan
                .removals
                .iter()
                .map(|path| journal_path(path))
                .collect::<PpResult<_>>()?,
            backups: Vec::new(),
        };
        write_marker(&transaction_dir, PREPARED_MARKER, &journal)?;
        sync_directory(transaction_dir.parent().expect("transaction parent"))?;
        Ok(Self {
            state: TransactionState::Prepared,
            root,
            transaction_dir,
            stage_dir,
            backup_dir,
            plan,
            journal,
        })
    }

    fn stage(&self, entries: &[AtomicArtifactSetEntry<'_>]) -> PpResult<()> {
        for index in &self.plan.ordered_entry_indexes {
            let entry = &entries[*index];
            write_new_file(&self.stage_dir.join(entry.relative_path), entry.bytes)?;
        }
        sync_touched_parents(&self.stage_dir, &self.plan.writes)
    }

    fn snapshot_existing(&mut self) -> PpResult<()> {
        debug_assert_eq!(self.state, TransactionState::Prepared);
        reject_blocked_managed_parents(&self.root, &self.plan.touched)?;
        let mut backups = Vec::new();
        let mut total = 0usize;
        for relative in &self.plan.touched {
            let target = self.root.join(relative);
            let Some(bytes) = read_optional_regular(&target, MAX_ARTIFACT_BYTES - total)? else {
                continue;
            };
            total = total
                .checked_add(bytes.len())
                .ok_or_else(|| file_error(&target, "backup byte count overflow"))?;
            if total > MAX_ARTIFACT_BYTES {
                return Err(file_error(
                    &target,
                    "rollback backup set exceeds byte limit",
                ));
            }
            let backup = self.backup_dir.join(relative);
            write_new_file(&backup, &bytes)?;
            backups.push(BackupRecord {
                path: journal_path(relative)?,
                byte_count: u64::try_from(bytes.len())
                    .map_err(|_| file_error(&target, "backup byte count overflow"))?,
                sha256: crate::sha256_hex(&bytes),
            });
        }
        backups.sort_by(|left, right| left.path.cmp(&right.path));
        sync_touched_parents(&self.backup_dir, &self.plan.touched)?;
        self.journal.backups = backups;
        write_marker(&self.transaction_dir, BACKUPS_READY_MARKER, &self.journal)?;
        self.transition(TransactionEvent::BackupsRecorded)
    }

    fn verify_snapshot_unchanged(&self) -> PpResult<()> {
        debug_assert_eq!(self.state, TransactionState::BackupsReady);
        reject_blocked_managed_parents(&self.root, &self.plan.touched)?;
        let recovery = self.journal.validate(&self.root)?;
        verify_backup_set(&self.transaction_dir, &recovery)?;
        for relative in &self.plan.touched {
            let target = self.root.join(relative);
            match recovery.backups.get(relative) {
                Some(record) => {
                    verify_file_record(&target, record, "managed artifact changed before mutation")?
                }
                None => verify_absent(&target, "managed artifact appeared before mutation")?,
            }
        }
        Ok(())
    }

    fn install(&self, entries: &[AtomicArtifactSetEntry<'_>]) -> PpResult<()> {
        debug_assert_eq!(self.state, TransactionState::BackupsReady);
        capability::create_dir_all(&self.root).map_err(|source| file_error(&self.root, source))?;

        for index in &self.plan.ordered_entry_indexes {
            let entry = &entries[*index];
            let staged = self.stage_dir.join(entry.relative_path);
            let target = self.root.join(entry.relative_path);
            if let Some(parent) = target.parent() {
                capability::create_dir_all(parent).map_err(|source| file_error(parent, source))?;
            }
            capability::rename(&staged, &target).map_err(|source| file_error(&target, source))?;
        }

        let recovery = self.journal.validate(&self.root)?;
        for relative in &self.plan.removals {
            let target = self.root.join(relative);
            match capability::remove_file(&target) {
                Ok(()) => {}
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
                Err(source) => return Err(file_error(&target, source)),
            }
            if recovery.backups.contains_key(relative) {
                remove_empty_parents(target.parent(), &self.root)?;
            }
        }
        sync_touched_parents(&self.root, &self.plan.touched)
    }

    fn verify_installed(&self, entries: &[AtomicArtifactSetEntry<'_>]) -> PpResult<()> {
        for index in &self.plan.ordered_entry_indexes {
            let entry = &entries[*index];
            verify_exact_bytes(
                &self.root.join(entry.relative_path),
                entry.bytes,
                "installed artifact does not match staged bytes",
            )?;
        }
        for relative in &self.plan.removals {
            verify_absent(
                &self.root.join(relative),
                "removed artifact reappeared before commit",
            )?;
        }
        Ok(())
    }

    fn record_install(&mut self) -> PpResult<TerminalMarkerOutcome> {
        let outcome =
            write_terminal_marker(&self.transaction_dir, INSTALLED_MARKER, &self.journal)?;
        self.transition(TransactionEvent::InstallRecorded)?;
        Ok(outcome)
    }

    fn retain_visible_commit(self) -> PpError {
        transaction_error(
            &self.root,
            TransactionState::Installed,
            "publication and installed marker are visible, but terminal directory durability could not be confirmed; transaction retained for deterministic recovery",
        )
    }

    fn commit(self) -> PpResult<()> {
        debug_assert_eq!(self.state, TransactionState::Installed);
        cleanup_transaction(&self.transaction_dir).map_err(|error| {
            transaction_error(
                &self.root,
                self.state,
                format!("publication committed, but journal cleanup durability failed: {error}"),
            )
        })
    }

    fn abort_without_mutation(self, primary: PpError) -> PpError {
        debug_assert_eq!(self.state, TransactionState::Prepared);
        self.abort(primary)
    }

    fn abort_before_install(self, primary: PpError) -> PpError {
        debug_assert_eq!(self.state, TransactionState::BackupsReady);
        self.abort(primary)
    }

    fn abort(mut self, primary: PpError) -> PpError {
        let marker = write_marker(&self.transaction_dir, ABORTED_MARKER, &self.journal);
        if marker.is_ok() {
            let _ = self.transition(TransactionEvent::PublicationAborted);
        }
        let cleanup = cleanup_transaction(&self.transaction_dir);
        match (marker, cleanup) {
            (Ok(()), Ok(())) => primary,
            (marker, cleanup) => transaction_error(
                &self.root,
                TransactionState::RecoveryRequired,
                format!("{primary}; abort marker={marker:?}; cleanup={cleanup:?}"),
            ),
        }
    }

    fn rollback(mut self, primary: PpError) -> PpError {
        let recovery = match self.journal.validate(&self.root) {
            Ok(value) => value,
            Err(error) => {
                let _ = self.transition(TransactionEvent::RecoveryRequired);
                return transaction_error(
                    &self.root,
                    TransactionState::RecoveryRequired,
                    format!("{primary}; rollback journal invalid: {error}"),
                );
            }
        };
        match rollback_from_journal(&self.root, &self.transaction_dir, &recovery) {
            Ok(()) => match self.transition(TransactionEvent::RollbackRestored) {
                Ok(()) => primary,
                Err(error) => transaction_error(
                    &self.root,
                    TransactionState::RecoveryRequired,
                    format!("{primary}; rollback transition failed: {error}"),
                ),
            },
            Err(error) => {
                let _ = self.transition(TransactionEvent::RecoveryRequired);
                transaction_error(
                    &self.root,
                    TransactionState::RecoveryRequired,
                    format!("{primary}; rollback failed: {error}"),
                )
            }
        }
    }

    fn transition(&mut self, event: TransactionEvent) -> PpResult<()> {
        self.state = reduce_transaction(self.state, event).map_err(|reason| {
            transaction_error(
                &self.root,
                self.state,
                format!("invalid transaction event {event:?}: {reason}"),
            )
        })?;
        Ok(())
    }
}

impl Journal {
    fn validate(&self, root: &Path) -> PpResult<RecoveryPlan> {
        if self.schema != JOURNAL_SCHEMA || self.root_name != root_file_name(root)? {
            return Err(transaction_error(
                root,
                TransactionState::RecoveryRequired,
                "journal schema/root mismatch",
            ));
        }
        let writes = parse_paths(root, &self.writes)?;
        let removals = parse_paths(root, &self.removals)?;
        if writes
            .len()
            .checked_add(removals.len())
            .is_none_or(|count| count > MAX_ARTIFACT_ENTRIES)
        {
            return Err(transaction_error(
                root,
                TransactionState::RecoveryRequired,
                "journal entry limit exceeded",
            ));
        }
        if !writes.is_disjoint(&removals) {
            return Err(transaction_error(
                root,
                TransactionState::RecoveryRequired,
                "journal write/removal paths overlap",
            ));
        }
        let touched = writes.union(&removals).cloned().collect::<Vec<_>>();
        let mut backups = BTreeMap::new();
        let mut total = 0u64;
        for backup in &self.backups {
            if !crate::is_sha256_hex(&backup.sha256) {
                return Err(transaction_error(
                    root,
                    TransactionState::RecoveryRequired,
                    "journal backup digest is invalid",
                ));
            }
            let path = parse_journal_path(root, &backup.path)?;
            if (!writes.contains(&path) && !removals.contains(&path))
                || backups.insert(path, backup.clone()).is_some()
            {
                return Err(transaction_error(
                    root,
                    TransactionState::RecoveryRequired,
                    "journal backup path is duplicate or foreign",
                ));
            }
            total = total.checked_add(backup.byte_count).ok_or_else(|| {
                transaction_error(
                    root,
                    TransactionState::RecoveryRequired,
                    "journal backup byte count overflow",
                )
            })?;
            if total > MAX_ARTIFACT_BYTES as u64 {
                return Err(transaction_error(
                    root,
                    TransactionState::RecoveryRequired,
                    "journal backup byte limit exceeded",
                ));
            }
        }
        if !self.root_existed && !backups.is_empty() {
            return Err(transaction_error(
                root,
                TransactionState::RecoveryRequired,
                "new root journal cannot contain backups",
            ));
        }
        Ok(RecoveryPlan {
            root_existed: self.root_existed,
            writes,
            backups,
            touched,
        })
    }
}

fn prepare_root(requested: &Path) -> PpResult<PathBuf> {
    if requested.as_os_str().is_empty() || requested.file_name().is_none() {
        return Err(file_error(
            requested,
            "artifact output root must name a directory",
        ));
    }
    let parent = requested
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    capability::create_dir_all(parent).map_err(|source| file_error(parent, source))?;
    Ok(requested.to_path_buf())
}

fn acquire_lock(root: &Path) -> PpResult<PublicationLock> {
    let path = lock_path(root)?;
    let file = capability::open_lock(&path).map_err(|source| file_error(&path, source))?;
    file.try_lock().map_err(|source| {
        file_error(
            &path,
            match source {
                std::fs::TryLockError::WouldBlock => {
                    "another artifact publication is already in progress".to_string()
                }
                std::fs::TryLockError::Error(source) => source.to_string(),
            },
        )
    })?;
    Ok(PublicationLock { _file: file })
}

fn create_transaction_dir(root: &Path) -> PpResult<PathBuf> {
    let parent = root.parent().expect("prepared root parent");
    let prefix = transaction_prefix(root)?;
    for attempt in 0..MAX_TEMP_ATTEMPTS {
        let path = parent.join(format!("{prefix}{}.{}", std::process::id(), attempt));
        match capability::create_directory_new(&path) {
            Ok(_) => return Ok(path),
            Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(source) => return Err(file_error(&path, source)),
        }
    }
    Err(file_error(
        root,
        "could not allocate unique transaction directory",
    ))
}

fn recover_stale_transactions(root: &Path) -> PpResult<()> {
    let parent = root.parent().expect("prepared root parent");
    let prefix = transaction_prefix(root)?;
    let mut transactions = Vec::new();
    for entry in fs::read_dir(parent).map_err(|source| file_error(parent, source))? {
        let entry = entry.map_err(|source| file_error(parent, source))?;
        let name = entry.file_name();
        if !is_transaction_name(&name, &prefix) {
            continue;
        }
        let file_type = entry
            .file_type()
            .map_err(|source| file_error(&entry.path(), source))?;
        if file_type.is_symlink() || !file_type.is_dir() {
            return Err(transaction_error(
                root,
                TransactionState::RecoveryRequired,
                "transaction entry is not a real directory",
            ));
        }
        transactions.push(entry.path());
    }
    transactions.sort();
    for transaction in transactions {
        recover_transaction(root, &transaction)?;
    }
    Ok(())
}

fn recover_transaction(root: &Path, transaction_dir: &Path) -> PpResult<()> {
    let installed = marker_present(&transaction_dir.join(INSTALLED_MARKER))?;
    let aborted = marker_present(&transaction_dir.join(ABORTED_MARKER))?;
    if installed && aborted {
        return Err(transaction_error(
            root,
            TransactionState::RecoveryRequired,
            "conflicting terminal transaction markers",
        ));
    }
    if installed {
        read_marker(root, &transaction_dir.join(INSTALLED_MARKER))?.validate(root)?;
        return cleanup_transaction(transaction_dir);
    }
    if aborted {
        read_marker(root, &transaction_dir.join(ABORTED_MARKER))?.validate(root)?;
        return cleanup_transaction(transaction_dir);
    }
    if marker_present(&transaction_dir.join(BACKUPS_READY_MARKER))? {
        let journal = read_marker(root, &transaction_dir.join(BACKUPS_READY_MARKER))?;
        let recovery = journal.validate(root)?;
        return rollback_from_journal(root, transaction_dir, &recovery);
    }
    if marker_present(&transaction_dir.join(PREPARED_MARKER))? {
        read_marker(root, &transaction_dir.join(PREPARED_MARKER))?.validate(root)?;
        return cleanup_transaction(transaction_dir);
    }
    Err(transaction_error(
        root,
        TransactionState::RecoveryRequired,
        "transaction has no valid recovery marker",
    ))
}

fn rollback_from_journal(
    root: &Path,
    transaction_dir: &Path,
    recovery: &RecoveryPlan,
) -> PpResult<()> {
    verify_backup_set(transaction_dir, recovery)?;

    for relative in &recovery.touched {
        let target = root.join(relative);
        if let Some(record) = recovery.backups.get(relative) {
            let backup = transaction_dir.join("backup").join(relative);
            verify_file_record(&backup, record, "rollback backup integrity mismatch")?;
            let bytes = read_regular(&backup, MAX_ARTIFACT_BYTES)?;
            let restore = restore_temp_path(&target);
            remove_if_present(&restore)?;
            write_new_file(&restore, &bytes)?;
            capability::rename(&restore, &target).map_err(|source| file_error(&target, source))?;
            verify_file_record(&target, record, "restored artifact integrity mismatch")?;
        } else if recovery.writes.contains(relative) {
            remove_if_present(&target)?;
        }
    }

    sync_touched_parents(root, &recovery.touched)?;
    for relative in &recovery.touched {
        let target = root.join(relative);
        match recovery.backups.get(relative) {
            Some(record) => verify_file_record(&target, record, "rollback verification failed")?,
            None if recovery.writes.contains(relative) => {
                verify_absent(&target, "rollback left a newly-created artifact")?
            }
            None => {}
        }
    }

    if !recovery.root_existed {
        match capability::remove_directory(root) {
            Ok(()) => {}
            Err(source)
                if source.kind() == std::io::ErrorKind::NotFound
                    || source.kind() == std::io::ErrorKind::DirectoryNotEmpty => {}
            Err(source) => return Err(file_error(root, source)),
        }
    }
    cleanup_transaction(transaction_dir)
}

fn write_marker(transaction_dir: &Path, name: &str, journal: &Journal) -> PpResult<()> {
    let bytes = serialize_journal(transaction_dir, name, journal)?;
    let marker = transaction_dir.join(name);
    let temp = marker_temp_path(&marker);
    remove_if_present(&temp)?;
    write_new_file(&temp, &bytes)?;
    capability::rename(&temp, &marker).map_err(|source| file_error(&marker, source))
}

fn write_terminal_marker(
    transaction_dir: &Path,
    name: &str,
    journal: &Journal,
) -> PpResult<TerminalMarkerOutcome> {
    let bytes = serialize_journal(transaction_dir, name, journal)?;
    let marker = transaction_dir.join(name);
    let temp = marker_temp_path(&marker);
    remove_if_present(&temp)?;
    write_new_file(&temp, &bytes)?;
    let commit =
        capability::rename_visible(&temp, &marker).map_err(|source| file_error(&marker, source))?;
    match commit.sync() {
        Ok(()) => Ok(TerminalMarkerOutcome::Durable),
        Err(_) => Ok(TerminalMarkerOutcome::VisibleDurabilityUnconfirmed),
    }
}

fn serialize_journal(transaction_dir: &Path, name: &str, journal: &Journal) -> PpResult<Vec<u8>> {
    let bytes = serde_json::to_vec_pretty(journal).map_err(|source| PpError::Json {
        path: transaction_dir.join(name),
        message: source.to_string(),
    })?;
    if bytes.len() > MAX_JOURNAL_BYTES {
        return Err(file_error(
            transaction_dir,
            "transaction journal exceeds byte limit",
        ));
    }
    Ok(bytes)
}

fn read_marker(root: &Path, path: &Path) -> PpResult<Journal> {
    let bytes = read_regular(path, MAX_JOURNAL_BYTES)?;
    serde_json::from_slice(&bytes).map_err(|source| {
        transaction_error(
            root,
            TransactionState::RecoveryRequired,
            format!("invalid transaction marker '{}': {source}", path.display()),
        )
    })
}

fn marker_present(path: &Path) -> PpResult<bool> {
    match capability::open_read(path) {
        Ok(file) => Ok(file
            .metadata()
            .map_err(|source| file_error(path, source))?
            .is_file()),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(file_error(path, source)),
    }
}

fn cleanup_transaction(transaction_dir: &Path) -> PpResult<()> {
    match fs::remove_dir_all(transaction_dir) {
        Ok(()) => {}
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => return Err(file_error(transaction_dir, source)),
    }
    sync_directory(transaction_dir.parent().expect("transaction parent"))
}

fn write_new_file(path: &Path, bytes: &[u8]) -> PpResult<()> {
    if let Some(parent) = path.parent() {
        capability::create_dir_all(parent).map_err(|source| file_error(parent, source))?;
    }
    let mut file = capability::create_new(path).map_err(|source| file_error(path, source))?;
    file.write_all(bytes)
        .map_err(|source| file_error(path, source))?;
    capability::sync_file_durable(&file).map_err(|source| file_error(path, source))?;
    if let Some(parent) = path.parent() {
        sync_directory(parent)?;
    }
    Ok(())
}

fn read_optional_regular(path: &Path, limit: usize) -> PpResult<Option<Vec<u8>>> {
    match capability::open_read(path) {
        Ok(file) => read_from_open_file(path, file, limit).map(Some),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(file_error(path, source)),
    }
}

fn read_regular(path: &Path, limit: usize) -> PpResult<Vec<u8>> {
    let file = capability::open_read(path).map_err(|source| file_error(path, source))?;
    read_from_open_file(path, file, limit)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileRevision {
    device: u64,
    inode: u64,
    byte_count: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

fn revision(metadata: &fs::Metadata) -> FileRevision {
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

fn read_from_open_file(path: &Path, mut file: File, limit: usize) -> PpResult<Vec<u8>> {
    let before = file.metadata().map_err(|source| file_error(path, source))?;
    if !before.is_file() {
        return Err(invalid_destination_error(
            path,
            "managed artifact must be a regular file",
        ));
    }
    let before_revision = revision(&before);
    let read_limit = u64::try_from(limit)
        .ok()
        .and_then(|value| value.checked_add(1))
        .ok_or_else(|| file_error(path, "read limit overflow"))?;
    let mut bytes = Vec::new();
    std::io::Read::by_ref(&mut file)
        .take(read_limit)
        .read_to_end(&mut bytes)
        .map_err(|source| file_error(path, source))?;
    if bytes.len() > limit {
        return Err(file_error(path, "managed artifact exceeds byte limit"));
    }
    let after = file.metadata().map_err(|source| file_error(path, source))?;
    if before_revision != revision(&after) || after.len() != bytes.len() as u64 {
        return Err(file_error(
            path,
            "managed artifact changed while its bytes were captured",
        ));
    }
    Ok(bytes)
}

fn verify_backup_set(transaction_dir: &Path, recovery: &RecoveryPlan) -> PpResult<()> {
    for (relative, record) in &recovery.backups {
        verify_file_record(
            &transaction_dir.join("backup").join(relative),
            record,
            "rollback backup integrity mismatch",
        )?;
    }
    Ok(())
}

fn verify_file_record(path: &Path, record: &BackupRecord, message: &str) -> PpResult<()> {
    let bytes = read_regular(path, MAX_ARTIFACT_BYTES)?;
    if u64::try_from(bytes.len()).ok() != Some(record.byte_count)
        || crate::sha256_hex(&bytes) != record.sha256
    {
        return Err(file_error(path, message));
    }
    Ok(())
}

fn verify_exact_bytes(path: &Path, expected: &[u8], message: &str) -> PpResult<()> {
    let bytes = read_regular(path, expected.len())?;
    if bytes != expected {
        return Err(file_error(path, message));
    }
    Ok(())
}

fn verify_absent(path: &Path, message: &str) -> PpResult<()> {
    match capability::open_read(path) {
        Ok(_) => Err(file_error(path, message)),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(file_error(path, source)),
    }
}

fn remove_if_present(path: &Path) -> PpResult<()> {
    match capability::remove_file(path) {
        Ok(()) => Ok(()),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(file_error(path, source)),
    }
}

fn remove_empty_parents(mut current: Option<&Path>, root: &Path) -> PpResult<()> {
    while let Some(directory) = current {
        if directory == root || !directory.starts_with(root) {
            break;
        }
        match capability::remove_directory(directory) {
            Ok(()) => {}
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) if source.kind() == std::io::ErrorKind::DirectoryNotEmpty => break,
            Err(source) => return Err(file_error(directory, source)),
        }
        current = directory.parent();
    }
    Ok(())
}

fn sync_touched_parents(root: &Path, paths: &[PathBuf]) -> PpResult<()> {
    let mut directories = BTreeSet::new();
    directories.insert(root.to_path_buf());
    for relative in paths {
        let mut parent = root.join(relative).parent().map(Path::to_path_buf);
        while let Some(directory) = parent {
            if !directory.starts_with(root) {
                break;
            }
            directories.insert(directory.clone());
            if directory == root {
                break;
            }
            parent = directory.parent().map(Path::to_path_buf);
        }
    }
    for directory in directories.iter().rev() {
        match capability::sync_directory(directory) {
            Ok(()) => {}
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => return Err(file_error(directory, source)),
        }
    }
    Ok(())
}

pub(super) fn sync_directory(path: &Path) -> PpResult<()> {
    capability::sync_directory(path).map_err(|source| file_error(path, source))
}

fn ensure_root_exists(root: &Path) -> PpResult<bool> {
    match capability::open_directory(root) {
        Ok(_) => return Ok(false),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => return Err(file_error(root, source)),
    }
    capability::create_directory_new(root).map_err(|source| file_error(root, source))?;
    sync_directory(root)?;
    sync_directory(root.parent().expect("root parent"))?;
    Ok(true)
}

fn rollback_empty_root_creation(root: &Path) -> PpResult<()> {
    capability::remove_directory(root).map_err(|source| file_error(root, source))
}

fn existing_directory(root: &Path) -> PpResult<bool> {
    match capability::open_directory(root) {
        Ok(_) => Ok(true),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(file_error(root, source)),
    }
}

fn preflight_root(root: &Path) -> PpResult<()> {
    match capability::open_directory(root) {
        Ok(_) => Ok(()),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(invalid_destination_error(
            root,
            &format!("artifact root is not a real directory: {source}"),
        )),
    }
}

fn preflight_targets(root: &Path, touched: &[PathBuf]) -> PpResult<()> {
    preflight_root(root)?;
    reject_blocked_managed_parents(root, touched)?;
    for relative in touched {
        let target = root.join(relative);
        match capability::open_read(&target) {
            Ok(file) => {
                if !file
                    .metadata()
                    .map_err(|source| file_error(&target, source))?
                    .is_file()
                {
                    return Err(invalid_destination_error(
                        &target,
                        "managed artifact must be a regular file",
                    ));
                }
            }
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(invalid_destination_error(
                    &target,
                    &format!("managed artifact is not a real file: {source}"),
                ))
            }
        }
    }
    Ok(())
}

pub fn reject_blocked_managed_parents(root: &Path, relative_paths: &[PathBuf]) -> PpResult<()> {
    for relative in relative_paths {
        validate_relative_path(relative)?;
        let mut cursor = root.to_path_buf();
        if let Some(parent) = relative.parent() {
            for component in parent.components() {
                cursor.push(component.as_os_str());
                match capability::open_directory(&cursor) {
                    Ok(_) => {}
                    Err(source) if source.kind() == std::io::ErrorKind::NotFound => break,
                    Err(source) => {
                        return Err(invalid_destination_error(
                            &cursor,
                            &format!("artifact parent must be a non-symlink directory: {source}"),
                        ))
                    }
                }
            }
        }
    }
    Ok(())
}

fn parse_paths(root: &Path, values: &[String]) -> PpResult<BTreeSet<PathBuf>> {
    let mut result = BTreeSet::new();
    for value in values {
        let path = parse_journal_path(root, value)?;
        if !result.insert(path) {
            return Err(transaction_error(
                root,
                TransactionState::RecoveryRequired,
                "journal contains duplicate paths",
            ));
        }
    }
    Ok(result)
}

fn parse_journal_path(root: &Path, value: &str) -> PpResult<PathBuf> {
    let path = PathBuf::from(value);
    validate_relative_path(&path).map_err(|_| {
        transaction_error(
            root,
            TransactionState::RecoveryRequired,
            "journal contains invalid relative path",
        )
    })?;
    Ok(path)
}

fn journal_path(path: &Path) -> PpResult<String> {
    validate_relative_path(path)?;
    path.to_str()
        .map(str::to_string)
        .ok_or_else(|| file_error(path, "artifact path must be UTF-8 for journal persistence"))
}

fn root_file_name(root: &Path) -> PpResult<String> {
    root.file_name()
        .and_then(|value| value.to_str())
        .map(str::to_string)
        .ok_or_else(|| file_error(root, "artifact output root must have a UTF-8 file name"))
}

fn lock_path(root: &Path) -> PpResult<PathBuf> {
    Ok(root.with_file_name(format!(".{}.artifact-set-lock", root_file_name(root)?)))
}

fn transaction_prefix(root: &Path) -> PpResult<String> {
    Ok(format!(".{}.artifact-set.", root_file_name(root)?))
}

fn is_transaction_name(name: &std::ffi::OsStr, prefix: &str) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    let Some(suffix) = name.strip_prefix(prefix) else {
        return false;
    };
    let Some((pid, attempt)) = suffix.split_once('.') else {
        return false;
    };
    !pid.is_empty()
        && pid.bytes().all(|byte| byte.is_ascii_digit())
        && !attempt.is_empty()
        && attempt.bytes().all(|byte| byte.is_ascii_digit())
}

fn marker_temp_path(marker: &Path) -> PathBuf {
    let name = marker
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("marker");
    marker.with_file_name(format!(".{name}.tmp-{}", std::process::id()))
}

fn restore_temp_path(target: &Path) -> PathBuf {
    let name = target
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("artifact");
    target.with_file_name(format!(".{name}.restore-{}", std::process::id()))
}

fn file_error(path: &Path, source: impl ToString) -> PpError {
    PpError::FileIo {
        path: path.to_path_buf(),
        message: source.to_string(),
    }
}

fn invalid_destination_error(path: &Path, message: &str) -> PpError {
    PpError::InvalidRequest(format!("destination '{}': {message}", path.display()))
}

fn transaction_error(root: &Path, state: TransactionState, message: impl Into<String>) -> PpError {
    PpError::FileIo {
        path: root.to_path_buf(),
        message: format!("artifact transaction {state:?}: {}", message.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_root(label: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "perfectpixel-artifact-clean-{label}-{}-{stamp}",
            std::process::id()
        ))
    }

    #[test]
    fn reducer_rejects_duplicate_or_stale_events() {
        let state = reduce_transaction(
            TransactionState::Prepared,
            TransactionEvent::BackupsRecorded,
        )
        .expect("transition");
        assert_eq!(state, TransactionState::BackupsReady);
        assert!(reduce_transaction(state, TransactionEvent::BackupsRecorded).is_err());
    }

    #[test]
    fn failed_before_commit_restores_previous_generation() -> PpResult<()> {
        let root = test_root("rollback");
        capability::create_dir_all(&root).map_err(|source| file_error(&root, source))?;
        write_new_file(&root.join("current.txt"), b"old")?;
        let result = AtomicArtifactSetWriter::publish_with_planner_checked(
            &root,
            |_locked| {
                Ok((
                    AtomicArtifactSetOwnedPlan {
                        entries: vec![AtomicArtifactSetOwnedEntry {
                            relative_path: "current.txt".into(),
                            bytes: b"new".to_vec(),
                        }],
                        removals: Vec::new(),
                    },
                    (),
                ))
            },
            |_root, phase, _| {
                if phase == ArtifactSetConditionPhase::BeforeCommit {
                    Err(PpError::InvalidRequest("forced failure".into()))
                } else {
                    Ok(())
                }
            },
        );
        assert!(result.is_err());
        assert_eq!(read_regular(&root.join("current.txt"), 16)?, b"old");
        let _ = fs::remove_dir_all(root);
        Ok(())
    }

    #[test]
    fn stale_output_change_before_mutation_is_rejected_without_overwrite() -> PpResult<()> {
        let root = test_root("stale");
        capability::create_dir_all(&root).map_err(|source| file_error(&root, source))?;
        write_new_file(&root.join("current.txt"), b"old")?;
        let result = AtomicArtifactSetWriter::publish_with_planner_checked(
            &root,
            |_locked| {
                Ok((
                    AtomicArtifactSetOwnedPlan {
                        entries: vec![AtomicArtifactSetOwnedEntry {
                            relative_path: "current.txt".into(),
                            bytes: b"new".to_vec(),
                        }],
                        removals: Vec::new(),
                    },
                    (),
                ))
            },
            |locked_root, phase, _| {
                if phase == ArtifactSetConditionPhase::BeforeMutation {
                    let target = locked_root.join("current.txt");
                    let temp = locked_root.join("external.tmp");
                    write_new_file(&temp, b"external")?;
                    capability::rename(&temp, &target)
                        .map_err(|source| file_error(&target, source))?;
                }
                Ok(())
            },
        );
        assert!(result.is_err());
        assert_eq!(read_regular(&root.join("current.txt"), 32)?, b"external");
        let _ = fs::remove_dir_all(root);
        Ok(())
    }
}
