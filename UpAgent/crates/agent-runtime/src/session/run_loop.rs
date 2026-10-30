//! Provider/tool execution loop for one prepared task.

use super::{
    AgentFailure, AgentResult, AgentRuntime, BTreeMap, BTreeSet, DurableSession, ExecutionTopology,
    FailureCode, KernelEvent, MAXIMUM_MODEL_MESSAGE_SCALARS, MAXIMUM_MODEL_MESSAGES_PER_TURN,
    MODEL_CANCELLATION_DRAIN_TIMEOUT, ModelMessage, ModelTurn, PreparedTask, StreamExt,
    SuspensionBinding, TaskCancellation, TaskEnvelope, ToolCall, ToolExecution, ToolProgress,
    Value, budget_exceeded, build_output_contract_payload, cancelled_failure, denied_tool,
    failed_tool, json, stream, succeeded_result, undurable_failure,
};
use agent_core::{
    ArtifactEvidence, ArtifactUse, MAX_REVIEW_SUBJECT_BUNDLE_BYTES, MessageRole, ModelContentPart,
    REVIEW_SUBJECT_BUNDLE_SCHEMA_VERSION, ReviewSubjectBundle, ReviewSubjectBundleRef,
    ToolDefinition,
};
use agent_provider_api::{ProviderError, ResolvedImageObservation};

