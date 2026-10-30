//! `AXDevSystem` execution-contract parsing and profile-policy resolution.

use super::{
    BTreeMap, BTreeSet, MessageRole, ModelMessage, ProfileId, ReasoningEffort, TaskBudget,
    TaskEnvelope, Value,
};
use agent_profile_api::{
    AxdResultBinding, ExecutionClass, ExecutionTopology, ProfileContext, ProfileError, ProfilePlan,
};

#[derive(Debug, Clone, PartialEq, Eq)]
struct AxdExecutionOverlay {
    execution_class: ExecutionClass,
    reasoning: ReasoningEffort,
    allowed_tools: Option<Vec<String>>,
    required_tools: Vec<String>,
    required_checks: Vec<String>,
    required_check_commands: BTreeMap<String, Vec<String>>,
    maximum_workers: Option<u16>,
    topology: Option<ExecutionTopology>,
    output_contract: Option<String>,
    budget: Option<AxdBudgetOverlay>,
    result_binding: Option<AxdResultBinding>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct AxdBudgetOverlay {
    maximum_transitions: Option<u32>,
    maximum_model_calls: Option<u32>,
    maximum_tool_calls: Option<u32>,
    timeout_ms: Option<u64>,
}

fn default_execution_overlay(plan: &ProfilePlan) -> AxdExecutionOverlay {
    AxdExecutionOverlay {
        execution_class: plan.execution_class,
        reasoning: plan.reasoning,
        allowed_tools: None,
        required_tools: Vec::new(),
        required_checks: Vec::new(),
        required_check_commands: BTreeMap::new(),
        maximum_workers: Some(plan.maximum_workers),
        topology: Some(plan.topology),
        output_contract: plan.output_contract.clone(),
        budget: None,
        result_binding: plan.axd_result_binding.clone(),
    }
}

#[allow(clippy::too_many_lines)]
pub(super) fn apply_axd_execution_policy(
    plan: &mut ProfilePlan,
    task: &TaskEnvelope,
    context: &ProfileContext,
) -> Result<(), ProfileError> {
    // Extension tools are activated by the runtime plugin graph and filtered
    // by profile capability/skill constraints before profile preparation.
    // Make them part of the real typed tool surface before applying AXD policy.
    plan.allowed_tools
        .extend(context.available_tools.iter().cloned());
    let overlay = resolve_axd_policy_overlay(&task.context)?
        .unwrap_or_else(|| default_execution_overlay(plan));

    let base_tools = plan.allowed_tools.clone();
    let mut selected = class_default_tools(overlay.execution_class, &plan.manifest.id, &base_tools);
    if let Some(allowed) = &overlay.allowed_tools {
        let allowed = expand_tool_selectors(allowed, &base_tools)?;
        selected.retain(|tool| allowed.contains(tool));
    }
    let required = expand_tool_selectors(&overlay.required_tools, &base_tools)?;
    if !required.is_subset(&selected) {
        return Err(ProfileError::Invalid(
            "AXD required tools are outside the active profile authority".to_owned(),
        ));
    }
    if selected.is_empty() {
        return Err(ProfileError::Invalid(
            "AXD execution policy selected an empty tool surface".to_owned(),
        ));
    }

    let maximum_workers = overlay.maximum_workers.unwrap_or_else(|| {
        default_workers_for_class(
            overlay.execution_class,
            plan.manifest.concurrency.default_workers,
        )
    });
    let topology = overlay.topology.unwrap_or_else(|| {
        if maximum_workers > 1 {
            ExecutionTopology::ParallelRead
        } else {
            default_topology_for_class(
                overlay.execution_class,
                plan.manifest.concurrency.default_workers,
            )
        }
    });
    let maximum_authorized_workers = u16::from(plan.manifest.concurrency.maximum_workers.max(1));
    if maximum_workers == 0 || maximum_workers > maximum_authorized_workers {
        return Err(ProfileError::Invalid(format!(
            "AXD maximum_workers {maximum_workers} exceeds profile {} authority {}",
            plan.manifest.id, maximum_authorized_workers,
        )));
    }
    if plan.track.is_some() && maximum_workers > plan.maximum_workers {
        return Err(ProfileError::Invalid(format!(
            "AXD maximum_workers {maximum_workers} exceeds the active track authority {}",
            plan.maximum_workers,
        )));
    }
    match topology {
        ExecutionTopology::Single | ExecutionTopology::SingleWithIndependentVerifier
            if maximum_workers != 1 =>
        {
            return Err(ProfileError::Invalid(format!(
                "AXD {} topology requires maximum_workers=1",
                topology.as_str(),
            )));
        }
        ExecutionTopology::ParallelRead if maximum_workers < 2 => {
            return Err(ProfileError::Invalid(
                "AXD parallel_read topology requires at least two workers".to_owned(),
            ));
        }
        _ => {}
    }

    let selected_mutates_workspace = selected
        .iter()
        .any(|tool| matches!(tool.as_str(), "write_file" | "replace_text"));
    if plan.track.is_some()
        && selected_mutates_workspace
        && execution_strength(overlay.execution_class) < execution_strength(plan.execution_class)
    {
        return Err(ProfileError::Invalid(
            "AXD execution class would weaken the active track policy".to_owned(),
        ));
    }
    if plan.track.is_some()
        && selected_mutates_workspace
        && reasoning_strength(overlay.reasoning) < reasoning_strength(plan.reasoning)
    {
        return Err(ProfileError::Invalid(
            "AXD reasoning effort would weaken the active track policy".to_owned(),
        ));
    }

    // `required_tools` declares availability. Only exact evidence-producing
    // selectors imply mandatory execution; broad groups such as
    // `workspace.read` must not force every expanded tool to run.
    let mut required_successful_tools = overlay
        .required_tools
        .iter()
        .filter_map(|tool| match tool.as_str() {
            "run_check" | "check.run" | "process.run" | "test" => Some("run_check".to_owned()),
            "inspect_diff" => Some("inspect_diff".to_owned()),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    if overlay.execution_class == ExecutionClass::Verified && !overlay.required_checks.is_empty() {
        if !base_tools.contains("run_check") {
            return Err(ProfileError::Invalid(
                "verified AXD checks require the run_check tool".to_owned(),
            ));
        }
        selected.insert("run_check".to_owned());
        required_successful_tools.insert("run_check".to_owned());
    }
    if overlay.execution_class == ExecutionClass::Verified
        && overlay.required_checks.is_empty()
        && required_successful_tools.is_empty()
        && plan.required_successful_tools_after_mutation.is_empty()
    {
        return Err(ProfileError::Invalid(
            "verified AXD execution requires explicit tool or check evidence".to_owned(),
        ));
    }
    if let Some(output_contract) = overlay.output_contract.as_deref() {
        if output_contract.trim().is_empty() || output_contract.len() > 256 {
            return Err(ProfileError::Invalid(
                "AXD output contract must be a bounded non-empty identifier".to_owned(),
            ));
        }
        if !matches!(
            output_contract,
            "structured_result_with_evidence" | "axd.agent-result/2"
        ) {
            return Err(ProfileError::Invalid(format!(
                "AXD output contract {output_contract} is not implemented"
            )));
        }
    }

    if plan.track.is_some()
        && selected_mutates_workspace
        && plan.topology == ExecutionTopology::SingleWithIndependentVerifier
        && topology != ExecutionTopology::SingleWithIndependentVerifier
    {
        return Err(ProfileError::Invalid(
            "AXD topology would remove the active independent verifier".to_owned(),
        ));
    }
    let mut required_successful_tools_after_mutation =
        plan.required_successful_tools_after_mutation.clone();
    if overlay.execution_class == ExecutionClass::Verified {
        required_successful_tools_after_mutation.insert("inspect_diff".to_owned());
    }
    if selected_mutates_workspace && !required_successful_tools_after_mutation.is_subset(&selected)
    {
        return Err(ProfileError::Invalid(
            "AXD mutation surface omits evidence required by the active track".to_owned(),
        ));
    }

    plan.execution_class = overlay.execution_class;
    plan.reasoning = overlay.reasoning;
    plan.allowed_tools = selected;
    plan.required_successful_tools = required_successful_tools;
    plan.required_checks = overlay.required_checks;
    plan.required_check_commands = overlay.required_check_commands;
    plan.required_successful_tools_after_mutation = required_successful_tools_after_mutation;
    plan.maximum_workers = maximum_workers;
    plan.topology = topology;
    plan.output_contract = overlay.output_contract;
    plan.axd_result_binding = overlay.result_binding;
    plan.effective_budget = effective_budget(
        &task.budget,
        overlay.execution_class,
        overlay.budget.as_ref(),
        plan.policy_budget_cap.as_ref(),
    )?;

    let explicit_non_check_tools = plan
        .required_successful_tools
        .iter()
        .filter(|tool| tool.as_str() != "run_check")
        .count();
    let check_calls = if plan.required_checks.is_empty()
        && plan.required_successful_tools.contains("run_check")
    {
        1
    } else {
        plan.required_checks.len()
    };
    let minimum_tool_calls = u32::try_from(check_calls.saturating_add(explicit_non_check_tools))
        .map_err(|_| ProfileError::Invalid("required evidence count exceeds u32".to_owned()))?;
    if plan.effective_budget.maximum_tool_calls < minimum_tool_calls {
        return Err(ProfileError::Invalid(format!(
            "effective tool budget {} cannot satisfy {} required evidence calls",
            plan.effective_budget.maximum_tool_calls, minimum_tool_calls,
        )));
    }

    let checks = if plan.required_checks.is_empty() {
        "none".to_owned()
    } else {
        plan.required_checks.join(", ")
    };
    let check_commands = serde_json::to_string(&plan.required_check_commands)
        .map_err(|error| ProfileError::Invalid(error.to_string()))?;
    let output_contract = plan.output_contract.as_deref().unwrap_or("default");
    let policy_message = ModelMessage::text(
        MessageRole::Developer,
        format!(
            "AXD execution policy: class={}, topology={}, maximumWorkers={}, modelCalls={}, toolCalls={}, transitions={}, timeoutMs={}, requiredChecks=[{}], requiredCheckCommands={}, outputContract={}. Use the exact host-authorized command for every bound check. The runtime will reject a final answer when required verification evidence is missing or a check command differs.",
            plan.execution_class.as_str(),
            plan.topology.as_str(),
            plan.maximum_workers,
            plan.effective_budget.maximum_model_calls,
            plan.effective_budget.maximum_tool_calls,
            plan.effective_budget.maximum_transitions,
            plan.effective_budget.timeout_ms,
            checks,
            check_commands,
            output_contract,
        ),
    );
    if let Some(index) = plan
        .messages
        .iter()
        .rposition(|message| matches!(message.role, MessageRole::User | MessageRole::Tool))
    {
        plan.messages.insert(index, policy_message);
    } else {
        // Preserve the previous behavior for custom plans that do not expose
        // a provider turn boundary; built-in profiles always end in User.
        plan.messages.push(policy_message);
    }
    Ok(())
}

fn class_default_tools(
    execution_class: ExecutionClass,
    profile: &ProfileId,
    base: &BTreeSet<String>,
) -> BTreeSet<String> {
    if profile != &ProfileId::developer() || execution_class != ExecutionClass::Direct {
        return base.clone();
    }
    let direct = BTreeSet::from([
        "repo_map".to_owned(),
        "search_code".to_owned(),
        "read_file".to_owned(),
        "git_status".to_owned(),
        "inspect_diff".to_owned(),
        "request_input".to_owned(),
    ]);
    base.intersection(&direct).cloned().collect()
}

#[allow(clippy::too_many_lines)]
fn resolve_axd_policy_overlay(
    context: &BTreeMap<String, Value>,
) -> Result<Option<AxdExecutionOverlay>, ProfileError> {
    let Some(raw) = context.get("axd") else {
        return Ok(None);
    };
    let owned;
    let value = match raw {
        Value::Object(_) => raw,
        Value::String(encoded) => {
            owned = serde_json::from_str::<Value>(encoded).map_err(|error| {
                ProfileError::Invalid(format!("AXD policy JSON is invalid: {error}"))
            })?;
            &owned
        }
        _ => {
            return Err(ProfileError::Invalid(
                "task context.axd must be a JSON object".to_owned(),
            ));
        }
    };
    let root = value.as_object().ok_or_else(|| {
        ProfileError::Invalid("task context.axd must be a JSON object".to_owned())
    })?;
    let policy_value = root
        .get("runtime_policy")
        .or_else(|| root.get("runtimePolicy"))
        .or_else(|| root.get("policy"))
        .unwrap_or(value);
    let policy = policy_value.as_object().ok_or_else(|| {
        ProfileError::Invalid("AXD runtime policy must be a JSON object".to_owned())
    })?;

    let class_value = string_field(
        policy,
        &[
            "execution_class",
            "executionClass",
            "execution_level",
            "executionLevel",
        ],
    )?
    .ok_or_else(|| ProfileError::Invalid("AXD execution class is required".to_owned()))?;
    let execution_class = parse_execution_class(class_value)?;
    let reasoning = match string_field(policy, &["reasoning_effort", "reasoningEffort"])? {
        Some(value) => parse_reasoning(value)?,
        None => reasoning_for_class(execution_class),
    };
    let allowed_tools = string_array_field(policy, &["allowed_tools", "allowedTools"])?;
    let required_tools =
        string_array_field(policy, &["required_tools", "requiredTools"])?.unwrap_or_default();
    let output_contract = string_field(
        policy,
        &[
            "output_schema",
            "outputSchema",
            "output_contract",
            "outputContract",
        ],
    )?
    .map(str::to_owned);
    let mut check_kinds = BTreeMap::new();
    let mut required_check_commands = BTreeMap::new();
    let required_checks = if let Some(values) =
        string_array_field(policy, &["required_checks", "requiredChecks"])?
    {
        for requirement in &values {
            check_kinds.insert(requirement.clone(), "other".to_owned());
        }
        values
    } else if let Some(check_value) = policy
        .get("checks")
        .or_else(|| policy.get("check_plan"))
        .or_else(|| policy.get("checkPlan"))
    {
        let checks = check_value
            .as_array()
            .ok_or_else(|| ProfileError::Invalid("AXD checks must be an array".to_owned()))?;
        let mut output = Vec::new();
        for check in checks {
            let check = check.as_object().ok_or_else(|| {
                ProfileError::Invalid("AXD checks must contain objects".to_owned())
            })?;
            let required = match check.get("required") {
                Some(value) => value.as_bool().ok_or_else(|| {
                    ProfileError::Invalid("AXD check.required must be a boolean".to_owned())
                })?,
                None => true,
            };
            if !required {
                continue;
            }
            let requirement = string_field(check, &["requirement_id", "requirementId"])?
                .ok_or_else(|| {
                    ProfileError::Invalid("AXD prepared checks require requirement_id".to_owned())
                })?;
            validate_bounded_identifier("AXD check requirement_id", requirement, 256)?;
            output.push(requirement.to_owned());
            let kind = string_field(check, &["kind"])?.unwrap_or("other");
            validate_evidence_kind(kind)?;
            check_kinds.insert(requirement.to_owned(), kind.to_owned());
            if let Some(argv) = argv_field(check)? {
                required_check_commands.insert(requirement.to_owned(), argv);
            } else if output_contract.as_deref() == Some("axd.agent-result/2") {
                return Err(ProfileError::Invalid(format!(
                    "AXD required check {requirement} has no host-authorized argv"
                )));
            }
        }
        output
    } else {
        Vec::new()
    };
    ensure_unique_bounded("AXD required tool", &required_tools)?;
    ensure_unique_bounded("AXD required check", &required_checks)?;
    if let Some(values) = &allowed_tools {
        ensure_unique_bounded("AXD allowed tool", values)?;
    }

    let maximum_workers = integer_field(
        policy,
        &[
            "maximum_workers",
            "maximumWorkers",
            "max_workers",
            "maxWorkers",
        ],
    )?
    .map(|value| {
        u16::try_from(value)
            .map_err(|_| ProfileError::Invalid("AXD maximum_workers exceeds u16".to_owned()))
    })
    .transpose()?;
    let topology = string_field(policy, &["topology"])?
        .map(parse_topology)
        .transpose()?;
    let budget = budget_overlay(policy)?;
    let result_binding = match output_contract.as_deref() {
        Some("axd.agent-result/2") => Some(AxdResultBinding {
            contract_digest: required_digest(root, "contract_digest", "contractDigest")?,
            work_item_id: required_string(root, "work_item_id", "workItemId")?,
            // A source revision is an opaque workspace-owned identifier. AXD's
            // v2 contract schema intentionally permits values such as a Git
            // SHA, a content digest, or a host revision like `source-1`.
            source_revision: required_string(root, "source_revision", "sourceRevision")?,
            contract_revision: integer_field(root, &["contract_revision", "contractRevision"])?
                .ok_or_else(|| {
                    ProfileError::Invalid(
                        "AXD axd.agent-result/2 requires contract_revision".to_owned(),
                    )
                })?,
            check_kinds,
        }),
        _ => None,
    };

    Ok(Some(AxdExecutionOverlay {
        execution_class,
        reasoning,
        allowed_tools,
        required_tools,
        required_checks,
        required_check_commands,
        maximum_workers,
        topology,
        output_contract,
        budget,
        result_binding,
    }))
}

fn parse_topology(value: &str) -> Result<ExecutionTopology, ProfileError> {
    match value.to_ascii_lowercase().as_str() {
        "single" => Ok(ExecutionTopology::Single),
        "parallel_read" | "parallel-read" => Ok(ExecutionTopology::ParallelRead),
        "single_with_independent_verifier" | "single-with-independent-verifier" => {
            Ok(ExecutionTopology::SingleWithIndependentVerifier)
        }
        _ => Err(ProfileError::Invalid(format!(
            "unknown AXD execution topology {value}"
        ))),
    }
}

fn required_string(
    object: &serde_json::Map<String, Value>,
    snake: &str,
    camel: &str,
) -> Result<String, ProfileError> {
    let value = string_field(object, &[snake, camel])?
        .ok_or_else(|| ProfileError::Invalid(format!("AXD axd.agent-result/2 requires {snake}")))?;
    if value.trim().is_empty() || value.len() > 512 {
        return Err(ProfileError::Invalid(format!(
            "AXD {snake} must be a bounded non-empty string"
        )));
    }
    Ok(value.to_owned())
}

fn required_digest(
    object: &serde_json::Map<String, Value>,
    snake: &str,
    camel: &str,
) -> Result<String, ProfileError> {
    let value = required_string(object, snake, camel)?;
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(ProfileError::Invalid(format!(
            "AXD {snake} must be a 64-character hexadecimal digest"
        )));
    }
    Ok(value.to_ascii_lowercase())
}

fn validate_bounded_identifier(
    label: &str,
    value: &str,
    maximum_bytes: usize,
) -> Result<(), ProfileError> {
    if value.trim().is_empty()
        || value.len() > maximum_bytes
        || value.contains('\0')
        || value.chars().any(char::is_control)
    {
        return Err(ProfileError::Invalid(format!(
            "{label} must be a bounded printable value"
        )));
    }
    Ok(())
}

fn validate_evidence_kind(value: &str) -> Result<(), ProfileError> {
    if matches!(
        value,
        "inspection"
            | "unit_test"
            | "integration_test"
            | "build"
            | "lint"
            | "runtime"
            | "security"
            | "performance"
            | "receipt"
            | "manual"
            | "other"
    ) {
        Ok(())
    } else {
        Err(ProfileError::Invalid(format!(
            "unknown AXD evidence kind {value}"
        )))
    }
}

fn budget_overlay(
    policy: &serde_json::Map<String, Value>,
) -> Result<Option<AxdBudgetOverlay>, ProfileError> {
    let Some(value) = policy.get("budget").or_else(|| policy.get("budgets")) else {
        return Ok(None);
    };
    let object = value
        .as_object()
        .ok_or_else(|| ProfileError::Invalid("AXD budget must be a JSON object".to_owned()))?;
    let maximum_transitions = integer_field(
        object,
        &["maximum_transitions", "maximumTransitions", "transitions"],
    )?
    .map(|value| bounded_u32("maximum_transitions", value, 1, 1_024))
    .transpose()?;
    let maximum_model_calls = integer_field(
        object,
        &[
            "maximum_model_calls",
            "maximumModelCalls",
            "model_calls",
            "modelCalls",
        ],
    )?
    .map(|value| bounded_u32("maximum_model_calls", value, 1, 64))
    .transpose()?;
    let maximum_tool_calls = integer_field(
        object,
        &[
            "maximum_tool_calls",
            "maximumToolCalls",
            "tool_calls",
            "toolCalls",
        ],
    )?
    .map(|value| bounded_u32("maximum_tool_calls", value, 0, 256))
    .transpose()?;
    let timeout_ms = match integer_field(object, &["timeout_ms", "timeoutMs"])? {
        Some(value) => Some(value),
        None => integer_field(object, &["timeout_seconds", "timeoutSeconds"])?
            .map(|value| {
                value.checked_mul(1_000).ok_or_else(|| {
                    ProfileError::Invalid("AXD timeout_seconds overflowed".to_owned())
                })
            })
            .transpose()?,
    };
    if let Some(timeout_ms) = timeout_ms
        && !(1_000..=24 * 60 * 60 * 1_000).contains(&timeout_ms)
    {
        return Err(ProfileError::Invalid(
            "AXD timeout is outside supported bounds".to_owned(),
        ));
    }
    Ok(Some(AxdBudgetOverlay {
        maximum_transitions,
        maximum_model_calls,
        maximum_tool_calls,
        timeout_ms,
    }))
}

fn bounded_u32(field: &str, value: u64, minimum: u32, maximum: u32) -> Result<u32, ProfileError> {
    let value = u32::try_from(value)
        .map_err(|_| ProfileError::Invalid(format!("AXD {field} exceeds u32")))?;
    if !(minimum..=maximum).contains(&value) {
        return Err(ProfileError::Invalid(format!(
            "AXD {field} is outside {minimum}..={maximum}"
        )));
    }
    Ok(value)
}

fn effective_budget(
    task: &TaskBudget,
    class: ExecutionClass,
    overlay: Option<&AxdBudgetOverlay>,
    policy_cap: Option<&TaskBudget>,
) -> Result<TaskBudget, ProfileError> {
    let defaults = budget_for_class(class);
    let mut requested = TaskBudget {
        maximum_transitions: overlay
            .and_then(|value| value.maximum_transitions)
            .unwrap_or(defaults.maximum_transitions),
        maximum_model_calls: overlay
            .and_then(|value| value.maximum_model_calls)
            .unwrap_or(defaults.maximum_model_calls),
        maximum_tool_calls: overlay
            .and_then(|value| value.maximum_tool_calls)
            .unwrap_or(defaults.maximum_tool_calls),
        maximum_output_bytes: task.maximum_output_bytes,
        // Wall-clock time is a caller-owned execution lease, not a semantic
        // property of a development track or execution class.
        timeout_ms: overlay
            .and_then(|value| value.timeout_ms)
            .unwrap_or(task.timeout_ms),
    };
    if let Some(cap) = policy_cap {
        requested.maximum_transitions = requested.maximum_transitions.min(cap.maximum_transitions);
        requested.maximum_model_calls = requested.maximum_model_calls.min(cap.maximum_model_calls);
        requested.maximum_tool_calls = requested.maximum_tool_calls.min(cap.maximum_tool_calls);
    }
    let effective = TaskBudget {
        maximum_transitions: task.maximum_transitions.min(requested.maximum_transitions),
        maximum_model_calls: task.maximum_model_calls.min(requested.maximum_model_calls),
        maximum_tool_calls: task.maximum_tool_calls.min(requested.maximum_tool_calls),
        maximum_output_bytes: task.maximum_output_bytes,
        timeout_ms: requested.timeout_ms.min(task.timeout_ms),
    };
    effective.validate().map_err(ProfileError::Invalid)?;
    Ok(effective)
}

fn execution_strength(class: ExecutionClass) -> u8 {
    match class {
        ExecutionClass::Direct => 0,
        ExecutionClass::Guided => 1,
        ExecutionClass::Grounded => 2,
        ExecutionClass::Verified => 3,
    }
}

fn reasoning_strength(reasoning: ReasoningEffort) -> u8 {
    match reasoning {
        ReasoningEffort::Off => 0,
        ReasoningEffort::Low => 1,
        ReasoningEffort::Medium => 2,
        ReasoningEffort::High => 3,
        ReasoningEffort::XHigh => 4,
        ReasoningEffort::Max => 5,
    }
}

const fn budget_for_class(class: ExecutionClass) -> TaskBudget {
    match class {
        ExecutionClass::Direct => TaskBudget {
            maximum_transitions: 32,
            maximum_model_calls: 2,
            maximum_tool_calls: 6,
            maximum_output_bytes: 8 * 1024 * 1024,
            timeout_ms: 5 * 60 * 1_000,
        },
        ExecutionClass::Guided => TaskBudget {
            maximum_transitions: 96,
            maximum_model_calls: 6,
            maximum_tool_calls: 20,
            maximum_output_bytes: 8 * 1024 * 1024,
            timeout_ms: 15 * 60 * 1_000,
        },
        ExecutionClass::Grounded => TaskBudget {
            maximum_transitions: 192,
            maximum_model_calls: 12,
            maximum_tool_calls: 48,
            maximum_output_bytes: 8 * 1024 * 1024,
            timeout_ms: 30 * 60 * 1_000,
        },
        ExecutionClass::Verified => TaskBudget {
            maximum_transitions: 320,
            maximum_model_calls: 20,
            maximum_tool_calls: 80,
            maximum_output_bytes: 8 * 1024 * 1024,
            timeout_ms: 60 * 60 * 1_000,
        },
    }
}

fn default_workers_for_class(class: ExecutionClass, grounded_default: u8) -> u16 {
    match class {
        ExecutionClass::Direct | ExecutionClass::Guided | ExecutionClass::Verified => 1,
        ExecutionClass::Grounded => u16::from(grounded_default.max(1)),
    }
}

const fn default_topology_for_class(
    class: ExecutionClass,
    profile_default_workers: u8,
) -> ExecutionTopology {
    match class {
        ExecutionClass::Verified => ExecutionTopology::SingleWithIndependentVerifier,
        ExecutionClass::Grounded if profile_default_workers > 1 => ExecutionTopology::ParallelRead,
        ExecutionClass::Direct | ExecutionClass::Guided | ExecutionClass::Grounded => {
            ExecutionTopology::Single
        }
    }
}

fn string_field<'a>(
    object: &'a serde_json::Map<String, Value>,
    names: &[&str],
) -> Result<Option<&'a str>, ProfileError> {
    for name in names {
        if let Some(value) = object.get(*name) {
            return value.as_str().map(Some).ok_or_else(|| {
                ProfileError::Invalid(format!("AXD field {name} must be a string"))
            });
        }
    }
    Ok(None)
}

