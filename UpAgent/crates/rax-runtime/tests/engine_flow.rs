#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::{
    fs,
    path::PathBuf,
    process::Command,
    time::{Duration, Instant},
};

#[cfg(unix)]
use std::{sync::Arc, thread};

use rax_core::{
    ContentEncoding, ErrorCode, FsReadInput, FsWriteInput, GitDiffInput, Operation,
    OperationOutput, Outcome, ProcessRunInput, RaxError, RequestEnvelope, SCHEMA_VERSION,
};
use rax_runtime::{CancellationToken, Engine, EngineOptions, RecoveryResolution};
use tempfile::TempDir;

struct Harness {
    root: TempDir,
    workspace: PathBuf,
    state: PathBuf,
    engine: Engine,
}

impl Harness {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("temporary root");
        let workspace = root.path().join("workspace");
        let state = root.path().join("state");
        fs::create_dir(&workspace).expect("workspace directory");
        let engine = Engine::open(
            &workspace,
            EngineOptions {
                state_dir: Some(state.clone()),
                ..EngineOptions::default()
            },
        )
        .expect("engine opens");
        Self {
            root,
            workspace,
            state,
            engine,
        }
    }
}

fn request(operation: Operation) -> RequestEnvelope {
    RequestEnvelope {
        schema_version: SCHEMA_VERSION.to_string(),
        request_id: None,
        operation,
        record: false,
    }
}

fn git(workspace: &PathBuf, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(workspace)
        .output()
        .expect("git executable");
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}

fn run_diff(harness: &Harness, staged: bool, paths: &[&str]) -> Outcome {
    harness
        .engine
        .run(request(Operation::GitDiff(GitDiffInput {
            repository: String::new(),
            staged,
            paths: paths.iter().map(|path| (*path).to_string()).collect(),
            max_bytes: 1024 * 1024,
        })))
        .expect("git diff result")
        .outcome
}

fn diff_precondition_error(outcome: Outcome) -> RaxError {
    match outcome {
        Outcome::Failed { error, .. } => error,
        other => panic!("git diff must fail with a precondition error, got {other:?}"),
    }
}

#[test]
fn git_diff_fails_closed_for_scoped_untracked_and_ignored_paths() {
    let harness = Harness::new();
    git(&harness.workspace, &["init"]);
    git(
        &harness.workspace,
        &["config", "user.email", "test@example.com"],
    );
    git(&harness.workspace, &["config", "user.name", "UpAgent Test"]);
    fs::write(harness.workspace.join("tracked.txt"), "before\n").expect("tracked fixture");
    git(&harness.workspace, &["add", "tracked.txt"]);
    git(&harness.workspace, &["commit", "-m", "initial"]);

    fs::write(harness.workspace.join("! old.txt"), "rename source\n").expect("rename fixture");
    git(&harness.workspace, &["add", "--", "! old.txt"]);
    git(&harness.workspace, &["commit", "-m", "rename source"]);
    git(
        &harness.workspace,
        &["mv", "--", "! old.txt", "renamed.txt"],
    );
    fs::write(harness.workspace.join("renamed.txt"), "rename target\n").expect("rename change");
    match run_diff(&harness, false, &["renamed.txt"]) {
        Outcome::Succeeded { .. } => {}
        other => {
            panic!("tracked rename with !-prefixed original must remain diffable, got {other:?}")
        }
    }

    fs::write(harness.workspace.join("tracked.txt"), "after\n").expect("tracked change");

    fs::write(harness.workspace.join("outside.txt"), "outside\n").expect("outside fixture");
    match run_diff(&harness, false, &["tracked.txt"]) {
        Outcome::Succeeded {
            output: OperationOutput::GitDiff(output),
            ..
        } => {
            assert!(output.text.contains("-before"));
            assert!(output.text.contains("+after"));
        }
        other => panic!("tracked path must remain diffable, got {other:?}"),
    }

    let error = diff_precondition_error(run_diff(&harness, false, &["tracked.txt", "outside.txt"]));
    assert_eq!(error.code, ErrorCode::PreconditionFailed);
    assert_eq!(error.phase, rax_core::ErrorPhase::Apply);
    assert!(!error.retryable);
    assert!(error.message.contains("read_file"));
    assert!(error.message.contains("git_status"));
    assert_eq!(error.details["untrackedPathCount"], 1);
    assert_eq!(error.details["ignoredPathCount"], 0);
    fs::remove_file(harness.workspace.join("outside.txt")).expect("remove outside fixture");

    fs::write(harness.workspace.join("single.txt"), "single\n").expect("untracked fixture");
    let error = diff_precondition_error(run_diff(&harness, false, &["single.txt"]));
    assert_eq!(error.code, ErrorCode::PreconditionFailed);
    assert_eq!(error.details["untrackedPathCount"], 1);
    assert_eq!(error.details["ignoredPathCount"], 0);
    fs::remove_file(harness.workspace.join("single.txt")).expect("remove untracked fixture");

    fs::write(harness.workspace.join(".gitignore"), "ignored.txt\n").expect("ignore fixture");
    git(&harness.workspace, &["add", ".gitignore"]);
    git(&harness.workspace, &["commit", "-m", "ignore fixture"]);
    fs::write(harness.workspace.join("ignored.txt"), "ignored\n").expect("ignored fixture");
    let error = diff_precondition_error(run_diff(&harness, false, &["ignored.txt"]));
    assert_eq!(error.code, ErrorCode::PreconditionFailed);
    assert_eq!(error.details["untrackedPathCount"], 0);
    assert_eq!(error.details["ignoredPathCount"], 1);
    fs::remove_file(harness.workspace.join("ignored.txt")).expect("remove ignored fixture");

    fs::write(harness.workspace.join("repo-wide.txt"), "repo-wide\n").expect("repo-wide fixture");
    let error = diff_precondition_error(run_diff(&harness, false, &[]));
    assert_eq!(error.code, ErrorCode::PreconditionFailed);
    assert_eq!(error.details["untrackedPathCount"], 1);
    assert_eq!(error.details["ignoredPathCount"], 0);
    fs::remove_file(harness.workspace.join("repo-wide.txt")).expect("remove repo-wide fixture");

    fs::write(harness.workspace.join("staged.txt"), "staged\n").expect("staged fixture");
    git(&harness.workspace, &["add", "staged.txt"]);
    match run_diff(&harness, true, &["staged.txt"]) {
        Outcome::Succeeded {
            output: OperationOutput::GitDiff(output),
            ..
        } => assert!(output.text.contains("+staged")),
        other => panic!("staged added file must remain diffable, got {other:?}"),
    }
}

