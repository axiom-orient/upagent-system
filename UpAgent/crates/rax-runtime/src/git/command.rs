use std::{collections::BTreeMap, path::PathBuf, time::Duration};

use rax_core::{ErrorCode, ErrorPhase, GitTextOutput, RaxError};

use super::strict_utf8;

use crate::{
    codec::{encode_protocol_bytes, encoding_name},
    process::{CancellationToken, CapturedProcess, ProcessSpec, run_raw},
    workspace::Workspace,
};

#[derive(Debug)]
pub(super) struct GitRunner<'a> {
    repository: PathBuf,
    environment: BTreeMap<String, String>,
    cancellation: &'a CancellationToken,
}

impl<'a> GitRunner<'a> {
    pub(super) fn open(
        workspace: &Workspace,
        repository: &str,
        cancellation: &'a CancellationToken,
    ) -> Result<Self, RaxError> {
        Ok(Self {
            repository: workspace.absolute_existing_dir(repository)?,
            environment: sanitized_git_environment(),
            cancellation,
        })
    }

    pub(super) fn run<S: AsRef<str>>(
        &self,
        args: &[S],
        timeout: Duration,
        max_output: u64,
    ) -> Result<CapturedProcess, RaxError> {
        let result = self.run_allow_failure(args, timeout, max_output)?;
        if result.status.success() && !result.timed_out && !result.cancelled {
            Ok(result)
        } else {
            Err(map_git_failure(&result))
        }
    }

    fn run_allow_failure<S: AsRef<str>>(
        &self,
        args: &[S],
        timeout: Duration,
        max_output: u64,
    ) -> Result<CapturedProcess, RaxError> {
        let mut full_args = vec![
            "--no-pager".to_string(),
            "-c".to_string(),
            "core.fsmonitor=false".to_string(),
            "-c".to_string(),
            "diff.external=".to_string(),
        ];
        full_args.extend(args.iter().map(|value| value.as_ref().to_string()));
        run_raw(
            ProcessSpec {
                program: "git",
                args: &full_args,
                cwd: &self.repository,
                environment: &self.environment,
                clear_env: true,
                timeout,
                max_output_bytes: max_output,
            },
            self.cancellation,
        )
        .map_err(map_git_invocation_error)
    }

    pub(super) fn head(&self, timeout: Duration) -> Result<Option<String>, RaxError> {
        let result =
            self.run_allow_failure(&["rev-parse", "--verify", "HEAD"], timeout, 64 * 1024)?;
        require_complete_output(&result, "git rev-parse")?;
        if result.status.success() {
            return Ok(Some(
                strict_utf8(&result.stdout, "git HEAD")?.trim().to_string(),
            ));
        }
        let stderr = String::from_utf8_lossy(&result.stderr);
        if stderr.contains("Needed a single revision")
            || stderr.contains("unknown revision")
            || stderr.contains("bad revision")
        {
            Ok(None)
        } else {
            Err(map_git_failure(&result))
        }
    }
}

pub(super) fn require_complete_output(
    process: &CapturedProcess,
    source: &str,
) -> Result<(), RaxError> {
    if process.stdout_truncated || process.stderr_truncated {
        return Err(RaxError::new(
            ErrorCode::LimitExceeded,
            ErrorPhase::Apply,
            false,
            format!("{source} exceeded the structured output limit"),
        )
        .with_detail("stdoutBytes", process.stdout_bytes)
        .with_detail("stderrBytes", process.stderr_bytes));
    }
    Ok(())
}

pub(super) fn require_complete_stderr(
    process: &CapturedProcess,
    source: &str,
) -> Result<(), RaxError> {
    if process.stderr_truncated {
        return Err(RaxError::new(
            ErrorCode::LimitExceeded,
            ErrorPhase::Apply,
            false,
            format!("{source} exceeded the diagnostic output limit"),
        )
        .with_detail("stderrBytes", process.stderr_bytes));
    }
    Ok(())
}

#[allow(clippy::needless_pass_by_value)]
pub(super) fn text_output(process: CapturedProcess) -> GitTextOutput {
    let (text, encoding) = encode_protocol_bytes(&process.stdout);
    GitTextOutput {
        encoding,
        text,
        bytes: process.stdout_bytes,
        truncated: process.stdout_truncated,
    }
}

