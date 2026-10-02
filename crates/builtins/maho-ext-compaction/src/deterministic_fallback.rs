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

fn has_visible_user_text(text: &str) -> bool {
    use unicode_normalization::UnicodeNormalization;
    text.nfkc().any(|character| !character.is_whitespace() && !matches!(character,
        '\u{00ad}' | '\u{034f}' | '\u{061c}' | '\u{115f}'..='\u{1160}' |
        '\u{17b4}'..='\u{17b5}' | '\u{180b}'..='\u{180f}' |
        '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' |
        '\u{2060}'..='\u{206f}' | '\u{3164}' | '\u{fe00}'..='\u{fe0f}' |
        '\u{feff}' | '\u{ffa0}' | '\u{fff0}'..='\u{fff8}' |
        '\u{1bca0}'..='\u{1bca3}' | '\u{1d173}'..='\u{1d17a}' |
        '\u{e0000}'..='\u{e0fff}'))
}

#[cfg(test)]
mod meaningful_user_tests {
    use super::has_visible_user_text;

    #[test]
    fn invisible_and_compatibility_blank_requests_are_not_fallback_boundaries() {
        for text in ["\u{200b}\u{2060}", "\u{3164}\u{ffa0}", "\u{00ad}\u{e0100}", "\u{00a0}\u{3000}"] {
            assert!(!has_visible_user_text(text), "{text:?}");
        }
    }

