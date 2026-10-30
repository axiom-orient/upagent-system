mod command;
mod parse;

use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};

use rax_core::{
    ErrorCode, ErrorPhase, GitBranchesOutput, GitCheckoutInput, GitCommitInput, GitDiffInput,
    GitDiscardInput, GitLogInput, GitLogOutput, GitMutationOutput, GitPathsInput, GitPushInput,
    GitRemoteInput, GitRemoteOutput, GitRepositoryInput, GitShowInput, GitStatusOutput,
    GitTextOutput, RaxError,
};

use self::command::{
    GitRunner, process_details, require_complete_output, require_complete_stderr, text_output,
};
use crate::{process::CancellationToken, workspace::Workspace};

const GIT_READ_TIMEOUT: Duration = Duration::from_mins(2);
const GIT_STATUS_LIMIT: u64 = 16 * 1024 * 1024;

pub fn status(
    workspace: &Workspace,
    input: &GitRepositoryInput,
    cancellation: &CancellationToken,
) -> Result<GitStatusOutput, RaxError> {
    let runner = GitRunner::open(workspace, &input.repository, cancellation)?;
    status_with_runner(&runner, &input.repository)
}

pub(crate) fn repository_observation(
    workspace: &Workspace,
    repository: &str,
    cancellation: &CancellationToken,
) -> Result<(Option<String>, GitStatusOutput), RaxError> {
    let runner = GitRunner::open(workspace, repository, cancellation)?;
    let head = runner.head(GIT_READ_TIMEOUT)?;
    let status = status_with_runner(&runner, repository)?;
    Ok((head, status))
}

fn status_with_runner(
    runner: &GitRunner<'_>,
    repository: &str,
) -> Result<GitStatusOutput, RaxError> {
    let captured = runner.run(
        &["status", "--porcelain=v2", "--branch", "-z"],
        GIT_READ_TIMEOUT,
        GIT_STATUS_LIMIT,
    )?;
    require_complete_output(&captured, "git status")?;
    parse::status(repository, &captured.stdout, false)
}

pub fn diff(
    workspace: &Workspace,
    input: &GitDiffInput,
    cancellation: &CancellationToken,
) -> Result<GitTextOutput, RaxError> {
    let runner = GitRunner::open(workspace, &input.repository, cancellation)?;
    let mut status_args = vec![
        "status".to_string(),
        "--porcelain=v2".to_string(),
        "--untracked-files=all".to_string(),
        "--ignored=matching".to_string(),
        "-z".to_string(),
        "--".to_string(),
    ];
    status_args.extend(input.paths.iter().cloned());
    let status_process = runner.run(&status_args, GIT_READ_TIMEOUT, GIT_STATUS_LIMIT)?;
    require_complete_output(&status_process, "git diff preflight status")?;
    let status = parse::status(&input.repository, &status_process.stdout, false)?;
    ensure_diff_not_cancelled(cancellation)?;
    let untracked_path_count = status
        .files
        .iter()
        .filter(|file| file.index_status == "?" && file.worktree_status == "?")
        .count();
    let ignored_path_count = count_ignored_status_records(&status_process.stdout);
    if untracked_path_count != 0 || ignored_path_count != 0 {
        return Err(RaxError::new(
            ErrorCode::PreconditionFailed,
            ErrorPhase::Apply,
            false,
            "inspect_diff failed closed because scoped paths include untracked or ignored entries; use read_file for exact content and git_status for presence",
        )
        .with_detail(
            "untrackedPathCount",
            u64::try_from(untracked_path_count).unwrap_or(u64::MAX),
        )
        .with_detail(
            "ignoredPathCount",
            u64::try_from(ignored_path_count).unwrap_or(u64::MAX),
        ));
    }
    let mut args = vec![
        "diff".to_string(),
        "--no-ext-diff".to_string(),
        "--no-textconv".to_string(),
        "--no-color".to_string(),
    ];
    if input.staged {
        args.push("--cached".to_string());
    }
    args.push("--".to_string());
    args.extend(input.paths.iter().cloned());
    runner
        .run(&args, GIT_READ_TIMEOUT, input.max_bytes)
        .map(text_output)
}

pub fn log(
    workspace: &Workspace,
    input: &GitLogInput,
    cancellation: &CancellationToken,
) -> Result<GitLogOutput, RaxError> {
    let runner = GitRunner::open(workspace, &input.repository, cancellation)?;
    let max = input.max_entries.to_string();
    let captured = runner.run(
        &[
            "log",
            "-z",
            "--no-decorate",
            "--format=%H%x1f%P%x1f%an%x1f%ae%x1f%ct%x1f%s",
            "-n",
            &max,
        ],
        GIT_READ_TIMEOUT,
        GIT_STATUS_LIMIT,
    )?;
    require_complete_stderr(&captured, "git log")?;
    parse::log(&captured.stdout, captured.stdout_truncated)
}

