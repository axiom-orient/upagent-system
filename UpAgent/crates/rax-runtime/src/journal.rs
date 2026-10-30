use std::{
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use directories::BaseDirs;
use fs2::FileExt;
use rax_core::{
    ErrorCode, ErrorPhase, EventEnvelope, EventKind, MAX_REQUEST_WIRE_BYTES, Outcome, Plan,
    RaxError, RequestEnvelope, ResultEnvelope, RunState, SCHEMA_VERSION, transition,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use uuid::Uuid;

use crate::{
    error::io_error, process::CancellationToken, timeutil::now_rfc3339, workspace::Workspace,
};

const LOCK_MINIMUM_BACKOFF: Duration = Duration::from_millis(2);
const LOCK_MAXIMUM_BACKOFF: Duration = Duration::from_millis(50);
const DEFAULT_LOCK_WAIT: Duration = Duration::from_mins(5);
const MAXIMUM_RUN_SCAN_ENTRIES: usize = 10_000;
// Recovery files carry the same request/result JSON envelope bounded by the
// core wire limit. NDJSON is additionally bounded as one stream and by event
// count; no recovery input is read into an unbounded buffer.
const MAXIMUM_JOURNAL_JSON_BYTES: u64 = MAX_REQUEST_WIRE_BYTES;
const MAXIMUM_NDJSON_TOTAL_BYTES: u64 = MAXIMUM_JOURNAL_JSON_BYTES;
const MAXIMUM_NDJSON_LINE_BYTES: u64 = MAXIMUM_JOURNAL_JSON_BYTES;
const MAXIMUM_NDJSON_EVENTS: usize = 1_024;
const MAXIMUM_RECOVERY_NOTE_BYTES: usize = 64 * 1024;

/// Take an exclusive file lock without ever parking indefinitely.
///
/// A blocking `flock` observes neither a cancellation token nor a deadline, so a
/// peer holding the lock would suspend the caller with no way to interrupt it.
/// Polling `try_lock_exclusive` keeps both observable at every step and turns
/// lock starvation into a typed, retryable failure instead of a hang.
fn acquire_exclusive(
    file: &File,
    path: &Path,
    phase: ErrorPhase,
    label: &str,
    maximum_wait: Duration,
    cancellation: Option<&CancellationToken>,
) -> Result<(), RaxError> {
    let deadline = Instant::now() + maximum_wait;
    let mut backoff = LOCK_MINIMUM_BACKOFF;
    loop {
        if cancellation.is_some_and(CancellationToken::is_cancelled) {
            return Err(RaxError::new(
                ErrorCode::ProcessCancelled,
                phase,
                false,
                format!("cancelled while waiting for the {label}"),
            )
            .with_detail("path", path.to_string_lossy().to_string()));
        }
        match FileExt::try_lock_exclusive(file) {
            Ok(()) => return Ok(()),
            Err(error) if is_lock_contended(&error) => {}
            Err(error) => {
                return Err(io_error(
                    phase,
                    format!("failed to acquire the {label}"),
                    Some(path),
                    error,
                ));
            }
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(RaxError::new(
                ErrorCode::OperationFailed,
                phase,
                true,
                format!("timed out waiting for the {label}"),
            )
            .with_detail("path", path.to_string_lossy().to_string())
            .with_detail(
                "waitedMs",
                u64::try_from(maximum_wait.as_millis()).unwrap_or(u64::MAX),
            ));
        }
        std::thread::sleep(backoff.min(remaining));
        backoff = (backoff * 2).min(LOCK_MAXIMUM_BACKOFF);
    }
}

/// `fs2` reports contention with the platform's "would block" error, which is
/// `WouldBlock` on Unix and a raw `ERROR_LOCK_VIOLATION` on Windows.
fn is_lock_contended(error: &std::io::Error) -> bool {
    const ERROR_LOCK_VIOLATION: i32 = 33;
    error.kind() == std::io::ErrorKind::WouldBlock
        || error.raw_os_error() == Some(ERROR_LOCK_VIOLATION)
}

#[derive(Debug, Clone)]
pub struct StateStore {
    root: PathBuf,
    workspace_root: PathBuf,
}

impl StateStore {
    pub fn open(workspace: &Workspace, override_root: Option<&Path>) -> Result<Self, RaxError> {
        let requested_root = match override_root {
            Some(path) => path.to_path_buf(),
            None => default_state_root()?,
        };
        if !requested_root.is_absolute() {
            return Err(RaxError::new(
                ErrorCode::InvalidPath,
                ErrorPhase::Validate,
                false,
                "state directory must be an absolute path",
            )
            .with_detail("path", requested_root.to_string_lossy().to_string()));
        }
        if requested_root.to_str().is_none() {
            return Err(journal_error(
                "state directory path must be valid UTF-8 for the JSON protocol",
            ));
        }
        validate_state_path_syntax(&requested_root)?;
        reject_preexisting_symlink(&requested_root, ErrorPhase::Validate, "state directory")?;
        let projected_root = resolve_path_with_missing_tail(&requested_root)?;
        if projected_root.starts_with(workspace.root()) {
            return Err(RaxError::new(
                ErrorCode::InvalidPath,
                ErrorPhase::Validate,
                false,
                "state directory must be outside the authorized workspace",
            )
            .with_detail("stateRoot", projected_root.to_string_lossy().to_string())
            .with_detail("workspace", workspace.root().to_string_lossy().to_string()));
        }
        create_private_dir_all(&requested_root)?;
        let root = std::fs::canonicalize(&requested_root).map_err(|error| {
            io_error(
                ErrorPhase::Validate,
                "failed to resolve state directory",
                Some(&requested_root),
                error,
            )
        })?;
        if root.starts_with(workspace.root()) {
            return Err(RaxError::new(
                ErrorCode::InvalidPath,
                ErrorPhase::Validate,
                false,
                "state directory must be outside the authorized workspace",
            )
            .with_detail("stateRoot", root.to_string_lossy().to_string())
            .with_detail("workspace", workspace.root().to_string_lossy().to_string()));
        }
        let workspace_root = root.join("workspaces").join(workspace.identity());
        for directory in [
            root.join("workspaces"),
            workspace_root.clone(),
            workspace_root.join("plans"),
            workspace_root.join("runs"),
        ] {
            reject_preexisting_symlink(&directory, ErrorPhase::Validate, "state directory")?;
            create_private_dir_all(&directory)?;
            validate_directory_boundary(&directory, ErrorPhase::Validate, "state directory")?;
        }
        validate_optional_regular_file(
            &workspace_root.join("execution.lock"),
            ErrorPhase::Validate,
            "workspace execution lock",
        )?;
        Ok(Self {
            root,
            workspace_root,
        })
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    /// Acquire the workspace execution lock without ever parking indefinitely.
    ///
    /// A blocking `flock` cannot observe a cancellation token, so a peer that
    /// holds the lock would suspend the caller past its own deadline with no
    /// way to interrupt it. Polling `try_lock_exclusive` keeps cancellation and
    /// the bounded wait observable at every step, and turns lock starvation into
    /// a typed, retryable failure instead of a hang.
    pub(crate) fn lock_execution(
        &self,
        cancellation: &CancellationToken,
        maximum_wait: Duration,
    ) -> Result<ExecutionLock, RaxError> {
        let path = self.workspace_root.join("execution.lock");
        let file = open_lock_file(&path, ErrorPhase::Authorize, "workspace execution lock")?;
        acquire_exclusive(
            &file,
            &path,
            ErrorPhase::Authorize,
            "workspace execution lock",
            maximum_wait,
            Some(cancellation),
        )?;
        Ok(ExecutionLock { file })
    }

    pub fn write_plan(&self, plan: &Plan) -> Result<PathBuf, RaxError> {
        validate_identifier(&plan.plan_id)?;
        let directory = self.plan_dir(&plan.plan_id)?;
        std::fs::create_dir(&directory).map_err(|error| {
            io_error(
                ErrorPhase::Record,
                "failed to create immutable plan directory",
                Some(&directory),
                error,
            )
        })?;
        validate_directory_boundary(&directory, ErrorPhase::Record, "plan directory")?;
        set_private_directory_permissions(&directory)?;
        let path = directory.join("plan.json");
        if let Err(error) = atomic_write_json(&path, plan) {
            let cleanup = std::fs::remove_dir_all(&directory)
                .err()
                .map(|cleanup_error| cleanup_error.to_string());
            let error = if let Some(cleanup) = cleanup {
                error.with_detail("cleanupError", cleanup)
            } else {
                error
            };
            return Err(error);
        }
        Ok(path)
    }

    pub fn load_plan(&self, plan_id: &str) -> Result<Plan, RaxError> {
        let path = self.plan_dir(plan_id)?.join("plan.json");
        read_json(&path)
    }

    pub fn plan_dir(&self, plan_id: &str) -> Result<PathBuf, RaxError> {
        validate_identifier(plan_id)?;
        let plans = self.canonical_plans_dir()?;
        let directory = plans.join(plan_id);
        match std::fs::symlink_metadata(&directory) {
            Ok(_) => {
                let canonical = validate_directory_boundary(
                    &directory,
                    ErrorPhase::Validate,
                    "plan directory",
                )?;
                if canonical.parent() != Some(plans.as_path()) {
                    return Err(RaxError::new(
                        ErrorCode::SymlinkNotAllowed,
                        ErrorPhase::Validate,
                        false,
                        "plan directory is outside the state plans directory",
                    )
                    .with_detail("path", directory.to_string_lossy().to_string()));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(io_error(
                    ErrorPhase::Validate,
                    "failed to inspect plan directory",
                    Some(&directory),
                    error,
                ));
            }
        }
        Ok(directory)
    }

    pub fn run_dir(&self, run_id: &str) -> Result<PathBuf, RaxError> {
        validate_identifier(run_id)?;
        Ok(self.workspace_root.join("runs").join(run_id))
    }

    fn canonical_runs_dir(&self, phase: ErrorPhase) -> Result<PathBuf, RaxError> {
        validate_directory_boundary(&self.workspace_root, phase, "state workspace")?;
        let runs = self.workspace_root.join("runs");
        let canonical = validate_directory_boundary(&runs, phase, "effect journal runs directory")?;
        if canonical.parent() != Some(self.workspace_root.as_path()) {
            return Err(RaxError::new(
                ErrorCode::SymlinkNotAllowed,
                phase,
                false,
                "effect journal runs directory escapes its state workspace",
            )
            .with_detail("path", runs.to_string_lossy().to_string()));
        }
        Ok(canonical)
    }

    fn canonical_plans_dir(&self) -> Result<PathBuf, RaxError> {
        validate_directory_boundary(
            &self.workspace_root,
            ErrorPhase::Validate,
            "state workspace",
        )?;
        let plans = self.workspace_root.join("plans");
        let canonical =
            validate_directory_boundary(&plans, ErrorPhase::Validate, "state plans directory")?;
        if canonical.parent() != Some(self.workspace_root.as_path()) {
            return Err(RaxError::new(
                ErrorCode::SymlinkNotAllowed,
                ErrorPhase::Validate,
                false,
                "state plans directory escapes its state workspace",
            )
            .with_detail("path", plans.to_string_lossy().to_string()));
        }
        Ok(canonical)
    }

    fn validated_run_dir(&self, run_id: &str, phase: ErrorPhase) -> Result<PathBuf, RaxError> {
        validate_identifier(run_id)?;
        let runs = self.canonical_runs_dir(phase)?;
        let directory = runs.join(run_id);
        validate_run_directory(&runs, &directory, phase)
    }

    pub fn begin_run(
        &self,
        request: &RequestEnvelope,
        plan: &Plan,
    ) -> Result<RunJournal, RaxError> {
        let run_id = Uuid::now_v7().to_string();
        let runs = self.canonical_runs_dir(ErrorPhase::Record)?;
        let directory = runs.join(&run_id);
        validate_identifier(&run_id)?;
        std::fs::create_dir(&directory).map_err(|error| {
            io_error(
                ErrorPhase::Record,
                "failed to create run journal",
                Some(&directory),
                error,
            )
        })?;
        set_private_directory_permissions(&directory)?;
        atomic_write_json(&directory.join("request.json"), request)?;
        atomic_write_json(&directory.join("plan.json"), plan)?;
        let events_path = directory.join("events.ndjson");
        let events = private_open_append(&events_path)?;
        let receipt_path = directory
            .to_str()
            .ok_or_else(|| journal_error("run journal path is not valid UTF-8"))?
            .to_string();
        Ok(RunJournal {
            run_id,
            directory,
            receipt_path,
            events,
            sequence: 0,
            state: RunState::Received,
        })
    }

    pub fn list_runs(&self) -> Result<Vec<RunSummary>, RaxError> {
        let runs = self.canonical_runs_dir(ErrorPhase::Record)?;
        let mut directories = read_directories(&runs)?;
        directories.sort_by(|left, right| right.file_name().cmp(&left.file_name()));
        Ok(directories
            .into_iter()
            .map(|directory| {
                match validate_run_directory(&runs, &directory, ErrorPhase::Record)
                    .and_then(|_| Self::summarize_run(&directory))
                {
                    Ok(summary) => summary,
                    Err(error) => {
                        let resolved = read_recovery_record(&directory).ok().flatten().is_some();
                        corrupt_run_summary(&directory, &error, resolved)
                    }
                }
            })
            .collect())
    }

    pub fn show_run(&self, run_id: &str) -> Result<RunRecord, RaxError> {
        let directory = self.validated_run_dir(run_id, ErrorPhase::Record)?;
        let request = read_json(&directory.join("request.json"))?;
        let plan = read_json(&directory.join("plan.json"))?;
        let events = read_ndjson(&directory.join("events.ndjson"))?;
        let result = read_optional_json(&directory.join("result.json"))?;
        let recovery = read_recovery_record(&directory)?;
        Ok(RunRecord {
            run_id: run_id.to_string(),
            request,
            plan,
            events,
            result,
            recovery,
        })
    }

    pub fn list_recovery(&self) -> Result<Vec<RunSummary>, RaxError> {
        Ok(self
            .list_runs()?
            .into_iter()
            .filter(|run| run.recovery_required)
            .collect())
    }

    pub fn resolve_recovery(
        &self,
        run_id: &str,
        resolution: RecoveryResolution,
        note: Option<String>,
    ) -> Result<RecoveryRecord, RaxError> {
        if note
            .as_deref()
            .is_some_and(|value| value.len() > MAXIMUM_RECOVERY_NOTE_BYTES)
        {
            return Err(RaxError::new(
                ErrorCode::LimitExceeded,
                ErrorPhase::Recover,
                false,
                format!("recovery note exceeds {MAXIMUM_RECOVERY_NOTE_BYTES} bytes"),
            )
            .with_detail("maximumBytes", MAXIMUM_RECOVERY_NOTE_BYTES));
        }
        let directory = self.validated_run_dir(run_id, ErrorPhase::Recover)?;

        let _execution_guard = self.lock_execution_for_recovery()?;
        let _guard = lock_recovery(&directory)?;
        let recovery_path = directory.join("recovery.json");
        if recovery_path.exists() {
            return Err(RaxError::new(
                ErrorCode::AlreadyExists,
                ErrorPhase::Recover,
                false,
                "recovery resolution already exists for this run",
            )
            .with_detail("runId", run_id.to_string()));
        }

        match Self::summarize_run(&directory) {
            Ok(summary) if !summary.recovery_required => {
                return Err(RaxError::new(
                    ErrorCode::InvalidRequest,
                    ErrorPhase::Recover,
                    false,
                    "this run has terminal evidence that does not require recovery",
                )
                .with_detail("runId", run_id.to_string()));
            }
            Ok(_) => {}
            Err(error) if resolution == RecoveryResolution::ManualReviewRequired => {
                // Corrupt or contradictory evidence cannot safely prove success or failure.
                // It may only be acknowledged as requiring manual review.
                let _ = error;
            }
            Err(error) => {
                return Err(RaxError::new(
                    ErrorCode::RecoveryRequired,
                    ErrorPhase::Recover,
                    false,
                    "corrupt run evidence can only be resolved as manual_review_required",
                )
                .with_detail("runId", run_id.to_string())
                .with_detail("diagnostic", error.to_string()));
            }
        }

        let record = RecoveryRecord {
            schema_version: SCHEMA_VERSION.to_string(),
            run_id: run_id.to_string(),
            resolution,
            note,
            resolved_at: now_rfc3339()?,
        };
        atomic_write_json(&recovery_path, &record)?;
        Ok(record)
    }

    fn lock_execution_for_recovery(&self) -> Result<ExecutionLock, RaxError> {
        let path = self.workspace_root.join("execution.lock");
        let file = open_lock_file(&path, ErrorPhase::Recover, "workspace execution lock")?;
        match FileExt::try_lock_exclusive(&file) {
            Ok(()) => Ok(ExecutionLock { file }),
            Err(error) if is_lock_contended(&error) => Err(RaxError::new(
                ErrorCode::OperationFailed,
                ErrorPhase::Recover,
                true,
                "workspace execution is active; recovery must wait for completion",
            )
            .with_detail("path", path.to_string_lossy().to_string())),
            Err(error) => Err(io_error(
                ErrorPhase::Recover,
                "failed to acquire workspace execution lock",
                Some(&path),
                error,
            )),
        }
    }

    fn summarize_run(directory: &Path) -> Result<RunSummary, RaxError> {
        let run_id = directory
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| journal_error("run directory name is not UTF-8"))?
            .to_string();
        let request: RequestEnvelope = read_json(&directory.join("request.json"))?;
        let plan: Plan = read_json(&directory.join("plan.json"))?;
        let events: Vec<EventEnvelope> = read_ndjson(&directory.join("events.ndjson"))?;
        let result: Option<ResultEnvelope> = read_optional_json(&directory.join("result.json"))?;
        let recovery = read_recovery_record(directory)?;
        let state = reconcile_run_evidence(&run_id, &request, &plan, &events, result.as_ref())?;
        let recovery_required = recovery.is_none() && result_requires_recovery(result.as_ref());
        let request_id = request.request_id.clone().ok_or_else(|| {
            RaxError::new(
                ErrorCode::InvariantViolation,
                ErrorPhase::Record,
                false,
                "recorded request is missing requestId",
            )
        })?;
        Ok(RunSummary {
            run_id,
            request_id,
            operation: request.operation.operation_name().to_string(),
            state: state.unwrap_or(RunState::Abandoned),
            result_present: result.is_some(),
            recovery_required,
            resolved: recovery.is_some(),
            corrupt: false,
            diagnostic: None,
        })
    }
}

#[derive(Debug)]
pub(crate) struct ExecutionLock {
    file: File,
}

impl Drop for ExecutionLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

#[derive(Debug)]
struct RecoveryLock {
    file: File,
}

impl Drop for RecoveryLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

fn validate_directory_boundary(
    path: &Path,
    phase: ErrorPhase,
    label: &str,
) -> Result<PathBuf, RaxError> {
    let metadata = std::fs::symlink_metadata(path).map_err(|error| {
        io_error(
            phase,
            format!("failed to access {label}"),
            Some(path),
            error,
        )
    })?;
    if metadata.file_type().is_symlink() {
        return Err(RaxError::new(
            ErrorCode::SymlinkNotAllowed,
            phase,
            false,
            format!("{label} must not be a symbolic link"),
        )
        .with_detail("path", path.to_string_lossy().to_string()));
    }
    if !metadata.is_dir() {
        return Err(RaxError::new(
            ErrorCode::NotADirectory,
            phase,
            false,
            format!("{label} must be a directory"),
        )
        .with_detail("path", path.to_string_lossy().to_string()));
    }
    let canonical = std::fs::canonicalize(path).map_err(|error| {
        io_error(
            phase,
            format!("failed to canonicalize {label}"),
            Some(path),
            error,
        )
    })?;
    if canonical != path {
        return Err(RaxError::new(
            ErrorCode::SymlinkNotAllowed,
            phase,
            false,
            format!("{label} contains a symbolic-link boundary"),
        )
        .with_detail("path", path.to_string_lossy().to_string()));
    }
    Ok(canonical)
}

fn reject_preexisting_symlink(path: &Path, phase: ErrorPhase, label: &str) -> Result<(), RaxError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(RaxError::new(
            ErrorCode::SymlinkNotAllowed,
            phase,
            false,
            format!("{label} must not be a symbolic link"),
        )
        .with_detail("path", path.to_string_lossy().to_string())),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_error(
            phase,
            format!("failed to inspect {label}"),
            Some(path),
            error,
        )),
    }
}