fn integer_field(
    object: &serde_json::Map<String, Value>,
    names: &[&str],
) -> Result<Option<u64>, ProfileError> {
    for name in names {
        if let Some(value) = object.get(*name) {
            return value.as_u64().map(Some).ok_or_else(|| {
                ProfileError::Invalid(format!("AXD field {name} must be an unsigned integer"))
            });
        }
    }
    Ok(None)
}

fn string_array_field(
    object: &serde_json::Map<String, Value>,
    names: &[&str],
) -> Result<Option<Vec<String>>, ProfileError> {
    for name in names {
        if let Some(value) = object.get(*name) {
            let values = value.as_array().ok_or_else(|| {
                ProfileError::Invalid(format!("AXD field {name} must be an array"))
            })?;
            let mut output = Vec::with_capacity(values.len());
            for item in values {
                let item = item.as_str().ok_or_else(|| {
                    ProfileError::Invalid(format!("AXD field {name} must contain strings"))
                })?;
                output.push(item.to_owned());
            }
            return Ok(Some(output));
        }
    }
    Ok(None)
}

fn argv_field(
    object: &serde_json::Map<String, Value>,
) -> Result<Option<Vec<String>>, ProfileError> {
    let Some(value) = object.get("argv") else {
        return Ok(None);
    };
    let values = value
        .as_array()
        .ok_or_else(|| ProfileError::Invalid("AXD check argv must be an array".to_owned()))?;
    if values.is_empty() || values.len() > 128 {
        return Err(ProfileError::Invalid(
            "AXD check argv must contain 1..=128 items".to_owned(),
        ));
    }
    let mut output = Vec::with_capacity(values.len());
    for value in values {
        let item = value.as_str().ok_or_else(|| {
            ProfileError::Invalid("AXD check argv must contain strings".to_owned())
        })?;
        if item.is_empty() || item.len() > 4_096 || item.contains('\0') {
            return Err(ProfileError::Invalid(
                "AXD check argv items must be bounded non-empty strings".to_owned(),
            ));
        }
        output.push(item.to_owned());
    }
    Ok(Some(output))
}