pub fn show(
    workspace: &Workspace,
    input: &GitShowInput,
    cancellation: &CancellationToken,
) -> Result<GitTextOutput, RaxError> {
    let runner = GitRunner::open(workspace, &input.repository, cancellation)?;
    let args = vec![
        "show".to_string(),
        "--no-ext-diff".to_string(),
        "--no-textconv".to_string(),
        "--no-color".to_string(),
        "--format=fuller".to_string(),
        input.revision.clone(),
        "--".to_string(),
    ];
    runner
        .run(&args, GIT_READ_TIMEOUT, input.max_bytes)
        .map(text_output)
}

pub fn remotes(
    workspace: &Workspace,
    input: &GitRepositoryInput,
    cancellation: &CancellationToken,
) -> Result<GitRemoteOutput, RaxError> {
    let runner = GitRunner::open(workspace, &input.repository, cancellation)?;
    let captured = runner.run(&["remote", "-v"], GIT_READ_TIMEOUT, GIT_STATUS_LIMIT)?;
    require_complete_output(&captured, "git remote")?;
    parse::remotes(&captured.stdout)
}

pub fn branches(
    workspace: &Workspace,
    input: &GitRepositoryInput,
    cancellation: &CancellationToken,
) -> Result<GitBranchesOutput, RaxError> {
    let runner = GitRunner::open(workspace, &input.repository, cancellation)?;
    let captured = runner.run(
        &[
            "for-each-ref",
            "--format=%(refname)%00%(objectname)%00%(HEAD)",
            "refs/heads",
            "refs/remotes",
        ],
        GIT_READ_TIMEOUT,
        GIT_STATUS_LIMIT,
    )?;
    require_complete_output(&captured, "git branches")?;
    parse::branches(&captured.stdout)
}

pub fn stage(
    workspace: &Workspace,
    input: &GitPathsInput,
    cancellation: &CancellationToken,
) -> Result<GitMutationOutput, RaxError> {
    mutate_with_paths(
        workspace,
        &input.repository,
        "stage",
        &["add"],
        &input.paths,
        cancellation,
    )
}

pub fn unstage(
    workspace: &Workspace,
    input: &GitPathsInput,
    cancellation: &CancellationToken,
) -> Result<GitMutationOutput, RaxError> {
    mutate_with_paths(
        workspace,
        &input.repository,
        "unstage",
        &["reset"],
        &input.paths,
        cancellation,
    )
}

pub fn discard(
    workspace: &Workspace,
    input: &GitDiscardInput,
    cancellation: &CancellationToken,
) -> Result<GitMutationOutput, RaxError> {
    let runner = GitRunner::open(workspace, &input.repository, cancellation)?;
    let before = runner.head(GIT_READ_TIMEOUT)?;

    let mut status_args = vec![
        "status".to_string(),
        "--porcelain=v2".to_string(),
        "--untracked-files=all".to_string(),
        "-z".to_string(),
        "--".to_string(),
    ];
    status_args.extend(input.paths.iter().cloned());
    let captured = runner.run(&status_args, GIT_READ_TIMEOUT, GIT_STATUS_LIMIT)?;
    require_complete_output(&captured, "git discard preflight status")?;
    let changed = parse::status(&input.repository, &captured.stdout, false)?;

    let mut tracked = BTreeSet::new();
    let mut untracked = BTreeSet::new();
    for file in changed.files {
        if file.index_status == "?" && file.worktree_status == "?" {
            untracked.insert(file.path);
        } else {
            tracked.insert(file.path);
        }
    }

    let mut details = BTreeMap::from([
        (
            "trackedPathCount".to_string(),
            u64::try_from(tracked.len()).unwrap_or(u64::MAX).into(),
        ),
        (
            "untrackedPathCount".to_string(),
            u64::try_from(untracked.len()).unwrap_or(u64::MAX).into(),
        ),
    ]);
    if !tracked.is_empty() {
        let mut restore = vec![
            "restore".to_string(),
            "--worktree".to_string(),
            "--".to_string(),
        ];
        restore.extend(tracked);
        let process = runner
            .run(&restore, GIT_READ_TIMEOUT, GIT_STATUS_LIMIT)
            .map_err(|error| uncertain_git_mutation(error, "discard", false))?;
        insert_process_details(&mut details, "restore", &process);
    }

    if input.include_untracked && !untracked.is_empty() {
        let mut clean = vec!["clean".to_string(), "-fd".to_string(), "--".to_string()];
        clean.extend(untracked);
        let process = runner
            .run(&clean, GIT_READ_TIMEOUT, GIT_STATUS_LIMIT)
            .map_err(|error| uncertain_git_mutation(error, "discard", false))?;
        insert_process_details(&mut details, "clean", &process);
    }

    verified_mutation(&runner, &input.repository, "discard", before, details)
        .map_err(|error| uncertain_git_mutation(error, "discard", false))
}