fn validate_optional_regular_file(
    path: &Path,
    phase: ErrorPhase,
    label: &str,
) -> Result<bool, RaxError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(RaxError::new(
            ErrorCode::SymlinkNotAllowed,
            phase,
            false,
            format!("{label} must not be a symbolic link"),
        )
        .with_detail("path", path.to_string_lossy().to_string())),
        Ok(metadata) if !metadata.is_file() => Err(RaxError::new(
            ErrorCode::NotAFile,
            phase,
            false,
            format!("{label} must be a regular file"),
        )
        .with_detail("path", path.to_string_lossy().to_string())),
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(io_error(
            phase,
            format!("failed to inspect {label}"),
            Some(path),
            error,
        )),
    }
}

fn validate_required_regular_file(
    path: &Path,
    phase: ErrorPhase,
    label: &str,
) -> Result<(), RaxError> {
    if validate_optional_regular_file(path, phase, label)? {
        Ok(())
    } else {
        Err(RaxError::new(
            ErrorCode::NotFound,
            phase,
            false,
            format!("{label} disappeared while it was being opened"),
        )
        .with_detail("path", path.to_string_lossy().to_string()))
    }
}

fn open_lock_file(path: &Path, phase: ErrorPhase, label: &str) -> Result<File, RaxError> {
    let existed = validate_optional_regular_file(path, phase, label)?;
    let file = if existed {
        OpenOptions::new()
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)
    } else {
        OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)
    };
    let file = match file {
        Ok(file) => file,
        Err(error) if !existed && error.kind() == std::io::ErrorKind::AlreadyExists => {
            validate_required_regular_file(path, phase, label)?;
            OpenOptions::new()
                .read(true)
                .write(true)
                .truncate(false)
                .open(path)
                .map_err(|error| {
                    io_error(phase, format!("failed to open {label}"), Some(path), error)
                })?
        }
        Err(error) => {
            return Err(io_error(
                phase,
                format!("failed to open {label}"),
                Some(path),
                error,
            ));
        }
    };
    validate_required_regular_file(path, phase, label)?;
    set_private_file_handle_permissions(&file, path, phase, label)?;
    Ok(file)
}

