use crate::checkpoint_state::get_latest_checkpoint;
use serde_json::{Value, json};
pub fn resolved_settings(
    legacy: &maho_ext_api::CompactionSettings,
    resolved: Option<&maho_ext_api::ResolvedCompactionSettings>,
) -> Result<maho_core::compaction::settings::CompactionSettings, maho_ext_api::ExtensionFailure> {
    let mut settings = maho_core::compaction::settings::default_compaction_settings();
    settings.enabled = legacy.enabled;
    settings.reserve_tokens = i64::try_from(legacy.reserve_tokens)
        .map_err(|error| maho_ext_api::ExtensionFailure::new(error.to_string()))?;
    settings.keep_recent_tokens = i64::try_from(legacy.keep_recent_tokens)
        .map_err(|error| maho_ext_api::ExtensionFailure::new(error.to_string()))?;
    let Some(resolved) = resolved else {
        settings.speculative_enabled = Some(false);
        settings.idle_compaction_enabled = Some(false);
        settings.restoration_enabled = Some(false);
        settings.ideal.grace_band_enabled = Some(false);
        settings.ideal.tool_admission_enabled = Some(false);
        settings.ideal.reminder_enabled = Some(false);
        settings.ideal.reserve_scaling_enabled = Some(false);
        return Ok(settings);
    };
    settings.enabled = resolved.enabled;
    settings.reserve_tokens = i64::try_from(resolved.reserve_tokens)
        .map_err(|error| maho_ext_api::ExtensionFailure::new(error.to_string()))?;
    settings.keep_recent_tokens = i64::try_from(resolved.keep_recent_tokens)
        .map_err(|error| maho_ext_api::ExtensionFailure::new(error.to_string()))?;
    settings.speculative_enabled = Some(resolved.speculative_enabled);
    settings.speculative_fraction = Some(resolved.speculative_fraction);
    settings.speculative_cooldown_ms = Some(resolved.speculative_cooldown_ms);
    settings.restoration_enabled = Some(resolved.restoration_enabled);
    settings.restoration_max_items = Some(resolved.restoration_max_items);
    settings.restoration_max_tokens_per_item = Some(resolved.restoration_max_tokens_per_item);
    settings.restoration_max_total_tokens = Some(resolved.restoration_max_total_tokens);
    settings.restoration_context_ratio = Some(resolved.restoration_context_ratio);
    settings.idle_compaction_enabled = Some(resolved.idle_compaction_enabled);
    settings.summarization_max_duration_ms = resolved.summarization_max_duration_ms;
    settings.ideal = maho_core::compaction::ideal_settings::IdealCompactionSettings {
        grace_band_enabled: Some(resolved.grace_band_enabled),
        tool_admission_enabled: Some(resolved.tool_admission_enabled),
        reminder_enabled: Some(resolved.reminder_enabled),
        reserve_scaling_enabled: Some(resolved.reserve_scaling_enabled),
        speculative_lead_tokens: resolved.speculative_lead_tokens,
    };
    Ok(settings)
}
pub fn live_settings(
    context: &maho_ext_api::ExtensionContext,
) -> Result<maho_core::compaction::settings::CompactionSettings, maho_ext_api::ExtensionFailure> {
    let resolved = context.get_resolved_compaction_settings()?;
    resolved_settings(&context.get_compaction_settings()?, resolved.as_ref())
}
pub fn estimate_pending_prompt_tokens(prompt: Option<&str>, image_count: usize) -> usize {
    prompt
        .unwrap_or_default()
        .encode_utf16()
        .count()
        .div_ceil(4)
        + image_count * 1200
}
pub fn get_prompt_context_window(window: f64, max_tokens: Option<f64>) -> f64 {
    match max_tokens {
        Some(max) if max.is_finite() && max > 0.0 && window > 0.0 => {
            window - max.min((window * 0.5).floor())
        }
        _ => window,
    }
}
pub fn with_additional_tokens(usage: &Value, additional: f64) -> Value {
    let Some(tokens) = usage.get("tokens").and_then(Value::as_f64) else {
        return usage.clone();
    };
    if additional <= 0.0 {
        return usage.clone();
    }
    let mut result = usage.clone();
    result["tokens"] = json!(tokens + additional);
    if let Some(window) = usage
        .get("contextWindow")
        .and_then(Value::as_f64)
        .filter(|w| *w > 0.0)
    {
        result["percent"] = json!((tokens + additional) / window * 100.0);
    }
    result
}
pub fn is_monitorable_message(message: &Value) -> bool {
    message.get("content").is_some_and(Value::is_array)
}
pub fn is_aborted_assistant_message(message: &Value) -> bool {
    message.get("role").and_then(Value::as_str) == Some("assistant")
        && message.get("stopReason").and_then(Value::as_str) == Some("aborted")
}
pub fn is_required_compaction_fallback_reason(reason: &str) -> bool {
    matches!(reason, "manual" | "threshold" | "overflow")
}
pub fn recent_checkpoint(entries: &[Value], now: f64) -> Option<Value> {
    let checkpoint = get_latest_checkpoint(entries)?;
    let timestamp = checkpoint.get("timestamp").and_then(Value::as_f64)?;
    if timestamp != 0.0 && now - timestamp <= 60000.0 {
        Some(checkpoint)
    } else {
        None
    }
}
pub fn compaction_feedback(
    applied: bool,
    reason: &str,
    aborted: bool,
    remote_fallback: Option<&str>,
) -> Option<Value> {
    if applied || reason == "rejected" {
        return None;
    }
    let parts: Vec<_> = remote_fallback
        .into_iter()
        .chain(std::iter::once(reason))
        .filter(|p| !p.is_empty())
        .collect();
    let mut result = json!({"reason":"extension","aborted":aborted});
    if !aborted && !parts.is_empty() {
        result["errorMessage"] = json!(format!(
            "Compaction did not apply: {}",
            parts.join("; local fallback ")
        ));
    }
    Some(result)
}