impl AgentRuntime {
    pub(super) async fn run_loop(
        &self,
        task: TaskEnvelope,
        prepared: PreparedTask,
        mut session: DurableSession,
        mut messages: Vec<ModelMessage>,
        deadline: tokio::time::Instant,
        cancellation: &TaskCancellation,
    ) -> AgentResult {
        loop {
            if cancellation.is_cancelled() {
                let failure = cancelled_failure("task was cancelled before the next model turn");
                return session.fail(&task, failure).await;
            }
            let image_review_request = match session.machine() {
                Ok(machine) => match machine.image_director.visual_review_request() {
                    Ok(request) => request,
                    Err(message) => {
                        return session
                            .fail(
                                &task,
                                AgentFailure::new(FailureCode::VerificationFailed, message),
                            )
                            .await;
                    }
                },
                Err(failure) => return undurable_failure(&task, failure),
            };
            if let Some(binding) = &image_review_request {
                messages.push(ModelMessage::text(MessageRole::Developer, format!(
                    "Current image review binding: {binding}. Observation alone is not approval. After inspecting every current candidate, return only IMAGE_REVIEW: followed by a JSON object with contractDigest, candidateRevision, candidateSetDigest and criteria. Each criterion must contain criterionId, decision (PASS or FAIL), and non-empty evidence. Cover each listed ID exactly once. Do not include tool calls in the review response. This is internal evidence, not the final answer; a separate normal final-output turn is required after all acceptance blockers clear."
                )));
            }
            if messages.len() > MAXIMUM_MODEL_MESSAGES_PER_TURN {
                let failure =
                    budget_exceeded("model conversation exceeded the provider message bound")
                        .with_detail("messages", messages.len().to_string())
                        .with_detail(
                            "maximumMessages",
                            MAXIMUM_MODEL_MESSAGES_PER_TURN.to_string(),
                        );
                return session.fail(&task, failure).await;
            }
            if tokio::time::Instant::now() >= deadline {
                return suspend_for_deadline(
                    &mut session,
                    &task,
                    &prepared,
                    messages,
                    budget_exceeded("task execution lease expired before the next model turn"),
                )
                .await;
            }
            let model_call = match session.machine() {
                Ok(machine) => match machine.ensure_model_budget(deadline) {
                    Ok(()) => machine.model_calls + 1,
                    Err(failure) if tokio::time::Instant::now() >= deadline => {
                        return suspend_for_deadline(
                            &mut session,
                            &task,
                            &prepared,
                            messages,
                            failure,
                        )
                        .await;
                    }
                    Err(failure)
                        if failure.code == FailureCode::BudgetExceeded
                            && machine.image_director.is_active() =>
                    {
                        return suspend_for_deadline(
                            &mut session,
                            &task,
                            &prepared,
                            messages,
                            failure,
                        )
                        .await;
                    }
                    Err(failure) => return session.fail(&task, failure).await,
                },
                Err(failure) => return undurable_failure(&task, failure),
            };
            let model_timeout_ms = match bounded_model_timeout(deadline) {
                Ok(timeout) => timeout,
                Err(failure) => return session.fail(&task, failure).await,
            };
            let turn = match record_model_request(
                &mut session,
                &task,
                &prepared,
                std::mem::take(&mut messages),
                model_timeout_ms,
                image_review_request,
                model_call,
            )
            .await
            {
                Ok(turn) => turn,
                Err(failure) => return session.fail(&task, failure).await,
            };
            match session.machine() {
                Ok(machine) => machine.model_calls = machine.model_calls.saturating_add(1),
                Err(failure) => return undurable_failure(&task, failure),
            }
            let observations = match self.resolve_model_observations(&turn) {
                Ok(value) => value,
                Err(failure) => return session.fail(&task, failure).await,
            };
            let observed_sha256 = observations
                .iter()
                .map(|observation| observation.sha256.clone())
                .collect::<Vec<_>>();
            let interrupted = {
                let model_cancellation = cancellation.token().child_token();
                let model_future = self.inner.model.complete_with_observations(
                    &turn,
                    &observations,
                    &model_cancellation,
                );
                tokio::pin!(model_future);
                tokio::select! {
                    biased;
                    () = cancellation.cancelled() => {
                        model_cancellation.cancel();
                        let drain = tokio::time::timeout(
                            MODEL_CANCELLATION_DRAIN_TIMEOUT,
                            &mut model_future,
                        )
                        .await;
                        let drain_status = if drain.is_ok() { "settled" } else { "timeout" };
                        Err(cancelled_failure("task was cancelled during model execution")
                            .with_detail("providerDrain", drain_status))
                    }
                    () = tokio::time::sleep_until(deadline) => {
                        model_cancellation.cancel();
                        let drain = tokio::time::timeout(
                            MODEL_CANCELLATION_DRAIN_TIMEOUT,
                            &mut model_future,
                        )
                        .await;
                        let drain_status = if drain.is_ok() { "settled" } else { "timeout" };
                        Err(budget_exceeded(
                            "model execution exceeded the remaining task deadline",
                        )
                        .with_detail("providerDrain", drain_status))
                    }
                    value = &mut model_future => Ok(value),
                }
            };
            messages = turn.messages;
            let model_result = match interrupted {
                Ok(Ok(value)) => value,
                Ok(Err(error)) => {
                    let failure = match error {
                        ProviderError::Failure(value) => value,
                        ProviderError::Contract(message) => {
                            AgentFailure::new(FailureCode::ProviderFailed, message)
                        }
                    };
                    return session.fail(&task, failure).await;
                }
                Err(failure) if failure.code == FailureCode::BudgetExceeded => {
                    return suspend_for_deadline(&mut session, &task, &prepared, messages, failure)
                        .await;
                }
                Err(failure) => return session.fail(&task, failure).await,
            };
            let response_json = match serde_json::to_string(&model_result) {
                Ok(value) => value,
                Err(error) => {
                    let failure = AgentFailure::new(
                        FailureCode::InternalInvariant,
                        "model response could not be encoded for durable replay",
                    )
                    .with_detail("cause", error.to_string());
                    return session.fail(&task, failure).await;
                }
            };
            let response_artifact = match session
                .put_artifact_text(
                    response_json,
                    "application/vnd.upagent.model-result+json; charset=utf-8",
                )
                .await
            {
                Ok(value) => value,
                Err(failure) => return session.fail(&task, failure).await,
            };
            if let Err(error) = session
                .record(
                    KernelEvent::ModelCompleted,
                    json!({
                        "modelCall":model_call,
                        "textBytes":model_result.text.len(),
                        "toolCalls":model_result.tool_calls.len(),
                        "usage":&model_result.usage,
                        "responseArtifact":&response_artifact,
                        "pluginExecutionDigest":prepared.plugin_binding.fingerprint(),
                    }),
                )
                .await
            {
                return session.fail(&task, error).await;
            }
            match session.machine() {
                Ok(machine) => {
                    machine.add_usage(model_result.usage.as_ref());
                    machine
                        .image_director
                        .mark_observed_hashes(&observed_sha256);
                }
                Err(failure) => return undurable_failure(&task, failure),
            }
            let agent_core::ModelTurnResult {
                text,
                tool_calls: calls,
                ..
            } = model_result;
            if !calls.is_empty() && text.chars().count() > MAXIMUM_MODEL_MESSAGE_SCALARS {
                let failure = AgentFailure::new(
                    FailureCode::ProviderFailed,
                    "provider returned assistant text too large to continue a tool turn",
                );
                return session.fail(&task, failure).await;
            }
            if cancellation.is_cancelled() {
                let failure = cancelled_failure("task was cancelled after model execution");
                return session.fail(&task, failure).await;
            }
            let image_active = match session.machine() {
                Ok(machine) => machine.image_director.is_active(),
                Err(failure) => return undurable_failure(&task, failure),
            };
            if image_active {
                let review = match agent_core::ImageVisualReview::from_model_response(&text) {
                    Ok(review) => review,
                    Err(message) => {
                        return session
                            .fail(
                                &task,
                                AgentFailure::new(FailureCode::VerificationFailed, message),
                            )
                            .await;
                    }
                };
                if let Some(review) = review {
                    if !calls.is_empty() {
                        return session
                            .fail(
                                &task,
                                AgentFailure::new(
                                    FailureCode::VerificationFailed,
                                    "IMAGE_REVIEW must not contain tool calls",
                                ),
                            )
                            .await;
                    }
                    let result = match session.machine() {
                        Ok(machine) => machine.image_director.record_visual_review(review),
                        Err(failure) => return undurable_failure(&task, failure),
                    };
                    if let Err(message) = result {
                        return session
                            .fail(
                                &task,
                                AgentFailure::new(FailureCode::VerificationFailed, message),
                            )
                            .await;
                    }
                    let blocker = match session.machine() {
                        Ok(machine) => machine.image_director.completion_blocker(),
                        Err(failure) => return undurable_failure(&task, failure),
                    };
                    messages.push(ModelMessage::text(MessageRole::Assistant, text));
                    messages.push(ModelMessage::text(MessageRole::Developer, match blocker {
                        Some(reason) => format!("Visual evidence was recorded, but completion remains blocked: {reason}. Correct the current candidate using permitted tools; new candidates require fresh review. Do not automatically retry an uncertain external edit."),
                        None => "Visual qualification is complete. Now provide a separate normal final answer describing the delivered artifacts; do not return IMAGE_REVIEW as the user-facing result.".to_owned(),
                    }));
                    continue;
                }
            }
            if calls.is_empty() {
                let blocker = match session.machine() {
                    Ok(machine) => machine.image_director.completion_blocker(),
                    Err(failure) => return undurable_failure(&task, failure),
                };
                if let Some(blocker) = blocker {
                    let revision_allowed = match session.machine() {
                        Ok(machine) => machine.image_director.request_revision(),
                        Err(failure) => return undurable_failure(&task, failure),
                    };
                    if revision_allowed.is_ok() {
                        if text.chars().count() <= MAXIMUM_MODEL_MESSAGE_SCALARS {
                            messages.push(ModelMessage::text(MessageRole::Assistant, text));
                        } else {
                            messages.push(ModelMessage::text(
                                MessageRole::Assistant,
                                "Image Director rejected the previous oversized final response.",
                            ));
                        }
                        messages.push(ModelMessage::text(
                            MessageRole::Developer,
                            format!(
                                "Image Director blocked completion: {blocker}. Revise the existing image candidate with deterministic image tools, re-run image_compare when deterministic assertions apply, visually inspect the complete resulting candidate set, submit bound IMAGE_REVIEW evidence, and only then provide a separate normal answer. Do not fall back to image generation after editing has started. Previous immutable candidate artifacts remain available for rollback."
                            ),
                        ));
                        continue;
                    }
                    let failure = AgentFailure::new(
                        FailureCode::VerificationFailed,
                        "image director could not accept the final candidate",
                    )
                    .with_detail("blocker", blocker)
                    .with_detail(
                        "cause",
                        revision_allowed
                            .err()
                            .unwrap_or_else(|| "revision denied".to_owned()),
                    );
                    return session.fail(&task, failure).await;
                }
                return self
                    .finish_task(&task, &prepared, &mut session, text, cancellation)
                    .await;
            }
            if let Some(result) = self
                .execute_tool_calls(
                    &task,
                    &prepared,
                    &mut session,
                    &mut messages,
                    text,
                    calls,
                    deadline,
                    cancellation,
                )
                .await
            {
                return result;
            }
        }
    }