fn validate_run_directory(
    runs: &Path,
    directory: &Path,
    phase: ErrorPhase,
) -> Result<PathBuf, RaxError> {
    let canonical = validate_directory_boundary(directory, phase, "run journal directory")?;
    if canonical.parent() != Some(runs) {
        return Err(RaxError::new(
            ErrorCode::SymlinkNotAllowed,
            phase,
            false,
            "run journal directory is outside the effect journal runs directory",
        )
        .with_detail("path", directory.to_string_lossy().to_string()));
    }
    Ok(canonical)
}

fn lock_recovery(directory: &Path) -> Result<RecoveryLock, RaxError> {
    let path = directory.join("recovery.lock");
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(RaxError::new(
                ErrorCode::SymlinkNotAllowed,
                ErrorPhase::Recover,
                false,
                "recovery lock must not be a symbolic link",
            )
            .with_detail("path", path.to_string_lossy().to_string()));
        }
        Ok(metadata) if !metadata.is_file() => {
            return Err(RaxError::new(
                ErrorCode::NotAFile,
                ErrorPhase::Recover,
                false,
                "recovery lock must be a regular file",
            )
            .with_detail("path", path.to_string_lossy().to_string()));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(io_error(
                ErrorPhase::Recover,
                "failed to inspect recovery lock",
                Some(&path),
                error,
            ));
        }
    }
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .map_err(|error| {
            io_error(
                ErrorPhase::Recover,
                "failed to open recovery lock",
                Some(&path),
                error,
            )
        })?;
    let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
        io_error(
            ErrorPhase::Recover,
            "failed to inspect recovery lock after opening",
            Some(&path),
            error,
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(RaxError::new(
            if metadata.file_type().is_symlink() {
                ErrorCode::SymlinkNotAllowed
            } else {
                ErrorCode::NotAFile
            },
            ErrorPhase::Recover,
            false,
            "recovery lock changed to an unsafe filesystem entry",
        )
        .with_detail("path", path.to_string_lossy().to_string()));
    }
    set_private_file_permissions(&path)?;
    acquire_exclusive(
        &file,
        &path,
        ErrorPhase::Recover,
        "recovery lock",
        DEFAULT_LOCK_WAIT,
        None,
    )?;
    Ok(RecoveryLock { file })
}

fn corrupt_run_summary(directory: &Path, error: &RaxError, resolved: bool) -> RunSummary {
    let run_id = directory.file_name().map_or_else(
        || "unknown".to_string(),
        |value| value.to_string_lossy().to_string(),
    );
    RunSummary {
        run_id,
        request_id: "unknown".to_string(),
        operation: "unknown".to_string(),
        state: RunState::Abandoned,
        result_present: false,
        recovery_required: !resolved,
        resolved,
        corrupt: true,
        diagnostic: Some(error.to_string()),
    }
}