pub fn commit(
    workspace: &Workspace,
    input: &GitCommitInput,
    cancellation: &CancellationToken,
) -> Result<GitMutationOutput, RaxError> {
    let runner = GitRunner::open(workspace, &input.repository, cancellation)?;
    let before = runner.head(GIT_READ_TIMEOUT)?;
    let args = vec![
        "commit".to_string(),
        "-m".to_string(),
        input.message.clone(),
    ];
    let captured = runner
        .run(&args, GIT_READ_TIMEOUT, GIT_STATUS_LIMIT)
        .map_err(|error| uncertain_git_mutation(error, "commit", false))?;
    verified_mutation(
        &runner,
        &input.repository,
        "commit",
        before,
        process_details(&captured),
    )
    .map_err(|error| uncertain_git_mutation(error, "commit", false))
}

pub fn checkout(
    workspace: &Workspace,
    input: &GitCheckoutInput,
    cancellation: &CancellationToken,
) -> Result<GitMutationOutput, RaxError> {
    let runner = GitRunner::open(workspace, &input.repository, cancellation)?;
    let before = runner.head(GIT_READ_TIMEOUT)?;
    let mut args = vec!["switch".to_string()];
    if input.create {
        args.push("--create".to_string());
    }
    args.push(input.target.clone());
    let captured = runner
        .run(&args, GIT_READ_TIMEOUT, GIT_STATUS_LIMIT)
        .map_err(|error| uncertain_git_mutation(error, "checkout", false))?;
    verified_mutation(
        &runner,
        &input.repository,
        "checkout",
        before,
        process_details(&captured),
    )
    .map_err(|error| uncertain_git_mutation(error, "checkout", false))
}

pub fn fetch(
    workspace: &Workspace,
    input: &GitRemoteInput,
    cancellation: &CancellationToken,
) -> Result<GitMutationOutput, RaxError> {
    remote_mutation(workspace, input, "fetch", false, cancellation)
}

pub fn pull_ff(
    workspace: &Workspace,
    input: &GitRemoteInput,
    cancellation: &CancellationToken,
) -> Result<GitMutationOutput, RaxError> {
    remote_mutation(workspace, input, "pull_ff", true, cancellation)
}

pub fn push(
    workspace: &Workspace,
    input: &GitPushInput,
    cancellation: &CancellationToken,
) -> Result<GitMutationOutput, RaxError> {
    let runner = GitRunner::open(workspace, &input.repository, cancellation)?;
    let before = runner.head(GIT_READ_TIMEOUT)?;
    let mut args = vec!["push".to_string()];
    if input.set_upstream {
        args.push("--set-upstream".to_string());
    }
    if let Some(remote) = &input.remote {
        args.push(remote.clone());
    }
    if let Some(branch) = &input.branch {
        args.push(branch.clone());
    }
    let captured = runner
        .run(
            &args,
            Duration::from_millis(input.timeout_ms),
            GIT_STATUS_LIMIT,
        )
        .map_err(|error| uncertain_git_mutation(error, "push", true))?;
    verified_mutation(
        &runner,
        &input.repository,
        "push",
        before,
        process_details(&captured),
    )
    .map_err(|error| uncertain_git_mutation(error, "push", true))
}

fn mutate_with_paths(
    workspace: &Workspace,
    repository: &str,
    action: &str,
    prefix: &[&str],
    paths: &[String],
    cancellation: &CancellationToken,
) -> Result<GitMutationOutput, RaxError> {
    let runner = GitRunner::open(workspace, repository, cancellation)?;
    let before = runner.head(GIT_READ_TIMEOUT)?;
    let mut args: Vec<String> = prefix.iter().map(|value| (*value).to_string()).collect();
    args.push("--".to_string());
    args.extend(paths.iter().cloned());
    let captured = runner
        .run(&args, GIT_READ_TIMEOUT, GIT_STATUS_LIMIT)
        .map_err(|error| uncertain_git_mutation(error, action, false))?;
    verified_mutation(
        &runner,
        repository,
        action,
        before,
        process_details(&captured),
    )
    .map_err(|error| uncertain_git_mutation(error, action, false))
}

