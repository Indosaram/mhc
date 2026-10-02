#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequiredCompactionFallbackFailure {
    SummarizationTimeout,
    SummarizationProviderFailure,
    UpstreamStreamTruncated,
    SummarizationOverflowExhausted,
    SummarizationEmptySummary,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SummaryFailure {
    Timeout,
    CredentialFailover,
    Request { transient: bool, refused: bool, truncated: bool },
    OverflowExhausted,
    EmptySummary,
    Other,
}

pub fn classify_required_compaction_fallback_failure(error: SummaryFailure, message: &str) -> Option<RequiredCompactionFallbackFailure> {
    match error {
        SummaryFailure::Timeout => Some(RequiredCompactionFallbackFailure::SummarizationTimeout),
        SummaryFailure::Request { transient: true, truncated: true, .. } => Some(RequiredCompactionFallbackFailure::UpstreamStreamTruncated),
        SummaryFailure::OverflowExhausted => Some(RequiredCompactionFallbackFailure::SummarizationOverflowExhausted),
        SummaryFailure::EmptySummary => Some(RequiredCompactionFallbackFailure::SummarizationEmptySummary),
        SummaryFailure::CredentialFailover | SummaryFailure::Request { transient: false, refused: false, truncated: false } => Some(RequiredCompactionFallbackFailure::SummarizationProviderFailure),
        _ if message.starts_with("senpi:no-turn-retry:") => Some(RequiredCompactionFallbackFailure::SummarizationProviderFailure),
        _ => None,
    }
}

pub fn format_required_compaction_fallback_notice(failure: RequiredCompactionFallbackFailure, cause: Option<&str>) -> String {
    let cause_text = match failure {
        RequiredCompactionFallbackFailure::SummarizationTimeout => "the summary stream ran out of its time budget",
        RequiredCompactionFallbackFailure::SummarizationProviderFailure => "the provider ended the summary stream with an error",
        RequiredCompactionFallbackFailure::UpstreamStreamTruncated => "the provider truncated the summary stream",
        RequiredCompactionFallbackFailure::SummarizationOverflowExhausted => "the summary input stayed over the provider's context limit",
        RequiredCompactionFallbackFailure::SummarizationEmptySummary => "the provider returned no summary text",
    };
    let mut parts = vec![format!("Compaction could not complete a provider summary: {cause_text}."), "A deterministic checkpoint was applied and older transcript detail was dropped, so it is safe to continue.".into()];
    if let Some(detail) = cause.map(|cause| cause.strip_prefix("senpi:no-turn-retry:").unwrap_or(cause).trim()).filter(|cause| !cause.is_empty()) {
        parts.push(format!("Provider reported: {}.", crate::task_intent::cap_utf8_bytes(detail, 512)));
    }
    parts.push("Run /compact on a different model for a richer summary.".into());
    parts.join(" ")
}

fn failure_name(failure: RequiredCompactionFallbackFailure) -> &'static str {
    match failure {
        RequiredCompactionFallbackFailure::SummarizationTimeout => "summarization-timeout",
        RequiredCompactionFallbackFailure::SummarizationProviderFailure => "summarization-provider-failure",
        RequiredCompactionFallbackFailure::UpstreamStreamTruncated => "upstream-stream-truncated",
        RequiredCompactionFallbackFailure::SummarizationOverflowExhausted => "summarization-overflow-exhausted",
        RequiredCompactionFallbackFailure::SummarizationEmptySummary => "summarization-empty-summary",
    }
}

fn bounded_value(value: &serde_json::Value, depth: usize) -> bool {
    if depth > 32 { return false; }
    match value {
        serde_json::Value::Array(items) => items.iter().all(|item| bounded_value(item, depth + 1)),
        serde_json::Value::Object(items) => items.values().all(|item| bounded_value(item, depth + 1)),
        _ => true,
    }
}

fn complete_tool_chains(messages: &[serde_json::Value]) -> bool {
    use std::collections::HashMap;
    let mut calls = HashMap::new();
    let mut results = HashMap::new();
    for (index, message) in messages.iter().enumerate() {
        if message["role"] == "assistant" {
            for block in message["content"].as_array().into_iter().flatten() {
                if block["type"] == "toolCall" && let Some(id) = block["id"].as_str()
                    && calls.insert(id, (index, block["incomplete"] == true)).is_some() { return false; }
            }
        } else if message["role"] == "toolResult" && let Some(id) = message["toolCallId"].as_str()
            && results.insert(id, index).is_some() { return false; }
    }
    calls.len() == results.len() && calls.iter().all(|(id, (index, incomplete))| !incomplete && results.get(id).is_some_and(|result| result > index))
}

#[derive(Clone, Debug, Default)]
pub struct DeterministicFallbackDiagnostic {
    pub rejection_reason: Option<&'static str>,
    pub candidates_checked: usize,
    pub budget_exceeded: bool,
    pub context_window: u64,
    pub reserve_tokens: u64,
    pub budget_tokens: u64,
    pub candidate_rejections: Vec<serde_json::Value>,
}

