use maho_core::compaction::{compaction::CompactionPreparation, warm_anchor::{create_warm_anchor_snapshot, is_warm_summary_anchor_valid}};
use maho_ext_api::{ApplyCompactionOptions, ApplyCompactionResult, CompactionReason, CompactionResult, ExtensionContext, ExtensionFailure};
use serde_json::{Value, json};
use crate::prompts::PromptVariant;

#[derive(Clone, Debug)]
pub struct SpeculativeCompactionSnapshot {
    pub generation: u64,
    pub expected_revision: u64,
    pub model: maho_ai::types::Model,
    pub context_window: u64,
    pub preparation: CompactionPreparation,
    pub branch_entries: Vec<Value>,
    pub prompt_variant: PromptVariant,
    pub custom_instructions: Option<String>,
    pub system_prompt: Option<String>,
    pub tools: Vec<maho_ai::types::Tool>,
    pub origin: Option<String>,
}

pub fn get_prompt_variant(reason: &str, preparation: &CompactionPreparation) -> PromptVariant {
    if reason == "branch" { PromptVariant::Branch }
    else if preparation.previous_summary.as_ref().is_some_and(|summary| !summary.is_empty()) { PromptVariant::Update }
    else if preparation.is_split_turn { PromptVariant::TurnPrefix }
    else { PromptVariant::Default }
}

pub fn branch_values(context: &ExtensionContext) -> Vec<Value> {
    context.session_manager.get_branch().into_iter().map(|entry| {
        let mut data = entry.data;
        data["id"] = json!(entry.id);
        data["parentId"] = json!(entry.parent_id);
        data["timestamp"] = json!(entry.timestamp);
        data["type"] = json!(entry.kind);
        data
    }).collect()
}

pub fn create_speculative_compaction_snapshot(
    context: &ExtensionContext,
    generation: u64,
    custom_instructions: Option<String>,
    tools: Vec<maho_ai::types::Tool>,
) -> Result<Option<SpeculativeCompactionSnapshot>, ExtensionFailure> {
    let Some(model) = context.model.clone() else { return Ok(None); };
    let expected_revision = context.get_message_revision()?;
    let branch_entries = branch_values(context);
    let context_window = context.get_context_usage()?.map_or(model.context_window, |usage| usage.context_window);
    let live_settings = context.get_compaction_settings()?;
    let mut settings = maho_core::compaction::settings::default_compaction_settings();
    settings.enabled = live_settings.enabled;
    settings.reserve_tokens = i64::try_from(live_settings.reserve_tokens).expect("reserve tokens fit native compaction settings");
    settings.keep_recent_tokens = crate::policy::compute_effective_keep_recent_tokens(
        live_settings.keep_recent_tokens as f64, context_window as f64,
        crate::policy::compute_effective_threshold(context_window as f64, None), 0.05,
    ) as i64;
    let Some(preparation) = maho_core::compaction::compaction::prepare_compaction(&branch_entries, &settings, false, false) else { return Ok(None); };
    let prompt_variant = get_prompt_variant("extension", &preparation);
    Ok(Some(SpeculativeCompactionSnapshot { generation, expected_revision, model,
        context_window, preparation, branch_entries, prompt_variant, custom_instructions,
        system_prompt: Some(context.get_system_prompt()), tools, origin: Some("speculative".into()) }))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpeculativeCompactionResult { Applied, Stale, Rejected, Unavailable, Failed }

#[derive(Debug)]
pub enum SummaryGenerationError {
    Auth(String),
    EmptySummary(String),
    Request(Box<maho_ai::types::AssistantMessage>),
    Stream(crate::speculative_summary::SummaryStreamError),
    Overflow(crate::overflow_retry::SummarizationOverflowExhaustedError),
    TotalBudget,
}

impl std::fmt::Display for SummaryGenerationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Auth(message) | Self::EmptySummary(message) => formatter.write_str(message),
            Self::Request(response) => formatter.write_str(response.error_message.as_deref().unwrap_or("Compaction summary request failed")),
            Self::Stream(error) => write!(formatter, "{error:?}"),
            Self::Overflow(error) => write!(formatter, "{error:?}"),
            Self::TotalBudget => formatter.write_str("summarization total budget exhausted"),
        }
    }
}

pub fn summary_request_failure(response: &maho_ai::types::AssistantMessage) -> crate::deterministic_fallback::SummaryFailure {
    let refused = matches!(response.stop_details, Some(maho_ai::types::AssistantStopDetails::Refusal { .. } | maho_ai::types::AssistantStopDetails::Sensitive));
    let message = response.error_message.as_deref().unwrap_or_default();
    let truncated = !refused && message.match_indices("upstream_stream_truncated").any(|(index, marker)| {
        let word = |character: char| character.is_ascii_alphanumeric() || character == '_';
        message[..index].chars().next_back().is_none_or(|character| !word(character))
            && message[index + marker.len()..].chars().next().is_none_or(|character| !word(character))
    });
    crate::deterministic_fallback::SummaryFailure::Request { transient: truncated || maho_ai::utils::retry::is_retryable_assistant_error(response), refused, truncated }
}