#[derive(Debug)]
pub struct RunJournal {
    run_id: String,
    directory: PathBuf,
    receipt_path: String,
    events: File,
    sequence: u64,
    state: RunState,
}

impl RunJournal {
    #[must_use]
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    #[must_use]
    pub fn receipt_path(&self) -> String {
        self.receipt_path.clone()
    }

    #[must_use]
    pub const fn state(&self) -> RunState {
        self.state
    }

    pub fn append(&mut self, event: EventKind) -> Result<(), RaxError> {
        let next = transition(self.state, event).map_err(|error| {
            RaxError::new(
                ErrorCode::InvariantViolation,
                ErrorPhase::Record,
                false,
                error.to_string(),
            )
        })?;
        let next_sequence = self.sequence.checked_add(1).ok_or_else(|| {
            RaxError::new(
                ErrorCode::InvariantViolation,
                ErrorPhase::Record,
                false,
                "run event sequence overflowed",
            )
        })?;
        let envelope = EventEnvelope {
            schema_version: SCHEMA_VERSION.to_string(),
            run_id: self.run_id.clone(),
            sequence: next_sequence,
            timestamp: now_rfc3339()?,
            state: next,
            event,
            data: std::collections::BTreeMap::new(),
        };
        serde_json::to_writer(&mut self.events, &envelope)
            .map_err(|error| journal_error(format!("failed to encode run event: {error}")))?;
        self.events
            .write_all(b"\n")
            .and_then(|()| self.events.flush())
            .and_then(|()| self.events.sync_data())
            .map_err(|error| {
                io_error(
                    ErrorPhase::Record,
                    "failed to persist run event",
                    Some(&self.directory.join("events.ndjson")),
                    error,
                )
            })?;
        self.sequence = next_sequence;
        self.state = next;
        Ok(())
    }

    pub fn finish(&mut self, result: &ResultEnvelope) -> Result<(), RaxError> {
        atomic_write_json(&self.directory.join("result.json"), result)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RunSummary {
    pub run_id: String,
    pub request_id: String,
    pub operation: String,
    pub state: RunState,
    pub result_present: bool,
    pub recovery_required: bool,
    pub resolved: bool,
    pub corrupt: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RunRecord {
    pub run_id: String,
    pub request: RequestEnvelope,
    pub plan: Plan,
    pub events: Vec<EventEnvelope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<ResultEnvelope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery: Option<RecoveryRecord>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryResolution {
    VerifiedSucceeded,
    VerifiedFailed,
    ManualReviewRequired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RecoveryRecord {
    pub schema_version: String,
    pub run_id: String,
    pub resolution: RecoveryResolution,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub resolved_at: String,
}

pub(crate) fn atomic_write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), RaxError> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| journal_error(format!("failed to encode journal JSON: {error}")))?;
    atomic_write_bytes(path, &bytes)
}

pub(crate) fn create_new_json<T: Serialize>(path: &Path, value: &T) -> Result<(), RaxError> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| journal_error(format!("failed to encode journal JSON: {error}")))?;
    let parent = path
        .parent()
        .ok_or_else(|| journal_error("journal path has no parent"))?;
    create_private_dir_all(parent)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    let mut file = options.open(path).map_err(|error| {
        io_error(
            ErrorPhase::Record,
            "failed to create journal file",
            Some(path),
            error,
        )
    })?;
    set_private_file_permissions(path)?;
    if let Err(error) = file.write_all(&bytes).and_then(|()| file.sync_all()) {
        let cleanup = std::fs::remove_file(path)
            .err()
            .map(|error| error.to_string());
        let mut result = io_error(
            ErrorPhase::Record,
            "failed to persist journal file",
            Some(path),
            error,
        );
        if let Some(cleanup) = cleanup {
            result = result.with_detail("cleanupError", cleanup);
        }
        return Err(result);
    }
    drop(file);
    sync_parent_directory(parent)?;
    Ok(())
}

pub(crate) fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T, RaxError> {
    let bytes = read_bounded_file(path, MAXIMUM_JOURNAL_JSON_BYTES, ErrorPhase::Record)?;
    serde_json::from_slice(&bytes)
        .map_err(|error| journal_error(format!("failed to decode {}: {error}", path.display())))
}

fn validate_state_path_syntax(path: &Path) -> Result<(), RaxError> {
    use std::path::Component;

    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(RaxError::new(
            ErrorCode::InvalidPath,
            ErrorPhase::Validate,
            false,
            "state directory must not contain parent-directory components",
        )
        .with_detail("path", path.to_string_lossy().to_string()));
    }
    Ok(())
}

fn resolve_path_with_missing_tail(path: &Path) -> Result<PathBuf, RaxError> {
    let mut cursor = path;
    let mut tail = Vec::new();
    loop {
        match std::fs::canonicalize(cursor) {
            Ok(mut resolved) => {
                for component in tail.iter().rev() {
                    resolved.push(component);
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let leaf = cursor.file_name().ok_or_else(|| {
                    io_error(
                        ErrorPhase::Validate,
                        "failed to resolve state directory ancestor",
                        Some(path),
                        error,
                    )
                })?;
                tail.push(leaf.to_os_string());
                cursor = cursor
                    .parent()
                    .ok_or_else(|| journal_error("state directory has no existing ancestor"))?;
            }
            Err(error) => {
                return Err(io_error(
                    ErrorPhase::Validate,
                    "failed to resolve state directory ancestor",
                    Some(cursor),
                    error,
                ));
            }
        }
    }
}

fn default_state_root() -> Result<PathBuf, RaxError> {
    if let Some(value) = std::env::var_os("RAX_STATE_DIR") {
        if value.is_empty() {
            return Err(journal_error("RAX_STATE_DIR must not be empty"));
        }
        return Ok(PathBuf::from(value));
    }
    let base = BaseDirs::new().ok_or_else(|| {
        journal_error("could not determine the operating-system local data directory")
    })?;
    Ok(base.data_local_dir().join("rax"))
}

fn atomic_write_bytes(path: &Path, bytes: &[u8]) -> Result<(), RaxError> {
    let parent = path
        .parent()
        .ok_or_else(|| journal_error("journal path has no parent"))?;
    create_private_dir_all(parent)?;
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| journal_error("journal file name is not UTF-8"))?;
    let temporary = parent.join(format!(".{name}.tmp-{}", Uuid::now_v7().simple()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    let mut file = options.open(&temporary).map_err(|error| {
        io_error(
            ErrorPhase::Record,
            "failed to create journal temporary file",
            Some(&temporary),
            error,
        )
    })?;
    set_private_file_permissions(&temporary)?;
    if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
        let cleanup = std::fs::remove_file(&temporary)
            .err()
            .map(|error| error.to_string());
        let mut result = io_error(
            ErrorPhase::Record,
            "failed to persist journal temporary file",
            Some(&temporary),
            error,
        );
        if let Some(cleanup) = cleanup {
            result = result.with_detail("cleanupError", cleanup);
        }
        return Err(result);
    }
    drop(file);
    std::fs::rename(&temporary, path).map_err(|error| {
        let cleanup = std::fs::remove_file(&temporary)
            .err()
            .map(|error| error.to_string());
        let mut result = io_error(
            ErrorPhase::Record,
            "failed to atomically replace journal file",
            Some(path),
            error,
        );
        if let Some(cleanup) = cleanup {
            result = result.with_detail("cleanupError", cleanup);
        }
        result
    })?;
    sync_parent_directory(parent)?;
    Ok(())
}

fn private_open_append(path: &Path) -> Result<File, RaxError> {
    let file = OpenOptions::new()
        .create_new(true)
        .append(true)
        .open(path)
        .map_err(|error| {
            io_error(
                ErrorPhase::Record,
                "failed to create event journal",
                Some(path),
                error,
            )
        })?;
    set_private_file_permissions(path)?;
    Ok(file)
}

fn journal_file_metadata(path: &Path, phase: ErrorPhase) -> Result<std::fs::Metadata, RaxError> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| io_error(phase, "failed to inspect journal file", Some(path), error))?;
    if metadata.file_type().is_symlink() {
        return Err(RaxError::new(
            ErrorCode::SymlinkNotAllowed,
            phase,
            false,
            "journal files must not be symbolic links",
        )
        .with_detail("path", path.to_string_lossy().to_string()));
    }
    if !metadata.is_file() {
        return Err(RaxError::new(
            ErrorCode::NotAFile,
            phase,
            false,
            "journal path must be a regular file",
        )
        .with_detail("path", path.to_string_lossy().to_string()));
    }
    Ok(metadata)
}