#[test]
fn bounded_read_executes_without_approval() {
    let harness = Harness::new();
    fs::write(harness.workspace.join("message.txt"), b"hello rax").expect("fixture");

    let result = harness
        .engine
        .run(request(Operation::FsRead(FsReadInput {
            path: "message.txt".to_string(),
            max_bytes: 1024,
        })))
        .expect("read result");

    match result.outcome {
        Outcome::Succeeded {
            output: OperationOutput::FsRead(output),
            receipt_path,
        } => {
            assert_eq!(output.content, "hello rax");
            assert!(!output.truncated);
            assert!(receipt_path.is_none());
        }
        other => panic!("unexpected read outcome: {other:?}"),
    }
}

#[test]
fn write_requires_exact_plan_grant_and_revalidation() {
    let harness = Harness::new();
    let target = harness.workspace.join("message.txt");
    fs::write(&target, b"before").expect("fixture");

    let planned = harness
        .engine
        .run(request(Operation::FsWrite(FsWriteInput {
            path: "message.txt".to_string(),
            content: "after".to_string(),
            encoding: ContentEncoding::Utf8,
            create_only: false,
            expected_sha256: None,
        })))
        .expect("plan result");
    let plan = match planned.outcome {
        Outcome::ApprovalRequired { plan } => plan,
        other => panic!("write must require approval, got {other:?}"),
    };
    let grant = harness.engine.approve(&plan.plan_id).expect("grant");

    fs::write(&target, b"changed outside rax").expect("stale fixture");
    let stale = harness
        .engine
        .apply(&plan.plan_id, &grant.token)
        .expect_err("stale plan must fail before grant consumption");
    assert_eq!(stale.code, ErrorCode::PreconditionFailed);

    fs::write(&target, b"before").expect("restore exact precondition");
    let applied = harness
        .engine
        .apply(&plan.plan_id, &grant.token)
        .expect("apply result");
    assert!(matches!(applied.outcome, Outcome::Succeeded { .. }));
    assert_eq!(fs::read_to_string(target).expect("written file"), "after");
}

#[test]
fn state_directory_inside_workspace_is_rejected_without_creation() {
    let root = tempfile::tempdir().expect("temporary root");
    let workspace = root.path().join("workspace");
    fs::create_dir(&workspace).expect("workspace directory");
    let state = workspace.join(".rax-state");

    let error = Engine::open(
        &workspace,
        EngineOptions {
            state_dir: Some(state.clone()),
            ..EngineOptions::default()
        },
    )
    .expect_err("state directory inside workspace must be rejected");

    assert_eq!(error.code, ErrorCode::InvalidPath);
    assert!(
        !state.exists(),
        "validation must not create rejected state paths"
    );
}