    async fn finish_task(
        &self,
        task: &TaskEnvelope,
        prepared: &PreparedTask,
        session: &mut DurableSession,
        text: String,
        cancellation: &TaskCancellation,
    ) -> AgentResult {
        if let Err(error) = prepared
            .profile
            .validate_output(&text, prepared.plan.effective_budget.maximum_output_bytes)
        {
            let failure = AgentFailure::new(FailureCode::InvalidProfile, error.to_string());
            return session.fail(task, failure).await;
        }
        let completion_contract = match session.machine() {
            Ok(machine) => self.verify_completion_contract(&prepared.plan, machine),
            Err(failure) => return undurable_failure(task, failure),
        };
        if let Err(failure) = completion_contract {
            return session.fail(task, failure).await;
        }
        if let Err(message) = match session.machine() {
            Ok(machine) => machine.image_director.accept(),
            Err(failure) => return undurable_failure(task, failure),
        } {
            let failure = AgentFailure::new(FailureCode::VerificationFailed, message)
                .with_detail("authority", "upagent.image-director/1");
            return session.fail(task, failure).await;
        }
        let artifact = match session
            .put_artifact_text(text.clone(), "text/markdown; charset=utf-8")
            .await
        {
            Ok(value) => value,
            Err(failure) => return session.fail(task, failure).await,
        };
        let contract_artifact = match build_output_contract_payload(
            task,
            &prepared.plan,
            match session.machine() {
                Ok(machine) => machine,
                Err(failure) => return undurable_failure(task, failure),
            },
            &artifact,
            prepared.plugin_binding.plugin_set_fingerprint(),
            prepared.plugin_binding.fingerprint(),
        ) {
            Ok(Some(payload)) => {
                let encoded = match serde_json::to_string_pretty(&payload) {
                    Ok(value) => value,
                    Err(error) => {
                        let failure = AgentFailure::new(
                            FailureCode::InternalInvariant,
                            "output contract result could not be encoded",
                        )
                        .with_detail("cause", error.to_string());
                        return session.fail(task, failure).await;
                    }
                };
                let media_type = match prepared.plan.output_contract.as_deref() {
                    Some("axd.agent-result/2") => {
                        "application/vnd.axd.agent-result+json; charset=utf-8"
                    }
                    Some("structured_result_with_evidence") => {
                        "application/vnd.upagent.structured-result+json; charset=utf-8"
                    }
                    _ => "application/json; charset=utf-8",
                };
                match session.put_artifact_text(encoded, media_type).await {
                    Ok(value) => Some(value),
                    Err(failure) => return session.fail(task, failure).await,
                }
            }
            Ok(None) => None,
            Err(failure) => return session.fail(task, failure).await,
        };
        if cancellation.is_cancelled() {
            let failure = cancelled_failure("task was cancelled before final result commit");
            return session.fail(task, failure).await;
        }
        let completed_sequence = match session.next_sequence().await {
            Ok(sequence) => sequence,
            Err(failure) => return session.fail(task, failure).await,
        };
        let final_output_use = ArtifactUse {
            sequence: completed_sequence,
            event: "completed".to_owned(),
            call_index: None,
            call_id: None,
        };
        let subject_bundle =
            if prepared.composition.as_ref().is_some_and(|receipt| {
                receipt.spec.review == agent_core::ReviewSpec::EmitSubjectBundle
            }) {
                let receipt = prepared.composition.as_ref().ok_or_else(|| {
                    AgentFailure::new(
                        FailureCode::InternalInvariant,
                        "review subject bundle requested without a composition receipt",
                    )
                });
                let receipt = match receipt {
                    Ok(value) => value,
                    Err(failure) => return session.fail(task, failure).await,
                };
                let (result_uses, evidence) = match session.machine() {
                    Ok(machine) => {
                        let all = machine.evidence();
                        let mut result_uses = all
                            .iter()
                            .find(|entry| entry.artifact.id == artifact.id)
                            .map_or_else(Vec::new, |entry| entry.uses.clone());
                        result_uses.push(final_output_use.clone());
                        let mut evidence = all
                            .into_iter()
                            .filter(|entry| entry.artifact.id != artifact.id)
                            .collect::<Vec<_>>();
                        if let Some(contract_artifact) = &contract_artifact {
                            if let Some(existing) = evidence
                                .iter_mut()
                                .find(|entry| entry.artifact.id == contract_artifact.id)
                            {
                                existing.uses.push(final_output_use.clone());
                            } else {
                                evidence.push(ArtifactEvidence {
                                    artifact: contract_artifact.clone(),
                                    uses: vec![final_output_use.clone()],
                                });
                            }
                        }
                        (result_uses, evidence)
                    }
                    Err(failure) => return undurable_failure(task, failure),
                };
                let bundle = ReviewSubjectBundle {
                    schema_version: REVIEW_SUBJECT_BUNDLE_SCHEMA_VERSION,
                    task_id: task.task_id.clone(),
                    session_id: task.session_id.clone(),
                    task_digest: receipt.task_digest.clone(),
                    composition_digest: receipt.digest.clone(),
                    result_artifact: artifact.clone(),
                    result_uses,
                    evidence,
                };
                let encoded = match encode_review_subject_bundle(&bundle) {
                    Ok(value) => value,
                    Err(failure) => return session.fail(task, failure).await,
                };
                let bundle_artifact = match session
                    .put_artifact_text(
                        encoded,
                        "application/vnd.upagent.review-subject+json; charset=utf-8",
                    )
                    .await
                {
                    Ok(value) => value,
                    Err(failure) => return session.fail(task, failure).await,
                };
                Some(ReviewSubjectBundleRef {
                    bundle_sha256: bundle_artifact.sha256.clone(),
                    artifact: bundle_artifact,
                })
            } else {
                None
            };
        let completed_payload = json!({
            "outputArtifact": &artifact,
            "contractArtifact": contract_artifact.as_ref(),
            "reviewSubjectBundle": subject_bundle.as_ref().map(|value| &value.artifact),
            "executionClass": prepared.plan.execution_class.as_str(),
            "pluginSetDigest": prepared.plugin_binding.plugin_set_fingerprint(),
            "pluginExecutionDigest": prepared.plugin_binding.fingerprint(),
            "requiredChecks": prepared.plan.required_checks,
            "outputContract": prepared.plan.output_contract,
        });
        // Once the provider has produced a valid final answer, finish the small
        // durable commit even if the lease expires. Suspending here would replay
        // a completed model turn without adding useful work. `record` registers
        // every payload artifact transactionally immediately before its journal
        // append; a separate reservation here would leave stale ledger uses if
        // the append could not obtain a sequence or transition.
        if let Err(error) = session
            .record(KernelEvent::Completed, completed_payload)
            .await
        {
            return session.fail(task, error).await;
        }
        let result = match session.machine() {
            Ok(machine) => {
                let mut result = succeeded_result(task, machine, text);
                result
                    .composition_digest
                    .clone_from(&prepared.composition_digest);
                result.review_subject_bundle.clone_from(&subject_bundle);
                // The terminal `Completed` event registers the bundle
                // reference in the same ledger projection as every other
                // artifact. No second result-side collection is maintained.
                result
            }
            Err(failure) => return undurable_failure(task, failure),
        };
        session.finish(task, result).await
    }