fn oversized_journal_input(path: &Path, label: &str, maximum: u64) -> RaxError {
    RaxError::new(
        ErrorCode::LimitExceeded,
        ErrorPhase::Record,
        false,
        format!("{label} exceeds {maximum} bytes"),
    )
    .with_detail("path", path.to_string_lossy().to_string())
    .with_detail("maximumBytes", maximum)
}

fn read_bounded_file(
    path: &Path,
    maximum_bytes: u64,
    phase: ErrorPhase,
) -> Result<Vec<u8>, RaxError> {
    let metadata = journal_file_metadata(path, phase)?;
    if metadata.len() > maximum_bytes {
        return Err(oversized_journal_input(path, "journal JSON", maximum_bytes));
    }
    let capacity = usize::try_from(metadata.len()).map_err(|_| {
        RaxError::new(
            ErrorCode::LimitExceeded,
            phase,
            false,
            "journal file length does not fit the host allocation size",
        )
        .with_detail("path", path.to_string_lossy().to_string())
    })?;
    let file = File::open(path)
        .map_err(|error| io_error(phase, "failed to open journal file", Some(path), error))?;
    let mut bytes = Vec::with_capacity(capacity);
    file.take(maximum_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| io_error(phase, "failed to read journal file", Some(path), error))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > maximum_bytes {
        return Err(oversized_journal_input(path, "journal JSON", maximum_bytes));
    }
    Ok(bytes)
}

fn read_optional_json<T: DeserializeOwned>(path: &Path) -> Result<Option<T>, RaxError> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => {
            let bytes = read_bounded_file(path, MAXIMUM_JOURNAL_JSON_BYTES, ErrorPhase::Record)?;
            serde_json::from_slice(&bytes).map(Some).map_err(|error| {
                journal_error(format!("failed to decode {}: {error}", path.display()))
            })
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(io_error(
            ErrorPhase::Record,
            "failed to read optional journal file",
            Some(path),
            error,
        )),
    }
}

fn read_recovery_record(directory: &Path) -> Result<Option<RecoveryRecord>, RaxError> {
    let Some(record) = read_optional_json::<RecoveryRecord>(&directory.join("recovery.json"))?
    else {
        return Ok(None);
    };
    let run_id = directory
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| journal_error("run directory name is not UTF-8"))?;
    if record.schema_version != SCHEMA_VERSION || record.run_id != run_id {
        return Err(
            journal_error("recovery identity does not match its recorded run")
                .with_detail("runId", run_id.to_string()),
        );
    }
    Ok(Some(record))
}

fn read_ndjson<T: DeserializeOwned>(path: &Path) -> Result<Vec<T>, RaxError> {
    read_ndjson_with_limits(
        path,
        MAXIMUM_NDJSON_TOTAL_BYTES,
        MAXIMUM_NDJSON_LINE_BYTES,
        MAXIMUM_NDJSON_EVENTS,
    )
}

fn read_ndjson_with_limits<T: DeserializeOwned>(
    path: &Path,
    maximum_total_bytes: u64,
    maximum_line_bytes: u64,
    maximum_events: usize,
) -> Result<Vec<T>, RaxError> {
    let _metadata = journal_file_metadata(path, ErrorPhase::Record)?;
    let file = File::open(path).map_err(|error| {
        io_error(
            ErrorPhase::Record,
            "failed to open NDJSON journal",
            Some(path),
            error,
        )
    })?;
    let mut reader = BufReader::new(file);
    let mut output = Vec::new();
    let mut total_bytes = 0_u64;
    let mut line = Vec::new();
    loop {
        line.clear();
        let mut complete = false;
        while !complete {
            let available = reader.fill_buf().map_err(|error| {
                io_error(
                    ErrorPhase::Record,
                    "failed to read NDJSON journal",
                    Some(path),
                    error,
                )
            })?;
            if available.is_empty() {
                if line.is_empty() {
                    return Ok(output);
                }
                complete = true;
                continue;
            }
            let take = available
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(available.len(), |index| index + 1);
            let next_total = total_bytes
                .checked_add(u64::try_from(take).unwrap_or(u64::MAX))
                .ok_or_else(|| journal_error("NDJSON journal byte count overflowed"))?;
            if next_total > maximum_total_bytes {
                return Err(oversized_journal_input(
                    path,
                    "NDJSON journal total",
                    maximum_total_bytes,
                ));
            }
            let next_line = line
                .len()
                .checked_add(take)
                .ok_or_else(|| journal_error("NDJSON journal line length overflowed"))?;
            if u64::try_from(next_line).unwrap_or(u64::MAX) > maximum_line_bytes {
                return Err(oversized_journal_input(
                    path,
                    "NDJSON journal line",
                    maximum_line_bytes,
                ));
            }
            line.extend_from_slice(&available[..take]);
            reader.consume(take);
            total_bytes = next_total;
            complete = line.last() == Some(&b'\n');
        }
        if output.len() >= maximum_events {
            return Err(RaxError::new(
                ErrorCode::LimitExceeded,
                ErrorPhase::Record,
                false,
                format!("NDJSON journal exceeds {maximum_events} events"),
            )
            .with_detail("path", path.to_string_lossy().to_string()));
        }
        if line.last() == Some(&b'\n') {
            line.pop();
        }
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        let index = output.len() + 1;
        output.push(serde_json::from_slice(&line).map_err(|error| {
            journal_error(format!(
                "failed to decode {} line {index}: {error}",
                path.display()
            ))
        })?);
    }
}

fn read_directories(path: &Path) -> Result<Vec<PathBuf>, RaxError> {
    validate_directory_boundary(path, ErrorPhase::Record, "effect journal runs directory")?;
    let iterator = std::fs::read_dir(path).map_err(|error| {
        io_error(
            ErrorPhase::Record,
            "failed to list journal directory",
            Some(path),
            error,
        )
    })?;
    let mut output = Vec::new();
    let mut scanned = 0_usize;
    for entry in iterator {
        let entry = entry.map_err(|error| {
            io_error(
                ErrorPhase::Record,
                "failed to inspect journal entry",
                Some(path),
                error,
            )
        })?;
        let file_type = entry.file_type().map_err(|error| {
            io_error(
                ErrorPhase::Record,
                "failed to inspect journal entry type",
                Some(&entry.path()),
                error,
            )
        })?;
        if file_type.is_symlink() {
            return Err(RaxError::new(
                ErrorCode::SymlinkNotAllowed,
                ErrorPhase::Record,
                false,
                "effect journal contains a symbolic link entry",
            )
            .with_detail("path", entry.path().to_string_lossy().to_string()));
        }
        scanned = scanned
            .checked_add(1)
            .ok_or_else(|| journal_error("effect journal scan overflowed"))?;
        if scanned > MAXIMUM_RUN_SCAN_ENTRIES {
            return Err(RaxError::new(
                ErrorCode::OperationFailed,
                ErrorPhase::Record,
                true,
                format!("effect journal scan exceeded {MAXIMUM_RUN_SCAN_ENTRIES} run directories"),
            ));
        }
        if file_type.is_dir() {
            output.push(entry.path());
        }
    }
    Ok(output)
}

fn validate_identifier(value: &str) -> Result<(), RaxError> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(RaxError::new(
            ErrorCode::InvalidRequest,
            ErrorPhase::Validate,
            false,
            "identifier contains prohibited characters",
        )
        .with_detail("identifier", value.to_string()));
    }
    Ok(())
}

fn create_private_dir_all(path: &Path) -> Result<(), RaxError> {
    reject_preexisting_symlink(path, ErrorPhase::Record, "state directory")?;
    std::fs::create_dir_all(path).map_err(|error| {
        io_error(
            ErrorPhase::Record,
            "failed to create state directory",
            Some(path),
            error,
        )
    })?;
    set_private_directory_permissions(path)
}

#[cfg(unix)]
fn set_private_directory_permissions(path: &Path) -> Result<(), RaxError> {
    use std::os::unix::fs::PermissionsExt;
    reject_preexisting_symlink(path, ErrorPhase::Record, "state directory")?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).map_err(|error| {
        io_error(
            ErrorPhase::Record,
            "failed to secure state directory",
            Some(path),
            error,
        )
    })
}

#[cfg(not(unix))]
fn set_private_directory_permissions(path: &Path) -> Result<(), RaxError> {
    reject_preexisting_symlink(path, ErrorPhase::Record, "state directory")
}