#[cfg(unix)]
#[test]
fn filesystem_reads_reject_symlink_escape() {
    use std::os::unix::fs::symlink;

    let harness = Harness::new();
    let outside = harness.root.path().join("outside.txt");
    fs::write(&outside, b"secret").expect("outside fixture");
    symlink(&outside, harness.workspace.join("link.txt")).expect("symlink fixture");

    let result = harness
        .engine
        .run(request(Operation::FsRead(FsReadInput {
            path: "link.txt".to_string(),
            max_bytes: 1024,
        })))
        .expect("read result");
    match result.outcome {
        Outcome::Failed { error, .. } => assert_eq!(error.code, ErrorCode::SymlinkNotAllowed),
        other => panic!("symlink escape must fail, got {other:?}"),
    }
}

#[cfg(unix)]
#[test]
fn one_time_process_grant_cannot_be_replayed() {
    let harness = Harness::new();
    let program = which::which("true")
        .expect("true executable")
        .to_string_lossy()
        .to_string();
    let planned = harness
        .engine
        .run(request(Operation::ProcessRun(ProcessRunInput {
            program,
            args: Vec::new(),
            cwd: None,
            env: std::collections::BTreeMap::new(),
            clear_env: false,
            timeout_ms: 5_000,
            max_output_bytes: 1024,
        })))
        .expect("plan result");
    let plan = match planned.outcome {
        Outcome::ApprovalRequired { plan } => plan,
        other => panic!("process must require approval, got {other:?}"),
    };
    let grant = harness.engine.approve(&plan.plan_id).expect("grant");
    let first = harness
        .engine
        .apply(&plan.plan_id, &grant.token)
        .expect("first apply");
    assert!(matches!(first.outcome, Outcome::Succeeded { .. }));

    let replay = harness
        .engine
        .apply(&plan.plan_id, &grant.token)
        .expect("replay is represented as a typed result");
    match replay.outcome {
        Outcome::Denied { error } => assert_eq!(error.code, ErrorCode::GrantConsumed),
        other => panic!("grant replay must be denied, got {other:?}"),
    }
}

#[cfg(unix)]
#[test]
fn failed_process_is_recoverable_until_operator_resolution() {
    let harness = Harness::new();
    let program = which::which("sh")
        .expect("POSIX shell")
        .to_string_lossy()
        .to_string();
    let planned = harness
        .engine
        .run(request(Operation::ProcessRun(ProcessRunInput {
            program,
            args: vec!["-c".to_string(), "exit 7".to_string()],
            cwd: None,
            env: std::collections::BTreeMap::new(),
            clear_env: false,
            timeout_ms: 5_000,
            max_output_bytes: 1024,
        })))
        .expect("plan result");
    let plan = match planned.outcome {
        Outcome::ApprovalRequired { plan } => plan,
        other => panic!("process must require approval, got {other:?}"),
    };
    let grant = harness.engine.approve(&plan.plan_id).expect("grant");
    let applied = harness
        .engine
        .apply(&plan.plan_id, &grant.token)
        .expect("apply result");
    assert!(matches!(applied.outcome, Outcome::PartiallyApplied { .. }));

    let run_id = applied.run_id;
    assert!(
        harness
            .engine
            .list_recovery()
            .expect("recovery list")
            .iter()
            .any(|run| run.run_id == run_id)
    );

    harness
        .engine
        .resolve_recovery(
            &run_id,
            RecoveryResolution::VerifiedFailed,
            Some("exit 7 was expected by the test".to_string()),
        )
        .expect("recovery resolution");
    assert!(
        !harness
            .engine
            .list_recovery()
            .expect("resolved recovery list")
            .iter()
            .any(|run| run.run_id == run_id)
    );

    assert!(harness.state.exists());
}

#[cfg(unix)]
#[test]
fn per_call_cancellation_terminates_and_reaps_a_running_process() {
    let harness = Harness::new();
    let program = which::which("sh")
        .expect("POSIX shell")
        .to_string_lossy()
        .to_string();
    let planned = harness
        .engine
        .run(request(Operation::ProcessRun(ProcessRunInput {
            program,
            args: vec![
                "-c".to_string(),
                "printf started > cancellation-marker; sleep 30".to_string(),
            ],
            cwd: None,
            env: std::collections::BTreeMap::new(),
            clear_env: false,
            timeout_ms: 60_000,
            max_output_bytes: 1024,
        })))
        .expect("plan result");
    let plan = match planned.outcome {
        Outcome::ApprovalRequired { plan } => plan,
        other => panic!("process must require approval, got {other:?}"),
    };
    let grant = harness.engine.approve(&plan.plan_id).expect("grant");
    let marker = harness.workspace.join("cancellation-marker");
    let engine = Arc::new(harness.engine);
    let cancellation = CancellationToken::default();
    let worker = {
        let engine = Arc::clone(&engine);
        let cancellation = cancellation.clone();
        let plan_id = plan.plan_id.clone();
        let token = grant.token.clone();
        thread::spawn(move || engine.apply_with_cancellation(&plan_id, &token, &cancellation))
    };

    let wait_started = Instant::now();
    while !marker.exists() && wait_started.elapsed() < Duration::from_secs(2) {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        marker.exists(),
        "child process did not start before cancellation"
    );
    cancellation.cancel();

    let result = worker
        .join()
        .expect("cancellation worker did not panic")
        .expect("typed cancellation result");
    match result.outcome {
        Outcome::PartiallyApplied { error, .. } => {
            assert_eq!(error.code, ErrorCode::ProcessCancelled);
        }
        other => panic!("running process cancellation must require resolution, got {other:?}"),
    }
}