    fn resolve_model_observations(
        &self,
        turn: &ModelTurn,
    ) -> Result<Vec<ResolvedImageObservation>, AgentFailure> {
        let mut resolved = Vec::new();
        let mut aggregate_bytes = 0_usize;
        for (message_index, message) in turn.messages.iter().enumerate() {
            for (part_index, part) in message.content_parts.iter().enumerate() {
                let ModelContentPart::ImageObservation { image } = part else {
                    continue;
                };
                let bytes = self
                    .inner
                    .artifacts
                    .read(&image.artifact)
                    .map_err(|error| {
                        AgentFailure::new(
                            FailureCode::StorageFailed,
                            "image observation artifact could not be resolved",
                        )
                        .with_detail("cause", error.to_string())
                    })?;
                aggregate_bytes = aggregate_bytes.checked_add(bytes.len()).ok_or_else(|| {
                    AgentFailure::new(
                        FailureCode::BudgetExceeded,
                        "image observation aggregate byte count overflowed",
                    )
                })?;
                if aggregate_bytes > 32 * 1024 * 1024 {
                    return Err(AgentFailure::new(
                        FailureCode::BudgetExceeded,
                        "image observations exceed the 32 MiB aggregate bound",
                    ));
                }
                let (width, height) = png_dimensions(&bytes)?;
                resolved.push(ResolvedImageObservation {
                    message_index,
                    part_index,
                    media_type: image.artifact.media_type.clone(),
                    sha256: image.artifact.sha256.clone(),
                    width,
                    height,
                    role: image.role,
                    bytes,
                });
            }
        }
        Ok(resolved)
    }

