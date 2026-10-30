#![forbid(unsafe_code)]

use agent_cli::composition;

use std::{
    env,
    io::{self, Write},
    path::{Path, PathBuf},
    process::ExitCode,
    sync::Arc,
};

use agent_core::{
    AgentResult, AgentSpec, AgentStatus, CompositionReceipt, ProfileManifest, TaskEnvelopeV3,
};
use agent_memory::MemoryStore;
use agent_provider_api::ProviderPluginEnvelope;
use agent_runtime::{
    AgentRuntime, AgentRuntimeOptions, ApprovalStore, ResolvedTask, TaskCancellation,
};
use agent_runtime::{CompositionStore, EffectRecoveryResolution};
use agent_skills::SkillCatalog;
use clap::{Parser, Subcommand, ValueEnum, error::ErrorKind};
use directories::ProjectDirs;
use schemars::schema_for;
use serde_json::{Value, json};

mod input;

#[cfg(test)]
use input::BoundedReader;
use input::read_json;
#[cfg(test)]
use std::io::Read;

#[derive(Debug, Parser)]
#[command(
    name = "agent",
    version,
    about = "One profiled agent runtime with bounded provider and effect planes"
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
    #[arg(long, default_value_t = 4, global = true)]
    maximum_parallel_tasks: usize,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Clone)]
struct GlobalOptions {
    state_root: Option<PathBuf>,
    skill_roots: Vec<PathBuf>,
    maximum_parallel_tasks: usize,
}

#[derive(Debug, Subcommand)]
enum Command {
    Profiles,
    Skills,
    /// List the linked-Rust provider/profile plugins and manifest digests that
    /// may be selected in a provider envelope or composition.
    Plugins,
    /// Resolve a task-only request, provider binding, and `AgentSpec` without
    /// opening provider transport or runtime state.
    Compose {
        #[arg(long)]
        spec: PathBuf,
        /// Provider plugin envelope (`pluginId`, `manifestSha256`, and owned configuration),
        /// separate from `AgentSpec`.
        #[arg(long = "provider-config")]
        provider_config: PathBuf,
        #[arg(long)]
        request: PathBuf,
    },
    Schema {
        #[arg(value_enum)]
        kind: SchemaKind,
    },
    Doctor,
    Run {
        /// Explicit agent policy. Provider transport configuration is separate.
        #[arg(long)]
        spec: PathBuf,
        /// Provider plugin envelope selected from the linked catalog.
        #[arg(long = "provider-config")]
        provider_config: PathBuf,
        #[arg(long, default_value = "-")]
        request: String,
        /// Exact digest printed by `agent compose` for the same provider binding.
        #[arg(long)]
        expect_digest: String,
    },
    /// Print and verify one stopped task's durable request, events, and result.
    Inspect {
        #[arg(long)]
        session: String,
        #[arg(long)]
        task: String,
    },
    Recover {
        #[command(subcommand)]
        command: RecoverCommand,
    },
    Memory {
        #[command(subcommand)]
        command: MemoryCommand,
    },
    /// Decide pending effect and extension approvals.
    Approvals {
        #[command(subcommand)]
        command: ApprovalCommand,
    },
}