pub fn end_compaction_feedback(
    context: &maho_ext_api::ExtensionContext,
    signal: Option<maho_ext_api::AbortSignal>,
    applied: bool,
    reason: &str,
    remote_fallback: Option<&str>,
) -> Result<(), maho_ext_api::ExtensionFailure> {
    let aborted = signal
        .as_ref()
        .is_some_and(maho_ext_api::AbortSignal::is_aborted);
    let Some(feedback) = compaction_feedback(applied, reason, aborted, remote_fallback) else {
        return Ok(());
    };
    context.end_compaction(maho_ext_api::EndCompactionOptions {
        reason: maho_ext_api::CompactionReason::Extension,
        signal,
        aborted: Some(aborted),
        error_message: feedback
            .get("errorMessage")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

pub fn create_blocking_remote_compaction_event(
    context: &maho_ext_api::ExtensionContext,
    preparation: maho_ext_api::CompactionPreparation,
    request_id: String,
    custom_instructions: String,
    signal: maho_ext_api::AbortSignal,
) -> maho_ext_api::SessionBeforeCompactEvent {
    maho_ext_api::SessionBeforeCompactEvent {
        reason: maho_ext_api::CompactionReason::Extension,
        will_retry: false,
        request_id,
        preparation,
        branch_entries: context.session_manager.get_branch(),
        custom_instructions: Some(custom_instructions),
        signal,
    }
}

pub struct AbortLink {
    source: maho_ai::utils::abort::AbortSignal,
    listener: maho_ai::utils::abort::ListenerId,
}
impl Drop for AbortLink {
    fn drop(&mut self) {
        self.source.remove_abort_listener(self.listener);
    }
}
pub fn link_abort_signal(
    source: Option<&maho_ai::utils::abort::AbortSignal>,
    target: &maho_ai::utils::abort::AbortController,
) -> Option<AbortLink> {
    let source = source?;
    if source.aborted() {
        target.abort(None);
        return None;
    }
    let target = target.clone();
    let listener = source.add_abort_listener(move |_| target.abort(None));
    Some(AbortLink {
        source: source.clone(),
        listener,
    })
}

pub fn create_live_blocking_remote_compaction_event(
    context: &maho_ext_api::ExtensionContext,
    preparation: maho_ext_api::CompactionPreparation,
    custom_instructions: String,
    signal: maho_ext_api::AbortSignal,
) -> maho_ext_api::SessionBeforeCompactEvent {
    create_blocking_remote_compaction_event(
        context,
        preparation,
        uuid::Uuid::new_v4().to_string(),
        custom_instructions,
        signal,
    )
}

pub fn summarization_tools(api: &maho_ext_api::ExtensionApi) -> Vec<maho_ai::types::Tool> {
    let Ok(definitions) = api.get_all_tools() else {
        return Vec::new();
    };
    let Ok(active) = api.get_active_tools() else {
        return Vec::new();
    };
    active
        .iter()
        .filter_map(|name| definitions.iter().find(|tool| &tool.name == name))
        .map(|tool| maho_ai::types::Tool {
            name: tool.name.clone(),
            description: tool.description.clone(),
            parameters: tool.parameters.clone(),
            output_schema: None,
            freeform: None,
            constrained_sampling: None,
        })
        .collect()
}

pub fn prepare_accepted_restoration(
    state: &mut crate::restoration_tracker::RestorationTrackerState,
    context: &maho_ext_api::ExtensionContext,
    event: &maho_ext_api::SessionCompactEvent,
    settings: &maho_core::compaction::settings::CompactionSettings,
) -> Result<(), maho_ext_api::ExtensionFailure> {
    let maho_ext_api::SessionCompactEvent::Accepted {
        reason,
        compaction_entry,
        ..
    } = event
    else {
        return Ok(());
    };
    if settings.restoration_enabled == Some(false) {
        return Ok(());
    }
    let entries = crate::speculative::branch_values(context);
    let first_kept = compaction_entry.data["firstKeptEntryId"].as_str();
    let kept: Vec<_> = entries
        .iter()
        .position(|entry| entry["id"].as_str() == first_kept)
        .map_or_else(Vec::new, |index| {
            entries[index..]
                .iter()
                .filter(|entry| entry["type"] == "message")
                .map(|entry| entry["message"].clone())
                .collect()
        });
    let usage = context.get_context_usage()?;
    let window = usage.as_ref().map_or_else(
        || {
            context
                .model
                .as_ref()
                .map_or(200000, |model| model.context_window)
        },
        |usage| usage.context_window,
    ) as f64;
    let restoration = crate::restoration_tracker::RestorationSettings {
        max_items: settings.restoration_max_items,
        max_tokens_per_item: settings.restoration_max_tokens_per_item,
        max_total_tokens: settings.restoration_max_total_tokens,
        context_ratio: settings.restoration_context_ratio,
    };
    let reason = match reason {
        maho_ext_api::CompactionReason::Manual => "manual",
        maho_ext_api::CompactionReason::Threshold => "threshold",
        maho_ext_api::CompactionReason::Overflow => "overflow",
        maho_ext_api::CompactionReason::Branch => "branch",
        maho_ext_api::CompactionReason::PrePrompt => "pre-prompt",
        maho_ext_api::CompactionReason::Extension => "extension",
    };
    crate::restoration_tracker::prepare_pending_payload(
        state,
        &crate::restoration_tracker::PreparePendingPayloadOptions {
            accepted: true,
            reason,
            compaction_entry_id: &compaction_entry.id,
            context_window: window,
            usage_tokens: usage
                .and_then(|usage| usage.tokens)
                .map(|tokens| tokens as f64),
            reserve_tokens: crate::policy::resolve_effective_reserve_tokens(
                window,
                settings.reserve_tokens as f64,
                settings.ideal.reserve_scaling_enabled,
            ),
            settings: &restoration,
            kept_messages: &kept,
        },
    );
    Ok(())
}

/// senpi `isAutomaticCompactionBlocked(ctx.sessionManager.getBranch(), ctx.model, ctx.getCompactionSettings())`.
pub fn automatic_compaction_blocked(context: &maho_ext_api::ExtensionContext) -> Result<bool, maho_ext_api::ExtensionFailure> {
    let settings = live_settings(context)?;
    Ok(crate::rejected_recovery::is_automatic_compaction_blocked(&crate::speculative::branch_values(context), context.model.as_ref(), &settings))
}

/// senpi `generateCompaction`: a successful summary is probed for a replayable suffix before it is
/// trusted, so a context fault is only ever latched by a summary that actually succeeded (#3061).
pub async fn generate_compaction(
    context: &maho_ext_api::ExtensionContext,
    snapshot: &crate::speculative::SpeculativeCompactionSnapshot,
    signal: Option<&maho_ai::utils::abort::AbortSignal>,
    on_progress: &(dyn Fn(&str) + Sync),
) -> Result<Option<maho_ext_api::CompactionResult>, crate::speculative::SummaryGenerationError> {
    use crate::{deterministic_fallback::{RecoveryMetadata, RequiredCompactionFallbackFailure, UnsafeRetainedSuffixError, create_required_compaction_fallback, DeterministicFallbackDiagnostic}, rejected_recovery::would_compaction_overflow};
    let compaction = crate::speculative::run_extension_compaction_for_context(context, snapshot, signal, on_progress).await?;
    let Some(summary) = compaction else { return Ok(None) };
    if signal.is_some_and(maho_ai::utils::abort::AbortSignal::aborted) {
        return Ok(Some(summary));
    }
    let (branch, model, settings) = (&snapshot.branch_entries, &snapshot.model, &snapshot.preparation.settings);
    if !would_compaction_overflow(branch, &summary, true, model, settings) {
        return Ok(Some(summary));
    }
    let empty = maho_ext_api::CompactionResult { summary: String::new(), ..summary.clone() };
    if !would_compaction_overflow(branch, &empty, true, model, settings) {
        return Ok(Some(summary));
    }
    // #3061: only a SUCCESSFUL summary can establish a terminal context fault. A failed summary plus
    // a rejected emergency checkpoint says nothing about whether a recovered provider could produce
    // a smaller, usable summary. Probe with zero summary text so marker/previous-summary overhead
    // alone cannot turn a provider failure or an oversized summary into a durable latch.
    let mut diagnostics = DeterministicFallbackDiagnostic::default();
    let viable = create_required_compaction_fallback(
        &snapshot.preparation, snapshot.context_window, RequiredCompactionFallbackFailure::UnsafeRetainedContent,
        RecoveryMetadata { task_intent: None, summary: Some("") }, branch, &mut diagnostics,
    );
    if viable.is_none() && matches!(diagnostics.rejection_reason, Some("unsafe-retained-content" | "atomic-tool-chain-cut" | "retained-token-budget-exceeded")) {
        return Err(crate::speculative::SummaryGenerationError::UnsafeRetainedSuffix(Box::new(UnsafeRetainedSuffixError { diagnostics })));
    }
    Ok(Some(summary))
}

/// One summary failure as the recovery path sees it, whether it was just thrown or inherited from a warm job.
pub struct RequiredFailure {
    pub kind: crate::deterministic_fallback::SummaryFailure,
    pub message: String,
    pub unsafe_diagnostics: Option<crate::deterministic_fallback::DeterministicFallbackDiagnostic>,
}

impl From<&crate::speculative::SummaryGenerationError> for RequiredFailure {
    fn from(error: &crate::speculative::SummaryGenerationError) -> Self {
        Self {
            kind: crate::speculative::summary_failure(error),
            message: error.to_string(),
            unsafe_diagnostics: match error {
                crate::speculative::SummaryGenerationError::UnsafeRetainedSuffix(unsafe_error) => Some(unsafe_error.diagnostics.clone()),
                _ => None,
            },
        }
    }
}

impl From<&crate::speculative_job::LiveSummaryFailure> for RequiredFailure {
    fn from(failure: &crate::speculative_job::LiveSummaryFailure) -> Self {
        Self { kind: failure.fallback, message: failure.message.clone(), unsafe_diagnostics: failure.unsafe_diagnostics.clone() }
    }
}

pub struct RequiredRecovery {
    pub compaction: Option<maho_ext_api::CompactionResult>,
    pub rejection_reason: Option<String>,
}

/// senpi `recoverRequiredCompaction`.
pub fn recover_required_compaction(
    api: &maho_ext_api::ExtensionApi,
    context: &maho_ext_api::ExtensionContext,
    snapshot: &crate::speculative::SpeculativeCompactionSnapshot,
    failure_kind: crate::deterministic_fallback::RequiredCompactionFallbackFailure,
    cause: &RequiredFailure,
) -> Result<RequiredRecovery, maho_ext_api::ExtensionFailure> {
    use crate::{deterministic_fallback as fallback, rejected_recovery as rejected};
    let terminal = cause.unsafe_diagnostics.is_some();
    let mut diagnostics = cause.unsafe_diagnostics.clone().unwrap_or_default();
    let intent = crate::task_intent::resolve_inherited_task_intent(&snapshot.branch_entries);
    let compaction = if terminal {
        None
    } else {
        fallback::create_required_compaction_fallback(
            &snapshot.preparation, snapshot.context_window, failure_kind,
            fallback::RecoveryMetadata { task_intent: intent.as_deref(), summary: None }, &snapshot.branch_entries, &mut diagnostics,
        )
    };
    let Some(compaction) = compaction else {
        let branch = &snapshot.branch_entries;
        let settings = live_settings(context)?;
        // Provider/timeout/abort/empty-summary failures never create or replace this record, even
        // when their opportunistic deterministic fallback fails.
        if terminal && !rejected::is_automatic_compaction_blocked(branch, Some(&snapshot.model), &settings) {
            let last = diagnostics.candidate_rejections.last();
            let entry_id = last.and_then(|candidate| candidate.get("unsafeEntryId").or_else(|| candidate.get("firstKeptEntryId"))).cloned();
            api.append_entry(rejected::REJECTED_RECOVERY_ENTRY, Some(serde_json::json!({
                "failureKind": "unsafe-retained-content",
                "stateKey": rejected::compaction_recovery_state_key(branch, Some(&snapshot.model), &settings),
                "reason": diagnostics.rejection_reason,
                "entryId": entry_id,
            })))?;
        }
        return Ok(RequiredRecovery { compaction: None, rejection_reason: Some(fallback::format_required_compaction_fallback_rejection(&diagnostics, terminal)) });
    };
    // The compaction itself succeeds, so nothing else tells the user their transcript was reduced
    // without a provider summary. Say it plainly here, and never with the internal retry-suppression marker (#1741).
    context.ui.notify(&fallback::format_required_compaction_fallback_notice(failure_kind, Some(&cause.message)), maho_ext_api::NotificationType::Warning);
    Ok(RequiredRecovery { compaction: Some(compaction), rejection_reason: None })
}

pub async fn generate_core_route_compaction(
    api: &maho_ext_api::ExtensionApi,
    context: &maho_ext_api::ExtensionContext,
    event: &maho_ext_api::SessionBeforeCompactEvent,
    inherited_warm_failure: Option<crate::speculative_job::LiveSummaryFailure>,
) -> Result<maho_ext_api::EventResult, maho_ext_api::ExtensionFailure> {
    use maho_ext_api::{EventResult, SessionBeforeEventResult};
    if event.signal.is_aborted() {
        return Ok(EventResult::None);
    }
    let emit = |data| {
        api.events
            .emit(crate::openai_remote::SENPI_COMPACTION_EVENT, &data)
    };
    let fallback = |model_id: Option<&str>, reason: &str| {
        let mut data = serde_json::json!({"version":1,"action":"remote_fallback","route":"builtin.compaction.openai_remote","requestId":event.request_id,"reason":reason});
        if let Some(id) = model_id {
            data["modelId"] = serde_json::json!(id);
        }
        emit(data);
    };
    if event.reason == maho_ext_api::CompactionReason::Branch
        || !crate::openai_remote_model::is_openai_remote_compaction_model(context.model.as_ref())
    {
        fallback(
            context.model.as_ref().map(|model| model.id.as_str()),
            if event.reason == maho_ext_api::CompactionReason::Branch {
                "branch-compaction"
            } else {
                "not-openai-responses"
            },
        );
    }
    let Some(model) = context.model.clone() else {
        return Ok(EventResult::None);
    };
    if event.reason != maho_ext_api::CompactionReason::Branch
        && crate::openai_remote_model::is_openai_remote_compaction_model(Some(&model))
    {
        'remote: {
            let auth = match context.model_registry.get_api_key_and_headers(&model).await {
                Ok(auth) => auth,
                Err(error) => {
                    fallback(Some(&model.id), &error.message);
                    break 'remote;
                }
            };
            if event.signal.is_aborted() {
                return Ok(EventResult::None);
            }
            let mut request_model = model.clone();
            if let Some(id) = auth.upstream_model_id.filter(|id| !id.is_empty()) {
                request_model.id = id;
            }
            if let Some(base_url) = auth.auth.base_url.filter(|base_url| !base_url.is_empty()) {
                request_model.base_url = base_url;
            }
            let service_tier = context
                .service_tier
                .map(|tier| match tier {
                    maho_ext_api::ServiceTier::Auto => "auto",
                    maho_ext_api::ServiceTier::Flex => "flex",
                    maho_ext_api::ServiceTier::Priority => "priority",
                    maho_ext_api::ServiceTier::Ultrafast => "ultrafast",
                })
                .or_else(|| {
                    auth.service_tier.map(|tier| match tier {
                        maho_ai::types::ServiceTierPreference::Auto => "auto",
                        maho_ai::types::ServiceTierPreference::Flex => "flex",
                        maho_ai::types::ServiceTierPreference::Priority => "priority",
                        maho_ai::types::ServiceTierPreference::Ultrafast => "ultrafast",
                    })
                });
            let key = auth.auth.api_key;
            let remote_deadline = crate::openai_remote_timeout::RemoteCompactionDeadline::new(
                std::time::Duration::from_millis(
                    crate::openai_remote::openai_remote_compaction_timeout_ms(
                        &model,
                        event.preparation.tokens_before,
                    ),
                ),
            );
            let branch = crate::speculative::branch_values(context);
            let messages =
                maho_core::session_manager::build_session_context(&branch, None).messages;
            let messages = messages
                .into_iter()
                .map(serde_json::from_value)
                .collect::<Result<Vec<maho_ext_api::AgentMessage>, _>>()
                .map_err(|error| maho_ext_api::ExtensionFailure::new(error.to_string()))?;
            let prepared = context.prepare_provider_request(messages).await?;
            let raw = prepared
                .messages
                .iter()
                .map(serde_json::to_value)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| maho_ext_api::ExtensionFailure::new(error.to_string()))?;
            let session_id = context.session_manager.session_id();
            let system_prompt = context.get_system_prompt();
            let Some(request) = crate::openai_remote::create_openai_remote_compaction_request(
                Some(&request_model),
                &system_prompt,
                &branch,
                Some(&raw),
                event.preparation.tokens_before,
                Some(session_id),
                service_tier,
            ) else {
                fallback(Some(&model.id), "empty-compaction-input");
                break 'remote;
            };
            {
                let configured = auth.auth.headers.unwrap_or_default();
                let transformed = (prepared.transform_headers)(configured).await?;
                let Some(headers) =
                    crate::openai_remote_model::create_live_openai_remote_compaction_headers(
                        &request_model,
                        key.as_deref(),
                        &transformed,
                        Some(session_id),
                    )
                    .map_err(|error| maho_ext_api::ExtensionFailure::new(error.to_string()))?
                else {
                    fallback(Some(&model.id), "missing-openai-auth");
                    break 'remote;
                };
                let Some(origin) = crate::openai_remote_model::openai_remote_compaction_origin(
                    &request_model,
                    &headers,
                ) else {
                    fallback(
                        Some(&request_model.id),
                        "missing-remote-replay-origin-provenance",
                    );
                    break 'remote;
                };
                {
                    let controller = maho_ai::utils::abort::AbortController::new();
                    let signal = controller.signal();
                    let runner: crate::openai_remote_dependencies::OpenAiResponsesStreamRunner =
                        std::sync::Arc::new(|model, context, options| {
                            maho_ai::compat::stream_simple(model, context, Some(options.clone()))
                        });
                    let client = reqwest::Client::new();
                    let operation = crate::openai_remote::run_remote_compaction(crate::openai_remote::RemoteCompactionOptions {
                            model:&request_model,request:&request,request_id:&event.request_id,first_kept_entry_id:&event.preparation.first_kept_entry_id,
                            system_prompt:&system_prompt,session_id,api_key:key,headers,extra_body:auth.extra_body,origin:serde_json::json!({"endpoint":origin.endpoint,"trustDomain":origin.trust_domain,"authTenantFingerprint":origin.auth_tenant_fingerprint}),
                            signal:&signal,timeout:remote_deadline.next_attempt(),deadline:remote_deadline.clone(),now_ms:chrono::Utc::now().timestamp_millis() as u64,client:&client,runner:&runner,provider_request:Some(&prepared),
                        },&emit);
                    tokio::pin!(operation);
                    let remote = tokio::select! {
                        biased;
                        () = event.signal.cancelled() => {
                            controller.abort(None);
                            operation.await.map_err(maho_ext_api::ExtensionFailure::new)?;
                            return Ok(EventResult::None);
                        }
                        result = &mut operation => result,
                    };
                    if event.signal.is_aborted() {
                        return Ok(EventResult::None);
                    }
                    match remote {
                        Ok(Some(compaction)) => {
                            return Ok(EventResult::SessionBefore(SessionBeforeEventResult {
                                compaction: Some(compaction),
                                ..Default::default()
                            }));
                        }
                        Ok(None) => {}
                        Err(error) => return Err(maho_ext_api::ExtensionFailure::new(error)),
                    }
                }
            }
        }
    }
    let convert = |messages: &[maho_ext_api::AgentMessage]| {
        messages
            .iter()
            .map(serde_json::to_value)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| maho_ext_api::ExtensionFailure::new(error.to_string()))
    };
    let messages = convert(&event.preparation.messages_to_summarize)?;
    let prefix = convert(&event.preparation.turn_prefix_messages)?;
    let resolved = context.get_resolved_compaction_settings()?;
    let settings = resolved_settings(&event.preparation.settings, resolved.as_ref())?;
    let preparation = maho_core::compaction::compaction::CompactionPreparation {
        first_kept_entry_id: event.preparation.first_kept_entry_id.clone(),
        source_messages: messages.clone(),
        messages_to_summarize: messages,
        turn_prefix_source_messages: prefix.clone(),
        is_split_turn: !prefix.is_empty(),
        turn_prefix_messages: prefix,
        tokens_before: event.preparation.tokens_before as i64,
        previous_summary: event.preparation.previous_summary.clone(),
        file_ops: Default::default(),
        settings,
    };
    let reason = match event.reason {
        maho_ext_api::CompactionReason::Manual => "manual",
        maho_ext_api::CompactionReason::Threshold => "threshold",
        maho_ext_api::CompactionReason::Overflow => "overflow",
        maho_ext_api::CompactionReason::Branch => "branch",
        _ => "extension",
    };
    let usage = context.get_context_usage()?;
    // senpi requiresDeterministicCompactionFallback: a pre-prompt compaction at the reserve boundary
    // must land too, or the prompt it admits overflows.
    let required_fallback = is_required_compaction_fallback_reason(reason)
        || (event.reason == maho_ext_api::CompactionReason::PrePrompt
            && usage.as_ref().is_some_and(|usage| {
                usage.context_window > 0
                    && usage.tokens.is_some_and(|tokens| {
                        tokens as f64
                            >= usage.context_window as f64
                                - maho_core::compaction::compaction::resolve_effective_reserve_tokens(
                                    usage.context_window as f64,
                                    preparation.settings.reserve_tokens as f64,
                                    preparation.settings.ideal.reserve_scaling_enabled,
                                )
                    })
            }));
    let snapshot = crate::speculative::SpeculativeCompactionSnapshot {
        generation: 0,
        expected_revision: context.get_message_revision()?,
        context_window: usage.map_or(model.context_window, |usage| usage.context_window),
        model,
        prompt_variant: crate::speculative::get_prompt_variant(reason, &preparation),
        preparation,
        branch_entries: crate::speculative::branch_values(context),
        custom_instructions: event.custom_instructions.clone(),
        system_prompt: Some(context.get_system_prompt()),
        tools: summarization_tools(api),
        origin: Some("core-route".into()),
    };
    let result = if let Some(inherited) = inherited_warm_failure.filter(|failure| required_fallback && crate::deterministic_fallback::classify_required_compaction_fallback_failure(failure.fallback, &failure.message).is_some()) {
        // senpi inheritedWarmFailure: reuse the warm job's terminal failure for the deterministic
        // fallback instead of paying for a second summary request.
        Err(RequiredFailure::from(&inherited))
    } else {
        let controller = maho_ai::utils::abort::AbortController::new();
        let signal = controller.signal();
        let progress = |delta: &str| {
            let _ = context.update_compaction(maho_ext_api::UpdateCompactionOptions {
                reason: event.reason,
                signal: Some(event.signal.clone()),
                delta: Some(delta.into()),
                text: None,
            });
        };
        let operation = generate_compaction(context, &snapshot, Some(&signal), &progress);
        tokio::pin!(operation);
        let generated = tokio::select! {
            biased;
            () = event.signal.cancelled() => {
                controller.abort(None);
                operation.await.map_err(|error| maho_ext_api::ExtensionFailure::new(error.to_string()))?;
                return Ok(EventResult::None);
            }
            result = &mut operation => result,
        };
        generated.map_err(|error| RequiredFailure::from(&error))
    };
    if event.signal.is_aborted() {
        return Ok(EventResult::None);
    }
    match result {
        Ok(Some(compaction)) => Ok(EventResult::SessionBefore(SessionBeforeEventResult {
            compaction: Some(compaction),
            ..Default::default()
        })),
        Ok(None) => Ok(EventResult::SessionBefore(SessionBeforeEventResult {
            cancel: Some(true),
            reason: Some("compaction generator returned no summary".into()),
            ..Default::default()
        })),
        Err(error) => {
            let failure = crate::deterministic_fallback::classify_required_compaction_fallback_failure(error.kind, &error.message);
            // senpi: a terminal unsafe-retained fault recovers (and latches) even when the reason
            // alone would not require a deterministic fallback.
            if let Some(failure) = failure.filter(|failure| required_fallback || *failure == crate::deterministic_fallback::RequiredCompactionFallbackFailure::UnsafeRetainedContent) {
                let recovery = recover_required_compaction(api, context, &snapshot, failure, &error)?;
                return Ok(match recovery.compaction {
                    Some(compaction) => EventResult::SessionBefore(SessionBeforeEventResult { compaction: Some(compaction), ..Default::default() }),
                    None => EventResult::SessionBefore(SessionBeforeEventResult { cancel: Some(true), reason: recovery.rejection_reason, ..Default::default() }),
                });
            }
            Ok(EventResult::SessionBefore(SessionBeforeEventResult {
                cancel: Some(true),
                reason: Some(error.message.replace("senpi:no-turn-retry:", "")),
                ..Default::default()
            }))
        }
    }
}