    #[allow(clippy::too_many_arguments)]
    async fn execute_tool_calls(
        &self,
        task: &TaskEnvelope,
        prepared: &PreparedTask,
        session: &mut DurableSession,
        messages: &mut Vec<ModelMessage>,
        assistant_text: String,
        calls: Vec<ToolCall>,
        deadline: tokio::time::Instant,
        cancellation: &TaskCancellation,
    ) -> Option<AgentResult> {
        let reserved = match session.machine() {
            Ok(machine) => machine.reserve_tools(calls.len(), deadline),
            Err(failure) => return Some(undurable_failure(task, failure)),
        };
        if let Err(failure) = reserved {
            if tokio::time::Instant::now() >= deadline {
                messages.push(ModelMessage::assistant_with_tools(
                    assistant_text,
                    calls.clone(),
                ));
                for call in calls {
                    if let Err(failure) = self.admit_tool_call(session, prepared, &call).await {
                        return Some(session.fail(task, failure).await);
                    }
                    let execution = ToolExecution::Plain(failed_tool(
                        &call,
                        budget_exceeded("tool was not started because the execution lease expired"),
                    ));
                    match session.apply_tool_outcome(task, &call, execution).await {
                        ToolProgress::Continue(message) => messages.push(message),
                        ToolProgress::Finished(result) => return Some(result),
                    }
                }
                return Some(
                    suspend_for_deadline(session, task, prepared, messages.clone(), failure).await,
                );
            }
            return Some(session.fail(task, failure).await);
        }
        messages.push(ModelMessage::assistant_with_tools(
            assistant_text,
            calls.clone(),
        ));
        let parallel_read = calls.len() > 1
            && prepared.plan.topology == ExecutionTopology::ParallelRead
            && prepared.plan.maximum_workers > 1
            && calls
                .iter()
                .all(|call| self.can_parallelize_read(prepared, call));
        if parallel_read {
            for call in &calls {
                if cancellation.is_cancelled() {
                    let failure =
                        cancelled_failure("task was cancelled before parallel tool execution");
                    return Some(session.fail(task, failure).await);
                }
                if let Err(failure) = self.admit_tool_call(session, prepared, call).await {
                    return Some(session.fail(task, failure).await);
                }
            }
            let maximum = usize::from(prepared.plan.maximum_workers)
                .min(calls.len())
                .max(1);
            let semantic_operations = match session.machine_ref() {
                Ok(machine) => calls
                    .iter()
                    .map(|call| machine.semantic_mask_operation_for(call))
                    .collect::<Vec<_>>(),
                Err(failure) => return Some(session.fail(task, failure).await),
            };
            let executions = stream::iter(calls.into_iter().zip(semantic_operations).map(
                |(call, semantic_mask_operation)| async move {
                    let result = self
                        .dispatch_tool(
                            task,
                            prepared,
                            &call,
                            deadline,
                            cancellation,
                            semantic_mask_operation,
                        )
                        .await;
                    (call, result)
                },
            ))
            .buffered(maximum)
            .collect::<Vec<_>>()
            .await;
            for (call, execution) in executions {
                let execution = match execution {
                    Ok(value) => value,
                    Err(failure) if failure.code == FailureCode::BudgetExceeded => {
                        ToolExecution::Plain(failed_tool(&call, failure))
                    }
                    Err(failure) => return Some(session.fail(task, failure).await),
                };
                match session.apply_tool_outcome(task, &call, execution).await {
                    ToolProgress::Continue(message) => messages.push(message),
                    ToolProgress::Finished(result) => return Some(result),
                }
            }
            return None;
        }
        let mut lease_expired = false;
        for call in calls {
            if cancellation.is_cancelled() {
                let failure = cancelled_failure("task was cancelled before tool execution");
                return Some(session.fail(task, failure).await);
            }
            if let Err(failure) = self.admit_tool_call(session, prepared, &call).await {
                return Some(session.fail(task, failure).await);
            }
            let execution = if lease_expired || tokio::time::Instant::now() >= deadline {
                lease_expired = true;
                ToolExecution::Plain(failed_tool(
                    &call,
                    budget_exceeded("tool was not started because the execution lease expired"),
                ))
            } else if prepared.plan.allowed_tools.contains(&call.name) {
                let semantic_mask_operation = match session.machine_ref() {
                    Ok(machine) => machine.semantic_mask_operation_for(&call),
                    Err(failure) => return Some(session.fail(task, failure).await),
                };
                match self
                    .dispatch_tool(
                        task,
                        prepared,
                        &call,
                        deadline,
                        cancellation,
                        semantic_mask_operation,
                    )
                    .await
                {
                    Ok(value) => value,
                    Err(failure) if failure.code == FailureCode::BudgetExceeded => {
                        lease_expired = true;
                        ToolExecution::Plain(failed_tool(&call, failure))
                    }
                    Err(failure) => return Some(session.fail(task, failure).await),
                }
            } else {
                ToolExecution::Plain(denied_tool(
                    &call,
                    "tool is outside the active profile allowlist",
                ))
            };
            match session.apply_tool_outcome(task, &call, execution).await {
                ToolProgress::Continue(message) => messages.push(message),
                ToolProgress::Finished(result) => return Some(result),
            }
        }
        None
    }
}

async fn suspend_for_deadline(
    session: &mut DurableSession,
    task: &TaskEnvelope,
    prepared: &PreparedTask,
    messages: Vec<ModelMessage>,
    reason: AgentFailure,
) -> AgentResult {
    session
        .suspend(
            task,
            messages,
            SuspensionBinding {
                plugin_set_digest: prepared.plugin_binding.plugin_set_fingerprint(),
                plugin_execution_digest: prepared.plugin_binding.fingerprint(),
                preparation_digest: &prepared.preparation_digest,
                composition_digest: prepared.composition_digest.as_deref(),
                approved_subjects: &prepared.approved_subjects,
            },
            reason,
        )
        .await
}