fn parse_execution_class(value: &str) -> Result<ExecutionClass, ProfileError> {
    match value.to_ascii_lowercase().as_str() {
        "direct" => Ok(ExecutionClass::Direct),
        "guided" => Ok(ExecutionClass::Guided),
        "grounded" => Ok(ExecutionClass::Grounded),
        "verified" => Ok(ExecutionClass::Verified),
        _ => Err(ProfileError::Invalid(format!(
            "unknown AXD execution class {value}"
        ))),
    }
}

fn parse_reasoning(value: &str) -> Result<ReasoningEffort, ProfileError> {
    match value.to_ascii_lowercase().as_str() {
        "off" | "minimal" => Ok(ReasoningEffort::Off),
        "low" => Ok(ReasoningEffort::Low),
        "medium" => Ok(ReasoningEffort::Medium),
        "high" => Ok(ReasoningEffort::High),
        "extra_high" | "extra-high" | "xhigh" => Ok(ReasoningEffort::XHigh),
        "max" => Ok(ReasoningEffort::Max),
        _ => Err(ProfileError::Invalid(format!(
            "unknown AXD reasoning effort {value}"
        ))),
    }
}

const fn reasoning_for_class(value: ExecutionClass) -> ReasoningEffort {
    match value {
        ExecutionClass::Direct => ReasoningEffort::Low,
        ExecutionClass::Guided => ReasoningEffort::Medium,
        ExecutionClass::Grounded | ExecutionClass::Verified => ReasoningEffort::High,
    }
}

