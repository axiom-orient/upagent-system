use std::{
    path::Path,
    time::{Duration, Instant},
};

use rax_core::{
    ErrorCode, ErrorPhase, EventKind, Outcome, Plan, RaxError, RequestEnvelope, ResultEnvelope,
    SCHEMA_VERSION,
};
use uuid::Uuid;

use crate::{
    approval::{self, DenialRecorded, GrantIssued},
    execution::{elapsed_ms, execute_outcome, outcome_event, outcome_name, result_envelope},
    journal::{RecoveryRecord, RecoveryResolution, RunRecord, RunSummary, StateStore},
    planning::{self, DEFAULT_PLAN_TTL},
    process::CancellationToken,
    timeutil::now_rfc3339,
    workspace::Workspace,
};

/// Backstop for the apply-boundary lock wait.
///
/// Cancellation is the primary interrupt; this bound only guarantees that a
/// caller with no cancellation signal still fails instead of parking forever.
pub const DEFAULT_EXECUTION_LOCK_WAIT: Duration = Duration::from_mins(5);

#[derive(Debug, Clone)]
pub struct EngineOptions {
    pub state_dir: Option<std::path::PathBuf>,
    pub plan_ttl: Duration,
    pub execution_lock_wait: Duration,
}

impl Default for EngineOptions {
    fn default() -> Self {
        Self {
            state_dir: None,
            plan_ttl: DEFAULT_PLAN_TTL,
            execution_lock_wait: DEFAULT_EXECUTION_LOCK_WAIT,
        }
    }
}

#[derive(Debug)]
pub struct Engine {
    workspace: Workspace,
    store: StateStore,
    cancellation: CancellationToken,
    plan_ttl: Duration,
    execution_lock_wait: Duration,
}

