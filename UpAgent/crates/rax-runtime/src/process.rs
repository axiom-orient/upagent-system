use std::{
    collections::BTreeMap,
    path::Path,
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use rax_core::{
    ErrorCode, ErrorPhase, ProcessOutput, ProcessRunInput, ProcessShellInput, RaxError,
};

use crate::{codec::encode_protocol_bytes, error::io_error, workspace::Workspace};

mod capture;
mod platform;

use capture::{ReaderHandle, join_reader, spawn_reader};
use platform::{ChildState, child_state, configure_process_group, shell_command, terminate_group};

const POLL_INTERVAL: Duration = Duration::from_millis(10);
const TERMINATION_GRACE: Duration = Duration::from_millis(300);
const NATURAL_DRAIN_GRACE: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Default)]
pub struct CancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl CancellationToken {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

#[derive(Debug)]
pub(crate) struct CapturedProcess {
    pub program: String,
    pub args: Vec<String>,
    pub status: ExitStatus,
    pub duration: Duration,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub stdout_bytes: u64,
    pub stderr_bytes: u64,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub timed_out: bool,
    pub cancelled: bool,
}

impl CapturedProcess {
    #[must_use]
    pub fn into_output(self) -> ProcessOutput {
        let (stdout, stdout_encoding) = encode_protocol_bytes(&self.stdout);
        let (stderr, stderr_encoding) = encode_protocol_bytes(&self.stderr);
        ProcessOutput {
            program: self.program,
            args: self.args,
            exit_code: self.status.code(),
            success: self.status.success() && !self.timed_out && !self.cancelled,
            timed_out: self.timed_out,
            cancelled: self.cancelled,
            duration_ms: u64::try_from(self.duration.as_millis()).unwrap_or(u64::MAX),
            stdout_encoding,
            stdout,
            stderr_encoding,
            stderr,
            stdout_bytes: self.stdout_bytes,
            stderr_bytes: self.stderr_bytes,
            stdout_truncated: self.stdout_truncated,
            stderr_truncated: self.stderr_truncated,
        }
    }
}

pub fn run(
    workspace: &Workspace,
    input: &ProcessRunInput,
    cancellation: &CancellationToken,
) -> Result<ProcessOutput, RaxError> {
    let cwd = workspace.absolute_existing_dir(input.cwd.as_deref().unwrap_or(""))?;
    run_raw(
        ProcessSpec {
            program: &input.program,
            args: &input.args,
            cwd: &cwd,
            environment: &input.env,
            clear_env: input.clear_env,
            timeout: Duration::from_millis(input.timeout_ms),
            max_output_bytes: input.max_output_bytes,
        },
        cancellation,
    )
    .map(CapturedProcess::into_output)
}

pub fn shell(
    workspace: &Workspace,
    input: &ProcessShellInput,
    cancellation: &CancellationToken,
) -> Result<ProcessOutput, RaxError> {
    let cwd = workspace.absolute_existing_dir(input.cwd.as_deref().unwrap_or(""))?;
    let (program, args) = shell_command(&input.command);
    run_raw(
        ProcessSpec {
            program: &program,
            args: &args,
            cwd: &cwd,
            environment: &input.env,
            clear_env: input.clear_env,
            timeout: Duration::from_millis(input.timeout_ms),
            max_output_bytes: input.max_output_bytes,
        },
        cancellation,
    )
    .map(CapturedProcess::into_output)
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ProcessSpec<'a> {
    pub(crate) program: &'a str,
    pub(crate) args: &'a [String],
    pub(crate) cwd: &'a Path,
    pub(crate) environment: &'a BTreeMap<String, String>,
    pub(crate) clear_env: bool,
    pub(crate) timeout: Duration,
    pub(crate) max_output_bytes: u64,
}

pub(crate) fn run_raw(
    spec: ProcessSpec<'_>,
    cancellation: &CancellationToken,
) -> Result<CapturedProcess, RaxError> {
    let program =
        platform::resolve_program(spec.program, spec.cwd, spec.environment, spec.clear_env)?;
    let mut command = Command::new(&program);
    command
        .args(spec.args)
        .current_dir(spec.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if spec.clear_env {
        command.env_clear();
    }
    command.envs(spec.environment);
    configure_process_group(&mut command);

    let started = Instant::now();
    let mut child = command.spawn().map_err(|error| {
        RaxError::new(
            ErrorCode::ProcessSpawnFailed,
            ErrorPhase::Apply,
            false,
            format!("failed to spawn {}: {error}", spec.program),
        )
        .with_detail("program", spec.program.to_string())
        .with_detail("resolvedProgram", program.display().to_string())
    })?;
    let stdout = child.stdout.take().ok_or_else(|| {
        mark_process_started(cleanup_reader_spawn_failure(
            &mut child,
            invariant("spawned process did not expose stdout"),
            None,
        ))
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        mark_process_started(cleanup_reader_spawn_failure(
            &mut child,
            invariant("spawned process did not expose stderr"),
            None,
        ))
    })?;
    let stdout_reader = spawn_reader(stdout, spec.max_output_bytes, "stdout").map_err(|error| {
        mark_process_started(cleanup_reader_spawn_failure(&mut child, error, None))
    })?;
    let stderr_reader = match spawn_reader(stderr, spec.max_output_bytes, "stderr") {
        Ok(reader) => reader,
        Err(error) => {
            return Err(mark_process_started(cleanup_reader_spawn_failure(
                &mut child,
                error,
                Some((stdout_reader, "stdout")),
            )));
        }
    };

    let pid = child.id();
    let supervision = supervise_child(
        &mut child,
        pid,
        started,
        spec.timeout,
        cancellation,
        &stdout_reader,
        &stderr_reader,
    );
    let (status, timed_out, cancelled) = match supervision {
        Ok(outcome) => outcome,
        Err(error) => {
            return Err(mark_process_started(cleanup_process_control_failure(
                &mut child,
                pid,
                error,
                stdout_reader,
                stderr_reader,
            )));
        }
    };

    // Resolve both reader results before returning. A reader that remains blocked
    // beyond the bounded drain timeout is reported explicitly as detached rather
    // than allowing the CLI to wait forever.
    let stdout_result = join_reader(stdout_reader, "stdout").map_err(mark_process_started);
    let stderr_result = join_reader(stderr_reader, "stderr").map_err(mark_process_started);
    let stdout = stdout_result?;
    let stderr = stderr_result?;
    Ok(CapturedProcess {
        program: spec.program.to_string(),
        args: spec.args.to_vec(),
        status,
        duration: started.elapsed(),
        stdout: stdout.bytes,
        stderr: stderr.bytes,
        stdout_bytes: stdout.total_bytes,
        stderr_bytes: stderr.total_bytes,
        stdout_truncated: stdout.truncated,
        stderr_truncated: stderr.truncated,
        timed_out,
        cancelled,
    })
}

fn supervise_child(
    child: &mut Child,
    pid: u32,
    started: Instant,
    timeout: Duration,
    cancellation: &CancellationToken,
    stdout_reader: &ReaderHandle,
    stderr_reader: &ReaderHandle,
) -> Result<(ExitStatus, bool, bool), RaxError> {
    loop {
        match poll_child(child, pid)? {
            ChildState::Exited { identifier_held } => {
                let status =
                    finish_natural_exit(child, pid, identifier_held, stdout_reader, stderr_reader)?;
                return Ok((status, false, false));
            }
            ChildState::Running => {}
        }
        if cancellation.is_cancelled() {
            let status = terminate_and_reap(child, "cancelled")?;
            return Ok((status, false, true));
        }
        if started.elapsed() >= timeout {
            let status = terminate_and_reap(child, "timed out")?;
            return Ok((status, true, false));
        }
        thread::sleep(POLL_INTERVAL);
    }
}

fn terminate_and_reap(child: &mut Child, reason: &str) -> Result<ExitStatus, RaxError> {
    terminate_tree(child)?;
    child.wait().map_err(|error| {
        io_error(
            ErrorPhase::Apply,
            format!("failed to reap {reason} process"),
            None,
            error,
        )
    })
}

fn poll_child(child: &mut Child, pid: u32) -> Result<ChildState, RaxError> {
    child_state(pid, child).map_err(|error| {
        io_error(
            ErrorPhase::Apply,
            "failed to poll child process",
            None,
            error,
        )
    })
}

/// Drain the captured pipes, clear residual descendants, then reap.
///
/// Rax owns the spawned process tree, so residual descendants are cleared even
/// when they closed the inherited pipes. The group signal is emitted before the
/// reap, while the exited leader still holds the process-group identifier; once
/// the identifier is released the kernel may reissue it, and signalling it then
/// could terminate an unrelated process group.
fn finish_natural_exit(
    child: &mut Child,
    pid: u32,
    identifier_held: bool,
    stdout: &ReaderHandle,
    stderr: &ReaderHandle,
) -> Result<ExitStatus, RaxError> {
    let deadline = Instant::now() + NATURAL_DRAIN_GRACE;
    while Instant::now() < deadline {
        if stdout.is_finished() && stderr.is_finished() {
            break;
        }
        thread::sleep(POLL_INTERVAL);
    }
    clear_residual_tree_before_reap(pid, identifier_held);
    child.wait().map_err(|error| {
        io_error(
            ErrorPhase::Apply,
            "failed to reap exited process",
            None,
            error,
        )
    })
}

fn cleanup_reader_spawn_failure(
    child: &mut Child,
    mut error: RaxError,
    reader: Option<(ReaderHandle, &'static str)>,
) -> RaxError {
    if let Err(cleanup_error) = terminate_tree(child) {
        error = error
            .with_detail("terminationError", cleanup_error.message)
            .with_detail("processTreeMayRemain", true);
    }
    if let Err(wait_error) = child.wait() {
        error = error
            .with_detail("reapError", wait_error.to_string())
            .with_detail("processMayRemain", true);
    }
    if let Some((handle, name)) = reader
        && let Err(reader_error) = join_reader(handle, name)
    {
        error = error.with_detail("readerCleanupError", reader_error.message);
    }
    error
}

fn cleanup_process_control_failure(
    child: &mut Child,
    pid: u32,
    mut error: RaxError,
    stdout_reader: ReaderHandle,
    stderr_reader: ReaderHandle,
) -> RaxError {
    if let Err(termination_error) = terminate_group(pid, true) {
        error = error
            .with_detail("terminationError", termination_error.message)
            .with_detail("processTreeMayRemain", true);
    }
    if let Err(kill_error) = child.kill()
        && kill_error.kind() != std::io::ErrorKind::InvalidInput
    {
        error = error
            .with_detail("directChildKillError", kill_error.to_string())
            .with_detail("processMayRemain", true);
    }
    if let Err(wait_error) = child.wait() {
        error = error
            .with_detail("reapError", wait_error.to_string())
            .with_detail("processMayRemain", true);
    }
    if let Err(reader_error) = join_reader(stdout_reader, "stdout") {
        error = error.with_detail("stdoutReaderCleanupError", reader_error.message);
    }
    if let Err(reader_error) = join_reader(stderr_reader, "stderr") {
        error = error.with_detail("stderrReaderCleanupError", reader_error.message);
    }
    error
}

fn mark_process_started(mut error: RaxError) -> RaxError {
    error
        .details
        .insert("partialApplied".to_string(), true.into());
    error.details.insert(
        "partialReason".to_string(),
        "the child process started and may have changed external state".into(),
    );
    error
}

fn terminate_tree(child: &mut Child) -> Result<(), RaxError> {
    let pid = child.id();
    let initial_poll_error = match child_state(pid, child) {
        Ok(ChildState::Exited { identifier_held }) => {
            clear_residual_tree_before_reap(pid, identifier_held);
            let _ = child.wait();
            return Ok(());
        }
        Ok(ChildState::Running) => None,
        Err(error) => Some(error.to_string()),
    };

    let graceful = terminate_group(pid, false);
    let deadline = Instant::now() + TERMINATION_GRACE;
    while Instant::now() < deadline {
        match child_state(pid, child) {
            Ok(ChildState::Exited { identifier_held }) => {
                clear_residual_tree_before_reap(pid, identifier_held);
                let _ = child.wait();
                return Ok(());
            }
            Ok(ChildState::Running) => thread::sleep(POLL_INTERVAL),
            Err(_) => break,
        }
    }

    let forced_error = match terminate_group(pid, true) {
        Ok(()) => return Ok(()),
        Err(error) => error,
    };
    let direct_kill = child.kill();
    let mut error = RaxError::new(
        ErrorCode::OperationFailed,
        ErrorPhase::Apply,
        false,
        "failed to prove termination of the complete child process tree",
    )
    .with_detail("forcedError", forced_error.message)
    .with_detail("processTreeMayRemain", true);
    if let Err(graceful_error) = graceful {
        error = error.with_detail("gracefulError", graceful_error.message);
    }
    if let Some(initial_poll_error) = initial_poll_error {
        error = error.with_detail("initialPollError", initial_poll_error);
    }
    match direct_kill {
        Ok(()) => {
            error = error.with_detail("directChildKillAttempted", true);
        }
        Err(kill_error) => {
            error = error.with_detail("directChildKillError", kill_error.to_string());
        }
    }
    Err(error)
}

/// Clear residual descendants of an exited leader.
///
/// `identifier_held` must be true: it is the caller's proof that the
/// process-group identifier is still reserved by this child, so the signal
/// cannot reach a process group that merely inherited a reissued number.
fn clear_residual_tree_before_reap(pid: u32, identifier_held: bool) {
    if !identifier_held {
        return;
    }
    // Non-zero means the group is already empty or inaccessible. Only wait and
    // escalate after a successful TERM delivery.
    if terminate_group(pid, false).is_ok() {
        thread::sleep(POLL_INTERVAL);
        let _ = terminate_group(pid, true);
    }
}

fn invariant(message: &str) -> RaxError {
    RaxError::new(
        ErrorCode::InvariantViolation,
        ErrorPhase::Apply,
        false,
        message,
    )
}