#[cfg(unix)]
fn set_private_file_permissions(path: &Path) -> Result<(), RaxError> {
    use std::os::unix::fs::PermissionsExt;
    reject_preexisting_symlink(path, ErrorPhase::Record, "state file")?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(|error| {
        io_error(
            ErrorPhase::Record,
            "failed to secure state file",
            Some(path),
            error,
        )
    })
}

#[cfg(not(unix))]
fn set_private_file_permissions(path: &Path) -> Result<(), RaxError> {
    reject_preexisting_symlink(path, ErrorPhase::Record, "state file")
}

#[cfg(unix)]
fn set_private_file_handle_permissions(
    file: &File,
    path: &Path,
    phase: ErrorPhase,
    label: &str,
) -> Result<(), RaxError> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
        .map_err(|error| {
            io_error(
                phase,
                format!("failed to secure {label}"),
                Some(path),
                error,
            )
        })
}

#[cfg(not(unix))]
fn set_private_file_handle_permissions(
    _file: &File,
    _path: &Path,
    _phase: ErrorPhase,
    _label: &str,
) -> Result<(), RaxError> {
    Ok(())
}

#[cfg(unix)]
fn sync_parent_directory(path: &Path) -> Result<(), RaxError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| {
            io_error(
                ErrorPhase::Record,
                "failed to sync journal parent directory",
                Some(path),
                error,
            )
        })
}

#[cfg(not(unix))]
fn sync_parent_directory(_path: &Path) -> Result<(), RaxError> {
    Ok(())
}

fn reconcile_run_evidence(
    run_id: &str,
    request: &RequestEnvelope,
    plan: &Plan,
    events: &[EventEnvelope],
    result: Option<&ResultEnvelope>,
) -> Result<Option<RunState>, RaxError> {
    if request.schema_version != SCHEMA_VERSION || plan.schema_version != SCHEMA_VERSION {
        return Err(journal_error(
            "run evidence uses an unsupported schema version",
        ));
    }
    let request_id = request.request_id.as_deref().ok_or_else(|| {
        RaxError::new(
            ErrorCode::InvariantViolation,
            ErrorPhase::Record,
            false,
            "recorded request is missing requestId",
        )
    })?;
    if plan.request_id != request_id || plan.operation != request.operation {
        return Err(
            journal_error("recorded request and plan do not describe the same operation")
                .with_detail("runId", run_id.to_string()),
        );
    }
    if !plan.verify_fingerprint().map_err(|error| {
        journal_error(format!(
            "failed to verify recorded plan fingerprint: {error}"
        ))
    })? {
        return Err(journal_error("recorded plan fingerprint is invalid")
            .with_detail("runId", run_id.to_string()));
    }

    let mut event_state = None;
    let mut state = RunState::Received;
    for (index, event) in events.iter().enumerate() {
        let expected_sequence = u64::try_from(index)
            .unwrap_or(u64::MAX)
            .checked_add(1)
            .ok_or_else(|| journal_error("run event sequence overflowed"))?;
        if event.schema_version != SCHEMA_VERSION
            || event.run_id != run_id
            || event.sequence != expected_sequence
        {
            return Err(journal_error("run event identity or sequence is invalid")
                .with_detail("runId", run_id.to_string())
                .with_detail("sequence", event.sequence));
        }
        let next = transition(state, event.event).map_err(|error| {
            RaxError::new(
                ErrorCode::InvariantViolation,
                ErrorPhase::Record,
                false,
                format!("recorded run event is not a valid transition: {error}"),
            )
            .with_detail("runId", run_id.to_string())
            .with_detail("sequence", event.sequence)
        })?;
        if event.state != next {
            return Err(
                journal_error("recorded event state does not match its transition")
                    .with_detail("runId", run_id.to_string())
                    .with_detail("sequence", event.sequence),
            );
        }
        state = next;
        event_state = Some(next);
    }

    let Some(result) = result else {
        return Ok(event_state);
    };
    if result.schema_version != SCHEMA_VERSION
        || result.run_id != run_id
        || result.request_id != request_id
        || result.operation != request.operation.operation_name()
    {
        return Err(
            journal_error("result identity does not match its recorded request")
                .with_detail("runId", run_id.to_string()),
        );
    }
    let result_state = result_state(Some(result)).ok_or_else(|| {
        RaxError::new(
            ErrorCode::InvariantViolation,
            ErrorPhase::Record,
            false,
            "recorded result has no lifecycle state",
        )
    })?;
    if event_state == Some(result_state) || record_failure_bridge(event_state, result) {
        return Ok(Some(result_state));
    }

    Err(
        journal_error("event journal and terminal result contradict each other")
            .with_detail("runId", run_id.to_string())
            .with_detail(
                "eventState",
                event_state.map_or_else(|| "none".to_string(), |value| format!("{value:?}")),
            )
            .with_detail("resultState", format!("{result_state:?}")),
    )
}

fn record_failure_bridge(event_state: Option<RunState>, result: &ResultEnvelope) -> bool {
    let Outcome::PartiallyApplied { error, .. } = &result.outcome else {
        return false;
    };
    if error.phase != ErrorPhase::Record {
        return false;
    }

    if error
        .details
        .get("grantConsumed")
        .and_then(serde_json::Value::as_bool)
        == Some(true)
    {
        return matches!(event_state, Some(RunState::Planned | RunState::Ready));
    }

    let Some(original) = error
        .details
        .get("originalOutcome")
        .and_then(serde_json::Value::as_str)
    else {
        return false;
    };
    if !is_terminal_outcome_name(original) {
        return false;
    }
    match event_state {
        Some(RunState::Running) => matches!(
            original,
            "succeeded" | "failed" | "cancelled" | "partially_applied"
        ),
        Some(state) => outcome_state_name(state) == Some(original),
        None => false,
    }
}

fn is_terminal_outcome_name(value: &str) -> bool {
    matches!(
        value,
        "succeeded" | "failed" | "cancelled" | "denied" | "expired" | "partially_applied"
    )
}

const fn outcome_state_name(state: RunState) -> Option<&'static str> {
    match state {
        RunState::Succeeded => Some("succeeded"),
        RunState::Failed => Some("failed"),
        RunState::Cancelled => Some("cancelled"),
        RunState::Denied => Some("denied"),
        RunState::Expired => Some("expired"),
        RunState::PartiallyApplied => Some("partially_applied"),
        _ => None,
    }
}

fn result_requires_recovery(result: Option<&ResultEnvelope>) -> bool {
    match result {
        None => true,
        Some(result) => matches!(&result.outcome, Outcome::PartiallyApplied { .. }),
    }
}

fn result_state(result: Option<&ResultEnvelope>) -> Option<RunState> {
    result.map(|result| match &result.outcome {
        rax_core::Outcome::Succeeded { .. } => RunState::Succeeded,
        rax_core::Outcome::ApprovalRequired { .. } => RunState::AwaitingApproval,
        rax_core::Outcome::Failed { .. } => RunState::Failed,
        rax_core::Outcome::Cancelled { .. } => RunState::Cancelled,
        rax_core::Outcome::Denied { .. } => RunState::Denied,
        rax_core::Outcome::Expired { .. } => RunState::Expired,
        rax_core::Outcome::PartiallyApplied { .. } => RunState::PartiallyApplied,
    })
}

