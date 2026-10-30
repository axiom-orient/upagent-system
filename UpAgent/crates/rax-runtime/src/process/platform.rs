use std::{
    collections::BTreeMap,
    env,
    ffi::OsString,
    io,
    path::{Path, PathBuf},
    process::{Child, Command},
};

use rax_core::{ErrorCode, ErrorPhase, RaxError};

#[cfg(windows)]
use crate::error::io_error;

#[cfg(windows)]
use std::process::Stdio;

pub(super) fn resolve_program(
    program: &str,
    cwd: &Path,
    environment: &BTreeMap<String, String>,
    clear_env: bool,
) -> Result<PathBuf, RaxError> {
    let requested = Path::new(program);
    if requested.is_absolute() {
        return Ok(requested.to_path_buf());
    }
    if requested.components().count() > 1 {
        return Ok(cwd.join(requested));
    }

    let path = environment_value(environment, "PATH")
        .map(OsString::from)
        .or_else(|| (!clear_env).then(|| env::var_os("PATH")).flatten())
        .ok_or_else(|| {
            executable_not_found(
                program,
                "a bare program name requires PATH when clearEnv is enabled",
            )
        })?;

    for directory in env::split_paths(&path) {
        let directory = if directory.as_os_str().is_empty() {
            cwd.to_path_buf()
        } else if directory.is_absolute() {
            directory
        } else {
            cwd.join(directory)
        };
        for candidate in executable_candidates(&directory, program, environment, clear_env) {
            if is_executable_file(&candidate) {
                return Ok(candidate);
            }
        }
    }

    Err(executable_not_found(
        program,
        "program was not found on the configured PATH",
    ))
}

#[cfg(windows)]
fn environment_value<'a>(environment: &'a BTreeMap<String, String>, name: &str) -> Option<&'a str> {
    environment
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

#[cfg(not(windows))]
fn environment_value<'a>(environment: &'a BTreeMap<String, String>, name: &str) -> Option<&'a str> {
    environment.get(name).map(String::as_str)
}

#[cfg(windows)]
fn executable_candidates(
    directory: &Path,
    program: &str,
    environment: &BTreeMap<String, String>,
    clear_env: bool,
) -> Vec<PathBuf> {
    let requested = Path::new(program);
    if requested.extension().is_some() {
        return vec![directory.join(requested)];
    }
    let path_ext = environment_value(environment, "PATHEXT")
        .map(ToOwned::to_owned)
        .or_else(|| (!clear_env).then(|| env::var("PATHEXT").ok()).flatten())
        .unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".to_string());
    path_ext
        .split(';')
        .filter(|extension| !extension.is_empty())
        .map(|extension| directory.join(format!("{program}{extension}")))
        .collect()
}

#[cfg(not(windows))]
fn executable_candidates(
    directory: &Path,
    program: &str,
    _environment: &BTreeMap<String, String>,
    _clear_env: bool,
) -> Vec<PathBuf> {
    vec![directory.join(program)]
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    path.metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

fn executable_not_found(program: &str, message: &str) -> RaxError {
    RaxError::new(
        ErrorCode::ProcessSpawnFailed,
        ErrorPhase::Apply,
        false,
        message,
    )
    .with_detail("program", program.to_string())
}

/// Observed liveness of the supervised child.
///
/// `Exited { identifier_held }` distinguishes the only ordering that makes
/// process-group signalling sound. A process-group identifier is released once
/// the group is empty and its leader has been reaped, and the kernel may then
/// reissue that number to an unrelated process that becomes a group leader of
/// its own. Signalling is therefore only ever safe while this process still
/// owns the identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ChildState {
    Running,
    Exited { identifier_held: bool },
}

/// Observe the child without releasing its process-group identifier.
///
/// On Unix `waitid` with `WNOWAIT` reports the exit while leaving the leader as
/// an unreaped zombie, so the identifier stays reserved until the caller
/// explicitly reaps. On other platforms `Child` keeps the OS process handle
/// open until it is dropped, which reserves the identifier by itself.
#[cfg(unix)]
pub(super) fn child_state(pid: u32, child: &mut Child) -> io::Result<ChildState> {
    use rustix::{
        io::Errno,
        process::{Pid, WaitId, WaitIdOptions, waitid},
    };

    let Some(target) = i32::try_from(pid).ok().and_then(Pid::from_raw) else {
        return reaped_child_state(child);
    };
    let options = WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT;
    match waitid(WaitId::Pid(target), options) {
        Ok(Some(_)) => Ok(ChildState::Exited {
            identifier_held: true,
        }),
        Ok(None) => Ok(ChildState::Running),
        // The child was already reaped, so the identifier is no longer ours and
        // must never be signalled.
        Err(Errno::CHILD) => reaped_child_state(child),
        Err(error) => Err(io::Error::from(error)),
    }
}

