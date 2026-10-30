use std::{
    io::Read,
    sync::mpsc::{self, Receiver, RecvTimeoutError},
    thread,
    time::Duration,
};

use rax_core::{ErrorCode, ErrorPhase, RaxError};

use crate::error::io_error;

const READER_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug)]
pub(super) struct CapturedStream {
    pub(super) bytes: Vec<u8>,
    pub(super) total_bytes: u64,
    pub(super) truncated: bool,
}

#[derive(Debug)]
pub(super) struct ReaderHandle {
    receiver: Receiver<Result<CapturedStream, RaxError>>,
    thread: thread::JoinHandle<()>,
}

impl ReaderHandle {
    pub(super) fn is_finished(&self) -> bool {
        self.thread.is_finished()
    }
}

pub(super) fn spawn_reader<R>(
    mut reader: R,
    max_output_bytes: u64,
    name: &'static str,
) -> Result<ReaderHandle, RaxError>
where
    R: Read + Send + 'static,
{
    let (sender, receiver) = mpsc::sync_channel(1);
    let thread = thread::Builder::new()
        .name(format!("rax-{name}-reader"))
        .spawn(move || {
            let result = capture_stream(&mut reader, max_output_bytes, name);
            drop(sender.send(result));
        })
        .map_err(|error| {
            RaxError::new(
                ErrorCode::ProcessSpawnFailed,
                ErrorPhase::Apply,
                false,
                format!("failed to start {name} reader thread: {error}"),
            )
        })?;
    Ok(ReaderHandle { receiver, thread })
}

pub(super) fn join_reader(handle: ReaderHandle, name: &str) -> Result<CapturedStream, RaxError> {
    let ReaderHandle { receiver, thread } = handle;
    match receiver.recv_timeout(READER_DRAIN_TIMEOUT) {
        Ok(result) => {
            thread.join().map_err(|_| {
                RaxError::new(
                    ErrorCode::InvariantViolation,
                    ErrorPhase::Apply,
                    false,
                    format!("{name} reader thread panicked"),
                )
            })?;
            result
        }
        Err(RecvTimeoutError::Timeout) => Err(RaxError::new(
            ErrorCode::RecoveryRequired,
            ErrorPhase::Apply,
            false,
            format!("{name} did not close after the child process exited"),
        )
        .with_detail("readerStillRunning", true)
        .with_detail("readerThreadDetached", true)
        .with_detail("processTreeMayRemain", true)
        .with_detail(
            "readerDrainTimeoutMs",
            u64::try_from(READER_DRAIN_TIMEOUT.as_millis()).unwrap_or(u64::MAX),
        )),
        Err(RecvTimeoutError::Disconnected) => {
            let panicked = thread.join().is_err();
            Err(RaxError::new(
                ErrorCode::InvariantViolation,
                ErrorPhase::Apply,
                false,
                if panicked {
                    format!("{name} reader thread panicked")
                } else {
                    format!("{name} reader exited without returning a result")
                },
            ))
        }
    }
}

fn capture_stream<R: Read>(
    reader: &mut R,
    max_output_bytes: u64,
    name: &str,
) -> Result<CapturedStream, RaxError> {
    let limit = usize::try_from(max_output_bytes).unwrap_or(usize::MAX);
    let mut bytes = Vec::with_capacity(limit.min(64 * 1024));
    let mut total_bytes = 0_u64;
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let count = reader.read(&mut buffer).map_err(|error| {
            io_error(
                ErrorPhase::Apply,
                format!("failed to read child {name}"),
                None,
                error,
            )
        })?;
        if count == 0 {
            break;
        }
        total_bytes = total_bytes
            .checked_add(u64::try_from(count).unwrap_or(u64::MAX))
            .ok_or_else(|| {
                RaxError::new(
                    ErrorCode::LimitExceeded,
                    ErrorPhase::Apply,
                    false,
                    format!("child {name} byte count exceeded the protocol range"),
                )
            })?;
        if bytes.len() < limit {
            let remaining = limit - bytes.len();
            bytes.extend_from_slice(&buffer[..count.min(remaining)]);
        }
    }
    Ok(CapturedStream {
        truncated: total_bytes > max_output_bytes,
        total_bytes,
        bytes,
    })
}