/// Resolve the per-workspace execution lock inside the state root.
fn execution_lock_path(state: &std::path::Path) -> PathBuf {
    let workspaces = state.join("workspaces");
    let mut entries = fs::read_dir(&workspaces)
        .expect("workspace state directory")
        .map(|entry| entry.expect("workspace state entry").path())
        .collect::<Vec<_>>();
    entries.sort();
    assert_eq!(
        entries.len(),
        1,
        "expected exactly one workspace state root"
    );
    entries[0].join("execution.lock")
}

/// Build an approval-gated write and take it to the point just before apply.
fn approved_write(harness: &Harness, content: &str) -> (String, String) {
    fs::write(harness.workspace.join("message.txt"), b"before").expect("fixture");
    let planned = harness
        .engine
        .run(request(Operation::FsWrite(FsWriteInput {
            path: "message.txt".to_string(),
            content: content.to_string(),
            encoding: ContentEncoding::Utf8,
            create_only: false,
            expected_sha256: None,
        })))
        .expect("plan result");
    let plan = match planned.outcome {
        Outcome::ApprovalRequired { plan } => plan,
        other => panic!("write must require approval, got {other:?}"),
    };
    let grant = harness.engine.approve(&plan.plan_id).expect("grant");
    (plan.plan_id, grant.token)
}

#[test]
fn a_contended_apply_boundary_times_out_instead_of_parking_forever() {
    let root = tempfile::tempdir().expect("temporary root");
    let workspace = root.path().join("workspace");
    let state = root.path().join("state");
    fs::create_dir(&workspace).expect("workspace directory");
    let engine = Engine::open(
        &workspace,
        EngineOptions {
            state_dir: Some(state.clone()),
            execution_lock_wait: Duration::from_millis(200),
            ..EngineOptions::default()
        },
    )
    .expect("engine opens");
    let harness = Harness {
        root,
        workspace,
        state,
        engine,
    };
    let (plan_id, token) = approved_write(&harness, "after");

    // Hold the same lock the apply boundary serializes on. A blocking `flock`
    // would suspend the apply here with no way to interrupt it.
    let lock_path = execution_lock_path(&harness.state);
    let held = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .expect("execution lock file");
    fs2::FileExt::lock_exclusive(&held).expect("hold the execution lock");

    let started = Instant::now();
    let error = harness
        .engine
        .apply(&plan_id, &token)
        .expect_err("a contended apply must fail rather than hang");
    let elapsed = started.elapsed();

    assert_eq!(error.code, ErrorCode::OperationFailed);
    assert!(
        error.message.contains("execution lock"),
        "unexpected message: {}",
        error.message
    );
    assert!(error.retryable, "lock contention must be retryable");
    // The lower bound proves the apply genuinely contended for the lock, so the
    // test cannot pass against an uncontended fast path.
    assert!(
        elapsed >= Duration::from_millis(200),
        "apply returned after {elapsed:?} without waiting on the lock at all"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "apply parked for {elapsed:?} instead of honouring its bounded wait"
    );

    fs2::FileExt::unlock(&held).expect("release the execution lock");
}

#[test]
fn a_cancelled_apply_does_not_wait_for_the_execution_lock() {
    let harness = Harness::new();
    let (plan_id, token) = approved_write(&harness, "after");

    let lock_path = execution_lock_path(&harness.state);
    let held = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .expect("execution lock file");
    fs2::FileExt::lock_exclusive(&held).expect("hold the execution lock");

    let cancellation = CancellationToken::default();
    cancellation.cancel();

    let started = Instant::now();
    let error = harness
        .engine
        .apply_with_cancellation(&plan_id, &token, &cancellation)
        .expect_err("a cancelled apply must not wait on the lock");
    let elapsed = started.elapsed();

    assert_eq!(error.code, ErrorCode::ProcessCancelled);
    assert!(
        elapsed < Duration::from_secs(5),
        "cancellation was observed only after {elapsed:?}"
    );

    fs2::FileExt::unlock(&held).expect("release the execution lock");
}