#[cfg(unix)]
fn reaped_child_state(child: &mut Child) -> io::Result<ChildState> {
    Ok(match child.try_wait()? {
        Some(_) => ChildState::Exited {
            identifier_held: false,
        },
        None => ChildState::Running,
    })
}

#[cfg(not(unix))]
pub(super) fn child_state(_pid: u32, child: &mut Child) -> io::Result<ChildState> {
    Ok(match child.try_wait()? {
        Some(_) => ChildState::Exited {
            identifier_held: true,
        },
        None => ChildState::Running,
    })
}

#[cfg(unix)]
pub(super) fn configure_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

#[cfg(windows)]
pub(super) fn configure_process_group(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    command.creation_flags(CREATE_NEW_PROCESS_GROUP);
}

#[cfg(not(any(unix, windows)))]
pub(super) fn configure_process_group(_command: &mut Command) {}

#[cfg(unix)]
pub(super) fn terminate_group(pid: u32, force: bool) -> Result<(), RaxError> {
    use rustix::{
        io::Errno,
        process::{Pid, Signal, kill_process_group},
    };

    let raw_pid = i32::try_from(pid).map_err(|_| {
        RaxError::new(
            ErrorCode::InvariantViolation,
            ErrorPhase::Apply,
            false,
            "child process identifier does not fit the platform pid type",
        )
        .with_detail("pid", u64::from(pid))
    })?;
    let process_group = Pid::from_raw(raw_pid).ok_or_else(|| {
        RaxError::new(
            ErrorCode::InvariantViolation,
            ErrorPhase::Apply,
            false,
            "child process identifier must be non-zero",
        )
    })?;
    let signal = if force { Signal::KILL } else { Signal::TERM };
    match kill_process_group(process_group, signal) {
        Ok(()) => Ok(()),
        // The child and every descendant may exit between observation and
        // signalling. An already-empty group satisfies the cleanup contract.
        Err(error) if error == Errno::SRCH => Ok(()),
        Err(error) => Err(RaxError::new(
            ErrorCode::OperationFailed,
            ErrorPhase::Apply,
            false,
            "failed to terminate the child process group",
        )
        .with_detail("pid", u64::from(pid))
        .with_detail("signal", if force { "KILL" } else { "TERM" })
        .with_detail("osError", error.to_string())),
    }
}

#[cfg(windows)]
pub(super) fn terminate_group(pid: u32, _force: bool) -> Result<(), RaxError> {
    let status = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|error| command_error("taskkill", "Windows process-tree terminator", error))?;
    if status.success() {
        Ok(())
    } else {
        Err(RaxError::new(
            ErrorCode::OperationFailed,
            ErrorPhase::Apply,
            false,
            format!("taskkill exited with {status}"),
        ))
    }
}

#[cfg(not(any(unix, windows)))]
pub(super) fn terminate_group(_pid: u32, _force: bool) -> Result<(), RaxError> {
    Err(RaxError::new(
        ErrorCode::Unsupported,
        ErrorPhase::Apply,
        false,
        "process-tree termination is not implemented on this platform",
    ))
}

#[cfg(not(windows))]
pub(super) fn shell_command(command: &str) -> (String, Vec<String>) {
    (
        "sh".to_string(),
        vec!["-c".to_string(), command.to_string()],
    )
}

#[cfg(windows)]
pub(super) fn shell_command(command: &str) -> (String, Vec<String>) {
    (
        "cmd.exe".to_string(),
        vec![
            "/D".to_string(),
            "/S".to_string(),
            "/C".to_string(),
            command.to_string(),
        ],
    )
}

#[cfg(windows)]
fn command_error(program: &str, role: &str, error: std::io::Error) -> RaxError {
    if error.kind() == std::io::ErrorKind::NotFound {
        return RaxError::new(
            ErrorCode::Unsupported,
            ErrorPhase::Apply,
            false,
            format!("{role} is unavailable"),
        )
        .with_detail("executable", program.to_string());
    }
    io_error(
        ErrorPhase::Apply,
        &format!("failed to invoke {role}"),
        None,
        error,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_program_with_a_separator_is_resolved_from_child_cwd() {
        let cwd = Path::new("workspace");
        let resolved =
            resolve_program("tools/runner", cwd, &BTreeMap::new(), true).expect("relative program");
        assert_eq!(resolved, cwd.join("tools/runner"));
    }

    #[test]
    fn clear_environment_requires_an_explicit_path_for_a_bare_program() {
        let error = resolve_program("runner", Path::new("workspace"), &BTreeMap::new(), true)
            .expect_err("PATH must be explicit");
        assert_eq!(error.code, ErrorCode::ProcessSpawnFailed);
    }
}