fn remote_mutation(
    workspace: &Workspace,
    input: &GitRemoteInput,
    action: &str,
    pull: bool,
    cancellation: &CancellationToken,
) -> Result<GitMutationOutput, RaxError> {
    let runner = GitRunner::open(workspace, &input.repository, cancellation)?;
    let before = runner.head(GIT_READ_TIMEOUT)?;
    let mut args = if pull {
        vec!["pull".to_string(), "--ff-only".to_string()]
    } else {
        vec!["fetch".to_string()]
    };
    if let Some(remote) = &input.remote {
        args.push(remote.clone());
    }
    let captured = runner
        .run(
            &args,
            Duration::from_millis(input.timeout_ms),
            GIT_STATUS_LIMIT,
        )
        .map_err(|error| uncertain_git_mutation(error, action, true))?;
    verified_mutation(
        &runner,
        &input.repository,
        action,
        before,
        process_details(&captured),
    )
    .map_err(|error| uncertain_git_mutation(error, action, true))
}

fn verified_mutation(
    runner: &GitRunner<'_>,
    repository: &str,
    action: &str,
    head_before: Option<String>,
    details: BTreeMap<String, serde_json::Value>,
) -> Result<GitMutationOutput, RaxError> {
    let head_after = runner.head(GIT_READ_TIMEOUT)?;
    let status = runner.run(
        &["status", "--porcelain=v2", "--branch", "-z"],
        GIT_READ_TIMEOUT,
        GIT_STATUS_LIMIT,
    )?;
    require_complete_output(&status, "post-mutation git status")?;
    let _ = parse::status(repository, &status.stdout, false)?;
    Ok(GitMutationOutput {
        repository: repository.to_string(),
        action: action.to_string(),
        head_before,
        head_after,
        verified: true,
        details,
    })
}

fn insert_process_details(
    target: &mut BTreeMap<String, serde_json::Value>,
    prefix: &str,
    process: &crate::process::CapturedProcess,
) {
    for (key, value) in process_details(process) {
        target.insert(format!("{prefix}.{key}"), value);
    }
}

fn uncertain_git_mutation(mut error: RaxError, action: &str, remote_effect: bool) -> RaxError {
    error
        .details
        .insert("operation".to_string(), action.to_string().into());
    if remote_effect {
        error
            .details
            .insert("remoteSideEffectsUnknown".to_string(), true.into());
    }
    if !matches!(
        error.code,
        ErrorCode::GitUnavailable | ErrorCode::ProcessSpawnFailed | ErrorCode::GitNotRepository
    ) {
        error
            .details
            .insert("partialApplied".to_string(), true.into());
        error.details.insert(
            "partialReason".to_string(),
            "the Git command started or post-apply verification failed; repository or remote state must be inspected"
                .into(),
        );
    }
    error
}

fn strict_utf8<'a>(bytes: &'a [u8], source: &str) -> Result<&'a str, RaxError> {
    std::str::from_utf8(bytes).map_err(|_| {
        protocol_error(format!(
            "{source} was not UTF-8 and cannot be represented by the Rax JSON protocol"
        ))
    })
}

fn protocol_error(message: impl Into<String>) -> RaxError {
    RaxError::new(ErrorCode::ProtocolError, ErrorPhase::Apply, false, message)
}

fn count_ignored_status_records(bytes: &[u8]) -> usize {
    let mut records = bytes
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty());
    let mut ignored = 0;
    while let Some(record) = records.next() {
        if record.starts_with(b"2 ") {
            // Porcelain-v2 rename records are followed by an original path
            // record. That path is data, not another status record.
            let _ = records.next();
        } else if record.starts_with(b"! ") {
            ignored += 1;
        }
    }
    ignored
}

fn ensure_diff_not_cancelled(cancellation: &CancellationToken) -> Result<(), RaxError> {
    if cancellation.is_cancelled() {
        return Err(RaxError::new(
            ErrorCode::ProcessCancelled,
            ErrorPhase::Apply,
            false,
            "inspect_diff was cancelled after status preflight and before diff execution",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{count_ignored_status_records, ensure_diff_not_cancelled};
    use crate::process::CancellationToken;
    use rax_core::{ErrorCode, ErrorPhase};

    #[test]
    fn ignored_count_skips_a_rename_original_path_record() {
        let status =
            b"2 R. 100 0000000 0000000 0000000 0000000 renamed.txt\0! old.txt\0! ignored.txt\0";
        assert_eq!(count_ignored_status_records(status), 1);
    }

    #[test]
    fn diff_cancellation_is_typed_before_execution() {
        let cancellation = CancellationToken::default();
        cancellation.cancel();
        let error = ensure_diff_not_cancelled(&cancellation).expect_err("cancelled diff");
        assert_eq!(error.code, ErrorCode::ProcessCancelled);
        assert_eq!(error.phase, ErrorPhase::Apply);
        assert!(!error.retryable);
    }
}
