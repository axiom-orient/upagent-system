use std::{
    fs::File,
    io::{self, Read},
    os::unix::{fs::MetadataExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use sha2::{Digest, Sha256};

use crate::Sha256Digest;

const POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
    bytes: u64,
}

impl FileIdentity {
    fn from_file(file: &File) -> Result<Self, ProcessRunError> {
        let metadata = file
            .metadata()
            .map_err(|error| ProcessRunError::Io(format!("stat executable handle: {error}")))?;
        if !metadata.is_file() {
            return Err(ProcessRunError::InvalidExecutable(
                "external executable must be a regular file".to_string(),
            ));
        }
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            bytes: metadata.len(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinnedExecutable {
    canonical_path: PathBuf,
    identity: FileIdentity,
    sha256: Sha256Digest,
}

impl PinnedExecutable {
    pub fn new(path: impl AsRef<Path>, sha256: Sha256Digest) -> Result<Self, ProcessRunError> {
        let path = path.as_ref();
        if !path.is_absolute() {
            return Err(ProcessRunError::InvalidExecutable(
                "external executable path must be absolute".to_string(),
            ));
        }

        // The caller-supplied path is opened through the no-follow capability
        // boundary before canonicalization. This rejects symlink components
        // instead of validating a pathname and reopening it later.
        let original = open_executable(path)?;
        let original_identity = FileIdentity::from_file(&original)?;
        let original_digest = digest_handle(original)?;
        if original_digest != sha256 {
            return Err(ProcessRunError::ExecutableDigestMismatch {
                expected: sha256,
                actual: original_digest,
            });
        }

        let canonical_path = std::fs::canonicalize(path)
            .map_err(|error| ProcessRunError::Io(format!("canonicalize executable: {error}")))?;
        let canonical = open_executable(&canonical_path)?;
        let canonical_identity = FileIdentity::from_file(&canonical)?;
        if canonical_identity != original_identity {
            return Err(ProcessRunError::ExecutableChanged(
                "executable identity changed while it was pinned".to_string(),
            ));
        }
        let canonical_digest = digest_handle(canonical)?;
        if canonical_digest != sha256 {
            return Err(ProcessRunError::ExecutableDigestMismatch {
                expected: sha256,
                actual: canonical_digest,
            });
        }

        Ok(Self {
            canonical_path,
            identity: canonical_identity,
            sha256,
        })
    }

    pub fn path(&self) -> &Path {
        &self.canonical_path
    }

    pub fn sha256(&self) -> &Sha256Digest {
        &self.sha256
    }

    fn revalidate(&self) -> Result<(), ProcessRunError> {
        let file = open_executable(&self.canonical_path)?;
        let identity = FileIdentity::from_file(&file)?;
        if identity != self.identity {
            return Err(ProcessRunError::ExecutableChanged(
                "pinned executable filesystem identity changed".to_string(),
            ));
        }
        let actual = digest_handle(file)?;
        if actual != self.sha256 {
            return Err(ProcessRunError::ExecutableDigestMismatch {
                expected: self.sha256.clone(),
                actual,
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct ProcessSpec {
    pub executable: PinnedExecutable,
    pub args: Vec<String>,
    /// The process inherits no ambient environment. Every required variable is explicit here.
    pub environment: Vec<(String, String)>,
    pub timeout: Duration,
    pub max_stdout_bytes: usize,
    pub max_stderr_bytes: usize,
}

impl ProcessSpec {
    pub fn validate(&self) -> Result<(), ProcessRunError> {
        if self.timeout.is_zero() {
            return Err(ProcessRunError::InvalidRequest(
                "external process timeout must be positive".to_string(),
            ));
        }
        if self.max_stdout_bytes == 0 || self.max_stderr_bytes == 0 {
            return Err(ProcessRunError::InvalidRequest(
                "external process output limits must be positive".to_string(),
            ));
        }
        if self.args.iter().any(|argument| argument.contains('\0')) {
            return Err(ProcessRunError::InvalidRequest(
                "external process argument contains NUL".to_string(),
            ));
        }
        if self.environment.iter().any(|(key, value)| {
            key.is_empty() || key.contains('=') || key.contains('\0') || value.contains('\0')
        }) {
            return Err(ProcessRunError::InvalidRequest(
                "external process environment contains an invalid key or NUL".to_string(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessTermination {
    Exited(i32),
    Signaled,
    TimedOut,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessOutput {
    pub termination: ProcessTermination,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessRunError {
    InvalidRequest(String),
    InvalidExecutable(String),
    ExecutableDigestMismatch {
        expected: Sha256Digest,
        actual: Sha256Digest,
    },
    ExecutableChanged(String),
    Io(String),
    Reader(String),
}

impl std::fmt::Display for ProcessRunError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidRequest(message) => {
                write!(formatter, "invalid process request: {message}")
            }
            Self::InvalidExecutable(message) => write!(formatter, "invalid executable: {message}"),
            Self::ExecutableDigestMismatch { expected, actual } => write!(
                formatter,
                "executable digest mismatch: expected {}, actual {}",
                expected.as_str(),
                actual.as_str()
            ),
            Self::ExecutableChanged(message) => write!(formatter, "executable changed: {message}"),
            Self::Io(message) => write!(formatter, "process I/O failed: {message}"),
            Self::Reader(message) => write!(formatter, "process output reader failed: {message}"),
        }
    }
}

impl std::error::Error for ProcessRunError {}

pub fn run_process(spec: &ProcessSpec) -> Result<ProcessOutput, ProcessRunError> {
    spec.validate()?;
    spec.executable.revalidate()?;
    let mut command = Command::new(spec.executable.path());
    command
        .args(&spec.args)
        .env_clear()
        .envs(spec.environment.iter().map(|(key, value)| (key, value)))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // One Effect owns one process group. Timeout and normal leader exit
        // reap/terminate the complete descendant group so a
        // helper cannot outlive the Result/Event returned to the domain.
        .process_group(0);

    // This is the final check before the pathname-based exec performed by
    // std::process. The remaining check->exec window is documented as a macOS
    // platform residual because Darwin exposes execve/posix_spawn by pathname,
    // not a stable executable-fd API.
    spec.executable.revalidate()?;
    let mut child = command
        .spawn()
        .map_err(|error| ProcessRunError::Io(format!("spawn: {error}")))?;
    let process_group = i32::try_from(child.id())
        .map_err(|_| ProcessRunError::Io("child PID exceeds pid_t range".to_string()))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| ProcessRunError::Io("stdout pipe unavailable".to_string()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| ProcessRunError::Io("stderr pipe unavailable".to_string()))?;
    let stdout_limit = spec.max_stdout_bytes;
    let stderr_limit = spec.max_stderr_bytes;
    let stdout_reader = thread::spawn(move || read_bounded_to_eof(stdout, stdout_limit));
    let stderr_reader = thread::spawn(move || read_bounded_to_eof(stderr, stderr_limit));

    let started = Instant::now();
    let termination = loop {
        if started.elapsed() >= spec.timeout {
            terminate_group_and_wait(process_group, &mut child, "timeout")?;
            break ProcessTermination::TimedOut;
        }
        match child
            .try_wait()
            .map_err(|error| ProcessRunError::Io(format!("wait: {error}")))?
        {
            Some(status) => {
                // The direct child is done, but descendants may still own stdout/stderr.
                // Terminate the now-orphaned group before joining readers.
                terminate_process_group(process_group).map_err(|error| {
                    ProcessRunError::Io(format!("leader exit cleanup: {error}"))
                })?;
                break status
                    .code()
                    .map(ProcessTermination::Exited)
                    .unwrap_or(ProcessTermination::Signaled);
            }
            None => thread::sleep(POLL_INTERVAL),
        }
    };

    let (stdout, stdout_truncated) = join_reader(stdout_reader)?;
    let (stderr, stderr_truncated) = join_reader(stderr_reader)?;
    spec.executable.revalidate()?;
    Ok(ProcessOutput {
        termination,
        stdout,
        stderr,
        stdout_truncated,
        stderr_truncated,
    })
}

fn terminate_group_and_wait(
    process_group: i32,
    child: &mut Child,
    reason: &str,
) -> Result<(), ProcessRunError> {
    terminate_process_group(process_group).map_err(|error| {
        ProcessRunError::Io(format!(
            "{reason}: failed to terminate process group: {error}"
        ))
    })?;
    child.wait().map_err(|error| {
        ProcessRunError::Io(format!("{reason}: wait after termination: {error}"))
    })?;
    Ok(())
}

fn terminate_process_group(process_group: i32) -> io::Result<()> {
    // Negative pid targets the complete process group. ESRCH is idempotent success:
    // the group disappeared between observation and termination.
    let result = unsafe { libc::kill(-process_group, libc::SIGKILL) };
    if result == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(error)
    }
}

fn join_reader(
    handle: thread::JoinHandle<io::Result<(Vec<u8>, bool)>>,
) -> Result<(Vec<u8>, bool), ProcessRunError> {
    handle
        .join()
        .map_err(|_| ProcessRunError::Reader("reader thread panicked".to_string()))?
        .map_err(|error| ProcessRunError::Reader(error.to_string()))
}

fn read_bounded_to_eof(mut reader: impl Read, limit: usize) -> io::Result<(Vec<u8>, bool)> {
    let mut kept = Vec::with_capacity(limit.min(64 * 1024));
    let mut buffer = [0u8; 16 * 1024];
    let mut truncated = false;
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        let remaining = limit.saturating_sub(kept.len());
        let copy = remaining.min(read);
        kept.extend_from_slice(&buffer[..copy]);
        truncated |= copy < read;
    }
    Ok((kept, truncated))
}

fn open_executable(path: &Path) -> Result<File, ProcessRunError> {
    crate::io::capability::open_read(path)
        .map_err(|error| ProcessRunError::Io(format!("open executable: {error}")))
}

fn digest_handle(mut file: File) -> Result<Sha256Digest, ProcessRunError> {
    let mut state = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| ProcessRunError::Io(format!("read executable: {error}")))?;
        if read == 0 {
            break;
        }
        state.update(&buffer[..read]);
    }
    Ok(Sha256Digest::from_digest_bytes(state.finalize().into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_reader_discards_beyond_evidence_limit_without_stopping_drain() {
        let input = vec![7u8; 1024];
        let (kept, truncated) = read_bounded_to_eof(&input[..], 16).expect("read");
        assert_eq!(kept.len(), 16);
        assert!(truncated);
    }
}