pub fn create_required_compaction_fallback(
    preparation: &maho_core::compaction::compaction::CompactionPreparation,
    context_window: u64,
    failure: RequiredCompactionFallbackFailure,
    task_intent: Option<&str>,
    branch_entries: &[serde_json::Value],
    diagnostics: &mut DeterministicFallbackDiagnostic,
) -> Option<maho_ext_api::CompactionResult> {
    use serde_json::json;
    use maho_core::{compaction::compaction::estimate_tokens, session_manager::{build_context_entries, session_entry_to_context_messages}};
    let reserve = crate::policy::resolve_effective_reserve_tokens(context_window as f64, preparation.settings.reserve_tokens as f64, preparation.settings.ideal.reserve_scaling_enabled) as u64;
    diagnostics.context_window = context_window;
    diagnostics.reserve_tokens = reserve;
    diagnostics.budget_tokens = context_window.saturating_sub(reserve);
    let Some(prepared_index) = branch_entries.iter().position(|entry| entry["id"].as_str() == Some(&preparation.first_kept_entry_id)) else {
        diagnostics.rejection_reason = Some("missing-preparation-boundary"); return None;
    };
    let mut summary = "[Deterministic compaction recovery checkpoint]\nGenerated summarization did not complete, so older context was reduced without another provider request.\nContinue from the retained messages after this checkpoint. Treat omitted transcript details as unknown.".to_owned();
    let intent = task_intent.map(str::trim).filter(|intent| !intent.is_empty());
    if let Some(intent) = intent { summary.push_str(&format!("\n\nTask intent:\n{intent}")); }
    if let Some(previous) = preparation.previous_summary.as_deref().map(str::trim).filter(|text| !text.is_empty()) {
        let prefix = format!("{summary}\n\nPrevious checkpoint:\n");
        let available = ((context_window as f64 * 0.4).floor() as usize).max(1024).saturating_sub(prefix.len());
        let marker = "\n[Older checkpoint truncated]";
        summary = if previous.len() <= available { format!("{prefix}{previous}") } else { format!("{prefix}{}{marker}", crate::task_intent::cap_utf8_bytes(previous, available.saturating_sub(marker.len()))) };
    }
    let mut candidates = vec![(prepared_index, "prepared")];
    for index in (0..prepared_index).rev() {
        if branch_entries[index]["type"] == "compaction" { break; }
        candidates.push((index, "earlier-safe-boundary"));
    }
    for index in (prepared_index + 1..branch_entries.len()).rev() {
        let entry = &branch_entries[index];
        if entry["type"] != "message" || entry["message"]["role"] != "user" { continue; }
        let content = &entry["message"]["content"];
        let meaningful = content.as_str().is_some_and(|text| !text.trim().is_empty()) || content.as_array().is_some_and(|blocks| blocks.iter().any(|block| block["type"] == "text" && block["text"].as_str().is_some_and(|text| !text.trim().is_empty())));
        if !meaningful { continue; }
        candidates.push((index, "latest-user-turn"));
        for earlier in (prepared_index + 1..index).rev() {
            if branch_entries[earlier]["type"] == "compaction" { break; }
            candidates.push((earlier, "earlier-safe-boundary"));
        }
        break;
    }
    for (index, retained_suffix) in candidates {
        diagnostics.candidates_checked += 1;
        let id = branch_entries[index]["id"].as_str().unwrap_or_default();
        let mut details = json!({"schema":"senpi.compaction.deterministic-fallback.v1","origin":"required-compaction-recovery","failureKind":failure_name(failure),"retainedSuffix":retained_suffix});
        if let Some(intent) = intent { details["taskIntent"] = json!(intent); }
        let preview = json!({"type":"compaction","id":"__senpi_deterministic_fallback_preview__","parentId":branch_entries.last().and_then(|entry| entry.get("id")),"timestamp":"1970-01-01T00:00:00.000Z","summary":summary,"firstKeptEntryId":id,"tokensBefore":preparation.tokens_before,"details":details,"fromHook":true});
        let mut entries = branch_entries.to_vec(); entries.push(preview);
        let context_entries = build_context_entries(&entries, None);
        let messages: Vec<_> = context_entries.iter().flat_map(session_entry_to_context_messages).collect();
        let retained: Vec<_> = messages.iter().skip(1).cloned().collect();
        let canonical = maho_core::messages::convert_to_llm(&retained);
        let summary_message = messages.first()?;
        let reason = if context_entries.get(1).and_then(|entry| entry["id"].as_str()) != Some(id) {
            Some("context-reconstruction-failed")
        } else if crate::retained_message_safety::has_unsafe_retained_content(&canonical) || !canonical.iter().all(|message| bounded_value(message, 0)) {
            Some("unsafe-retained-content")
        } else if !complete_tool_chains(&canonical) { Some("atomic-tool-chain-cut") } else {
            let tokens: u64 = canonical.iter().chain(std::iter::once(summary_message)).map(|message| estimate_tokens(message).max(estimate_tokens(&json!({"role":"user","content":message.to_string(),"timestamp":0})))).sum();
            if tokens > diagnostics.budget_tokens { diagnostics.budget_exceeded = true; Some("retained-token-budget-exceeded") } else { None }
        };
        if let Some(reason) = reason {
            diagnostics.rejection_reason = Some(reason);
            diagnostics.candidate_rejections.push(json!({"firstKeptEntryId":id,"rejectionReason":reason}));
            continue;
        }
        return Some(maho_ext_api::CompactionResult { summary, first_kept_entry_id: id.into(), tokens_before: preparation.tokens_before as u64, details: Some(details) });
    }
    None
}
