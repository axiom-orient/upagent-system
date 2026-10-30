#![forbid(unsafe_code)]

//! Independent `ReviewCampaign` executable.
//!
//! This binary intentionally owns only the review campaign lifecycle. The
//! general `agent` executable does not link this package, so a review cannot
//! be started accidentally as a hidden agent mode.

use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    io::{self, Write},
    path::{Path, PathBuf},
    process::ExitCode,
    sync::Arc,
};

use agent_profile_api::CompositionPluginCatalog;
use agent_provider_api::ProviderPluginEnvelope;
use agent_review::{
    ProviderReviewRunner, ProviderRoute, ProviderSetupFailure, ReviewCampaign, ReviewCancellation,
    ReviewExecutionLease, ReviewPreflight, ReviewRequest, ReviewResult, ReviewRole, ReviewRunner,
    ReviewSkill, ReviewSubjectInput, RoutesFile, claim_repair_handoff, inspect_review,
    materialize_review_subject, read_json_file, recover_review, verify_review,
};
use agent_skills::SkillCatalog;
use clap::{Parser, Subcommand, error::ErrorKind};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use serde_json::json;

const DEFAULT_MAXIMUM_PARALLEL_TASKS: usize = 4;
const PLUGIN_ROUTES_SCHEMA_VERSION: u32 = 5;

/// Host-owned review transport configuration. The review domain never sees
/// plugin-owned fields; it receives only the derived provider-neutral
/// `RoutesFile`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct PluginRoutesFile {
    schema_version: u32,
    routes: Vec<PluginReviewRoute>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct PluginReviewRoute {
    id: String,
    provider: ProviderPluginEnvelope,
}

impl PluginRoutesFile {
    /// Select only routes referenced by the validated request. Unselected
    /// provider envelopes are never resolved beyond bounded JSON/ID parsing,
    /// so one unused route cannot block an otherwise valid campaign.
    fn select(
        &self,
        request: &ReviewRequest,
        plugin_catalog: &CompositionPluginCatalog,
    ) -> Result<(RoutesFile, BTreeMap<String, ProviderPluginEnvelope>), String> {
        if self.schema_version != PLUGIN_ROUTES_SCHEMA_VERSION {
            return Err(format!(
                "unsupported review routes schema {}; expected {PLUGIN_ROUTES_SCHEMA_VERSION}",
                self.schema_version
            ));
        }
        if self.routes.is_empty() || self.routes.len() > 16 {
            return Err("routes must contain 1..=16 entries".to_owned());
        }
        request.validate().map_err(|error| error.to_string())?;

        let mut by_id = BTreeMap::<String, &PluginReviewRoute>::new();
        for route in &self.routes {
            validate_route_id(&route.id)?;
            if by_id.insert(route.id.clone(), route).is_some() {
                return Err(format!("duplicate provider route {}", route.id));
            }
        }

        let selected_ids = request
            .recipe
            .discoverers
            .iter()
            .chain(request.recipe.fact_checkers.iter())
            .map(|lane| lane.route_id.clone())
            .collect::<BTreeSet<_>>();
        let mut routes = Vec::with_capacity(selected_ids.len());
        let mut configs = BTreeMap::<String, ProviderPluginEnvelope>::new();
        for route_id in selected_ids {
            let route = by_id
                .get(&route_id)
                .ok_or_else(|| format!("provider route {route_id} is not defined"))?;
            let binding = plugin_catalog
                .provider_binding_from_envelope(&route.provider)
                .map_err(|error| error.to_string())?;
            routes.push(ProviderRoute {
                id: route_id.clone(),
                binding,
            });
            configs.insert(route_id, route.provider.clone());
        }
        let routes = RoutesFile { routes };
        routes.validate().map_err(|error| error.to_string())?;
        Ok((routes, configs))
    }
}

fn validate_route_id(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value == "."
        || value == ".."
        || value.len() > 96
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        return Err("route id must be 1..=96 stable identifier bytes".to_owned());
    }
    Ok(())
}

#[derive(Debug, Parser)]
#[command(
    name = "agent-review",
    version,
    about = "Independent multi-lane ReviewCampaign executor"
)]
struct Cli {
    #[arg(long, env = "AGENT_STATE_ROOT", global = true)]
    state_root: Option<PathBuf>,
    #[arg(
        long = "skills-root",
        env = "AGENT_SKILLS_ROOT",
        value_delimiter = ',',
        global = true
    )]
    skill_roots: Vec<PathBuf>,
    #[arg(long, default_value_t = DEFAULT_MAXIMUM_PARALLEL_TASKS, global = true)]
    maximum_parallel_tasks: usize,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run or resume the fixed discovery -> fact-check -> reduce campaign.
    Run {
        #[arg(long, default_value = "-")]
        request: String,
        #[arg(long)]
        routes: PathBuf,
        /// Artifact store root required for an `artifactBundle` subject.
        #[arg(long)]
        artifact_root: Option<PathBuf>,
    },
    /// Print durable request, event, lane, and result evidence.
    Inspect {
        #[arg(long)]
        review: String,
    },
    /// Recompute immutable digests and the deterministic consensus reducer.
    Verify {
        #[arg(long)]
        review: String,
    },
    /// Report resumable lanes and unresolved external outcomes.
    Recover {
        #[arg(long)]
        review: String,
    },
    /// Claim the one immutable repair handoff, without executing a repair.
    ClaimRepair {
        #[arg(long)]
        review: String,
    },
}

