use std::time::Instant;

use rax_core::{
    ErrorCode, ErrorPhase, EventKind, Operation, OperationOutput, Outcome, Plan, RaxError,
    ResultEnvelope, SCHEMA_VERSION,
};
use serde_json::Value;

use crate::{
    codec::encoding_name, fs, git, process, process::CancellationToken, timeutil::now_rfc3339,
    workspace::Workspace,
};

pub(crate) fn execute_outcome(
    workspace: &Workspace,
    cancellation: &CancellationToken,
    operation: &Operation,
    receipt: Option<String>,
) -> Outcome {
    if cancellation.is_cancelled() {
        return Outcome::Cancelled {
            error: RaxError::new(
                ErrorCode::ProcessCancelled,
                ErrorPhase::Apply,
                false,
                "operation was cancelled before execution",
            ),
            receipt_path: receipt,
        };
    }

    match execute_operation(workspace, cancellation, operation) {
        Ok(OperationOutput::Process(output)) if output.cancelled => Outcome::PartiallyApplied {
            error: process_error(
                ErrorCode::ProcessCancelled,
                "process was cancelled after it started; side effects require inspection",
                output,
            ),
            receipt_path: receipt,
        },
        Ok(OperationOutput::Process(output)) if output.timed_out => Outcome::PartiallyApplied {
            error: process_error(
                ErrorCode::ProcessTimedOut,
                "process timed out after it started; side effects require inspection",
                output,
            ),
            receipt_path: receipt,
        },
        Ok(OperationOutput::Process(output)) if !output.success => Outcome::PartiallyApplied {
            error: process_error(
                ErrorCode::OperationFailed,
                "process exited unsuccessfully after it started; side effects require inspection",
                output,
            ),
            receipt_path: receipt,
        },
        Ok(output) => Outcome::Succeeded {
            output,
            receipt_path: receipt,
        },
        Err(error) if is_partial(&error) => Outcome::PartiallyApplied {
            error,
            receipt_path: receipt,
        },
        Err(error) if error.code == ErrorCode::ProcessCancelled => Outcome::Cancelled {
            error,
            receipt_path: receipt,
        },
        Err(error) => Outcome::Failed {
            error,
            receipt_path: receipt,
        },
    }
}

pub(crate) fn execute_operation(
    workspace: &Workspace,
    cancellation: &CancellationToken,
    operation: &Operation,
) -> Result<OperationOutput, RaxError> {
    match operation {
        Operation::FsRead(input) => fs::read(workspace, input).map(OperationOutput::FsRead),
        Operation::FsStat(input) => fs::stat(workspace, input).map(OperationOutput::FsStat),
        Operation::FsList(input) => fs::list(workspace, input).map(OperationOutput::FsList),
        Operation::FsGlob(input) => fs::glob(workspace, input).map(OperationOutput::FsGlob),
        Operation::FsGrep(input) => fs::grep(workspace, input).map(OperationOutput::FsGrep),
        Operation::FsWrite(input) => fs::write(workspace, input).map(OperationOutput::FsMutation),
        Operation::FsPatch(input) => fs::patch(workspace, input).map(OperationOutput::FsMutation),
        Operation::FsMkdir(input) => fs::mkdir(workspace, input).map(OperationOutput::FsMutation),
        Operation::FsMove(input) => {
            fs::move_path(workspace, input).map(OperationOutput::FsMutation)
        }
        Operation::FsCopy(input) => {
            fs::copy_path(workspace, input).map(OperationOutput::FsMutation)
        }
        Operation::FsRemove(input) => fs::remove(workspace, input).map(OperationOutput::FsMutation),
        Operation::ProcessRun(input) => {
            process::run(workspace, input, cancellation).map(OperationOutput::Process)
        }
        Operation::ProcessShell(input) => {
            process::shell(workspace, input, cancellation).map(OperationOutput::Process)
        }
        Operation::GitStatus(input) => {
            git::status(workspace, input, cancellation).map(OperationOutput::GitStatus)
        }
        Operation::GitDiff(input) => {
            git::diff(workspace, input, cancellation).map(OperationOutput::GitDiff)
        }
        Operation::GitLog(input) => {
            git::log(workspace, input, cancellation).map(OperationOutput::GitLog)
        }
        Operation::GitShow(input) => {
            git::show(workspace, input, cancellation).map(OperationOutput::GitShow)
        }
        Operation::GitRemote(input) => {
            git::remotes(workspace, input, cancellation).map(OperationOutput::GitRemote)
        }
        Operation::GitBranches(input) => {
            git::branches(workspace, input, cancellation).map(OperationOutput::GitBranches)
        }
        Operation::GitStage(input) => {
            git::stage(workspace, input, cancellation).map(OperationOutput::GitMutation)
        }
        Operation::GitUnstage(input) => {
            git::unstage(workspace, input, cancellation).map(OperationOutput::GitMutation)
        }
        Operation::GitDiscard(input) => {
            git::discard(workspace, input, cancellation).map(OperationOutput::GitMutation)
        }
        Operation::GitCommit(input) => {
            git::commit(workspace, input, cancellation).map(OperationOutput::GitMutation)
        }
        Operation::GitCheckout(input) => {
            git::checkout(workspace, input, cancellation).map(OperationOutput::GitMutation)
        }
        Operation::GitFetch(input) => {
            git::fetch(workspace, input, cancellation).map(OperationOutput::GitMutation)
        }
        Operation::GitPullFf(input) => {
            git::pull_ff(workspace, input, cancellation).map(OperationOutput::GitMutation)
        }
        Operation::GitPush(input) => {
            git::push(workspace, input, cancellation).map(OperationOutput::GitMutation)
        }
    }
}

