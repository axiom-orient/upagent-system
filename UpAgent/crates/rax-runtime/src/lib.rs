#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
#![forbid(unsafe_code)]

mod approval;
mod codec;
mod engine;
mod error;
mod execution;
mod fs;
mod git;
mod journal;
mod planning;
mod process;
mod timeutil;
mod workspace;

pub use approval::{DenialRecorded, GrantIssued};
pub use engine::{DEFAULT_EXECUTION_LOCK_WAIT, Engine, EngineOptions};
pub use journal::{RecoveryRecord, RecoveryResolution, RunRecord, RunSummary, StateStore};
pub use process::CancellationToken;
pub use workspace::Workspace;