pub async fn run_extension_compaction(
    snapshot: &SpeculativeCompactionSnapshot,
    api_key: Option<String>,
    headers: Option<maho_ai::types::ProviderHeaders>,
    signal: Option<&maho_ai::utils::abort::AbortSignal>,
    stream_runner: Option<&crate::speculative_summary::SummaryStreamRunner>,
    on_progress: &dyn Fn(&str),
) -> Result<Option<CompactionResult>, SummaryGenerationError> {
    use maho_ai::types::StopReason;
    use crate::{overflow_retry as overflow, speculative_summary as summary};
    if signal.is_some_and(maho_ai::utils::abort::AbortSignal::aborted) { return Ok(None); }
    if api_key.as_ref().is_none_or(|key| key.is_empty()) && headers.as_ref().is_none_or(|headers| !headers.iter().any(|(key, value)| value.as_ref().is_some_and(|value| !value.is_empty()) && matches!(key.to_ascii_lowercase().as_str(), "authorization" | "x-api-key" | "api-key"))) {
        return Err(SummaryGenerationError::Auth(format!("summarization credentials unavailable: no credentials resolved for provider \"{}\"", snapshot.model.provider)));
    }
    let inherited_intent = crate::task_intent::resolve_inherited_task_intent(&snapshot.branch_entries);
    let prompt = crate::prompts::build_prompt(&crate::prompts::PromptOptions { variant: snapshot.prompt_variant, previous_summary: snapshot.preparation.previous_summary.as_deref(), task_intent: inherited_intent.as_deref(), prompt_family: None, custom_instructions: snapshot.custom_instructions.as_deref() });
    let prompt_tokens = prompt.user.encode_utf16().count().div_ceil(4) as u64;
    let source: Vec<_> = snapshot.preparation.messages_to_summarize.iter().chain(&snapshot.preparation.turn_prefix_messages).cloned().collect();
    let mut messages = overflow::bound_summarization_input(&crate::emergency_prune::prune_tool_results(&source, snapshot.context_window, 0.6), snapshot.context_window, prompt_tokens);
    let started = tokio::time::Instant::now();
    let total_ms = maho_core::compaction::stream_watchdog::summarization_total_budget_ms(snapshot.preparation.settings.summarization_max_duration_ms);
    let mut overflow_attempts = 0;
    let mut tool_retry_spent = false;
    let mut reasoning_retry_spent = false;
    loop {
        if signal.is_some_and(maho_ai::utils::abort::AbortSignal::aborted) { return Ok(None); }
        let remaining = total_ms - started.elapsed().as_secs_f64() * 1000.;
        if remaining <= 0. { return Err(SummaryGenerationError::TotalBudget); }
        let attempt_ms = maho_core::compaction::stream_watchdog::summarization_max_duration_ms(overflow::estimate_total_tokens(&messages) as f64, snapshot.preparation.settings.summarization_max_duration_ms).min(remaining);
        let retry_started = tokio::time::Instant::now();
        let retry_eligible = matches!(snapshot.origin.as_deref(), Some("blocking" | "core-route"));
        let response = maho_ai::utils::retry::retry_transient_call(
            || async {
                let remaining = total_ms - started.elapsed().as_secs_f64() * 1000.;
                if remaining <= 0. { return Err(SummaryGenerationError::TotalBudget); }
                let response = summary::generate_summary_message(summary::SummaryRequestOptions { snapshot, messages: &messages, prompt: &prompt, api_key: api_key.clone(), headers: headers.clone(), extra_body: None, signal, max_duration: std::time::Duration::from_secs_f64(attempt_ms.min(remaining) / 1000.), omit_reasoning_options: reasoning_retry_spent, forbid_tool_calls: tool_retry_spent, stream_runner }, on_progress).await.map_err(SummaryGenerationError::Stream)?;
                if let Some(response) = &response && response.stop_reason == StopReason::Error && !maho_ai::utils::overflow::is_context_overflow(response, Some(snapshot.context_window)) {
                    return Err(SummaryGenerationError::Request(Box::new(response.clone())));
                }
                Ok(response)
            },
            |error| retry_eligible && started.elapsed().as_secs_f64() * 1000. < total_ms
                && crate::summarization_retry::allow_summarization_retry(retry_started.elapsed().as_secs_f64() * 1000., Some(attempt_ms))
                && match error {
                    SummaryGenerationError::Request(response) => !response.error_message.as_deref().is_some_and(|message|message.starts_with("senpi:no-turn-retry:"))
                        && matches!(summary_request_failure(response), crate::deterministic_fallback::SummaryFailure::Request { transient: true, truncated: false, .. }),
                    SummaryGenerationError::Stream(summary::SummaryStreamError::Provider(error)) => maho_ai::utils::retry::is_retryable_error_message(&error.to_string()),
                    _ => false,
                },
            Some(&crate::summarization_retry::DEFAULT_SUMMARIZATION_RETRY_POLICY), signal, None,
        ).await;
        if signal.is_some_and(maho_ai::utils::abort::AbortSignal::aborted) { return Ok(None); }
        let response = response?;
        let Some(response) = response else { return Ok(None); };
        if maho_ai::utils::overflow::is_context_overflow(&response, Some(snapshot.context_window)) {
            overflow_attempts += 1;
            let elapsed_ms = started.elapsed().as_secs_f64() * 1000.;
            let retry = overflow::allow_overflow_retry(overflow_attempts, elapsed_ms).then(|| overflow::shrink_summarization_input_for_overflow_retry(&messages, snapshot.context_window, prompt_tokens)).flatten();
            let Some(retry) = retry else { return Err(SummaryGenerationError::Overflow(overflow::SummarizationOverflowExhaustedError { attempts: overflow_attempts, elapsed_ms })); };
            messages = retry; continue;
        }
        if response.stop_reason == StopReason::Aborted { return Ok(None); }
        if response.stop_reason == StopReason::Error { return Err(SummaryGenerationError::Request(Box::new(response))); }
        let serialized = serde_json::to_value(&response).expect("assistant messages serialize");
        let text = summary::get_summary_text(&serialized);
        if text.is_empty() {
            if response.stop_reason == StopReason::ToolUse && !snapshot.tools.is_empty() && !tool_retry_spent { tool_retry_spent = true; continue; }
            if response.stop_reason == StopReason::Stop && summary::has_summarization_reasoning_override(&snapshot.model) && !reasoning_retry_spent { reasoning_retry_spent = true; continue; }
            return Err(SummaryGenerationError::EmptySummary(format!("summarization response contained no text (stopReason: {:?})", response.stop_reason)));
        }
        let parsed = crate::task_intent::extract_task_intent(&text);
        let structural = crate::r#yield::compute_structural_yield(snapshot.preparation.previous_summary.as_deref().unwrap_or_default(), &snapshot.preparation.messages_to_summarize, &snapshot.preparation.turn_prefix_messages, &parsed.summary_text, snapshot.preparation.tokens_before as f64);
        let mut details = json!({"schema":"senpi.compaction.summary.v1","promptVariant":match snapshot.prompt_variant { PromptVariant::Default=>"default",PromptVariant::Update=>"update",PromptVariant::Branch=>"branch",PromptVariant::TurnPrefix=>"turn_prefix" },"tokenEstimate":maho_core::compaction::compaction::estimate_context_tokens(&maho_core::messages::convert_to_llm(&messages)).tokens + text.encode_utf16().count().div_ceil(4) as u64,"structuralYield":{"savedTokens":structural.saved_tokens,"savingsRatio":structural.savings_ratio,"tokensBefore":structural.tokens_before}});
        if let Some(intent) = parsed.task_intent.or(inherited_intent) { details["taskIntent"] = json!(intent); }
        if let Some(origin) = &snapshot.origin { details["origin"] = json!(origin); }
        return Ok(Some(CompactionResult { summary: parsed.summary_text, first_kept_entry_id: snapshot.preparation.first_kept_entry_id.clone(), tokens_before: snapshot.preparation.tokens_before as u64, details: Some(details) }));
    }
}