    #[test]
    fn visible_compatibility_letters_remain_fallback_boundaries() {
        assert!(has_visible_user_text("\u{200b}\u{ff21}\u{fe0f}"));
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

pub fn format_required_compaction_fallback_rejection(diagnostics: &DeterministicFallbackDiagnostic) -> String {
    use serde_json::json;
    let reason = diagnostics.rejection_reason.unwrap_or("context-reconstruction-failed");
    let recovery = match reason {
        "retained-token-budget-exceeded" => "The retained turn exceeds the usable context. Select a model with a larger context and run /compact, or start a new session with an explicit checkpoint.",
        "atomic-tool-chain-cut" => "The retained tool calls and results do not form complete pairs. Finish the pending tool operation before /compact; if the transcript is damaged, start a new session with an explicit checkpoint.",
        "unsafe-retained-content" => "The retained message cannot be replayed safely. Inspect the identified entry; start a new session with an explicit checkpoint if it cannot be repaired.",
        "missing-preparation-boundary" => "The prepared boundary is absent from the branch. Reload the session and run /compact.",
        _ => "The retained context could not be reconstructed. Reload the session and run /compact.",
    };
    let mut diagnostic = json!({"rejectionReason":reason,"contextWindow":diagnostics.context_window,
        "reserveTokens":diagnostics.reserve_tokens,"budgetTokens":diagnostics.budget_tokens,
        "budgetExceeded":diagnostics.budget_exceeded,"candidatesChecked":diagnostics.candidates_checked});
    if let Some(candidate) = diagnostics.candidate_rejections.last() {
        let mut candidate = candidate.clone();
        for (key, max) in [("firstKeptEntryId",128), ("unsafeEntryId",128), ("unsafeMessageRole",32)] {
            if let Some(text) = candidate[key].as_str() { candidate[key] = json!(crate::task_intent::cap_utf8_bytes(text,max)); }
        }
        diagnostic["candidate"] = candidate;
    }
    format!("deterministic compaction fallback could not retain a safe suffix\n{diagnostic}\n{recovery} The original transcript has not been changed.")
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
        let meaningful = content.as_str().is_some_and(has_visible_user_text) || content.as_array().is_some_and(|blocks| blocks.iter().any(|block| block["type"] == "text" && block["text"].as_str().is_some_and(has_visible_user_text)));
        if !meaningful { continue; }
        candidates.push((index, "latest-user-turn"));
        for earlier in (prepared_index + 1..index).rev() {
            if branch_entries[earlier]["type"] == "compaction" { break; }
            candidates.push((earlier, "earlier-safe-boundary"));
        }
        break;
    }
    let projection_preview = json!({"type":"compaction","id":"__senpi_deterministic_fallback_preview__","parentId":branch_entries.last().and_then(|entry| entry.get("id")),"timestamp":"1970-01-01T00:00:00.000Z","summary":summary,"firstKeptEntryId":branch_entries.first().and_then(|entry|entry.get("id")),"tokensBefore":preparation.tokens_before,"fromHook":true});
    let mut projection_entries = branch_entries.to_vec();
    projection_entries.push(projection_preview);
    let projected: Vec<_> = build_context_entries(&projection_entries, None).iter()
        .flat_map(|entry| session_entry_to_context_messages(entry).into_iter().map(|message| (entry["id"].clone(), message))).collect();
    let projected_messages: Vec<_> = projected.iter().map(|(_, message)| message.clone()).collect();
    let normalized = maho_core::messages::drop_failed_assistant_turns(projected_messages.clone());
    let mut next = 0;
    let retained_positions: Vec<_> = normalized.iter().map(|message| {
        let offset = projected_messages[next..].iter().position(|candidate|candidate == message)
            .expect("normalization preserves original message order");
        next += offset + 1;
        next - 1
    }).collect();
    for (index, retained_suffix) in candidates {
        diagnostics.candidates_checked += 1;
        let id = branch_entries[index]["id"].as_str().unwrap_or_default();
        let mut details = json!({"schema":"senpi.compaction.deterministic-fallback.v1","origin":"required-compaction-recovery","failureKind":failure_name(failure),"retainedSuffix":retained_suffix});
        if let Some(intent) = intent { details["taskIntent"] = json!(intent); }
        let preview = json!({"type":"compaction","id":"__senpi_deterministic_fallback_preview__","parentId":branch_entries.last().and_then(|entry| entry.get("id")),"timestamp":"1970-01-01T00:00:00.000Z","summary":summary,"firstKeptEntryId":id,"tokensBefore":preparation.tokens_before,"details":details,"fromHook":true});
        let mut entries = branch_entries.to_vec(); entries.push(preview);
        let context_entries = build_context_entries(&entries, None);
        let messages: Vec<_> = context_entries.iter().flat_map(session_entry_to_context_messages).collect();
        // Preserve the raw session envelope for safety and token accounting.
        // convert_to_llm would erase custom/bash fields before they are checked.
        let start = projected.iter().position(|(entry_id,_)| entry_id.as_str() == Some(id));
        let retained_indexes: Vec<_> = retained_positions.iter().copied().filter(|position|start.is_some_and(|start|*position >= start)).collect();
        let canonical: Vec<_> = retained_indexes.iter().map(|position|projected[*position].1.clone()).collect();
        let summary_message = messages.first()?;
        let mut rejection_details = json!({});
        let reason = if start.is_none() {
            Some("context-reconstruction-failed")
        } else if crate::retained_message_safety::has_unsafe_retained_content(&canonical) || !canonical.iter().all(|message| bounded_value(message, 0)) {
            if let Some(unsafe_index) = canonical.iter().position(|message| crate::retained_message_safety::has_unsafe_retained_content(std::slice::from_ref(message)) || !bounded_value(message, 0)) {
                let projected_index = retained_indexes[unsafe_index];
                rejection_details["unsafeMessageIndex"] = json!(projected_index);
                if let Some(role) = canonical[unsafe_index].get("role") { rejection_details["unsafeMessageRole"] = role.clone(); }
                rejection_details["unsafeEntryId"] = projected[projected_index].0.clone();
            }
            Some("unsafe-retained-content")
        } else if !complete_tool_chains(&canonical) { Some("atomic-tool-chain-cut") } else {
            let tokens: u64 = canonical.iter().chain(std::iter::once(summary_message)).map(|message| {
                let mut envelope = message.clone();
                let mut image_tokens = 0;
                if envelope["role"] == "toolResult" && let Some(blocks) = envelope["content"].as_array_mut() {
                    for block in blocks {
                        if block["type"] == "image" {
                            let mut single_image = message.clone();
                            single_image["content"] = json!([block.clone()]);
                            image_tokens += estimate_tokens(&single_image);
                            block["data"] = json!("");
                        }
                    }
                }
                estimate_tokens(message).max(estimate_tokens(&json!({"role":"user","content":envelope.to_string(),"timestamp":0})) + image_tokens)
            }).sum();
            rejection_details["estimatedTokens"] = json!(tokens);
            if tokens > diagnostics.budget_tokens { diagnostics.budget_exceeded = true; Some("retained-token-budget-exceeded") } else { None }
        };
        if let Some(reason) = reason {
            diagnostics.rejection_reason = Some(reason);
            rejection_details["firstKeptEntryId"] = json!(id);
            rejection_details["rejectionReason"] = json!(reason);
            diagnostics.candidate_rejections.push(rejection_details);
            continue;
        }
        return Some(maho_ext_api::CompactionResult { summary, first_kept_entry_id: id.into(), tokens_before: preparation.tokens_before as u64, details: Some(details) });
    }
    None
}