fn encode_review_subject_bundle(bundle: &ReviewSubjectBundle) -> Result<String, AgentFailure> {
    bundle.validate().map_err(|failure| {
        AgentFailure::new(FailureCode::InternalInvariant, failure.to_string())
    })?;
    let encoded = serde_json::to_string(bundle).map_err(|error| {
        AgentFailure::new(
            FailureCode::StorageFailed,
            "review subject bundle could not be encoded",
        )
        .with_detail("cause", error.to_string())
    })?;
    if encoded.len() > MAX_REVIEW_SUBJECT_BUNDLE_BYTES {
        return Err(AgentFailure::new(
            FailureCode::StorageFailed,
            "review subject bundle exceeds its byte bound",
        )
        .with_detail("maximumBytes", MAX_REVIEW_SUBJECT_BUNDLE_BYTES.to_string())
        .with_detail("encodedBytes", encoded.len().to_string()));
    }
    Ok(encoded)
}

fn png_dimensions(bytes: &[u8]) -> Result<(u32, u32), AgentFailure> {
    if bytes.len() < 33 || !bytes.starts_with(b"\x89PNG\r\n\x1a\n") || &bytes[12..16] != b"IHDR" {
        return Err(AgentFailure::new(
            FailureCode::VerificationFailed,
            "image observation artifact is not a PNG raster",
        ));
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().map_err(|_| {
        AgentFailure::new(
            FailureCode::VerificationFailed,
            "PNG width header is invalid",
        )
    })?);
    let height = u32::from_be_bytes(bytes[20..24].try_into().map_err(|_| {
        AgentFailure::new(
            FailureCode::VerificationFailed,
            "PNG height header is invalid",
        )
    })?);
    if width == 0 || height == 0 || width > 8192 || height > 8192 {
        return Err(AgentFailure::new(
            FailureCode::VerificationFailed,
            "image observation dimensions exceed the bounded raster limit",
        ));
    }
    Ok((width, height))
}

fn bounded_model_timeout(deadline: tokio::time::Instant) -> Result<u64, AgentFailure> {
    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
    u64::try_from(remaining.as_millis().min(u128::from(3_600_000_u64)).max(1)).map_err(|_| {
        AgentFailure::new(
            FailureCode::InternalInvariant,
            "bounded model timeout does not fit u64",
        )
    })
}

async fn record_model_request(
    session: &mut DurableSession,
    task: &TaskEnvelope,
    prepared: &PreparedTask,
    messages: Vec<ModelMessage>,
    timeout_ms: u64,
    image_review_request: Option<Value>,
    model_call: u32,
) -> Result<ModelTurn, AgentFailure> {
    let turn = ModelTurn {
        messages,
        tools: model_tools_for_task(task, &prepared.tools)?,
        reasoning: prepared.plan.reasoning,
        timeout_ms,
        maximum_response_bytes: prepared.plan.effective_budget.maximum_output_bytes,
        metadata: BTreeMap::from([
            (
                "imageReview".to_owned(),
                image_review_request.unwrap_or(Value::Null),
            ),
            (
                "profile".to_owned(),
                Value::String(task.profile.to_string()),
            ),
            ("taskId".to_owned(), Value::String(task.task_id.to_string())),
            (
                "executionClass".to_owned(),
                Value::String(prepared.plan.execution_class.as_str().to_owned()),
            ),
            (
                "pluginSetDigest".to_owned(),
                Value::String(prepared.plugin_binding.plugin_set_fingerprint().to_owned()),
            ),
            (
                "pluginExecutionDigest".to_owned(),
                Value::String(prepared.plugin_binding.fingerprint().to_owned()),
            ),
            (
                "pluginExecution".to_owned(),
                prepared.plugin_binding.as_json(),
            ),
            (
                "requiredChecks".to_owned(),
                json!(&prepared.plan.required_checks),
            ),
            (
                "outputContract".to_owned(),
                json!(&prepared.plan.output_contract),
            ),
            (
                "effectiveBudget".to_owned(),
                json!(&prepared.plan.effective_budget),
            ),
        ]),
    };
    let request_json = serde_json::to_string(&turn).map_err(|error| {
        AgentFailure::new(
            FailureCode::InternalInvariant,
            "model request could not be encoded for durable replay",
        )
        .with_detail("cause", error.to_string())
    })?;
    let request_artifact = session
        .put_artifact_text(
            request_json,
            "application/vnd.upagent.model-turn+json; charset=utf-8",
        )
        .await?;
    session
        .record(
            KernelEvent::ModelRequested,
            json!({
                "modelCall":model_call,
                "executionClass":prepared.plan.execution_class.as_str(),
                "pluginSetDigest":prepared.plugin_binding.plugin_set_fingerprint(),
                "pluginExecutionDigest":prepared.plugin_binding.fingerprint(),
                "requestArtifact":&request_artifact,
            }),
        )
        .await?;
    Ok(turn)
}