fn journal_error(message: impl Into<String>) -> RaxError {
    RaxError::new(ErrorCode::JournalError, ErrorPhase::Record, false, message)
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, fs, time::Duration};

    use rax_core::{
        ContentEncoding, ErrorCode, ErrorPhase, FsReadInput, Operation, OperationOutput, Outcome,
        Plan, RaxError, RequestEnvelope, ResultEnvelope, RunState, SCHEMA_VERSION,
    };

    use super::{
        StateStore, corrupt_run_summary, read_bounded_file, read_ndjson_with_limits,
        record_failure_bridge, result_requires_recovery,
    };
    use crate::{CancellationToken, Workspace};

    fn result(outcome: Outcome) -> ResultEnvelope {
        ResultEnvelope {
            schema_version: SCHEMA_VERSION.to_string(),
            request_id: "request".to_string(),
            run_id: "run".to_string(),
            operation: "fs_read".to_string(),
            started_at: "2026-08-03T00:00:00Z".to_string(),
            finished_at: "2026-08-03T00:00:00Z".to_string(),
            duration_ms: 0,
            outcome,
        }
    }

    #[test]
    fn resolved_corrupt_run_is_not_relisted_for_recovery() {
        let summary = corrupt_run_summary(
            std::path::Path::new("run"),
            &RaxError::new(
                ErrorCode::InvariantViolation,
                ErrorPhase::Record,
                false,
                "corrupt evidence",
            ),
            true,
        );
        assert!(summary.corrupt);
        assert!(summary.resolved);
        assert!(!summary.recovery_required);
    }

    fn assert_invalid_recovery_identity_is_unresolved(
        corrupt_request: bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        for invalid_schema in [false, true] {
            let root = tempfile::tempdir()?;
            let workspace_path = root.path().join("workspace");
            fs::create_dir(&workspace_path)?;
            let workspace = Workspace::open(&workspace_path)?;
            let store = StateStore::open(&workspace, Some(&root.path().join("state")))?;
            let operation = Operation::FsRead(FsReadInput {
                path: "message.txt".to_string(),
                max_bytes: 1024,
            });
            let request = RequestEnvelope {
                schema_version: SCHEMA_VERSION.to_string(),
                request_id: Some("request".to_string()),
                operation: operation.clone(),
                record: true,
            };
            let mut plan = Plan {
                schema_version: SCHEMA_VERSION.to_string(),
                plan_id: "plan".to_string(),
                request_id: "request".to_string(),
                workspace: workspace.binding(),
                operation,
                effects: Vec::new(),
                preconditions: Vec::new(),
                observations: BTreeMap::new(),
                created_at: "2026-08-25T00:00:00Z".to_string(),
                expires_at: "2026-08-26T00:00:00Z".to_string(),
                fingerprint: String::new(),
            };
            plan.fingerprint = plan.compute_fingerprint()?;
            let journal = store.begin_run(&request, &plan)?;
            let run_id = journal.run_id().to_string();
            drop(journal);
            let directory = store.workspace_root().join("runs").join(&run_id);
            if corrupt_request {
                fs::write(directory.join("request.json"), b"{")?;
            }
            let recovery = super::RecoveryRecord {
                schema_version: if invalid_schema {
                    "unsupported".to_string()
                } else {
                    SCHEMA_VERSION.to_string()
                },
                run_id: if invalid_schema {
                    run_id.clone()
                } else {
                    "different-run".to_string()
                },
                resolution: super::RecoveryResolution::ManualReviewRequired,
                note: None,
                resolved_at: "2026-08-26T00:00:00Z".to_string(),
            };
            let bytes = serde_json::to_vec(&recovery)?;
            let path = directory.join("recovery.json");
            fs::write(&path, &bytes)?;

            let summaries = store.list_runs()?;
            assert_eq!(summaries.len(), 1);
            assert!(
                !summaries[0].resolved,
                "invalid recovery must not resolve a run"
            );
            assert!(summaries[0].recovery_required);
            assert!(summaries[0].corrupt);
            assert_eq!(store.list_recovery()?.len(), 1);
            assert!(store.show_run(&run_id).is_err());
            assert_eq!(fs::read(&path)?, bytes);

            fs::remove_file(&path)?;
            let resolved = store.resolve_recovery(
                &run_id,
                super::RecoveryResolution::ManualReviewRequired,
                None,
            )?;
            assert!(store.list_runs()?[0].resolved);
            assert_eq!(store.list_recovery()?.len(), 0);
            if !corrupt_request {
                assert_eq!(store.show_run(&run_id)?.recovery, Some(resolved));
            }
        }
        Ok(())
    }

    #[test]
    fn invalid_recovery_identity_cannot_hide_missing_result()
    -> Result<(), Box<dyn std::error::Error>> {
        assert_invalid_recovery_identity_is_unresolved(false)
    }

    #[test]
    fn invalid_recovery_identity_cannot_hide_corrupt_run() -> Result<(), Box<dyn std::error::Error>>
    {
        assert_invalid_recovery_identity_is_unresolved(true)
    }

    #[test]
    fn missing_and_partially_applied_results_require_recovery() {
        assert!(result_requires_recovery(None));
        let partial = result(Outcome::PartiallyApplied {
            error: RaxError::new(
                ErrorCode::OperationFailed,
                ErrorPhase::Apply,
                false,
                "side effects require inspection",
            ),
            receipt_path: None,
        });
        assert!(result_requires_recovery(Some(&partial)));
    }

    #[test]
    fn record_failure_bridge_accepts_only_known_terminal_outcomes() {
        let partial = result(Outcome::PartiallyApplied {
            error: RaxError::new(
                ErrorCode::JournalError,
                ErrorPhase::Record,
                false,
                "terminal event could not be recorded",
            )
            .with_detail("originalOutcome", "succeeded"),
            receipt_path: None,
        });
        assert!(record_failure_bridge(Some(RunState::Running), &partial));

        let corrupt = result(Outcome::PartiallyApplied {
            error: RaxError::new(
                ErrorCode::JournalError,
                ErrorPhase::Record,
                false,
                "terminal event could not be recorded",
            )
            .with_detail("originalOutcome", "invented_outcome"),
            receipt_path: None,
        });
        assert!(!record_failure_bridge(Some(RunState::Running), &corrupt));
    }

    #[test]
    fn running_state_rejects_pre_execution_terminal_outcomes() {
        for original in ["denied", "expired"] {
            let partial = result(Outcome::PartiallyApplied {
                error: RaxError::new(
                    ErrorCode::JournalError,
                    ErrorPhase::Record,
                    false,
                    "terminal event could not be recorded",
                )
                .with_detail("originalOutcome", original),
                receipt_path: None,
            });
            assert!(!record_failure_bridge(Some(RunState::Running), &partial));
        }
    }

    #[test]
    fn grant_consumption_bridge_is_limited_to_pre_authorized_states() {
        let partial = result(Outcome::PartiallyApplied {
            error: RaxError::new(
                ErrorCode::JournalError,
                ErrorPhase::Record,
                false,
                "authorized event could not be recorded",
            )
            .with_detail("grantConsumed", true),
            receipt_path: None,
        });
        assert!(record_failure_bridge(Some(RunState::Planned), &partial));
        assert!(record_failure_bridge(Some(RunState::Ready), &partial));
        assert!(!record_failure_bridge(Some(RunState::Received), &partial));
        assert!(!record_failure_bridge(Some(RunState::Running), &partial));
    }

    #[test]
    fn non_record_failures_cannot_bridge_conflicting_evidence() {
        let partial = result(Outcome::PartiallyApplied {
            error: RaxError::new(
                ErrorCode::OperationFailed,
                ErrorPhase::Apply,
                false,
                "mutation requires inspection",
            )
            .with_detail("originalOutcome", "succeeded"),
            receipt_path: None,
        });
        assert!(!record_failure_bridge(Some(RunState::Running), &partial));
    }

    #[test]
    fn successful_result_does_not_require_recovery() {
        let success = result(Outcome::Succeeded {
            output: OperationOutput::GitDiff(rax_core::GitTextOutput {
                encoding: ContentEncoding::Utf8,
                text: String::new(),
                bytes: 0,
                truncated: false,
            }),
            receipt_path: None,
        });
        assert!(!result_requires_recovery(Some(&success)));
    }

    #[test]
    fn recovery_resolution_rejects_an_active_workspace_execution()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let workspace_path = root.path().join("workspace");
        let state_path = root.path().join("state");
        fs::create_dir(&workspace_path)?;
        let workspace = Workspace::open(&workspace_path)?;
        let store = StateStore::open(&workspace, Some(&state_path))?;
        fs::create_dir(store.workspace_root().join("runs/run-active"))?;
        let cancellation = CancellationToken::default();
        let _execution = store.lock_execution(&cancellation, Duration::from_secs(1))?;

        let error = store
            .resolve_recovery(
                "run-active",
                super::RecoveryResolution::ManualReviewRequired,
                None,
            )
            .expect_err("recovery must not resolve while execution is active");
        assert_eq!(error.code, ErrorCode::OperationFailed);
        assert!(error.message.contains("execution is active"));
        Ok(())
    }

    #[test]
    fn recovery_inputs_are_bounded_before_decode_or_write() -> Result<(), Box<dyn std::error::Error>>
    {
        let root = tempfile::tempdir()?;
        let json_path = root.path().join("oversized.json");
        fs::write(&json_path, b"{}")?;
        let json_error = read_bounded_file(&json_path, 1, ErrorPhase::Record)
            .expect_err("oversized JSON must be rejected");
        assert_eq!(json_error.code, ErrorCode::LimitExceeded);

        let event_path = root.path().join("oversized.ndjson");
        fs::write(&event_path, b"{}\n{}\n")?;
        let line_error = read_ndjson_with_limits::<serde_json::Value>(&event_path, 100, 1, 10)
            .expect_err("oversized NDJSON line must be rejected");
        assert_eq!(line_error.code, ErrorCode::LimitExceeded);
        let total_error = read_ndjson_with_limits::<serde_json::Value>(&event_path, 3, 100, 10)
            .expect_err("oversized NDJSON stream must be rejected");
        assert_eq!(total_error.code, ErrorCode::LimitExceeded);
        let event_error = read_ndjson_with_limits::<serde_json::Value>(&event_path, 100, 100, 1)
            .expect_err("oversized NDJSON event count must be rejected");
        assert_eq!(event_error.code, ErrorCode::LimitExceeded);

        let workspace_path = root.path().join("workspace");
        fs::create_dir(&workspace_path)?;
        let workspace = Workspace::open(&workspace_path)?;
        let state_path = root.path().join("state");
        let store = StateStore::open(&workspace, Some(&state_path))?;
        let note_error = store
            .resolve_recovery(
                "run-note",
                super::RecoveryResolution::ManualReviewRequired,
                Some("x".repeat(super::MAXIMUM_RECOVERY_NOTE_BYTES + 1)),
            )
            .expect_err("oversized recovery note must be rejected");
        assert_eq!(note_error.code, ErrorCode::LimitExceeded);
        assert!(!store.workspace_root().join("runs/run-note").exists());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn recovery_listing_and_resolution_reject_symlink_boundaries()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir()?;
        let workspace_path = root.path().join("workspace");
        fs::create_dir(&workspace_path)?;
        let workspace = Workspace::open(&workspace_path)?;
        let state_path = root.path().join("state");
        let store = StateStore::open(&workspace, Some(&state_path))?;
        let runs = store.workspace_root().join("runs");

        let outside_runs = root.path().join("outside-runs");
        fs::create_dir(&outside_runs)?;
        fs::remove_dir(&runs)?;
        symlink(&outside_runs, &runs)?;
        let base_error = store
            .list_runs()
            .expect_err("symlinked runs base must fail");
        assert_eq!(base_error.code, ErrorCode::SymlinkNotAllowed);
        fs::remove_file(&runs)?;
        fs::create_dir(&runs)?;

        let outside_run = root.path().join("outside-run");
        fs::create_dir(&outside_run)?;
        symlink(&outside_run, runs.join("run-escape"))?;
        let child_error = store
            .show_run("run-escape")
            .expect_err("symlinked run directory must fail");
        assert_eq!(child_error.code, ErrorCode::SymlinkNotAllowed);
        let list_error = store
            .list_runs()
            .expect_err("symlinked run entry must fail");
        assert_eq!(list_error.code, ErrorCode::SymlinkNotAllowed);
        fs::remove_file(runs.join("run-escape"))?;

        let run = runs.join("run-lock");
        fs::create_dir(&run)?;
        let outside_lock = root.path().join("outside-lock");
        fs::write(&outside_lock, b"outside")?;
        symlink(&outside_lock, run.join("recovery.lock"))?;
        let lock_error = store
            .resolve_recovery(
                "run-lock",
                super::RecoveryResolution::ManualReviewRequired,
                None,
            )
            .expect_err("symlinked recovery lock must fail");
        assert_eq!(lock_error.code, ErrorCode::SymlinkNotAllowed);
        assert_eq!(fs::read(&outside_lock)?, b"outside");
        assert!(!run.join("recovery.json").exists());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn state_store_rejects_preexisting_plan_symlink_before_mutating_target()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let root = tempfile::tempdir()?;
        let workspace_path = root.path().join("workspace");
        let state_path = root.path().join("state");
        fs::create_dir(&workspace_path)?;
        let workspace = Workspace::open(&workspace_path)?;
        let state_workspace = state_path.join("workspaces").join(workspace.identity());
        fs::create_dir_all(&state_workspace)?;

        let outside_plans = root.path().join("outside-plans");
        fs::create_dir(&outside_plans)?;
        let outside_plan = outside_plans.join("sentinel");
        fs::write(&outside_plan, b"outside")?;
        fs::set_permissions(&outside_plans, fs::Permissions::from_mode(0o755))?;
        let before_mode = fs::metadata(&outside_plans)?.permissions().mode() & 0o777;
        symlink(&outside_plans, state_workspace.join("plans"))?;

        let error = StateStore::open(&workspace, Some(&state_path))
            .expect_err("pre-existing plans symlink must be rejected at state open");
        assert_eq!(error.code, ErrorCode::SymlinkNotAllowed);
        assert_eq!(fs::read(&outside_plan)?, b"outside");
        assert_eq!(
            fs::metadata(&outside_plans)?.permissions().mode() & 0o777,
            before_mode,
            "state open must not chmod a symlink target"
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn plan_write_rejects_plan_base_symlink_without_outside_content()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::{
            collections::BTreeMap,
            os::unix::fs::{PermissionsExt, symlink},
        };

        let root = tempfile::tempdir()?;
        let workspace_path = root.path().join("workspace");
        let state_path = root.path().join("state");
        fs::create_dir(&workspace_path)?;
        let workspace = Workspace::open(&workspace_path)?;
        let store = StateStore::open(&workspace, Some(&state_path))?;
        let plans = store.workspace_root().join("plans");
        fs::remove_dir(&plans)?;

        let outside_plans = root.path().join("outside-plans");
        fs::create_dir(&outside_plans)?;
        let outside_plan = outside_plans.join("plan-escape");
        fs::set_permissions(&outside_plans, fs::Permissions::from_mode(0o755))?;
        let before_mode = fs::metadata(&outside_plans)?.permissions().mode() & 0o777;
        symlink(&outside_plans, &plans)?;

        let plan = Plan {
            schema_version: SCHEMA_VERSION.to_string(),
            plan_id: "plan-escape".to_string(),
            request_id: "request".to_string(),
            workspace: workspace.binding(),
            operation: Operation::FsRead(FsReadInput {
                path: "message.txt".to_string(),
                max_bytes: 1024,
            }),
            effects: Vec::new(),
            preconditions: Vec::new(),
            observations: BTreeMap::new(),
            created_at: "2026-08-25T00:00:00Z".to_string(),
            expires_at: "2026-08-26T00:00:00Z".to_string(),
            fingerprint: String::new(),
        };
        let error = store
            .write_plan(&plan)
            .expect_err("plan write must reject a symlinked plans base");
        assert_eq!(error.code, ErrorCode::SymlinkNotAllowed);
        assert!(!outside_plan.exists(), "plan bytes must stay inside state");
        assert_eq!(
            fs::metadata(&outside_plans)?.permissions().mode() & 0o777,
            before_mode,
            "plan write must not chmod a symlink target"
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn execution_lock_symlink_is_rejected_without_mutating_target()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let root = tempfile::tempdir()?;
        let workspace_path = root.path().join("workspace");
        let state_path = root.path().join("state");
        fs::create_dir(&workspace_path)?;
        let workspace = Workspace::open(&workspace_path)?;
        let store = StateStore::open(&workspace, Some(&state_path))?;
        let lock_path = store.workspace_root().join("execution.lock");
        let outside_lock = root.path().join("outside-lock");
        fs::write(&outside_lock, b"outside")?;
        fs::set_permissions(&outside_lock, fs::Permissions::from_mode(0o644))?;
        let before_mode = fs::metadata(&outside_lock)?.permissions().mode() & 0o777;
        symlink(&outside_lock, &lock_path)?;

        let cancellation = CancellationToken::default();
        let error = store
            .lock_execution(&cancellation, Duration::from_millis(100))
            .expect_err("execution lock must reject a pre-existing symlink");
        assert_eq!(error.code, ErrorCode::SymlinkNotAllowed);
        assert_eq!(fs::read(&outside_lock)?, b"outside");
        assert_eq!(
            fs::metadata(&outside_lock)?.permissions().mode() & 0o777,
            before_mode,
            "lock acquisition must not chmod a symlink target"
        );

        let recovery_error = store
            .lock_execution_for_recovery()
            .expect_err("recovery lock must reject a pre-existing symlink");
        assert_eq!(recovery_error.code, ErrorCode::SymlinkNotAllowed);
        assert_eq!(fs::read(&outside_lock)?, b"outside");
        assert_eq!(
            fs::metadata(&outside_lock)?.permissions().mode() & 0o777,
            before_mode,
            "recovery lock must not chmod a symlink target"
        );
        Ok(())
    }
}