fn expand_tool_selectors(
    selectors: &[String],
    base: &BTreeSet<String>,
) -> Result<BTreeSet<String>, ProfileError> {
    let mut output = BTreeSet::new();
    for selector in selectors {
        let candidates: &[&str] = match selector.as_str() {
            "repo.read" | "workspace.read" => &[
                "repo_map",
                "search_code",
                "read_file",
                "git_status",
                "inspect_diff",
            ],
            "repo.write" | "workspace.write" | "repo_edit" => &["write_file", "replace_text"],
            "process.run" | "check.run" | "test" => &["run_check"],
            "git.read" => &["git_status", "inspect_diff"],
            "interaction.request" => &["request_input"],
            "effect.plan" => &[],
            exact if base.contains(exact) => {
                output.insert(exact.to_owned());
                continue;
            }
            _ => {
                return Err(ProfileError::Invalid(format!(
                    "AXD tool selector {selector} is not supported by this profile"
                )));
            }
        };
        for candidate in candidates {
            if base.contains(*candidate) {
                output.insert((*candidate).to_owned());
            }
        }
    }
    Ok(output)
}

fn ensure_unique_bounded(label: &str, values: &[String]) -> Result<(), ProfileError> {
    if values.len() > 64 {
        return Err(ProfileError::Invalid(format!(
            "{label} list exceeds 64 entries"
        )));
    }
    let mut unique = BTreeSet::new();
    for value in values {
        if value.trim().is_empty() || value.len() > 256 || !unique.insert(value) {
            return Err(ProfileError::Invalid(format!(
                "{label} values must be unique bounded strings"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod reasoning_tests {
    use super::*;

    #[test]
    fn reasoning_parser_and_strength_preserve_all_supported_levels() {
        let cases = [
            ("off", ReasoningEffort::Off, 0),
            ("low", ReasoningEffort::Low, 1),
            ("medium", ReasoningEffort::Medium, 2),
            ("high", ReasoningEffort::High, 3),
            ("xhigh", ReasoningEffort::XHigh, 4),
            ("extra-high", ReasoningEffort::XHigh, 4),
            ("max", ReasoningEffort::Max, 5),
        ];
        for (input, expected, strength) in cases {
            let parsed = parse_reasoning(input).expect("supported reasoning");
            assert_eq!(parsed, expected);
            assert_eq!(reasoning_strength(parsed), strength);
        }
    }
}