fn model_tools_for_task(
    task: &TaskEnvelope,
    tools: &[ToolDefinition],
) -> Result<Vec<ToolDefinition>, AgentFailure> {
    let Some(specification) = task
        .image_task
        .as_ref()
        .and_then(|contract| contract.verification.deterministic.as_ref())
    else {
        return Ok(tools.to_vec());
    };
    let selected_types = specification
        .assertions
        .iter()
        .map(agent_core::ImageCompareAssertion::assertion_type)
        .collect::<BTreeSet<_>>();
    let invalid_schema = || {
        AgentFailure::new(
            FailureCode::InternalInvariant,
            "task-bound image comparison schema cannot represent the verification contract",
        )
    };
    let mut model_tools = tools.to_vec();
    for tool in model_tools
        .iter_mut()
        .filter(|tool| tool.name == agent_core::IMAGE_COMPARE_TOOL)
    {
        let branches = tool
            .input_schema
            .pointer_mut("/properties/assertions/items/oneOf")
            .and_then(Value::as_array_mut)
            .filter(|branches| !branches.is_empty())
            .ok_or_else(invalid_schema)?;
        let mut offered_types = BTreeSet::new();
        for branch in branches.iter() {
            let assertion_type = branch
                .pointer("/properties/type/const")
                .and_then(Value::as_str)
                .ok_or_else(invalid_schema)?;
            if branch.get("type").and_then(Value::as_str) != Some("object")
                || !branch
                    .get("required")
                    .and_then(Value::as_array)
                    .is_some_and(|required| required.iter().any(|name| name == "type"))
                || !offered_types.insert(assertion_type.to_owned())
            {
                return Err(invalid_schema());
            }
        }
        if selected_types.is_empty()
            || !selected_types
                .iter()
                .all(|selected| offered_types.contains(*selected))
        {
            return Err(invalid_schema());
        }
        // This is only the model's request view. Keep the sealed definitions
        // and every retained branch unchanged; admission still checks the exact spec.
        branches.retain(|branch| {
            branch
                .pointer("/properties/type/const")
                .and_then(Value::as_str)
                .is_some_and(|assertion_type| selected_types.contains(assertion_type))
        });
    }
    Ok(model_tools)
}

#[cfg(test)]
mod tests {
    use super::{encode_review_subject_bundle, model_tools_for_task};
    use agent_core::{
        ArtifactEvidence, ArtifactId, ArtifactRef, ArtifactUse, FailureCode,
        IMAGE_TASK_CONTRACT_SCHEMA, IMAGE_VERIFICATION_SPEC_SCHEMA, ImageCompareArtifactKind,
        ImageCompareAssertion, ImageCompareSeverity, ImageSource, ImageTaskContract,
        ImageVerificationContract, ImageVerificationSpec, MAX_REVIEW_SUBJECT_BUNDLE_BYTES,
        REVIEW_SUBJECT_BUNDLE_SCHEMA_VERSION, ReviewSubjectBundle, SessionId, TaskEnvelope, TaskId,
    };
    use serde_json::{Value, json};

    fn verification_task(
        assertions: Vec<ImageCompareAssertion>,
    ) -> Result<TaskEnvelope, Box<dyn std::error::Error>> {
        let artifact = ArtifactRef {
            id: ArtifactId::parse(format!("art-{}", "a".repeat(64)))?,
            sha256: "a".repeat(64),
            media_type: "image/png".to_owned(),
            byte_length: 1,
            relative_path: format!("aa/{}.png", "a".repeat(64)),
        };
        Ok(TaskEnvelope {
            schema_version: agent_core::PROTOCOL_VERSION,
            task_id: TaskId::parse("task-model-tool-view")?,
            session_id: SessionId::parse("session-model-tool-view")?,
            parent_task_id: None,
            profile: agent_core::ProfileId::creator(),
            track: None,
            objective: "Verify the existing image candidate".to_owned(),
            image_task: Some(ImageTaskContract {
                schema: IMAGE_TASK_CONTRACT_SCHEMA.to_owned(),
                goal: "Verify the existing image candidate".to_owned(),
                source: ImageSource::Provided {
                    artifact: artifact.clone(),
                },
                maximum_attempts: 1,
                preserve: Vec::new(),
                visual_criteria: vec!["review the candidate".to_owned()],
                verification: ImageVerificationContract {
                    visual_review: true,
                    deterministic: Some(ImageVerificationSpec {
                        schema: IMAGE_VERIFICATION_SPEC_SCHEMA.to_owned(),
                        before_artifact: artifact,
                        after_kind: ImageCompareArtifactKind::Raster,
                        assertions,
                    }),
                },
            }),
            history: Vec::new(),
            workspace: None,
            skills: Vec::new(),
            context: std::collections::BTreeMap::new(),
            budget: agent_core::TaskBudget::default(),
            effect_approval: agent_core::EffectApprovalMode::Deny,
            allow_memory_write: false,
            approvals: Vec::new(),
        })
    }

    fn changed_ratio() -> ImageCompareAssertion {
        ImageCompareAssertion::ChangedRatio {
            id: "changed".to_owned(),
            severity: ImageCompareSeverity::Required,
            minimum: Some(0.005),
            maximum: None,
        }
    }

    #[test]
    fn model_tool_view_keeps_only_bound_types_without_mutating_canonical_branches()
    -> Result<(), Box<dyn std::error::Error>> {
        let task = verification_task(vec![changed_ratio()])?;
        let tools = crate::canonical_tool_bindings(&[agent_core::IMAGE_COMPARE_TOOL.to_owned()])?
            .into_iter()
            .map(|binding| binding.definition)
            .collect::<Vec<_>>();
        let original = tools.clone();
        let view = model_tools_for_task(&task, &tools)?;
        let mut expected = tools.clone();
        let branch = tools[0].input_schema["properties"]["assertions"]["items"]["oneOf"]
            .as_array()
            .and_then(|branches| {
                branches
                    .iter()
                    .find(|branch| branch["properties"]["type"]["const"] == "changed_ratio")
            })
            .ok_or("canonical comparison schema has no changed_ratio branch")?;
        expected[0].input_schema["properties"]["assertions"]["items"]["oneOf"] = json!([branch]);
        assert_eq!(view, expected);
        assert_eq!(tools, original);
        assert_eq!(
            view[0].input_schema["properties"]["assertions"]["items"]["oneOf"][0]["anyOf"],
            json!([{"required":["minimum"]},{"required":["maximum"]}])
        );
        Ok(())
    }