#[derive(Debug, Clone)]
struct GlobalOptions {
    state_root: Option<PathBuf>,
    skill_roots: Vec<PathBuf>,
    maximum_parallel_tasks: usize,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => return handle_parse_error(&error),
    };
    let global = GlobalOptions {
        state_root: cli.state_root,
        skill_roots: cli.skill_roots,
        maximum_parallel_tasks: cli.maximum_parallel_tasks,
    };
    match execute(global, cli.command).await {
        Ok(code) => code,
        Err(error) => {
            let _ = write_json(&json!({
                "ok": false,
                "error": {"code": "review_cli_failed", "message": error}
            }));
            ExitCode::from(2)
        }
    }
}

fn handle_parse_error(error: &clap::Error) -> ExitCode {
    if matches!(
        error.kind(),
        ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
    ) {
        print!("{error}");
        return ExitCode::SUCCESS;
    }
    let _ = write_json(&json!({
        "ok": false,
        "error": {"code": "review_cli_failed", "message": error.to_string()}
    }));
    ExitCode::from(2)
}

async fn execute(global: GlobalOptions, command: Command) -> Result<ExitCode, String> {
    match command {
        Command::Run {
            request,
            routes,
            artifact_root,
        } => run_campaign(&global, &request, &routes, artifact_root).await,
        Command::Inspect { review } => {
            let root = resolve_existing_state_root(global.state_root)?;
            write_json(&inspect_review(&root, &review).map_err(|error| error.to_string())?)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Verify { review } => {
            let root = resolve_existing_state_root(global.state_root)?;
            verify_review(&root, &review).map_err(|error| error.to_string())?;
            write_json(&json!({"ok": true, "reviewId": review}))?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Recover { review } => {
            let root = resolve_existing_state_root(global.state_root)?;
            let report = recover_review(&root, &review).map_err(|error| error.to_string())?;
            let code = if report.resumable {
                ExitCode::from(10)
            } else {
                ExitCode::SUCCESS
            };
            write_json(&report)?;
            Ok(code)
        }
        Command::ClaimRepair { review } => {
            let root = resolve_existing_state_root(global.state_root)?;
            let claim = claim_repair_handoff(&root, &review).map_err(|error| error.to_string())?;
            write_json(&claim)?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

async fn run_campaign(
    global: &GlobalOptions,
    request_source: &str,
    routes_path: &Path,
    artifact_root: Option<PathBuf>,
) -> Result<ExitCode, String> {
    if global.maximum_parallel_tasks == 0 || global.maximum_parallel_tasks > 64 {
        return Err("--maximum-parallel-tasks must be in 1..=64".to_owned());
    }
    let request: ReviewRequest =
        read_json_file(request_source, agent_review::MAX_REVIEW_REQUEST_BYTES)
            .map_err(|error| error.to_string())?;
    match (&request.subject, artifact_root.as_deref()) {
        (ReviewSubjectInput::ArtifactBundle { .. }, None) => {
            return Err("--artifact-root is required for an artifactBundle subject".to_owned());
        }
        (ReviewSubjectInput::Inline { .. }, Some(_)) => {
            return Err("--artifact-root is only valid for an artifactBundle subject".to_owned());
        }
        _ => {}
    }
    // Resolve the subject before route/skill discovery. This is intentionally
    // a provider- and state-free ingress check; campaign preflight repeats the
    // bounded read while binding the materialized bytes into its recipe.
    materialize_review_subject(&request.subject, artifact_root.as_deref())
        .map_err(|error| error.to_string())?;
    let routes_source = routes_path
        .to_str()
        .ok_or_else(|| "review routes path must be UTF-8".to_owned())?;
    let stock_routes: PluginRoutesFile =
        read_json_file(routes_source, agent_review::MAX_REVIEW_REQUEST_BYTES)
            .map_err(|error| error.to_string())?;
    let mut plugin_catalog = CompositionPluginCatalog::new();
    for plugin in agent_provider::linked_provider_plugins()? {
        plugin_catalog
            .register_provider(plugin)
            .map_err(|error| error.to_string())?;
    }
    for plugin in agent_profiles::linked_profile_plugins()? {
        plugin_catalog
            .register_profile(plugin)
            .map_err(|error| error.to_string())?;
    }
    plugin_catalog
        .finalize()
        .map_err(|error| error.to_string())?;
    let profile_registry = plugin_catalog
        .profiles()
        .map_err(|error| error.to_string())?;
    profile_registry
        .resolve(&agent_core::ProfileId::reviewer())
        .map_err(|error| format!("reviewer profile plugin is unavailable: {error}"))?;
    let (routes, provider_configs) = stock_routes.select(&request, &plugin_catalog)?;
    // Artifact ingress is performed by campaign preflight before it creates
    // state or touches provider credentials.
    let state_root = resolve_state_path(global.state_root.clone())?;
    let skill_roots = resolve_skill_roots(global.skill_roots.clone())?;
    let skill_catalog =
        SkillCatalog::discover(skill_roots.clone()).map_err(|error| error.to_string())?;
    let skill_digests = reviewer_skill_digests(&skill_catalog)?;
    let campaign = ReviewCampaign::new(state_root.clone(), skill_digests);
    let preflight = campaign
        .preflight_with_artifact_root(&request, &routes, artifact_root.as_deref())
        .map_err(|error| error.to_string())?;
    let validated = ReviewCampaign::validate_request_routes(&request, &routes)
        .map_err(|error| error.to_string())?;
    if let Some(result) = preflight.cached_result.as_ref() {
        write_json(result)?;
        return Ok(exit_for_review(result));
    }
    let mut lease = campaign
        .acquire_execution_lease(&preflight)
        .map_err(|error| error.to_string())?;
    campaign
        .record_provider_setup_started(&preflight, &mut lease)
        .map_err(|error| error.to_string())?;

    let mut runners: BTreeMap<String, Arc<dyn ReviewRunner>> = BTreeMap::new();
    for route_id in validated.identities.keys().cloned().collect::<Vec<_>>() {
        let configuration = provider_configs
            .get(&route_id)
            .cloned()
            .ok_or_else(|| format!("validated route {route_id} has no provider plugin envelope"))?;
        let planes = match plugin_catalog
            .connect_provider_envelope(&configuration)
            .await
        {
            Ok(value) => value,
            Err(error) => {
                let message = review_setup_failure(
                    &campaign,
                    &preflight,
                    &mut lease,
                    ProviderSetupFailure::Connection,
                    error.to_string(),
                );
                return Err(message);
            }
        };
        runners.insert(
            route_id.clone(),
            Arc::new(
                ProviderReviewRunner::new(planes.model, &route_id)
                    .map_err(|error| error.to_string())?,
            ),
        );
    }
    if let Err(error) = campaign.record_provider_setup_completed(&preflight, &mut lease) {
        return Err(error.to_string());
    }
    let result =
        run_review_with_signals(&campaign, request, routes, runners, &preflight, &lease).await;
    let result = result?;
    write_json(&result)?;
    Ok(exit_for_review(&result))
}

fn reviewer_skill_digests(catalog: &SkillCatalog) -> Result<BTreeMap<String, ReviewSkill>, String> {
    [
        ReviewRole::Neutral,
        ReviewRole::Critical,
        ReviewRole::Adversarial,
        ReviewRole::FactChecker,
    ]
    .into_iter()
    .map(|role| {
        let id = agent_core::SkillId::parse(role.skill_id()).map_err(|error| error.to_string())?;
        let document = catalog
            .load(&id, &agent_core::ProfileId::reviewer())
            .map_err(|error| error.to_string())?;
        Ok((
            role.skill_id().to_owned(),
            ReviewSkill {
                digest: document.instructions_sha256,
                instructions: document.instructions,
            },
        ))
    })
    .collect()
}

fn review_setup_failure(
    campaign: &ReviewCampaign,
    preflight: &ReviewPreflight,
    lease: &mut ReviewExecutionLease,
    failure: ProviderSetupFailure,
    original: String,
) -> String {
    match campaign.record_provider_setup_failed(preflight, lease, failure) {
        Ok(()) => original,
        Err(error) => format!("{original}; failed to record review provider setup: {error}"),
    }
}

#[cfg(unix)]
async fn run_review_with_signals(
    campaign: &ReviewCampaign,
    request: ReviewRequest,
    routes: RoutesFile,
    runners: BTreeMap<String, Arc<dyn ReviewRunner>>,
    preflight: &ReviewPreflight,
    lease: &ReviewExecutionLease,
) -> Result<ReviewResult, String> {
    use tokio::signal::unix::{SignalKind, signal};
    let cancellation = ReviewCancellation::new();
    let mut interrupt = signal(SignalKind::interrupt()).map_err(|error| error.to_string())?;
    let mut terminate = signal(SignalKind::terminate()).map_err(|error| error.to_string())?;
    let execution = campaign.run_with_execution_lease(
        request,
        routes,
        runners,
        preflight,
        lease,
        cancellation.clone(),
    );
    tokio::pin!(execution);
    let result = tokio::select! {
        biased;
        result = &mut execution => result,
        _ = interrupt.recv() => { cancellation.cancel(); execution.await }
        _ = terminate.recv() => { cancellation.cancel(); execution.await }
    };
    result.map_err(|error| error.to_string())
}

#[cfg(not(unix))]
async fn run_review_with_signals(
    campaign: &ReviewCampaign,
    request: ReviewRequest,
    routes: RoutesFile,
    runners: BTreeMap<String, Arc<dyn ReviewRunner>>,
    preflight: &ReviewPreflight,
    lease: &ReviewExecutionLease,
) -> Result<ReviewResult, String> {
    let cancellation = ReviewCancellation::new();
    let execution = campaign.run_with_execution_lease(
        request,
        routes,
        runners,
        preflight,
        lease,
        cancellation.clone(),
    );
    tokio::pin!(execution);
    let result = tokio::select! {
        biased;
        result = &mut execution => result,
        signal = tokio::signal::ctrl_c() => {
            signal.map_err(|error| error.to_string())?;
            cancellation.cancel();
            execution.await
        }
    };
    result.map_err(|error| error.to_string())
}

fn exit_for_review(result: &ReviewResult) -> ExitCode {
    match result.status {
        agent_review::CampaignStatus::Clean => ExitCode::SUCCESS,
        agent_review::CampaignStatus::Findings
        | agent_review::CampaignStatus::Blocked
        | agent_review::CampaignStatus::Incomplete => ExitCode::from(1),
    }
}

fn resolve_state_path(value: Option<PathBuf>) -> Result<PathBuf, String> {
    let path = value.unwrap_or_else(|| {
        ProjectDirs::from("dev", "axiom", "unified-profiled-agent").map_or_else(
            || PathBuf::from(".upagent-state"),
            |dirs| dirs.data_local_dir().to_path_buf(),
        )
    });
    if !path.is_absolute() {
        return Err("state root must be an absolute path".to_owned());
    }
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            Err("state root must be a real directory".to_owned())
        }
        Ok(_) => path.canonicalize().map_err(|error| error.to_string()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(path),
        Err(error) => Err(error.to_string()),
    }
}

fn resolve_existing_state_root(value: Option<PathBuf>) -> Result<PathBuf, String> {
    let path = value.unwrap_or_else(|| {
        ProjectDirs::from("dev", "axiom", "unified-profiled-agent").map_or_else(
            || PathBuf::from(".upagent-state"),
            |dirs| dirs.data_local_dir().to_path_buf(),
        )
    });
    let metadata = std::fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err("state root must be an existing real directory".to_owned());
    }
    path.canonicalize().map_err(|error| error.to_string())
}

fn resolve_skill_roots(values: Vec<PathBuf>) -> Result<Vec<PathBuf>, String> {
    let candidates = if values.is_empty() {
        default_skill_candidates()?
    } else {
        values
    };
    let mut roots = Vec::new();
    for path in candidates {
        let canonical = path
            .canonicalize()
            .map_err(|error| format!("skill root {} is unavailable: {error}", path.display()))?;
        if !canonical.is_dir() {
            return Err(format!(
                "skill root {} is not a directory",
                canonical.display()
            ));
        }
        if !roots.contains(&canonical) {
            roots.push(canonical);
        }
    }
    if roots.is_empty() {
        return Err("no skill roots were configured".to_owned());
    }
    Ok(roots)
}

fn default_skill_candidates() -> Result<Vec<PathBuf>, String> {
    let mut candidates = Vec::new();
    if let Ok(executable) = env::current_exe()
        && let Some(parent) = executable.parent()
    {
        let sibling = parent.join("skills");
        if sibling.is_dir() {
            candidates.push(sibling);
        }
    }
    let working = env::current_dir()
        .map_err(|error| error.to_string())?
        .join("skills");
    if working.is_dir() {
        candidates.push(working);
    }
    if candidates.is_empty() {
        return Err("no default skills directory was found; pass --skills-root".to_owned());
    }
    Ok(candidates)
}

fn write_json(value: &impl Serialize) -> Result<(), String> {
    let stdout = io::stdout();
    let mut writer = io::BufWriter::new(stdout.lock());
    serde_json::to_writer_pretty(&mut writer, value).map_err(|error| error.to_string())?;
    writer.write_all(b"\n").map_err(|error| error.to_string())?;
    writer.flush().map_err(|error| error.to_string())
}