/// senpi `applyBlockingCompaction` (minus the rejected-recovery and OpenAI-remote routes): honors the
/// circuit breaker, consumes a pending warm speculative job before regenerating, and debits the
/// breaker on transient summarization failures.
pub async fn apply_live_blocking_compaction(
    api: &maho_ext_api::ExtensionApi,
    context: &maho_ext_api::ExtensionContext,
    generation: &std::sync::atomic::AtomicU64,
    warm: &std::sync::Mutex<Option<crate::speculative_job::LiveSpeculativeJob>>,
    state: &std::sync::Mutex<crate::state::CompactionExtensionState>,
    instructions: String,
) -> Result<crate::speculative::SpeculativeCompactionResult, maho_ext_api::ExtensionFailure> {
    use crate::speculative::SpeculativeCompactionResult as Outcome;
    use std::sync::{PoisonError, atomic::Ordering};
    if automatic_compaction_blocked(context)? {
        return Ok(Outcome::Rejected);
    }
    if context
        .model
        .as_ref()
        .is_some_and(|model| matches!(model.provider.as_str(), "cursor" | "cursor-cli-oauth"))
        && !context.is_idle()
    {
        return Ok(Outcome::Rejected);
    }
    let now = || chrono::Utc::now().timestamp_millis() as f64;
    {
        let state = state.lock().unwrap_or_else(PoisonError::into_inner);
        if crate::circuit_breaker::is_tripped(&state, now())
            || crate::per_turn_cap::should_reject_by_cap(&state).cancel
        {
            return Ok(Outcome::Rejected);
        }
    }
    let record_failure = || {
        let mut state = state.lock().unwrap_or_else(PoisonError::into_inner);
        let previous = std::mem::take(&mut *state);
        *state = crate::circuit_breaker::record_failure(
            previous,
            now(),
            Some(maho_ext_api::CompactionReason::Extension),
            None,
        );
    };
    let mut signal = context.begin_compaction(maho_ext_api::BeginCompactionOptions {
        reason: maho_ext_api::CompactionReason::Extension,
    })?;
    // Upstream awaits the pending warm job and applies it instead of regenerating (`warm_consumed`).
    let pending = warm.lock().unwrap_or_else(PoisonError::into_inner).clone();
    if let Some(job) = pending {
        let settlement = {
            let cancelled = async {
                match &signal {
                    Some(signal) => signal.cancelled().await,
                    None => std::future::pending::<()>().await,
                }
            };
            tokio::pin!(cancelled);
            let settled = job.settled();
            tokio::pin!(settled);
            tokio::select! {
                biased;
                settlement = &mut settled => settlement,
                () = &mut cancelled => {
                    job.controller.abort(None);
                    settled.await
                }
            }
        };
        let current = generation.load(Ordering::SeqCst);
        if let Some(failure) = settlement.error {
            warm.lock().unwrap_or_else(PoisonError::into_inner).take();
            if crate::transient_failure::is_transient_summarization_failure(
                failure.classification,
                &failure.message,
            ) {
                end_compaction_feedback(
                    context,
                    signal,
                    false,
                    "failed",
                    Some(&failure.message.replace("senpi:no-turn-retry:", "")),
                )?;
                record_failure();
                return Ok(Outcome::Failed);
            }
        } else {
            let result = crate::speculative::apply_generated_compaction(
                context,
                Some(&job.snapshot),
                current,
                settlement.result,
                signal.clone(),
            )
            .await?;
            warm.lock().unwrap_or_else(PoisonError::into_inner).take();
            if result == Outcome::Applied {
                end_compaction_feedback(context, signal, true, "applied", None)?;
                return Ok(result);
            }
            if result == Outcome::Rejected {
                signal = context.begin_compaction(maho_ext_api::BeginCompactionOptions {
                    reason: maho_ext_api::CompactionReason::Extension,
                })?;
            }
        }
    }
    let generation = generation.fetch_add(1, Ordering::SeqCst) + 1;
    let Some(mut snapshot) = crate::speculative::create_speculative_compaction_snapshot(
        context,
        generation,
        Some(instructions),
        summarization_tools(api),
    )?
    else {
        end_compaction_feedback(context, signal, false, "unavailable", None)?;
        return Ok(Outcome::Unavailable);
    };
    snapshot.origin = Some("blocking".into());
    let controller = maho_ai::utils::abort::AbortController::new();
    let ai_signal = controller.signal();
    let generated = {
        let progress = |delta: &str| {
            let _ = context.update_compaction(maho_ext_api::UpdateCompactionOptions {
                reason: maho_ext_api::CompactionReason::Extension,
                signal: signal.clone(),
                delta: Some(delta.into()),
                text: None,
            });
        };
        let cancelled = async {
            match &signal {
                Some(signal) => signal.cancelled().await,
                None => std::future::pending::<()>().await,
            }
        };
        let operation = generate_compaction(context,&snapshot,Some(&ai_signal),&progress);
        tokio::pin!(operation);
        tokio::select! {
            biased;
            () = cancelled => {controller.abort(None);operation.await}
            result = &mut operation => result,
        }
    };
    let aborted = signal.as_ref().is_some_and(maho_ext_api::AbortSignal::is_aborted);
    // senpi applyBlockingCompaction: a required-fallback failure recovers through the deterministic checkpoint.
    let generated = match generated {
        Err(error) if !aborted => {
            let cause = RequiredFailure::from(&error);
            match crate::deterministic_fallback::classify_required_compaction_fallback_failure(cause.kind, &cause.message) {
                Some(failure_kind) => {
                    let recovery = recover_required_compaction(api, context, &snapshot, failure_kind, &cause)?;
                    if recovery.compaction.is_none() {
                        end_compaction_feedback(context, signal, false, "failed", recovery.rejection_reason.as_deref())?;
                        return Ok(crate::speculative::SpeculativeCompactionResult::Failed);
                    }
                    Ok(recovery.compaction)
                }
                None => Err(error),
            }
        }
        other => other,
    };
    match generated {
        Ok(compaction) => {
            let result = crate::speculative::apply_generated_compaction(
                context,
                Some(&snapshot),
                generation,
                compaction,
                signal.clone(),
            )
            .await?;
            let reason = match result {
                Outcome::Applied => "applied",
                Outcome::Stale => "stale",
                Outcome::Rejected => "rejected",
                Outcome::Unavailable => "unavailable",
                Outcome::Failed => "failed",
            };
            end_compaction_feedback(context, signal, result == Outcome::Applied, reason, None)?;
            Ok(result)
        }
        Err(error) => {
            end_compaction_feedback(
                context,
                signal,
                false,
                "failed",
                Some(&error.to_string().replace("senpi:no-turn-retry:", "")),
            )?;
            let failure = crate::speculative_job::LiveSummaryFailure::from(error);
            if crate::transient_failure::is_transient_summarization_failure(
                failure.classification,
                &failure.message,
            ) {
                record_failure();
            }
            Ok(Outcome::Failed)
        }
    }
}