    #[test]
    fn model_tool_view_includes_advisory_types_and_preserves_other_tools()
    -> Result<(), Box<dyn std::error::Error>> {
        let task = verification_task(vec![
            changed_ratio(),
            ImageCompareAssertion::MaximumChannelError {
                id: "diagnostic".to_owned(),
                severity: ImageCompareSeverity::Advisory,
                maximum: 10,
            },
        ])?;
        let tools = crate::canonical_tool_bindings(&[
            agent_core::IMAGE_COMPARE_TOOL.to_owned(),
            agent_core::IMAGE_INSPECT_TOOL.to_owned(),
        ])?
        .into_iter()
        .map(|binding| binding.definition)
        .collect::<Vec<_>>();
        let view = model_tools_for_task(&task, &tools)?;
        let branches = view[0].input_schema["properties"]["assertions"]["items"]["oneOf"]
            .as_array()
            .ok_or("missing comparison branches")?;
        assert_eq!(branches.len(), 2);
        assert_eq!(branches[0]["properties"]["type"]["const"], "changed_ratio");
        assert_eq!(
            branches[1]["properties"]["type"]["const"],
            "maximum_channel_error"
        );
        assert_eq!(view[1], tools[1]);
        Ok(())
    }

    #[test]
    fn model_tool_view_without_deterministic_contract_is_unchanged()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut task = verification_task(vec![changed_ratio()])?;
        let tools = crate::image_tools::tool_definitions();
        task.image_task
            .as_mut()
            .ok_or("missing contract")?
            .verification
            .deterministic = None;
        assert_eq!(model_tools_for_task(&task, &tools)?, tools);
        task.image_task = None;
        assert_eq!(model_tools_for_task(&task, &tools)?, tools);
        Ok(())
    }

    #[test]
    fn model_tool_view_rejects_malformed_or_unrepresentable_bound_schema()
    -> Result<(), Box<dyn std::error::Error>> {
        let task = verification_task(vec![changed_ratio()])?;
        let tools = crate::canonical_tool_bindings(&[agent_core::IMAGE_COMPARE_TOOL.to_owned()])?
            .into_iter()
            .map(|binding| binding.definition)
            .collect::<Vec<_>>();
        for replacement in [
            Value::Null,
            json!([]),
            json!([{"type":"object","required":["type"],"properties":{"type":{"const":"exact_equal"}}}]),
            json!([{"type":"object","required":["type"],"properties":{"type":{"const":"unknown"}}}]),
            json!([{"type":"object","required":["type"],"properties":{"type":{"const":17}}}]),
            json!([{"type":"string","required":["type"],"properties":{"type":{"const":"changed_ratio"}}}]),
            json!([{"type":"object","required":[],"properties":{"type":{"const":"changed_ratio"}}}]),
            json!([
                {"type":"object","required":["type"],"properties":{"type":{"const":"changed_ratio"}}},
                {"type":"object","required":["type"],"properties":{"type":{"const":"changed_ratio"}}}
            ]),
        ] {
            let mut malformed = tools.clone();
            malformed[0].input_schema["properties"]["assertions"]["items"]["oneOf"] = replacement;
            let error = model_tools_for_task(&task, &malformed)
                .err()
                .ok_or("invalid bound schema must not fall back to a permissive view")?;
            assert_eq!(error.code, FailureCode::InternalInvariant);
        }
        Ok(())
    }

    #[test]
    fn producer_rejects_oversized_bundle_before_artifact_write() {
        let evidence = (0..800)
            .map(|index| {
                let digest = format!("{index:064x}");
                ArtifactEvidence {
                    artifact: ArtifactRef {
                        id: ArtifactId::parse(format!("art-{digest}")).expect("artifact id"),
                        sha256: digest.clone(),
                        media_type: "text/plain".to_owned(),
                        byte_length: 1,
                        relative_path: format!("{}/{}.txt", &digest[..2], digest),
                    },
                    uses: vec![ArtifactUse {
                        sequence: index + 1,
                        event: "completed".to_owned(),
                        call_index: None,
                        call_id: None,
                    }],
                }
            })
            .collect();
        let bundle = ReviewSubjectBundle {
            schema_version: REVIEW_SUBJECT_BUNDLE_SCHEMA_VERSION,
            task_id: TaskId::parse("task-oversized-bundle").expect("task id"),
            session_id: SessionId::parse("session-oversized-bundle").expect("session id"),
            task_digest: "a".repeat(64),
            composition_digest: "b".repeat(64),
            result_artifact: ArtifactRef {
                id: ArtifactId::parse(format!("art-{}", "c".repeat(64)))
                    .expect("result artifact id"),
                sha256: "c".repeat(64),
                media_type: "text/plain".to_owned(),
                byte_length: 1,
                relative_path: format!("cc/{}.txt", "c".repeat(64)),
            },
            result_uses: vec![ArtifactUse {
                sequence: 1,
                event: "completed".to_owned(),
                call_index: None,
                call_id: None,
            }],
            evidence,
        };
        let error = encode_review_subject_bundle(&bundle)
            .expect_err("producer must reject oversized metadata before put_artifact_text");
        assert_eq!(error.code, FailureCode::StorageFailed);
        assert_eq!(
            error
                .details
                .get("maximumBytes")
                .and_then(|value| value.as_str()),
            Some("131072")
        );
        assert!(
            error
                .details
                .get("encodedBytes")
                .and_then(|value| value.as_str())
                .and_then(|value| value.parse::<usize>().ok())
                .is_some_and(|value| value > MAX_REVIEW_SUBJECT_BUNDLE_BYTES)
        );
    }
}