#[derive(Debug, Subcommand)]
enum ApprovalCommand {
    /// List every recorded decision and its status.
    List,
    /// Approve one subject and print the single-use grant to redeem it.
    Approve {
        #[arg(long)]
        subject: String,
    },
    /// Refuse one subject. A denial is terminal.
    Deny {
        #[arg(long)]
        subject: String,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum SchemaKind {
    TaskV3,
    AgentSpec,
    Composition,
    Result,
    Profile,
    SharedContract,
}

#[derive(Debug, Subcommand)]
enum RecoverCommand {
    TaskList,
    TaskResolve {
        #[arg(long)]
        session: String,
        #[arg(long)]
        task: String,
        #[arg(long, value_parser = ["acknowledged", "discarded", "retry_with_new_task"])]
        resolution: String,
    },
    EffectList {
        #[arg(long)]
        workspace: PathBuf,
    },
    EffectResolve {
        #[arg(long)]
        workspace: PathBuf,
        #[arg(long = "run-id")]
        run_id: String,
        #[arg(long, value_parser = ["verified_succeeded", "verified_failed", "manual_review_required"])]
        resolution: String,
        #[arg(long)]
        note: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
enum MemoryCommand {
    Snapshot {
        #[arg(long)]
        scope: String,
    },
    Recall {
        #[arg(long)]
        scope: String,
        #[arg(long)]
        query: String,
        #[arg(long, default_value_t = 20)]
        maximum: usize,
    },
}

#[allow(clippy::large_futures)]
#[tokio::main]
async fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => return handle_parse_error(&error),
    };
    match execute(cli).await {
        Ok(code) => code,
        Err(error) => {
            let output = json!({
                "ok":false,
                "error":{
                    "code":"cli_failed",
                    "message":error
                }
            });
            if write_json(&output).is_err() {
                eprintln!("agent: {error}");
            }
            ExitCode::from(2)
        }
    }
}

fn handle_parse_error(error: &clap::Error) -> ExitCode {
    let kind = error.kind();
    let message = error.to_string();
    if matches!(kind, ErrorKind::DisplayHelp | ErrorKind::DisplayVersion) {
        let stdout = io::stdout();
        let mut writer = io::BufWriter::new(stdout.lock());
        if writer
            .write_all(message.as_bytes())
            .and_then(|()| writer.flush())
            .is_err()
        {
            eprintln!("agent: failed to write CLI help");
            return ExitCode::from(2);
        }
        return ExitCode::SUCCESS;
    }

    let output = json!({
        "ok":false,
        "error":{
            "code":"cli_failed",
            "message":message
        }
    });
    if write_json(&output).is_err() {
        eprintln!("agent: failed to write CLI error");
    }
    ExitCode::from(2)
}

async fn execute(cli: Cli) -> Result<ExitCode, String> {
    let global = GlobalOptions {
        state_root: cli.state_root,
        skill_roots: cli.skill_roots,
        maximum_parallel_tasks: cli.maximum_parallel_tasks,
    };
    match cli.command {
        Command::Profiles => {
            let profiles = composition::profiles()
                .map_err(|error| error.to_string())?
                .manifests();
            write_json(&json!({"ok":true,"profiles":profiles}))?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Skills => {
            let roots = resolve_skill_roots(global.skill_roots.clone())?;
            let catalog = SkillCatalog::discover(roots).map_err(|error| error.to_string())?;
            write_json(&json!({"ok":true,"skills":catalog.descriptors()}))?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Plugins => {
            let catalog = composition::stock_plugin_catalog()?;
            let bindings = catalog
                .plugin_bindings()
                .map_err(|error| error.to_string())?;
            write_json(&json!({"ok":true,"plugins":bindings}))?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Compose {
            spec,
            provider_config,
            request,
        } => {
            let inputs = resolve_explicit_inputs(&global, &spec, &provider_config, &request)?;
            let receipt = inputs.resolution.receipt;
            let digest = receipt.digest.clone();
            write_json(&json!({
                "ok": true,
                "composition": receipt,
                "compositionDigest": digest,
                "providerConnected": false,
                "stateWritten": false,
            }))?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Schema { kind } => {
            let schema = match kind {
                SchemaKind::TaskV3 => serde_json::to_value(schema_for!(TaskEnvelopeV3)),
                SchemaKind::AgentSpec => serde_json::to_value(schema_for!(AgentSpec)),
                SchemaKind::Composition => serde_json::to_value(schema_for!(CompositionReceipt)),
                SchemaKind::Result => serde_json::to_value(schema_for!(AgentResult)),
                SchemaKind::Profile => serde_json::to_value(schema_for!(ProfileManifest)),
                SchemaKind::SharedContract => Ok(agent_contract::schema::catalog()),
            }
            .map_err(|error| error.to_string())?;
            write_json(&schema)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Doctor => {
            let state_root = resolve_state_root(global.state_root.clone())?;
            let skill_roots = resolve_skill_roots(global.skill_roots.clone());
            let output = match skill_roots {
                Ok(roots) => {
                    let catalog = SkillCatalog::discover(roots.clone());
                    match catalog {
                        Ok(value) => json!({
                            "ok":true,
                            "stateRoot":state_root,
                            "skillRoots":roots,
                            "skillCount":value.descriptors().len(),
                            "profiles":composition::profiles().map_err(|error| error.to_string())?.manifests()
                        }),
                        Err(error) => json!({
                            "ok":false,
                            "stateRoot":state_root,
                            "skillRoots":roots,
                            "error":error.to_string()
                        }),
                    }
                }
                Err(error) => json!({"ok":false,"stateRoot":state_root,"error":error}),
            };
            let ok = matches!(output.get("ok"), Some(Value::Bool(true)));
            write_json(&output)?;
            Ok(if ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            })
        }
        Command::Run {
            spec,
            provider_config,
            request,
            expect_digest,
        } => {
            let result = Box::pin(run_explicit(
                &global,
                &spec,
                &provider_config,
                &request,
                &expect_digest,
            ))
            .await?;
            write_json(&result)?;
            Ok(exit_for_result(&result))
        }
        Command::Inspect { session, task } => {
            let state_root = resolve_existing_state_root(global.state_root.clone())?;
            let value = agent_runtime::inspect_task(&state_root, &session, &task)
                .map_err(|error| error.to_string())?;
            let mut output = serde_json::to_value(&value).map_err(|error| error.to_string())?;
            let store =
                CompositionStore::existing(&state_root).map_err(|error| error.to_string())?;
            let expected_digest = value
                .task
                .context
                .get("_upagent_composition_digest")
                .and_then(Value::as_str);
            let receipt = if let Some(expected_digest) = expected_digest {
                let receipt = store
                    .read(&session, &task)
                    .map_err(|error| error.to_string())?;
                if receipt.digest != expected_digest {
                    return Err(
                        "inspection composition digest does not match the durable receipt"
                            .to_owned(),
                    );
                }
                Some(receipt)
            } else {
                store
                    .read_if_exists(&session, &task)
                    .map_err(|error| error.to_string())?
            };
            if let Some(receipt) = receipt {
                output["composition"] =
                    serde_json::to_value(receipt).map_err(|error| error.to_string())?;
            }
            write_json(&output)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Recover { command } => {
            let state_root = resolve_state_root(global.state_root.clone())?;
            let value = match command {
                RecoverCommand::TaskList => serde_json::to_value(
                    agent_runtime::list_recoveries(&state_root)
                        .map_err(|error| error.to_string())?,
                ),
                RecoverCommand::TaskResolve {
                    session,
                    task,
                    resolution,
                } => serde_json::to_value(
                    agent_runtime::resolve_recovery(&state_root, &session, &task, &resolution)
                        .map_err(|error| error.to_string())?,
                ),
                RecoverCommand::EffectList { workspace } => serde_json::to_value(
                    agent_runtime::list_effect_recoveries(&workspace, &state_root)
                        .map_err(|error| error.to_string())?,
                ),
                RecoverCommand::EffectResolve {
                    workspace,
                    run_id,
                    resolution,
                    note,
                } => serde_json::to_value(
                    agent_runtime::resolve_effect_recovery(
                        &workspace,
                        &state_root,
                        &run_id,
                        parse_effect_recovery_resolution(&resolution)?,
                        note,
                    )
                    .map_err(|error| error.to_string())?,
                ),
            }
            .map_err(|error| error.to_string())?;
            write_json(&json!({"ok":true,"value":value}))?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Approvals { command } => {
            let state_root = resolve_state_root(global.state_root.clone())?;
            let store = ApprovalStore::open(state_root.join("approvals"))
                .map_err(|error| error.to_string())?;
            let value = match command {
                ApprovalCommand::List => {
                    serde_json::to_value(store.list().map_err(|error| error.to_string())?)
                }
                ApprovalCommand::Approve { subject } => serde_json::to_value(
                    store.approve(&subject).map_err(|error| error.to_string())?,
                ),
                ApprovalCommand::Deny { subject } => {
                    serde_json::to_value(store.deny(&subject).map_err(|error| error.to_string())?)
                }
            }
            .map_err(|error| error.to_string())?;
            write_json(&json!({"ok":true,"value":value}))?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Memory { command } => {
            let state_root = resolve_state_root(global.state_root.clone())?;
            let store =
                MemoryStore::open(state_root.join("memory")).map_err(|error| error.to_string())?;
            let output = match command {
                MemoryCommand::Snapshot { scope } => {
                    serde_json::to_value(store.snapshot(&scope).map_err(|error| error.to_string())?)
                }
                MemoryCommand::Recall {
                    scope,
                    query,
                    maximum,
                } => serde_json::to_value(
                    store
                        .recall(&scope, &query, maximum)
                        .map_err(|error| error.to_string())?,
                ),
            }
            .map_err(|error| error.to_string())?;
            write_json(&json!({"ok":true,"value":output}))?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

#[cfg(unix)]
async fn run_with_signals(
    runtime: &AgentRuntime,
    task: ResolvedTask,
) -> Result<AgentResult, String> {
    use tokio::signal::unix::{SignalKind, signal};

    let cancellation = TaskCancellation::new();
    let mut interrupt = signal(SignalKind::interrupt()).map_err(|error| error.to_string())?;
    let mut terminate = signal(SignalKind::terminate()).map_err(|error| error.to_string())?;
    let execution = runtime.run_with_cancellation(task, cancellation.clone());
    tokio::pin!(execution);
    let result = tokio::select! {
        biased;
        result = &mut execution => result,
        _ = interrupt.recv() => {
            cancellation.cancel();
            execution.await
        }
        _ = terminate.recv() => {
            cancellation.cancel();
            execution.await
        }
    };
    Ok(result)
}

#[cfg(not(unix))]
async fn run_with_signals(
    runtime: &AgentRuntime,
    task: ResolvedTask,
) -> Result<AgentResult, String> {
    let cancellation = TaskCancellation::new();
    let execution = runtime.run_with_cancellation(task, cancellation.clone());
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
    Ok(result)
}

#[derive(Debug)]
struct ExplicitInputs {
    resolution: composition::ExplicitResolution,
    provider_selection: ProviderPluginEnvelope,
}

fn resolve_explicit_inputs(
    global: &GlobalOptions,
    spec_path: &Path,
    provider_config_path: &Path,
    request_path: &Path,
) -> Result<ExplicitInputs, String> {
    let spec_source = spec_path
        .to_str()
        .ok_or_else(|| "AgentSpec path must be UTF-8".to_owned())?;
    let provider_source = provider_config_path
        .to_str()
        .ok_or_else(|| "provider config path must be UTF-8".to_owned())?;
    let request_source = request_path
        .to_str()
        .ok_or_else(|| "TaskEnvelope v3 path must be UTF-8".to_owned())?;
    let spec: AgentSpec = read_json(spec_source)?;
    let provider_selection: ProviderPluginEnvelope = read_json(provider_source)?;
    let plugin_catalog = composition::stock_plugin_catalog()?;
    let task: TaskEnvelopeV3 = read_json(request_source)?;
    // This is the only skill/profile/spec resolver used by compose and run.
    // It performs discovery and pure policy checks, never credentials or state
    // writes.
    let skill_roots = resolve_skill_roots(global.skill_roots.clone())?;
    let catalog = SkillCatalog::discover(skill_roots.clone()).map_err(|error| error.to_string())?;
    let resolution = composition::resolve_explicit_with_catalog(
        task,
        &spec,
        &provider_selection,
        &catalog,
        skill_roots,
        plugin_catalog,
    )?;
    Ok(ExplicitInputs {
        resolution,
        provider_selection,
    })
}

async fn run_explicit(
    global: &GlobalOptions,
    spec_path: &Path,
    provider_config_path: &Path,
    request_path: &str,
    expect_digest: &str,
) -> Result<AgentResult, String> {
    let request_path = Path::new(request_path);
    let inputs = resolve_explicit_inputs(global, spec_path, provider_config_path, request_path)?;
    let resolution = inputs.resolution;
    if resolution.receipt.digest != expect_digest {
        return Err(format!(
            "composition digest mismatch before state/provider activation: expected {expect_digest}, resolved {}",
            resolution.receipt.digest
        ));
    }
    let selected_tools: std::collections::BTreeSet<String> =
        resolution.receipt.spec.tools.iter().cloned().collect();
    let promptgen = if selected_tools
        .iter()
        .any(|name| agent_core::is_promptgen_tool_name(name))
    {
        Some(Arc::new(agent_runtime::PromptGenAdapter::from_spec(
            resolution
                .receipt
                .spec
                .promptgen
                .as_ref()
                .ok_or("selected promptGen tool has no mechanism identity")?,
        )?))
    } else {
        None
    };
    // Composition evidence is created before any provider process or network
    // connection is attempted. Existing evidence is immutable and
    // any spec/request drift fails here.
    let state_root = resolve_state_root(global.state_root.clone())?;
    let store = CompositionStore::open(&state_root).map_err(|error| error.to_string())?;
    store
        .create_only(&resolution.receipt)
        .map_err(|error| error.to_string())?;
    let resolved_task = ResolvedTask::new(resolution.task, resolution.receipt.clone())
        .map_err(|error| error.to_string())?;
    let provider_planes = resolution
        .plugin_catalog
        .connect_provider_envelope(&inputs.provider_selection)
        .await
        .map_err(|error| error.to_string())?;
    // The CLI composition root supplies the same content-addressed artifact
    // authority used by AgentRuntime to the optional semantic-mask Effect.
    // Absence is an explicit unavailable adapter; no PATH discovery occurs.
    let runtime_artifacts = agent_runtime::ArtifactStore::open(state_root.join("artifacts"))
        .map_err(|error| format!("runtime artifact store could not open: {error}"))?;
    let semantic_mask_adapter: Arc<dyn agent_runtime::SemanticMaskAdapter> =
        match resolution.receipt.spec.semantic_mask.as_ref() {
            Some(config) => Arc::new(
                agent_runtime::ProcessSemanticMaskAdapter::from_spec(
                    config,
                    runtime_artifacts.clone(),
                )
                .map_err(|error| {
                    format!("semantic-mask helper could not be configured: {error}")
                })?,
            ),
            None => Arc::new(agent_runtime::UnavailableSemanticMaskAdapter),
        };
    let image_module = resolution
        .receipt
        .spec
        .image
        .as_ref()
        .map(|image| {
            agent_runtime::ImageToolModule::new_with_semantic_mask_adapter(
                provider_planes.image.clone(),
                &resolution.receipt.provider,
                image.perfectpixel_executable.clone(),
                image.perfectpixel_sha256.clone(),
                image.protocol_schema.clone(),
                image.protocol_version.clone(),
                image.behavior_version.clone(),
                image.capability_manifest_sha256.clone(),
                image.implementation_version.clone(),
                semantic_mask_adapter,
            )
            .and_then(|module| module.with_promptgen(promptgen.clone(), &selected_tools))
            .map(|module| Arc::new(module) as Arc<dyn agent_runtime::ToolModule>)
        })
        .transpose()
        .map_err(|error| format!("image tool identity is invalid: {error}"))?;
    let mut tool_modules: Vec<Arc<dyn agent_runtime::ToolModule>> =
        image_module.into_iter().collect();
    if selected_tools
        .iter()
        .any(|name| agent_core::IMAGE_MAKER_TOOL_NAMES.contains(&name.as_str()))
    {
        tool_modules.push(Arc::new(agent_runtime::ImageMakerToolModule::new(
            &selected_tools,
            promptgen,
        )?));
    }
    let options = AgentRuntimeOptions {
        state_root,
        skill_roots: resolution.skill_roots.clone(),
        profiles: resolution.profiles.clone(),
        model: provider_planes.model.clone(),
        maximum_parallel_tasks: global.maximum_parallel_tasks,
        tool_modules,
    };
    let runtime = match AgentRuntime::open_with_plugins_and_catalog_and_memory(
        options,
        Vec::new(),
        Some(resolution.skill_catalog.clone()),
        resolution.receipt.spec.memory != agent_core::MemorySpec::None,
    ) {
        Ok(runtime) => runtime,
        Err(error) => {
            return Err(error.to_string());
        }
    };
    Box::pin(run_with_signals(&runtime, resolved_task)).await
}

fn parse_effect_recovery_resolution(value: &str) -> Result<EffectRecoveryResolution, String> {
    match value {
        "verified_succeeded" => Ok(EffectRecoveryResolution::VerifiedSucceeded),
        "verified_failed" => Ok(EffectRecoveryResolution::VerifiedFailed),
        "manual_review_required" => Ok(EffectRecoveryResolution::ManualReviewRequired),
        _ => Err(format!("unsupported effect recovery resolution {value}")),
    }
}

fn resolve_state_root(value: Option<PathBuf>) -> Result<PathBuf, String> {
    let path = match value {
        Some(path) => path,
        None => ProjectDirs::from("dev", "axiom", "unified-profiled-agent")
            .ok_or_else(|| "could not determine a user data directory".to_owned())?
            .data_local_dir()
            .to_path_buf(),
    };
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err("state root must be a real directory".to_owned());
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            std::fs::create_dir_all(&path).map_err(|error| error.to_string())?;
        }
        Err(error) => return Err(error.to_string()),
    }
    let metadata = std::fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err("state root must be a real directory".to_owned());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| error.to_string())?;
    }
    path.canonicalize().map_err(|error| error.to_string())
}

fn resolve_existing_state_root(value: Option<PathBuf>) -> Result<PathBuf, String> {
    let path = match value {
        Some(path) => path,
        None => ProjectDirs::from("dev", "axiom", "unified-profiled-agent")
            .ok_or_else(|| "could not determine a user data directory".to_owned())?
            .data_local_dir()
            .to_path_buf(),
    };
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

fn write_json(value: &impl serde::Serialize) -> Result<(), String> {
    let stdout = io::stdout();
    let mut writer = io::BufWriter::new(stdout.lock());
    serde_json::to_writer_pretty(&mut writer, value).map_err(|error| error.to_string())?;
    writer.write_all(b"\n").map_err(|error| error.to_string())?;
    writer.flush().map_err(|error| error.to_string())
}

fn exit_for_result(result: &AgentResult) -> ExitCode {
    match result.status {
        AgentStatus::Succeeded => ExitCode::SUCCESS,
        AgentStatus::Suspended | AgentStatus::AwaitingApproval | AgentStatus::AwaitingInput => {
            ExitCode::from(10)
        }
        AgentStatus::ResolutionRequired => ExitCode::from(20),
        AgentStatus::Failed | AgentStatus::Cancelled => ExitCode::from(1),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use agent_core::{ProfileId, SessionId, TaskId, TaskState};
    use serde::Deserialize;

    use super::*;

    fn result(status: AgentStatus) -> Result<AgentResult, Box<dyn std::error::Error>> {
        Ok(AgentResult {
            schema_version: agent_core::PROTOCOL_VERSION,
            task_id: TaskId::parse("task-exit")?,
            session_id: SessionId::parse("session-exit")?,
            profile: ProfileId::creator(),
            track: None,
            state: TaskState::Created,
            status,
            output: None,
            artifacts: Vec::new(),
            receipts: Vec::new(),
            composition_digest: None,
            review_subject_bundle: None,
            approval: None,
            failure: None,
            metrics: BTreeMap::new(),
        })
    }

    fn code(exit: ExitCode) -> String {
        format!("{exit:?}")
    }

    #[test]
    fn exit_codes_match_the_documented_cli_contract() -> Result<(), Box<dyn std::error::Error>> {
        // README documents 0 success, 10 caller input or approval, 20 manual
        // recovery, 1 failure or cancellation.
        for (status, expected) in [
            (AgentStatus::Succeeded, ExitCode::SUCCESS),
            (AgentStatus::Suspended, ExitCode::from(10)),
            (AgentStatus::AwaitingInput, ExitCode::from(10)),
            (AgentStatus::AwaitingApproval, ExitCode::from(10)),
            (AgentStatus::ResolutionRequired, ExitCode::from(20)),
            (AgentStatus::Failed, ExitCode::from(1)),
            (AgentStatus::Cancelled, ExitCode::from(1)),
        ] {
            assert_eq!(
                code(exit_for_result(&result(status)?)),
                code(expected),
                "unexpected exit code for {status:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn bounded_reader_accepts_the_limit_and_rejects_one_byte_more() {
        let payload = vec![b'x'; 64];

        let mut exact = Vec::new();
        BoundedReader::new(payload.as_slice(), 64)
            .read_to_end(&mut exact)
            .expect("a payload at the limit must be readable");
        assert_eq!(exact.len(), 64);

        let mut overflow = Vec::new();
        let error = BoundedReader::new(payload.as_slice(), 63)
            .read_to_end(&mut overflow)
            .expect_err("a payload past the limit must fail");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn bounded_reader_refuses_an_oversized_request_body() {
        let payload = format!(r#"{{"value":"{}"}}"#, "x".repeat(4096));
        let reader = BoundedReader::new(payload.as_bytes(), 128);
        let mut deserializer = serde_json::Deserializer::from_reader(reader);
        assert!(Value::deserialize(&mut deserializer).is_err());
    }

    #[test]
    fn skill_roots_are_canonicalized_and_deduplicated() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("skills");
        std::fs::create_dir_all(&root)?;

        let roots = resolve_skill_roots(vec![root.clone(), root.join("."), root.clone()])?;
        assert_eq!(roots.len(), 1, "the same root must be configured once");
        assert_eq!(roots[0], root.canonicalize()?);

        let file = temp.path().join("not-a-directory");
        std::fs::write(&file, b"")?;
        assert!(resolve_skill_roots(vec![file]).is_err());
        assert!(resolve_skill_roots(vec![temp.path().join("missing")]).is_err());
        Ok(())
    }
}