pub(super) fn process_details(process: &CapturedProcess) -> BTreeMap<String, serde_json::Value> {
    let (stdout, stdout_encoding) = encode_protocol_bytes(&process.stdout);
    let (stderr, stderr_encoding) = encode_protocol_bytes(&process.stderr);
    BTreeMap::from([
        ("stdout".to_string(), stdout.into()),
        (
            "stdoutEncoding".to_string(),
            encoding_name(stdout_encoding).into(),
        ),
        ("stderr".to_string(), stderr.into()),
        (
            "stderrEncoding".to_string(),
            encoding_name(stderr_encoding).into(),
        ),
        (
            "stdoutTruncated".to_string(),
            process.stdout_truncated.into(),
        ),
        (
            "stderrTruncated".to_string(),
            process.stderr_truncated.into(),
        ),
    ])
}

fn map_git_invocation_error(error: RaxError) -> RaxError {
    let child_started = error
        .details
        .get("partialApplied")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    if error.code == ErrorCode::ProcessSpawnFailed && !child_started {
        return RaxError::new(
            ErrorCode::GitUnavailable,
            ErrorPhase::Apply,
            false,
            "git executable is unavailable or could not be started",
        )
        .with_detail("cause", error.message)
        .with_detail("program", "git");
    }
    error
}

fn sanitized_git_environment() -> BTreeMap<String, String> {
    const PASSTHROUGH: &[&str] = &[
        "PATH",
        "HOME",
        "USERPROFILE",
        "XDG_CONFIG_HOME",
        "SSH_AUTH_SOCK",
        "SSH_AGENT_PID",
        "SYSTEMROOT",
        "COMSPEC",
        "PATHEXT",
        "TEMP",
        "TMP",
        "TMPDIR",
    ];
    let mut environment = BTreeMap::new();
    for key in PASSTHROUGH {
        if let Ok(value) = std::env::var(key) {
            environment.insert((*key).to_string(), value);
        }
    }
    environment.insert("GIT_TERMINAL_PROMPT".to_string(), "0".to_string());
    environment.insert("GIT_CONFIG_NOSYSTEM".to_string(), "1".to_string());
    environment.insert("GIT_OPTIONAL_LOCKS".to_string(), "0".to_string());
    environment.insert("GIT_MERGE_AUTOEDIT".to_string(), "no".to_string());
    environment.insert(
        "GIT_SSH_COMMAND".to_string(),
        "ssh -oBatchMode=yes".to_string(),
    );
    environment.insert("LC_ALL".to_string(), "C".to_string());
    environment
}

fn map_git_failure(process: &CapturedProcess) -> RaxError {
    let diagnostic = String::from_utf8_lossy(&process.stderr);
    let lower = diagnostic.to_ascii_lowercase();
    let (stderr, stderr_encoding) = encode_protocol_bytes(&process.stderr);
    let (stdout, stdout_encoding) = encode_protocol_bytes(&process.stdout);
    let (code, retryable) = if process.cancelled {
        (ErrorCode::ProcessCancelled, false)
    } else if process.timed_out {
        (ErrorCode::ProcessTimedOut, true)
    } else if lower.contains("not a git repository") {
        (ErrorCode::GitNotRepository, false)
    } else if lower.contains("conflict") || lower.contains("would be overwritten") {
        (ErrorCode::GitConflict, false)
    } else if lower.contains("authentication failed")
        || lower.contains("permission denied (publickey)")
        || lower.contains("could not read username")
    {
        (ErrorCode::GitAuthentication, false)
    } else if lower.contains("could not resolve host")
        || lower.contains("connection timed out")
        || lower.contains("failed to connect")
    {
        (ErrorCode::GitNetwork, true)
    } else {
        (ErrorCode::OperationFailed, false)
    };
    RaxError::new(
        code,
        ErrorPhase::Apply,
        retryable,
        format!(
            "git command failed{}",
            process
                .status
                .code()
                .map(|code| format!(" with exit code {code}"))
                .unwrap_or_default()
        ),
    )
    .with_detail("stderr", stderr)
    .with_detail("stderrEncoding", encoding_name(stderr_encoding))
    .with_detail("stdout", stdout)
    .with_detail("stdoutEncoding", encoding_name(stdout_encoding))
    .with_detail("args", serde_json::json!(&process.args))
}