pub(crate) fn result_envelope(
    plan: &Plan,
    run_id: &str,
    started_at: &str,
    started: Instant,
    outcome: Outcome,
) -> Result<ResultEnvelope, RaxError> {
    Ok(ResultEnvelope {
        schema_version: SCHEMA_VERSION.to_string(),
        request_id: plan.request_id.clone(),
        run_id: run_id.to_string(),
        operation: plan.operation.operation_name().to_string(),
        started_at: started_at.to_string(),
        finished_at: now_rfc3339()?,
        duration_ms: elapsed_ms(started),
        outcome,
    })
}

#[must_use]
pub(crate) const fn outcome_event(outcome: &Outcome) -> EventKind {
    match outcome {
        Outcome::Succeeded { .. } => EventKind::Succeeded,
        Outcome::ApprovalRequired { .. } => EventKind::ApprovalRequested,
        Outcome::Failed { .. } => EventKind::Failed,
        Outcome::Cancelled { .. } => EventKind::Cancelled,
        Outcome::Denied { .. } => EventKind::Denied,
        Outcome::Expired { .. } => EventKind::Expired,
        Outcome::PartiallyApplied { .. } => EventKind::PartiallyApplied,
    }
}

fn process_error(code: ErrorCode, message: &str, output: rax_core::ProcessOutput) -> RaxError {
    let rax_core::ProcessOutput {
        program,
        args,
        exit_code,
        success: _,
        timed_out,
        cancelled,
        duration_ms,
        stdout_encoding,
        stdout,
        stderr_encoding,
        stderr,
        stdout_bytes,
        stderr_bytes,
        stdout_truncated,
        stderr_truncated,
    } = output;

    RaxError::new(
        code,
        ErrorPhase::Verify,
        code == ErrorCode::ProcessTimedOut,
        message,
    )
    .with_detail("program", program)
    .with_detail("args", serde_json::json!(args))
    .with_detail("exitCode", exit_code.map_or(Value::Null, Value::from))
    .with_detail("timedOut", timed_out)
    .with_detail("cancelled", cancelled)
    .with_detail("durationMs", duration_ms)
    .with_detail("stdoutEncoding", encoding_name(stdout_encoding))
    .with_detail("stdout", stdout)
    .with_detail("stdoutBytes", stdout_bytes)
    .with_detail("stdoutTruncated", stdout_truncated)
    .with_detail("stderrEncoding", encoding_name(stderr_encoding))
    .with_detail("stderr", stderr)
    .with_detail("stderrBytes", stderr_bytes)
    .with_detail("stderrTruncated", stderr_truncated)
    .with_detail("sideEffectsUnknown", true)
    .with_detail("partialApplied", true)
    .with_detail(
        "partialReason",
        "the child process started and may have changed external state",
    )
}

#[must_use]
pub(crate) const fn outcome_name(outcome: &Outcome) -> &'static str {
    match outcome {
        Outcome::Succeeded { .. } => "succeeded",
        Outcome::ApprovalRequired { .. } => "approval_required",
        Outcome::Failed { .. } => "failed",
        Outcome::Cancelled { .. } => "cancelled",
        Outcome::Denied { .. } => "denied",
        Outcome::Expired { .. } => "expired",
        Outcome::PartiallyApplied { .. } => "partially_applied",
    }
}

fn is_partial(error: &RaxError) -> bool {
    error
        .details
        .get("partialApplied")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

#[must_use]
pub(crate) fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}