impl Engine {
    #[allow(
        clippy::needless_pass_by_value,
        reason = "EngineOptions is the owned construction contract for Engine"
    )]
    pub fn open(workspace: impl AsRef<Path>, options: EngineOptions) -> Result<Self, RaxError> {
        const MAX_PLAN_TTL: Duration = Duration::from_hours(24);
        if options.plan_ttl.is_zero() || options.plan_ttl > MAX_PLAN_TTL {
            return Err(RaxError::new(
                ErrorCode::InvalidRequest,
                ErrorPhase::Validate,
                false,
                "plan TTL must be between 1 second and 24 hours",
            ));
        }
        if options.execution_lock_wait.is_zero() || options.execution_lock_wait > MAX_PLAN_TTL {
            return Err(RaxError::new(
                ErrorCode::InvalidRequest,
                ErrorPhase::Validate,
                false,
                "execution lock wait must be between 1 second and 24 hours",
            ));
        }
        let workspace = Workspace::open(workspace)?;
        let store = StateStore::open(&workspace, options.state_dir.as_deref())?;
        Ok(Self {
            workspace,
            store,
            cancellation: CancellationToken::default(),
            plan_ttl: options.plan_ttl,
            execution_lock_wait: options.execution_lock_wait,
        })
    }

    #[must_use]
    pub fn workspace(&self) -> &Workspace {
        &self.workspace
    }

    #[must_use]
    pub fn state_store(&self) -> &StateStore {
        &self.store
    }

    pub fn get_plan(&self, plan_id: &str) -> Result<Plan, RaxError> {
        self.store.load_plan(plan_id)
    }

    #[must_use]
    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancellation.clone()
    }

    pub fn plan(&self, request: RequestEnvelope) -> Result<Plan, RaxError> {
        self.plan_with_cancellation(request, &self.cancellation)
    }

    pub fn plan_with_cancellation(
        &self,
        request: RequestEnvelope,
        cancellation: &CancellationToken,
    ) -> Result<Plan, RaxError> {
        let request = planning::normalize_request(request)?;
        let plan = planning::build_plan(&self.workspace, &request, cancellation, self.plan_ttl)?;
        self.store.write_plan(&plan)?;
        Ok(plan)
    }

    pub fn run(&self, request: RequestEnvelope) -> Result<ResultEnvelope, RaxError> {
        self.run_with_cancellation(request, &self.cancellation)
    }

    pub fn run_with_cancellation(
        &self,
        request: RequestEnvelope,
        cancellation: &CancellationToken,
    ) -> Result<ResultEnvelope, RaxError> {
        let started = Instant::now();
        let started_at = now_rfc3339()?;
        let request = planning::normalize_request(request)?;
        let plan = planning::build_plan(&self.workspace, &request, cancellation, self.plan_ttl)?;
        if planning::approval_required(&plan) {
            self.store.write_plan(&plan)?;
            return Ok(ResultEnvelope {
                schema_version: SCHEMA_VERSION.to_string(),
                request_id: plan.request_id.clone(),
                run_id: Uuid::now_v7().to_string(),
                operation: plan.operation.operation_name().to_string(),
                started_at,
                finished_at: now_rfc3339()?,
                duration_ms: elapsed_ms(started),
                outcome: Outcome::ApprovalRequired { plan },
            });
        }
        self.execute(request, plan, started_at, started, cancellation)
    }

    pub fn approve(&self, plan_id: &str) -> Result<GrantIssued, RaxError> {
        let plan = self.store.load_plan(plan_id)?;
        approval::approve(&self.store, &plan)
    }

    pub fn deny(&self, plan_id: &str) -> Result<DenialRecorded, RaxError> {
        let plan = self.store.load_plan(plan_id)?;
        approval::deny(&self.store, &plan)
    }

    pub fn apply(&self, plan_id: &str, token: &str) -> Result<ResultEnvelope, RaxError> {
        self.apply_with_cancellation(plan_id, token, &self.cancellation)
    }

    pub fn apply_with_cancellation(
        &self,
        plan_id: &str,
        token: &str,
        cancellation: &CancellationToken,
    ) -> Result<ResultEnvelope, RaxError> {
        let started = Instant::now();
        let started_at = now_rfc3339()?;
        let plan = self.store.load_plan(plan_id)?;
        if !planning::approval_required(&plan) {
            return Err(RaxError::new(
                ErrorCode::InvalidRequest,
                ErrorPhase::Authorize,
                false,
                "apply accepts only approval-gated plans; use run for read-only operations",
            )
            .with_detail("planId", plan.plan_id.clone()));
        }

        // Serialize the authoritative apply boundary: current-state
        // revalidation, one-time grant consumption, and execution. Approval
        // authorizes the sealed plan; it does not redundantly rescan a workspace
        // that apply must validate again immediately before mutation.
        //
        // The wait is bounded and cancellation-aware so a peer holding the lock
        // cannot suspend this apply past the caller's own deadline.
        let execution_lock = self
            .store
            .lock_execution(cancellation, self.execution_lock_wait)?;
        planning::revalidate_plan(&self.workspace, &plan, cancellation)?;

        let request = RequestEnvelope {
            schema_version: SCHEMA_VERSION.to_string(),
            request_id: Some(plan.request_id.clone()),
            operation: plan.operation.clone(),
            record: true,
        };
        let mut journal = self.store.begin_run(&request, &plan)?;
        journal.append(EventKind::Validated)?;
        journal.append(EventKind::Planned)?;

        if let Err(error) = approval::consume(&self.store, &plan, token) {
            let event = match error.code {
                ErrorCode::ApprovalDenied | ErrorCode::GrantConsumed | ErrorCode::GrantMismatch => {
                    EventKind::Denied
                }
                ErrorCode::GrantExpired => EventKind::Expired,
                _ => EventKind::Failed,
            };
            journal.append(event)?;
            let outcome = match error.code {
                ErrorCode::ApprovalDenied | ErrorCode::GrantConsumed | ErrorCode::GrantMismatch => {
                    Outcome::Denied { error }
                }
                ErrorCode::GrantExpired => Outcome::Expired { error },
                _ => Outcome::Failed {
                    error,
                    receipt_path: Some(journal.receipt_path()),
                },
            };
            let result = result_envelope(&plan, journal.run_id(), &started_at, started, outcome)?;
            journal.finish(&result)?;
            return Ok(result);
        }

        if let Err(record_error) = journal.append(EventKind::Authorized) {
            let receipt = journal.receipt_path();
            let mut result = result_envelope(
                &plan,
                journal.run_id(),
                &started_at,
                started,
                Outcome::PartiallyApplied {
                    error: record_error
                        .with_detail("grantConsumed", true)
                        .with_detail("planId", plan.plan_id.clone()),
                    receipt_path: Some(receipt),
                },
            )?;
            persist_partial_result(&mut journal, &mut result);
            return Ok(result);
        }
        let result =
            self.execute_with_journal(request, plan, started_at, started, journal, cancellation);
        drop(execution_lock);
        result
    }

    pub fn list_runs(&self) -> Result<Vec<RunSummary>, RaxError> {
        self.store.list_runs()
    }

    pub fn show_run(&self, run_id: &str) -> Result<RunRecord, RaxError> {
        self.store.show_run(run_id)
    }

    pub fn list_recovery(&self) -> Result<Vec<RunSummary>, RaxError> {
        self.store.list_recovery()
    }

    pub fn resolve_recovery(
        &self,
        run_id: &str,
        resolution: RecoveryResolution,
        note: Option<String>,
    ) -> Result<RecoveryRecord, RaxError> {
        self.store.resolve_recovery(run_id, resolution, note)
    }

    fn execute(
        &self,
        request: RequestEnvelope,
        plan: Plan,
        started_at: String,
        started: Instant,
        cancellation: &CancellationToken,
    ) -> Result<ResultEnvelope, RaxError> {
        let should_record = request.record || !request.operation.is_read_only();
        if should_record {
            let mut journal = self.store.begin_run(&request, &plan)?;
            journal.append(EventKind::Validated)?;
            journal.append(EventKind::Planned)?;
            journal.append(EventKind::Authorized)?;
            return self.execute_with_journal(
                request,
                plan,
                started_at,
                started,
                journal,
                cancellation,
            );
        }

        let run_id = Uuid::now_v7().to_string();
        let outcome = execute_outcome(&self.workspace, cancellation, &request.operation, None);
        result_envelope(&plan, &run_id, &started_at, started, outcome)
    }

    #[allow(clippy::needless_pass_by_value)]
    fn execute_with_journal(
        &self,
        request: RequestEnvelope,
        plan: Plan,
        started_at: String,
        started: Instant,
        mut journal: crate::journal::RunJournal,
        cancellation: &CancellationToken,
    ) -> Result<ResultEnvelope, RaxError> {
        journal.append(EventKind::Started)?;
        let receipt = journal.receipt_path();
        let outcome = execute_outcome(
            &self.workspace,
            cancellation,
            &request.operation,
            Some(receipt.clone()),
        );
        let terminal = outcome_event(&outcome);
        if let Err(record_error) = journal.append(terminal) {
            let mut result = result_envelope(
                &plan,
                journal.run_id(),
                &started_at,
                started,
                Outcome::PartiallyApplied {
                    error: record_error.with_detail("originalOutcome", outcome_name(&outcome)),
                    receipt_path: Some(receipt),
                },
            )?;
            persist_partial_result(&mut journal, &mut result);
            return Ok(result);
        }
        let mut result = result_envelope(&plan, journal.run_id(), &started_at, started, outcome)?;
        if let Err(record_error) = journal.finish(&result) {
            result.outcome = Outcome::PartiallyApplied {
                error: record_error.with_detail("originalOutcome", outcome_name(&result.outcome)),
                receipt_path: Some(receipt),
            };
            // A write can become visible before the parent-directory sync fails.
            // Persist the recovery-required outcome so disk evidence cannot retain a
            // terminal success while the caller receives PartiallyApplied.
            persist_partial_result(&mut journal, &mut result);
        }
        Ok(result)
    }
}

fn persist_partial_result(journal: &mut crate::journal::RunJournal, result: &mut ResultEnvelope) {
    if let Err(record_error) = journal.finish(result)
        && let Outcome::PartiallyApplied { error, .. } = &mut result.outcome
    {
        *error = error
            .clone()
            .with_detail("resultRecordError", record_error.to_string());
    }
}