pub async fn apply_generated_compaction(
    context: &ExtensionContext,
    snapshot: Option<&SpeculativeCompactionSnapshot>,
    current_generation: u64,
    compaction: Option<CompactionResult>,
    signal: Option<maho_ext_api::AbortSignal>,
) -> Result<SpeculativeCompactionResult, ExtensionFailure> {
    if context.model.as_ref().is_some_and(|model| matches!(model.provider.as_str(), "cursor" | "cursor-cli-oauth")) && !context.is_idle() {
        return Ok(SpeculativeCompactionResult::Rejected);
    }
    let (Some(snapshot), Some(compaction)) = (snapshot, compaction) else { return Ok(SpeculativeCompactionResult::Unavailable); };
    if snapshot.generation != current_generation { return Ok(SpeculativeCompactionResult::Stale); }
    let revision_unchanged = snapshot.expected_revision == context.get_message_revision()?;
    let warm_anchor = create_warm_anchor_snapshot(&snapshot.preparation.first_kept_entry_id, &snapshot.branch_entries);
    if !revision_unchanged && !warm_anchor.as_ref().is_some_and(|anchor| is_warm_summary_anchor_valid(anchor, &branch_values(context))) {
        return Ok(SpeculativeCompactionResult::Stale);
    }
    let options = ApplyCompactionOptions {
        reason: CompactionReason::Extension,
        expected_revision: revision_unchanged.then_some(snapshot.expected_revision),
        expected_warm_anchor: if revision_unchanged { None } else { warm_anchor.map(|anchor| maho_ext_api::WarmAnchorSnapshot {
            first_kept_entry_id: anchor.first_kept_entry_id,
            prefix_entry_ids: anchor.prefix_entry_ids,
            latest_compaction_entry_id: anchor.latest_compaction_entry_id,
        }) },
        signal,
    };
    Ok(match context.apply_compaction(compaction, options).await? {
        ApplyCompactionResult::Applied => SpeculativeCompactionResult::Applied,
        ApplyCompactionResult::Stale => SpeculativeCompactionResult::Stale,
        ApplyCompactionResult::Rejected => SpeculativeCompactionResult::Rejected,
    })
}
