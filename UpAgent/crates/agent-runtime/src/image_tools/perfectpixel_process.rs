use std::{path::Path, time::Duration};

use agent_core::{AgentFailure, FailureCode};
use tokio::io::{AsyncRead, AsyncReadExt as _};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

use crate::ToolModuleError;

use super::MAX_CLI_OUTPUT_BYTES;

pub(super) struct PerfectPixelOutput {
    pub(super) stdout: Vec<u8>,
}

pub(super) async fn run_perfectpixel(
    executable: &Path,
    args: &[String],
    timeout: Duration,
    cancellation: &CancellationToken,
) -> Result<PerfectPixelOutput, ToolModuleError> {
    if cancellation.is_cancelled() {
        return Err(ToolModuleError::Cancelled(AgentFailure::new(
            FailureCode::Cancelled,
            "PerfectPixel operation was cancelled before dispatch",
        )));
    }
    let mut command = Command::new(executable);
    command
        .kill_on_drop(true)
        .env_clear()
        .env("NO_COLOR", "1")
        .env("PATH", "/usr/bin:/bin")
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = command.spawn().map_err(|error| {
        ToolModuleError::Failure(AgentFailure::new(
            FailureCode::ProviderUnavailable,
            format!("PerfectPixel process could not start: {error}"),
        ))
    })?;
    let stdout = child.stdout.take().ok_or_else(|| {
        ToolModuleError::Failure(AgentFailure::new(
            FailureCode::ProviderUnavailable,
            "PerfectPixel process did not expose stdout",
        ))
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        ToolModuleError::Failure(AgentFailure::new(
            FailureCode::ProviderUnavailable,
            "PerfectPixel process did not expose stderr",
        ))
    })?;
    let stdout_task = tokio::spawn(read_bounded(stdout, MAX_CLI_OUTPUT_BYTES + 1));
    let stderr_task = tokio::spawn(read_bounded(stderr, 64 * 1024));
    let mut wait = Box::pin(child.wait());
    let status = tokio::select! {
        biased;
        result = &mut wait => result.map_err(|error| ToolModuleError::Failure(AgentFailure::new(FailureCode::ProviderUnavailable, format!("PerfectPixel wait failed: {error}"))))?,
        () = cancellation.cancelled() => {
            drop(wait);
            let _ = child.kill().await;
            let _ = child.wait().await;
            let _ = stdout_task.await;
            let _ = stderr_task.await;
            return Err(ToolModuleError::Cancelled(AgentFailure::new(
                FailureCode::Cancelled,
                "PerfectPixel operation was cancelled after dispatch",
            )));
        }
        () = tokio::time::sleep(timeout) => {
            drop(wait);
            let _ = child.kill().await;
            let _ = child.wait().await;
            let _ = stdout_task.await;
            let _ = stderr_task.await;
            return Err(ToolModuleError::TimedOut(AgentFailure::new(
                FailureCode::BudgetExceeded,
                "PerfectPixel operation exceeded its timeout",
            )));
        }
    };
    let stdout = stdout_task
        .await
        .map_err(|error| {
            ToolModuleError::Failure(AgentFailure::new(
                FailureCode::InternalInvariant,
                format!("PerfectPixel stdout task failed: {error}"),
            ))
        })?
        .map_err(|error| {
            ToolModuleError::Failure(AgentFailure::new(FailureCode::ProviderFailed, error))
        })?;
    let stderr = stderr_task
        .await
        .map_err(|error| {
            ToolModuleError::Failure(AgentFailure::new(
                FailureCode::InternalInvariant,
                format!("PerfectPixel stderr task failed: {error}"),
            ))
        })?
        .map_err(|error| {
            ToolModuleError::Failure(AgentFailure::new(FailureCode::ProviderFailed, error))
        })?;
    if !status.success() {
        return Err(ToolModuleError::Failure(AgentFailure::new(
            FailureCode::ProviderFailed,
            format!(
                "PerfectPixel rejected the image request: status={} stderr={}",
                status,
                String::from_utf8_lossy(&stderr)
                    .chars()
                    .take(512)
                    .collect::<String>()
            ),
        )));
    }
    Ok(PerfectPixelOutput { stdout })
}

async fn read_bounded<R>(reader: R, limit: usize) -> Result<Vec<u8>, String>
where
    R: AsyncRead + Unpin,
{
    let mut output = Vec::new();
    reader
        .take(limit as u64)
        .read_to_end(&mut output)
        .await
        .map_err(|error| error.to_string())?;
    if output.len() > limit.saturating_sub(1) {
        return Err("PerfectPixel output exceeded its bound".to_owned());
    }
    Ok(output)
}
