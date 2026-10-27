//! Port of senpi packages/coding-agent/src/core/compaction/compaction.ts.
//!
//! Ported whole: token accounting, cut-point selection, the summarization prompts, the
//! provider-calling summarization functions and prepareCompaction.
//!
//! Adaptation: senpi's long positional parameter lists (model, apiKey, headers, signal, extraBody,
//! thinkingLevel, streamFn, env, transformContext, retry, callbacks, sessionId,
//! summarizationMaxDurationMs) travel together in [`SummarizationRequest`]; message transforms
//! operate on the session's JSON message values.

use super::overflow_retry::{allow_overflow_retry, bound_summarization_input, shrink_summarization_input_for_overflow_retry, SummarizationOverflowExhaustedError};
use std::collections::BTreeMap;
use std::sync::{Arc, LazyLock, Mutex, PoisonError};

use maho_ai::model::Model;
use maho_ai::types::{
    AllowedFallbackModel, AnthropicRefusalFallback, AssistantMessage, AssistantMessageEventStream,
    BoxFuture, CacheRetention, ContentBlock, Context, Message, ModelThinkingLevel, OnPayload,
    OnResponse, ProviderEnv, RefusalFallbackModel, SimpleStreamOptions, StopReason, TextContent,
    ThinkingBudgets, ThinkingLevel, Transport, Usage, UsageCost, UserContent, UserMessage,
};
use maho_ai::utils::abort::{AbortController, AbortSignal};
use maho_ai::utils::retry::{RetryCallbacks, RetryPolicy, retry_assistant_call};
use regex::Regex;
use serde_json::Value;

use super::settings::CompactionSettings;
use super::utils::{FileOperations, create_file_ops, extract_file_ops_from_message};
use crate::messages::{filter_context_excluded_messages, is_context_excluded_custom_message};
use crate::session_manager::{build_session_context, session_entry_to_context_messages};

/// senpi CompactionDetails.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CompactionDetails {
    pub read_files: Vec<String>,
    pub modified_files: Vec<String>,
}

fn entry_type(entry: &Value) -> Option<&str> {
    entry.get("type").and_then(Value::as_str)
}

fn entry_id(entry: &Value) -> Option<String> {
    entry.get("id").and_then(Value::as_str).map(str::to_string)
}

fn entry_message(entry: &Value) -> Option<&Value> {
    entry.get("message")
}

fn message_role(message: &Value) -> Option<&str> {
    message.get("role").and_then(Value::as_str)
}

/// senpi contextMessagesForCompactionEntry.
pub fn context_messages_for_compaction_entry(entry: &Value) -> Vec<Value> {
    if entry_type(entry) == Some("compaction") {
        return Vec::new();
    }
    session_entry_to_context_messages(entry)
        .into_iter()
        .filter(|message| {
            message_role(message) != Some("custom")
                || !message
                    .get("customType")
                    .and_then(Value::as_str)
                    .is_some_and(is_context_excluded_custom_message)
        })
        .collect()
}

/// senpi getMessageFromEntryForCompaction.
pub fn get_message_from_entry_for_compaction(entry: &Value) -> Option<Value> {
    context_messages_for_compaction_entry(entry)
        .into_iter()
        .next()
}

/// senpi collectSourceMessages.
pub fn collect_source_messages(
    entries: &[Value],
    start_index: usize,
    end_index: usize,
    previous_compaction_index: Option<usize>,
) -> Vec<Value> {
    let mut messages: Vec<Value> = Vec::new();
    if let Some(previous_compaction_index) = previous_compaction_index {
        messages.extend(session_entry_to_context_messages(
            &entries[previous_compaction_index],
        ));
    }
    for (index, entry) in entries.iter().enumerate().take(end_index).skip(start_index) {
        if Some(index) != previous_compaction_index {
            messages.extend(session_entry_to_context_messages(entry));
        }
    }
    messages
}

/// senpi CompactionResult.
#[derive(Debug, Clone, PartialEq)]
pub struct CompactionResult {
    pub summary: String,
    pub first_kept_entry_id: String,
    pub tokens_before: i64,
    pub estimated_tokens_after: Option<i64>,
    pub usage: Option<Usage>,
    pub details: Option<Value>,
}

/// senpi combineUsage.
pub fn combine_usage(first: &Usage, second: &Usage) -> Usage {
    let cache_write_1h = if first.cache_write_1h.is_some() || second.cache_write_1h.is_some() {
        Some(first.cache_write_1h.unwrap_or(0) + second.cache_write_1h.unwrap_or(0))
    } else {
        None
    };
    let reasoning = if first.reasoning.is_some() || second.reasoning.is_some() {
        Some(first.reasoning.unwrap_or(0) + second.reasoning.unwrap_or(0))
    } else {
        None
    };
    let cost_unknown = if first.cost_unknown == Some(true) || second.cost_unknown == Some(true) {
        Some(true)
    } else {
        None
    };
    Usage {
        context_usage: None,
        cost_unknown,
        input: first.input + second.input,
        output: first.output + second.output,
        cache_read: first.cache_read + second.cache_read,
        cache_write: first.cache_write + second.cache_write,
        cache_write_1h,
        reasoning,
        total_tokens: first.total_tokens + second.total_tokens,
        cost: UsageCost {
            input: first.cost.input + second.cost.input,
            output: first.cost.output + second.cost.output,
            cache_read: first.cost.cache_read + second.cost.cache_read,
            cache_write: first.cost.cache_write + second.cost.cache_write,
            total: first.cost.total + second.cost.total,
        },
    }
}

/// senpi calculateContextTokens.
pub fn calculate_context_tokens(usage: &Usage) -> u64 {
    if let Some(context_usage) = usage.context_usage {
        return context_usage.map_or(0, |context| context.input + context.output);
    }
    if usage.total_tokens != 0 {
        usage.total_tokens
    } else {
        usage.input + usage.output + usage.cache_read + usage.cache_write
    }
}

/// senpi resolveThresholdContextTokens.
pub fn resolve_threshold_context_tokens(usage_tokens: f64, estimate_tokens: f64) -> f64 {
    let usage = if usage_tokens > 0.0 {
        usage_tokens
    } else {
        0.0
    };
    let estimate = if estimate_tokens > 0.0 {
        estimate_tokens
    } else {
        0.0
    };
    if estimate >= 50_000.0 && usage > estimate * 8.0 {
        return estimate;
    }
    usage.max(estimate)
}

fn usage_from_value(value: &Value) -> Option<Usage> {
    // PERF-DIVERGENCE(senpi compaction.ts): upstream reads the usage object in place; the port deep-cloned it per candidate via from_value, this deserializes straight from the borrowed Value. Same parsed Usage.
    <Usage as serde::Deserialize>::deserialize(value).ok()
}

/// senpi getAssistantUsage.
pub fn get_assistant_usage(message: &Value) -> Option<Usage> {
    if message_role(message) != Some("assistant") {
        return None;
    }
    let stop_reason = message
        .get("stopReason")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if stop_reason == "aborted" || stop_reason == "error" {
        return None;
    }
    let usage = usage_from_value(message.get("usage")?)?;
    if calculate_context_tokens(&usage) == 0 {
        return None;
    }
    Some(usage)
}

/// senpi getLastAssistantUsage.
pub fn get_last_assistant_usage(entries: &[Value]) -> Option<Usage> {
    for entry in entries.iter().rev() {
        if entry_type(entry) == Some("message")
            && let Some(usage) = entry_message(entry).and_then(get_assistant_usage)
        {
            return Some(usage);
        }
    }
    None
}

/// senpi ContextUsageEstimate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ContextUsageEstimate {
    pub tokens: u64,
    pub usage_tokens: u64,
    pub trailing_tokens: u64,
    pub last_usage_index: Option<usize>,
}

/// senpi dropFailedAssistantTurns as an index mask: failed assistant turns and the tool results
/// their calls alone declared are not counted.
fn counted_message_mask(messages: &[Value]) -> Vec<bool> {
    let mut kept_call_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut failed_call_ids: std::collections::HashSet<String> = std::collections::HashSet::new();

    for message in messages {
        if message_role(message) != Some("assistant") {
            continue;
        }
        let failed = matches!(
            message.get("stopReason").and_then(Value::as_str),
            Some("error") | Some("aborted")
        );
        let declared = if failed {
            &mut failed_call_ids
        } else {
            &mut kept_call_ids
        };
        if let Some(blocks) = message.get("content").and_then(Value::as_array) {
            for block in blocks {
                if block.get("type").and_then(Value::as_str) == Some("toolCall")
                    && let Some(id) = block.get("id").and_then(Value::as_str)
                {
                    declared.insert(id.to_string());
                }
            }
        }
    }
    for id in &kept_call_ids {
        failed_call_ids.remove(id);
    }

    messages
        .iter()
        .map(|message| match message_role(message) {
            Some("assistant") => !matches!(
                message.get("stopReason").and_then(Value::as_str),
                Some("error") | Some("aborted")
            ),
            Some("toolResult") => message
                .get("toolCallId")
                .and_then(Value::as_str)
                .is_none_or(|id| !failed_call_ids.contains(id)),
            _ => true,
        })
        .collect()
}

/// senpi estimateContextTokens: the last valid assistant usage plus the trailing estimate.
pub fn estimate_context_tokens(messages: &[Value]) -> ContextUsageEstimate {
    let counted = counted_message_mask(messages);

    let mut usage_info: Option<(Usage, usize)> = None;
    for (index, message) in messages.iter().enumerate().rev() {
        if !counted[index] {
            continue;
        }
        if let Some(usage) = get_assistant_usage(message) {
            usage_info = Some((usage, index));
            break;
        }
    }

    let Some((usage, index)) = usage_info else {
        let estimated: u64 = messages
            .iter()
            .enumerate()
            .filter(|(index, _)| counted[*index])
            .map(|(_, message)| estimate_tokens(message))
            .sum();
        return ContextUsageEstimate {
            tokens: estimated,
            usage_tokens: 0,
            trailing_tokens: estimated,
            last_usage_index: None,
        };
    };

    let usage_tokens = calculate_context_tokens(&usage);
    let trailing_tokens: u64 = messages
        .iter()
        .enumerate()
        .skip(index + 1)
        .filter(|(index, _)| counted[*index])
        .map(|(_, message)| estimate_tokens(message))
        .sum();

    ContextUsageEstimate {
        tokens: usage_tokens + trailing_tokens,
        usage_tokens,
        trailing_tokens,
        last_usage_index: Some(index),
    }
}

/// senpi estimateProjectedContextTokens: estimate the projected context without trusting usage
/// captured before a later compaction (maho-core has no `context_edit` entries).
pub fn estimate_projected_context_tokens(path_entries: &[Value]) -> ContextUsageEstimate {
    let context = build_session_context(path_entries, None);
    let messages = filter_context_excluded_messages(context.messages);
    let estimate = estimate_context_tokens(&messages);
    if let Some(usage_index) = estimate.last_usage_index {
        let usage_entry_index = context.entry_ids.get(usage_index).and_then(|usage_entry_id| {
            path_entries
                .iter()
                .position(|entry| entry_id(entry).as_deref() == Some(usage_entry_id.as_str()))
        });
        let latest_compaction_index = path_entries
            .iter()
            .rposition(|entry| entry_type(entry) == Some("compaction"));
        if usage_entry_index.is_some_and(|usage| latest_compaction_index.is_none_or(|latest| usage > latest)) {
            return estimate;
        }
    }
    let counted = counted_message_mask(&messages);
    let tokens = messages
        .iter()
        .zip(counted)
        .filter(|(_, counted)| *counted)
        .map(|(message, _)| estimate_tokens(message))
        .sum();
    ContextUsageEstimate {
        tokens,
        usage_tokens: 0,
        trailing_tokens: tokens,
        last_usage_index: None,
    }
}

/// senpi policy.ts RESERVE_WINDOW_FRACTION.
pub const RESERVE_WINDOW_FRACTION: f64 = 0.04;
/// senpi policy.ts MAX_SCALED_RESERVE_TOKENS.
pub const MAX_SCALED_RESERVE_TOKENS: f64 = 49_152.0;

/// senpi policy.ts resolveReserveTokens.
pub fn resolve_reserve_tokens(context_window: f64, configured_reserve: f64) -> f64 {
    configured_reserve.max(
        (RESERVE_WINDOW_FRACTION * context_window)
            .floor()
            .min(MAX_SCALED_RESERVE_TOKENS),
    )
}

/// senpi policy.ts resolveEffectiveReserveTokens.
pub fn resolve_effective_reserve_tokens(
    context_window: f64,
    configured_reserve: f64,
    reserve_scaling_enabled: Option<bool>,
) -> f64 {
    if reserve_scaling_enabled == Some(false) {
        configured_reserve
    } else {
        resolve_reserve_tokens(context_window, configured_reserve)
    }
}

/// senpi shouldCompact.
pub fn should_compact(
    context_tokens: f64,
    context_window: f64,
    settings: &CompactionSettings,
) -> bool {
    if !settings.enabled {
        return false;
    }
    let reserve = resolve_effective_reserve_tokens(
        context_window,
        settings.reserve_tokens as f64,
        settings.ideal.reserve_scaling_enabled,
    );
    context_tokens > context_window - reserve
}

/// senpi ESTIMATED_IMAGE_CHARS.
pub const ESTIMATED_IMAGE_CHARS: usize = 4800;

/// senpi BASE64_RUN_RE.
pub static BASE64_RUN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[A-Za-z0-9+/=_-]{512,}").expect("base64 run pattern"));

/// senpi BASE64_CHAR_WEIGHT.
pub const BASE64_CHAR_WEIGHT: usize = 4;

/// senpi weightedChars: long base64-ish runs count four times over.
pub fn weighted_chars(text: &str) -> usize {
    let mut chars = text.encode_utf16().count();
    // PERF-DIVERGENCE(senpi compaction.ts weightedChars): upstream always runs the base64 regex; a run needs 512 chars (>= 512 bytes), so shorter texts skip the regex scan. Result is identical.
    if text.len() < 512 {
        return chars;
    }
    for found in BASE64_RUN_RE.find_iter(text) {
        chars += found.as_str().chars().count() * (BASE64_CHAR_WEIGHT - 1);
    }
    chars
}

fn estimate_text_and_image_content_chars(content: &Value) -> usize {
    match content {
        Value::String(text) => weighted_chars(text),
        Value::Array(blocks) => blocks
            .iter()
            .map(|block| match block.get("type").and_then(Value::as_str) {
                Some("text") => block
                    .get("text")
                    .and_then(Value::as_str)
                    .map(weighted_chars)
                    .unwrap_or(0),
                Some("image") => ESTIMATED_IMAGE_CHARS,
                _ => 0,
            })
            .sum(),
        _ => 0,
    }
}

/// senpi estimateTokens: the chars/4 heuristic, deliberately conservative.
pub fn estimate_tokens(message: &Value) -> u64 {
    let chars = match message_role(message) {
        Some("user") => {
            estimate_text_and_image_content_chars(message.get("content").unwrap_or(&Value::Null))
        }
        Some("assistant") => {
            let mut chars = 0usize;
            if let Some(blocks) = message.get("content").and_then(Value::as_array) {
                for block in blocks {
                    match block.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            chars += block
                                .get("text")
                                .and_then(Value::as_str)
                                .map(|text| text.encode_utf16().count())
                                .unwrap_or(0);
                        }
                        Some("thinking") => {
                            chars += block
                                .get("thinking")
                                .and_then(Value::as_str)
                                .map(|text| text.encode_utf16().count())
                                .unwrap_or(0);
                        }
                        Some("toolCall") => {
                            let name = block
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or_default();
                            let arguments = block
                                .get("arguments")
                                .map(|arguments| {
                                    serde_json::to_string(arguments).unwrap_or_default()
                                })
                                .unwrap_or_default();
                            chars += name.encode_utf16().count() + weighted_chars(&arguments);
                        }
                        _ => {}
                    }
                }
            }
            chars
        }
        Some("custom") | Some("toolResult") => {
            estimate_text_and_image_content_chars(message.get("content").unwrap_or(&Value::Null))
        }
        Some("bashExecution") => {
            let command = message
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let output = message
                .get("output")
                .and_then(Value::as_str)
                .unwrap_or_default();
            command.encode_utf16().count() + weighted_chars(output)
        }
        Some("branchSummary") | Some("compactionSummary") => message
            .get("summary")
            .and_then(Value::as_str)
            .map(|summary| summary.encode_utf16().count())
            .unwrap_or(0),
        _ => 0,
    };
    chars.div_ceil(4) as u64
}

/// senpi isCutPointMessage.
pub fn is_cut_point_message(message: &Value) -> bool {
    matches!(
        message_role(message),
        Some("user")
            | Some("assistant")
            | Some("bashExecution")
            | Some("custom")
            | Some("branchSummary")
            | Some("compactionSummary")
    )
}

/// senpi isTurnStartMessage.
pub fn is_turn_start_message(message: &Value) -> bool {
    matches!(
        message_role(message),
        Some("user")
            | Some("bashExecution")
            | Some("custom")
            | Some("branchSummary")
            | Some("compactionSummary")
    )
}

fn is_turn_start_entry(entry: &Value) -> bool {
    if entry_type(entry) == Some("compaction") {
        return false;
    }
    context_messages_for_compaction_entry(entry)
        .iter()
        .any(is_turn_start_message)
}

/// senpi findValidCutPoints.
pub fn find_valid_cut_points(
    entries: &[Value],
    start_index: usize,
    end_index: usize,
) -> Vec<usize> {
    let mut cut_points: Vec<usize> = Vec::new();
    for (index, entry) in entries
        .iter()
        .enumerate()
        .take(end_index.min(entries.len()))
        .skip(start_index)
    {
        if entry_type(entry) == Some("compaction") {
            continue;
        }
        if context_messages_for_compaction_entry(entry)
            .iter()
            .any(is_cut_point_message)
        {
            cut_points.push(index);
        }
    }
    cut_points
}

/// senpi findTurnStartIndex; -1 when no turn start precedes the entry.
pub fn find_turn_start_index(entries: &[Value], entry_index: usize, start_index: usize) -> i64 {
    if entry_index >= entries.len() {
        return -1;
    }
    let mut index = entry_index as i64;
    while index >= start_index as i64 {
        if is_turn_start_entry(&entries[index as usize]) {
            return index;
        }
        index -= 1;
    }
    -1
}

/// senpi CutPointResult.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CutPointResult {
    pub first_kept_entry_index: usize,
    pub turn_start_index: i64,
    pub is_split_turn: bool,
}

/// senpi findCutPoint: keep roughly keepRecentTokens, never cutting a tool result off its call.
pub fn find_cut_point(
    entries: &[Value],
    start_index: usize,
    end_index: usize,
    keep_recent_tokens: i64,
) -> CutPointResult {
    let cut_points = find_valid_cut_points(entries, start_index, end_index);

    if cut_points.is_empty() {
        return CutPointResult {
            first_kept_entry_index: start_index,
            turn_start_index: -1,
            is_split_turn: false,
        };
    }

    let mut accumulated_tokens: i64 = 0;
    let mut cut_index = cut_points[0];
    // senpi keepBudgetWeighted: a failed turn is never sent, so it may not absorb the keep budget.
    let entry_messages: Vec<Vec<Value>> = (start_index..end_index)
        .map(|index| context_messages_for_compaction_entry(&entries[index]))
        .collect();
    let flat: Vec<Value> = entry_messages.iter().flatten().cloned().collect();
    let weighted = counted_message_mask(&flat);
    let mut entry_offsets = Vec::with_capacity(entry_messages.len());
    let mut offset = 0;
    for messages in &entry_messages {
        entry_offsets.push(offset);
        offset += messages.len();
    }

    let mut index = end_index as i64 - 1;
    while index >= start_index as i64 {
        let local = index as usize - start_index;
        let message_tokens: i64 = entry_messages[local]
            .iter()
            .enumerate()
            .filter(|(position, _)| weighted[entry_offsets[local] + position])
            .map(|(_, message)| estimate_tokens(message) as i64)
            .sum();
        if message_tokens != 0 {
            accumulated_tokens += message_tokens;
            if accumulated_tokens >= keep_recent_tokens {
                match cut_points
                    .iter()
                    .find(|cut_point| **cut_point >= index as usize)
                {
                    Some(found) => cut_index = *found,
                    None => cut_index = *cut_points.last().expect("cut points are non-empty"),
                }
                break;
            }
        }
        index -= 1;
    }

    while cut_index > start_index {
        let previous = &entries[cut_index - 1];
        if entry_type(previous) == Some("compaction")
            || !context_messages_for_compaction_entry(previous).is_empty()
        {
            break;
        }
        if entry_type(previous) == Some("custom_message")
            && previous
                .get("customType")
                .and_then(Value::as_str)
                .is_some_and(is_context_excluded_custom_message)
        {
            break;
        }
        cut_index -= 1;
    }

    let cut_entry = &entries[cut_index];
    let starts_turn = is_turn_start_entry(cut_entry);
    let turn_start_index = if starts_turn {
        -1
    } else {
        find_turn_start_index(entries, cut_index, start_index)
    };

    CutPointResult {
        first_kept_entry_index: cut_index,
        turn_start_index,
        is_split_turn: !starts_turn && turn_start_index != -1,
    }
}

/// senpi getSummarizationFailure.
pub fn get_summarization_failure(response: &AssistantMessage, label: &str) -> Option<String> {
    if response.stop_reason == StopReason::Error {
        let message = response
            .error_message
            .clone()
            .unwrap_or_else(|| "Unknown error".to_string());
        return Some(format!("{label} failed: {message}"));
    }
    if response.stop_reason == StopReason::Length {
        return Some(format!(
            "{label} failed: generation hit the token cap and the summary is incomplete"
        ));
    }
    None
}

/// senpi SUMMARIZATION_PROMPT.
pub const SUMMARIZATION_PROMPT: &str = "The messages above are a conversation to summarize. Create a structured context checkpoint summary that another LLM will use to continue the work.

Use this EXACT format:

## Goal
[What is the user trying to accomplish? Can be multiple items if the session covers different tasks.]

## Constraints & Preferences
- [Any constraints, preferences, or requirements mentioned by user]
- [Or \"(none)\" if none were mentioned]

## Progress
### Done
- [x] [Completed tasks/changes]

### In Progress
- [ ] [Current work]

### Blocked
- [Issues preventing progress, if any]

## Key Decisions
- **[Decision]**: [Brief rationale]

## Next Steps
1. [Ordered list of what should happen next]

## Critical Context
- [Any data, examples, or references needed to continue]
- [Or \"(none)\" if not applicable]

Keep each section concise. Preserve exact file paths, function names, and error messages.";

/// senpi UPDATE_SUMMARIZATION_INSTRUCTIONS.
pub const UPDATE_SUMMARIZATION_INSTRUCTIONS: &str =
    "Update the existing structured summary with new information. RULES:
- PRESERVE all existing information from the previous summary
- ADD new progress, decisions, and context from the new messages
- UPDATE the Progress section: move items from \"In Progress\" to \"Done\" when completed
- UPDATE \"Next Steps\" based on what was accomplished
- PRESERVE exact file paths, function names, and error messages
- If something is no longer relevant, you may remove it

Use this EXACT format:

## Goal
[Preserve existing goals, add new ones if the task expanded]

## Constraints & Preferences
- [Preserve existing, add new ones discovered]

## Progress
### Done
- [x] [Include previously done items AND newly completed items]

### In Progress
- [ ] [Current work - update based on progress]

### Blocked
- [Current blockers - remove if resolved]

## Key Decisions
- **[Decision]**: [Brief rationale] (preserve all previous, add new)

## Next Steps
1. [Update based on current state]

## Critical Context
- [Preserve important context, add new if needed]

Keep each section concise. Preserve exact file paths, function names, and error messages.";

/// senpi UPDATE_SUMMARIZATION_PROMPT.
pub fn update_summarization_prompt() -> String {
    format!(
        "The messages above are NEW conversation messages to incorporate into the existing summary provided in <previous-summary> tags.\n\n{UPDATE_SUMMARIZATION_INSTRUCTIONS}"
    )
}

/// senpi SOURCE_CONTEXT_UPDATE_SUMMARIZATION_PROMPT.
pub const SOURCE_CONTEXT_UPDATE_SUMMARIZATION_PROMPT: &str = "The messages above contain an existing structured summary of earlier conversation history followed by NEW conversation messages.

Update the existing structured summary with new information. RULES:
- PRESERVE all existing information from the previous summary
- ADD new progress, decisions, and context from the new messages
- UPDATE the Progress section: move items from \"In Progress\" to \"Done\" when completed
- UPDATE \"Next Steps\" based on what was accomplished
- PRESERVE exact file paths, function names, and error messages
- If something is no longer relevant, you may remove it

Use this EXACT format:

## Goal
[Preserve existing goals, add new ones if the task expanded]

## Constraints & Preferences
- [Preserve existing, add new ones discovered]

## Progress
### Done
- [x] [Include previously done items AND newly completed items]

### In Progress
- [ ] [Current work - update based on progress]

### Blocked
- [Current blockers - remove if resolved]

## Key Decisions
- **[Decision]**: [Brief rationale] (preserve all previous, add new)

## Next Steps
1. [Update based on current state]

## Critical Context
- [Preserve important context, add new if needed]

Keep each section concise. Preserve exact file paths, function names, and error messages.";

/// senpi SummarizationStreamFn (the agent StreamFn shape).
pub type SummarizationStreamFn<'a> =
    dyn Fn(&Model, &Context, SimpleStreamOptions) -> AssistantMessageEventStream + Send + Sync + 'a;

/// senpi `transformContext` over session message values.
pub type SummaryTransformContext = Arc<
    dyn Fn(Vec<Value>, Option<AbortSignal>) -> BoxFuture<'static, Result<Vec<Value>, String>>
        + Send
        + Sync,
>;

/// senpi CacheFriendlySummaryOptions.requestOptions: request settings copied from the active agent
/// request path.
#[derive(Clone, Default)]
pub struct SummaryRequestOptions {
    pub session_id: Option<String>,
    pub on_payload: Option<OnPayload>,
    pub on_response: Option<OnResponse>,
    pub transport: Option<Transport>,
    pub thinking_budgets: Option<ThinkingBudgets>,
    pub max_retry_delay_ms: Option<u64>,
}

/// senpi CacheFriendlySummaryOptions.
#[derive(Clone, Default)]
pub struct CacheFriendlySummaryOptions {
    /// Exact provider context prefix containing the history to summarize.
    pub source_context: Option<Context>,
    /// Exact provider context prefix containing a split turn's prefix.
    pub turn_prefix_source_context: Option<Context>,
    pub request_options: Option<SummaryRequestOptions>,
}

/// The shared summarization request parameters senpi passes positionally.
#[derive(Clone)]
pub struct SummarizationRequest<'a> {
    pub model: &'a Model,
    pub api_key: Option<String>,
    pub headers: Option<BTreeMap<String, String>>,
    pub env: Option<ProviderEnv>,
    pub signal: Option<AbortSignal>,
    pub extra_body: Option<serde_json::Map<String, Value>>,
    pub thinking_level: Option<ModelThinkingLevel>,
    pub stream_fn: Option<&'a SummarizationStreamFn<'a>>,
    pub transform_context: Option<SummaryTransformContext>,
    pub retry: Option<&'a RetryPolicy>,
    pub callbacks: Option<&'a RetryCallbacks<'a>>,
    pub session_id: Option<String>,
    pub summarization_max_duration_ms: Option<f64>,
}

/// A generated summary and its provider usage.
#[derive(Debug, Clone, PartialEq)]
pub struct SummaryWithUsage {
    pub text: String,
    pub usage: Usage,
}

/// senpi getAnthropicSummarizationFallback.
fn get_anthropic_summarization_fallback(model: &Model) -> Option<AnthropicRefusalFallback> {
    if model.provider != "anthropic" || model.api != "anthropic-messages" {
        return None;
    }
    let first = model
        .compat
        .as_ref()?
        .anthropic_messages()
        .allowed_fallback_models?
        .into_iter()
        .next()?;
    let model = match first {
        AllowedFallbackModel::Id(id) => id,
        AllowedFallbackModel::Model(model) => model.model,
    };
    Some(AnthropicRefusalFallback::Models(vec![
        RefusalFallbackModel { model },
    ]))
}

/// senpi createSummarizationOptions.
pub fn create_summarization_options(
    model: &Model,
    max_tokens: u64,
    request: &SummarizationRequest<'_>,
    request_options: Option<&SummaryRequestOptions>,
    cache_retention: Option<CacheRetention>,
) -> SimpleStreamOptions {
    let mut options = SimpleStreamOptions::default();
    if let Some(request_options) = request_options {
        options.stream.session_id = request_options.session_id.clone();
        options.stream.request.on_payload = request_options.on_payload.clone();
        options.stream.request.on_response = request_options.on_response.clone();
        options.stream.transport = request_options.transport;
        options.thinking_budgets = request_options.thinking_budgets.clone();
        options.stream.request.max_retry_delay_ms = request_options.max_retry_delay_ms;
    }
    options.stream.max_tokens = Some(max_tokens);
    options.stream.request.signal = request.signal.clone();
    options.stream.request.api_key = request.api_key.clone();
    options.stream.request.headers = request.headers.clone().map(|headers| {
        headers
            .into_iter()
            .map(|(key, value)| (key, Some(value)))
            .collect()
    });
    options.stream.request.env = request.env.clone();
    options.stream.extra_body = request.extra_body.clone();
    options.stream.cache_retention = cache_retention;
    if let Some(session_id) = request
        .session_id
        .as_ref()
        .filter(|session_id| !session_id.is_empty())
    {
        options.stream.request.affinity_session_id = Some(session_id.clone());
    }
    if let Some(refusal_fallbacks) = get_anthropic_summarization_fallback(model) {
        options.refusal_fallbacks = Some(refusal_fallbacks);
    }
    if model.reasoning {
        // ThinkingLevel has no "off", so an off level leaves reasoning unset.
        options.reasoning = request.thinking_level.and_then(|level| {
            ThinkingLevel::ALL
                .into_iter()
                .find(|candidate| candidate.as_str() == level.as_str())
        });
    }
    options
}

/// senpi completeSummarization: one summarization call under the idle/duration watchdogs, retried
/// through retryAssistantCall. A watchdog failure ends the call with its error, as the thrown
/// error does upstream.
pub async fn complete_summarization(
    model: &Model,
    context: &Context,
    options: SimpleStreamOptions,
    request: &SummarizationRequest<'_>,
) -> Result<AssistantMessage, String> {
    use super::stream_watchdog::{
        DEFAULT_SUMMARIZATION_IDLE_TIMEOUT_MS, consume_stream_with_idle_timeout,
        summarization_max_duration_ms,
    };
    let mut isolated = options;
    isolated
        .stream
        .cache_retention
        .get_or_insert(CacheRetention::None);
    if isolated.stream.request.affinity_session_id.is_none() {
        isolated.stream.request.affinity_session_id = isolated.stream.session_id.clone();
    }
    isolated.stream.session_id = Some(uuid::Uuid::now_v7().to_string());
    let caller = isolated.stream.request.signal.clone();
    let max_duration_ms = summarization_max_duration_ms(
        maho_ai::utils::estimate::estimate_context_tokens(context).tokens as f64,
        request.summarization_max_duration_ms,
    ) as u64;
    let idle_timeout_ms = u64::try_from(DEFAULT_SUMMARIZATION_IDLE_TIMEOUT_MS).unwrap_or_default();
    let failure: Mutex<Option<String>> = Mutex::new(None);
    let failure_slot = &failure;
    let produce = || {
        let mut options = isolated.clone();
        let caller = caller.clone();
        async move {
            // Request-local controller: the watchdog tears down a stalled request without aborting
            // the caller's own signal.
            let controller = AbortController::new();
            let listener = caller.as_ref().map(|signal| {
                let request_controller = controller.clone();
                let listener = signal.add_abort_listener(move |reason| {
                    request_controller.abort(Some(reason.clone()))
                });
                if signal.aborted() {
                    controller.abort(signal.reason());
                }
                listener
            });
            options.stream.request.signal = Some(controller.signal());
            let stream = match request.stream_fn {
                Some(stream_fn) => stream_fn(model, context, options),
                None => maho_ai::stream::stream_simple(model, context, Some(options)),
            };
            let result = consume_stream_with_idle_timeout(
                &stream,
                idle_timeout_ms,
                Some(max_duration_ms),
                caller.as_ref(),
                || controller.abort(None),
            )
            .await;
            if let (Some(signal), Some(listener)) = (caller, listener) {
                signal.remove_abort_listener(listener);
            }
            match result {
                Ok(message) => message,
                Err(error) => {
                    let error = error.to_string();
                    let mut message = maho_ai::utils::lazy::setup_error_message(model, &error);
                    // Aborted stops the retry loop; the recorded failure is returned below.
                    message.stop_reason = StopReason::Aborted;
                    *failure_slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(error);
                    message
                }
            }
        }
    };
    let response =
        retry_assistant_call(produce, request.retry, caller.as_ref(), request.callbacks).await;
    if let Some(error) = failure
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take()
    {
        return Err(error);
    }
    Ok(response)
}

/// senpi transformSummarySource: the previous summary rides through transformContext as a user
/// message with a unique negative timestamp and is split back out afterwards.
async fn transform_summary_source(
    current_messages: &[Value],
    previous_summary: Option<&str>,
    transform_context: Option<&SummaryTransformContext>,
    signal: Option<&AbortSignal>,
) -> Result<(Vec<Value>, Option<String>), String> {
    let Some(transform_context) = transform_context else {
        return Ok((
            current_messages.to_vec(),
            previous_summary.map(str::to_owned),
        ));
    };
    let Some(previous_summary) = previous_summary else {
        return Ok((
            transform_context(current_messages.to_vec(), signal.cloned()).await?,
            None,
        ));
    };
    let timestamps = current_messages
        .iter()
        .filter_map(|message| message.get("timestamp").and_then(Value::as_i64))
        .collect::<std::collections::HashSet<_>>();
    let mut summary_timestamp = -1_i64;
    while timestamps.contains(&summary_timestamp) {
        summary_timestamp -= 1;
    }
    let mut messages = vec![serde_json::json!({
        "role": "user",
        "content": [{ "type": "text", "text": previous_summary }],
        "timestamp": summary_timestamp,
    })];
    messages.extend(current_messages.iter().cloned());
    let transformed = transform_context(messages, signal.cloned()).await?;
    let is_summary = |message: &Value| {
        message.get("timestamp").and_then(Value::as_i64) == Some(summary_timestamp)
    };
    let transformed_summary = transformed
        .iter()
        .filter(|message| is_summary(message))
        .cloned()
        .collect::<Vec<_>>();
    let messages = transformed
        .into_iter()
        .filter(|message| !is_summary(message))
        .collect();
    let previous_summary = (!transformed_summary.is_empty()).then(|| {
        super::utils::serialize_conversation(&crate::messages::convert_to_llm(&transformed_summary))
    });
    Ok((messages, previous_summary))
}

/// senpi generateSummary.
pub async fn generate_summary(
    current_messages: &[Value],
    reserve_tokens: i64,
    custom_instructions: Option<&str>,
    previous_summary: Option<&str>,
    cache_friendly: Option<&CacheFriendlySummaryOptions>,
    request: &SummarizationRequest<'_>,
) -> Result<String, String> {
    Ok(generate_summary_with_usage(
        current_messages,
        reserve_tokens,
        custom_instructions,
        previous_summary,
        cache_friendly.and_then(|options| options.source_context.as_ref()),
        cache_friendly.and_then(|options| options.request_options.as_ref()),
        request,
    )
    .await?
    .text)
}

/// senpi buildSummarizationContext.
pub fn build_summarization_context(prompt_text: &str, source_context: Option<&Context>) -> Context {
    let instruction_message = Message::User(UserMessage {
        content: UserContent::Blocks(vec![ContentBlock::Text(TextContent {
            text: prompt_text.to_owned(),
            ..Default::default()
        })]),
        timestamp: chrono::Utc::now().timestamp_millis(),
    });
    match source_context {
        Some(source_context) => {
            let mut context = source_context.clone();
            context.messages.push(instruction_message);
            context
        }
        None => Context {
            active_tool_names: None,
            system_prompt: Some(super::utils::SUMMARIZATION_SYSTEM_PROMPT.to_owned()),
            messages: vec![instruction_message],
            tools: None,
        },
    }
}

/// senpi CACHE_FRIENDLY_CONTEXT_SAFETY_TOKENS.
const CACHE_FRIENDLY_CONTEXT_SAFETY_TOKENS: u64 = 4096;

/// senpi cacheFriendlyContextFits.
pub fn cache_friendly_context_fits(model: &Model, context: &Context, max_tokens: u64) -> bool {
    model.context_window == 0
        || maho_ai::utils::estimate::estimate_context_tokens(context)
            .tokens
            .saturating_add(max_tokens)
            .saturating_add(CACHE_FRIENDLY_CONTEXT_SAFETY_TOKENS)
            <= model.context_window
}

/// `min(floor(fraction * reserveTokens), model.maxTokens > 0 ? model.maxTokens : Infinity)`.
pub(crate) fn summary_max_tokens(model: &Model, reserve_tokens: i64, fraction: f64) -> u64 {
    ((fraction * reserve_tokens as f64).floor().max(0.0) as u64).min(if model.max_tokens > 0 {
        model.max_tokens
    } else {
        u64::MAX
    })
}

/// senpi generateSummaryWithUsage.
pub async fn generate_summary_with_usage(
    current_messages: &[Value],
    reserve_tokens: i64,
    custom_instructions: Option<&str>,
    previous_summary: Option<&str>,
    source_context: Option<&Context>,
    request_options: Option<&SummaryRequestOptions>,
    request: &SummarizationRequest<'_>,
) -> Result<SummaryWithUsage, String> {
    let model = request.model;
    let max_tokens = summary_max_tokens(model, reserve_tokens, 0.8);
    let mut source_context = source_context;
    let mut transformed = (
        current_messages.to_vec(),
        previous_summary.map(str::to_owned),
    );
    if source_context.is_none() {
        transformed = transform_summary_source(
            current_messages,
            previous_summary,
            request.transform_context.as_ref(),
            request.signal.as_ref(),
        )
        .await?;
    }
    let with_focus =
        |base: String| match custom_instructions.filter(|instructions| !instructions.is_empty()) {
            Some(instructions) => format!("{base}\n\nAdditional focus: {instructions}"),
            None => base,
        };
    let mut base_prompt = with_focus(match (&transformed.1, source_context) {
        (Some(_), Some(_)) => SOURCE_CONTEXT_UPDATE_SUMMARIZATION_PROMPT.to_owned(),
        (Some(_), None) => update_summarization_prompt(),
        (None, _) => SUMMARIZATION_PROMPT.to_owned(),
    });
    if let Some(source) = source_context
        && !cache_friendly_context_fits(
            model,
            &build_summarization_context(&base_prompt, Some(source)),
            max_tokens,
        )
    {
        source_context = None;
        transformed = transform_summary_source(
            current_messages,
            previous_summary,
            request.transform_context.as_ref(),
            request.signal.as_ref(),
        )
        .await?;
        base_prompt = with_focus(if transformed.1.is_some() {
            update_summarization_prompt()
        } else {
            SUMMARIZATION_PROMPT.to_owned()
        });
    }

    let bounds_history = source_context.is_none() && model.context_window > 0;
    let wrappers = "<conversation>\n\n</conversation>\n\n<previous-summary>\n\n</previous-summary>\n\n";
    let prompt_tokens = (format!("{}{}{}{}", super::utils::SUMMARIZATION_SYSTEM_PROMPT, base_prompt, transformed.1.as_deref().unwrap_or_default(), wrappers).encode_utf16().count() as u64).div_ceil(4);
    let mut summarized = transformed.0.clone();
    let serialized = super::utils::serialize_conversation(&crate::messages::convert_to_llm(&summarized));
    if bounds_history && (serialized.encode_utf16().count() as u64).div_ceil(4) + prompt_tokens + max_tokens > model.context_window {
        summarized = bound_summarization_input(&summarized, model.context_window, prompt_tokens);
    }
    let started = std::time::Instant::now();
    let mut attempts = 0;
    let response = loop {
        let mut prompt_text = String::new();
        if source_context.is_none() {
            let text = super::utils::serialize_conversation(&crate::messages::convert_to_llm(&summarized));
            let omitted = transformed.0.len() - summarized.len();
            let note = if omitted > 0 { format!("<omitted-history messages=\"{omitted}\" />\n") } else { String::new() };
            prompt_text = format!("<conversation>\n{note}{text}\n</conversation>\n\n");
            if let Some(previous) = &transformed.1 { prompt_text.push_str(&format!("<previous-summary>\n{previous}\n</previous-summary>\n\n")); }
        }
        prompt_text.push_str(&base_prompt);
        let cached = source_context.is_some();
        let options = create_summarization_options(model, max_tokens, request, request_options.filter(|_| cached), cached.then_some(CacheRetention::Short));
        let response = complete_summarization(model, &build_summarization_context(&prompt_text, source_context), options, request).await?;
        if !bounds_history || !maho_ai::utils::overflow::is_context_overflow(&response, Some(model.context_window)) { break response; }
        attempts += 1;
        let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
        summarized = if allow_overflow_retry(attempts, elapsed_ms) {
            shrink_summarization_input_for_overflow_retry(&summarized, model.context_window, prompt_tokens)
        } else { None }.ok_or_else(|| SummarizationOverflowExhaustedError { attempts, elapsed_ms }.to_string())?;
    };

    if let Some(failure) = get_summarization_failure(&response, "Summarization") {
        return Err(failure);
    }
    if response
        .content
        .iter()
        .any(|block| matches!(block, ContentBlock::ToolCall(_)))
    {
        return Err("Summarization attempted to call a tool".to_owned());
    }
    let content = serde_json::to_value(&response.content).map_err(|error| error.to_string())?;
    Ok(SummaryWithUsage {
        text: super::utils::content_text_for_summary(&content, "\n"),
        usage: response.usage,
    })
}

/// senpi TURN_PREFIX_SUMMARIZATION_PROMPT (compaction-execution.ts).
pub const TURN_PREFIX_SUMMARIZATION_PROMPT: &str = "The messages above are earlier context from an ongoing conversation. Later messages are stored separately and do not need to be reconstructed.

Create a concise checkpoint of the user's request and the progress shown above. This checkpoint will be placed before the later messages so the conversation can continue with the necessary context.

## Original Request
[What did the user ask for?]

## Progress So Far
- [Key decisions and work completed in these messages]

## Context Needed to Continue
- [Information from these messages needed to understand the later work]

Only summarize information explicitly present above. Do not infer or recreate later messages.";

/// senpi SOURCE_CONTEXT_TURN_PREFIX_SUMMARIZATION_PROMPT (compaction-execution.ts).
pub const SOURCE_CONTEXT_TURN_PREFIX_SUMMARIZATION_PROMPT: &str = "The messages above are context from an ongoing conversation whose final turn is still in progress. Later messages of that turn are stored separately and do not need to be reconstructed.

The conversation may also contain complete earlier turns for background. Summarize only the final, incomplete turn. It begins with the last user-role request before this instruction. Do not summarize earlier turns except for details needed to understand the final turn.

Create a concise checkpoint of that request and the progress shown so far. This checkpoint will be placed before the later messages so the conversation can continue with the necessary context.

## Original Request
[What did the user ask for?]

## Progress So Far
- [Key decisions and work completed in these messages]

## Context Needed to Continue
- [Information from these messages needed to understand the later work]

Only summarize information explicitly present above. Do not infer or recreate later messages.";

/// senpi compact (compaction-execution.ts): generate the history (and split-turn prefix) summary
/// for a prepared compaction.
pub async fn compact(
    preparation: &CompactionPreparation,
    custom_instructions: Option<&str>,
    cache_friendly: Option<&CacheFriendlySummaryOptions>,
    request: &SummarizationRequest<'_>,
) -> Result<CompactionResult, String> {
    let mut request = request.clone();
    request.summarization_max_duration_ms = preparation.settings.summarization_max_duration_ms;
    let request = &request;
    let reserve_tokens = preparation.settings.reserve_tokens;
    let request_options = cache_friendly.and_then(|options| options.request_options.as_ref());
    let source_context = cache_friendly.and_then(|options| options.source_context.as_ref());
    let (mut summary, summary_usage) =
        if preparation.is_split_turn && !preparation.turn_prefix_messages.is_empty() {
            let mut history_text = preparation
                .previous_summary
                .clone()
                .unwrap_or_else(|| "No prior history.".to_owned());
            let mut history_usage = None;
            if !preparation.messages_to_summarize.is_empty() {
                let history = generate_summary_with_usage(
                    &preparation.messages_to_summarize,
                    reserve_tokens,
                    custom_instructions,
                    preparation.previous_summary.as_deref(),
                    source_context,
                    request_options,
                    request,
                )
                .await?;
                history_text = history.text;
                history_usage = Some(history.usage);
            }
            let turn_prefix = generate_turn_prefix_summary(
                &preparation.turn_prefix_messages,
                reserve_tokens,
                cache_friendly.and_then(|options| options.turn_prefix_source_context.as_ref()),
                request_options,
                request,
            )
            .await?;
            let summary = format!(
                "{history_text}\n\n---\n\n**Turn Context (split turn):**\n\n{}",
                turn_prefix.text
            );
            let usage = match history_usage {
                Some(history_usage) => combine_usage(&history_usage, &turn_prefix.usage),
                None => turn_prefix.usage,
            };
            (summary, usage)
        } else {
            let result = generate_summary_with_usage(
                &preparation.messages_to_summarize,
                reserve_tokens,
                custom_instructions,
                preparation.previous_summary.as_deref(),
                source_context,
                request_options,
                request,
            )
            .await?;
            (result.text, result.usage)
        };

    let (read_files, modified_files) = super::utils::compute_file_lists(&preparation.file_ops);
    summary.push_str(&super::utils::format_file_operations(
        &read_files,
        &modified_files,
    ));

    if preparation.first_kept_entry_id.is_empty() {
        return Err("First kept entry has no UUID - session may need migration".to_owned());
    }

    Ok(CompactionResult {
        summary,
        first_kept_entry_id: preparation.first_kept_entry_id.clone(),
        tokens_before: preparation.tokens_before,
        estimated_tokens_after: None,
        usage: Some(summary_usage),
        details: Some(
            serde_json::json!({ "readFiles": read_files, "modifiedFiles": modified_files }),
        ),
    })
}

/// senpi generateTurnPrefixSummary (compaction-execution.ts).
pub async fn generate_turn_prefix_summary(
    messages: &[Value],
    reserve_tokens: i64,
    source_context: Option<&Context>,
    request_options: Option<&SummaryRequestOptions>,
    request: &SummarizationRequest<'_>,
) -> Result<SummaryWithUsage, String> {
    let model = request.model;
    let max_tokens = summary_max_tokens(model, reserve_tokens, 0.5);
    let source_context = source_context.filter(|source| cache_friendly_context_fits(model,
        &build_summarization_context(SOURCE_CONTEXT_TURN_PREFIX_SUMMARIZATION_PROMPT, Some(*source)), max_tokens));
    let bounds_turn = source_context.is_none() && model.context_window > 0;
    let prompt_tokens = (format!("{}# Conversation\n\n\n# Instructions\n{}", super::utils::SUMMARIZATION_SYSTEM_PROMPT, TURN_PREFIX_SUMMARIZATION_PROMPT).encode_utf16().count() as u64).div_ceil(4);
    let provider_messages = if source_context.is_none() {
        match &request.transform_context {
            Some(transform) => transform(messages.to_vec(), request.signal.clone()).await?,
            None => messages.to_vec(),
        }
    } else { Vec::new() };
    let mut summarized = provider_messages.clone();
    let serialized = super::utils::serialize_conversation(&crate::messages::convert_to_llm(&summarized));
    if bounds_turn && (serialized.encode_utf16().count() as u64).div_ceil(4) + prompt_tokens + max_tokens > model.context_window {
        summarized = bound_summarization_input(&summarized, model.context_window, prompt_tokens);
    }
    let started = std::time::Instant::now();
    let mut attempts = 0;
    let response = loop {
        let prompt_text = if source_context.is_some() { SOURCE_CONTEXT_TURN_PREFIX_SUMMARIZATION_PROMPT.to_owned() } else {
            let text = super::utils::serialize_conversation(&crate::messages::convert_to_llm(&summarized));
            let omitted = provider_messages.len() - summarized.len();
            let note = if omitted > 0 { format!("<omitted-history messages=\"{omitted}\" />\n") } else { String::new() };
            format!("# Conversation\n{note}{text}\n\n# Instructions\n{TURN_PREFIX_SUMMARIZATION_PROMPT}")
        };
        let cached = source_context.is_some();
        let options = create_summarization_options(model, max_tokens, request, request_options.filter(|_| cached), cached.then_some(CacheRetention::Short));
        let response = complete_summarization(model, &build_summarization_context(&prompt_text, source_context), options, request).await?;
        if !bounds_turn || !maho_ai::utils::overflow::is_context_overflow(&response, Some(model.context_window)) { break response; }
        attempts += 1;
        let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
        summarized = if allow_overflow_retry(attempts, elapsed_ms) {
            shrink_summarization_input_for_overflow_retry(&summarized, model.context_window, prompt_tokens)
        } else { None }.ok_or_else(|| SummarizationOverflowExhaustedError { attempts, elapsed_ms }.to_string())?;
    };

    if let Some(failure) = get_summarization_failure(&response, "Turn prefix summarization") {
        return Err(failure);
    }
    if response
        .content
        .iter()
        .any(|block| matches!(block, ContentBlock::ToolCall(_)))
    {
        return Err("Turn prefix summarization attempted to call a tool".to_owned());
    }
    let content = serde_json::to_value(&response.content).map_err(|error| error.to_string())?;
    Ok(SummaryWithUsage {
        text: super::utils::content_text_for_summary(&content, "\n"),
        usage: response.usage,
    })
}

fn extract_file_operations(
    messages: &[Value],
    entries: &[Value],
    previous_compaction_index: Option<usize>,
) -> FileOperations {
    let mut file_ops = create_file_ops();

    if let Some(previous_compaction_index) = previous_compaction_index {
        let previous_compaction = &entries[previous_compaction_index];
        let from_hook = previous_compaction
            .get("fromHook")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !from_hook && let Some(details) = previous_compaction.get("details") {
            if let Some(read_files) = details.get("readFiles").and_then(Value::as_array) {
                for path in read_files.iter().filter_map(Value::as_str) {
                    file_ops.read.insert(path.to_string());
                }
            }
            if let Some(modified_files) = details.get("modifiedFiles").and_then(Value::as_array) {
                for path in modified_files.iter().filter_map(Value::as_str) {
                    file_ops.edited.insert(path.to_string());
                }
            }
        }
    }

    for message in messages {
        extract_file_ops_from_message(message, &mut file_ops);
    }

    file_ops
}

/// senpi CompactionPreparation.
#[derive(Debug, Clone, PartialEq)]
pub struct CompactionPreparation {
    pub first_kept_entry_id: String,
    pub messages_to_summarize: Vec<Value>,
    pub source_messages: Vec<Value>,
    pub turn_prefix_messages: Vec<Value>,
    pub turn_prefix_source_messages: Vec<Value>,
    pub is_split_turn: bool,
    pub tokens_before: i64,
    pub previous_summary: Option<String>,
    pub file_ops: FileOperations,
    pub settings: CompactionSettings,
}

/// senpi prepareCompaction.
pub fn prepare_compaction(
    path_entries: &[Value],
    settings: &CompactionSettings,
    force_progress: bool,
    allow_summary_only: bool,
) -> Option<CompactionPreparation> {
    if let Some(last) = path_entries.last()
        && entry_type(last) == Some("compaction")
    {
        return None;
    }

    let previous_compaction_index = path_entries
        .iter()
        .rposition(|entry| entry_type(entry) == Some("compaction"));

    let mut previous_summary: Option<String> = None;
    let mut boundary_start = 0usize;
    if let Some(previous_compaction_index) = previous_compaction_index {
        let previous_compaction = &path_entries[previous_compaction_index];
        previous_summary = previous_compaction
            .get("summary")
            .and_then(Value::as_str)
            .map(str::to_string);
        let first_kept_entry_id = previous_compaction
            .get("firstKeptEntryId")
            .and_then(Value::as_str);
        let first_kept_entry_index = first_kept_entry_id.and_then(|id| {
            path_entries
                .iter()
                .position(|entry| entry_id(entry).as_deref() == Some(id))
        });
        boundary_start = first_kept_entry_index.unwrap_or(previous_compaction_index + 1);
    }
    let boundary_end = path_entries.len();

    let tokens_before = estimate_projected_context_tokens(path_entries).tokens as i64;

    let mut cut_point = find_cut_point(
        path_entries,
        boundary_start,
        boundary_end,
        settings.keep_recent_tokens,
    );
    if force_progress
        && cut_point.first_kept_entry_index == boundary_start
        && let Some(next_cut_point) =
            find_valid_cut_points(path_entries, boundary_start, boundary_end)
                .get(1)
                .copied()
    {
        let turn_start_index = find_turn_start_index(path_entries, next_cut_point, boundary_start);
        cut_point = CutPointResult {
            first_kept_entry_index: next_cut_point,
            turn_start_index,
            is_split_turn: turn_start_index != -1,
        };
    }

    let first_kept_entry = path_entries.get(cut_point.first_kept_entry_index)?;
    let first_kept_entry_id = entry_id(first_kept_entry)?;

    let history_end = if cut_point.is_split_turn {
        cut_point.turn_start_index as usize
    } else {
        cut_point.first_kept_entry_index
    };

    let messages_to_summarize: Vec<Value> = (boundary_start..history_end)
        .filter_map(|index| get_message_from_entry_for_compaction(&path_entries[index]))
        .collect();

    let source_messages = collect_source_messages(
        path_entries,
        boundary_start,
        history_end,
        previous_compaction_index,
    );

    let mut turn_prefix_messages: Vec<Value> = Vec::new();
    if cut_point.is_split_turn {
        for entry in path_entries
            .iter()
            .take(cut_point.first_kept_entry_index)
            .skip(cut_point.turn_start_index as usize)
        {
            if let Some(message) = get_message_from_entry_for_compaction(entry) {
                turn_prefix_messages.push(message);
            }
        }
    }
    let turn_prefix_source_messages = if cut_point.is_split_turn {
        collect_source_messages(
            path_entries,
            boundary_start,
            cut_point.first_kept_entry_index,
            previous_compaction_index,
        )
    } else {
        Vec::new()
    };

    if messages_to_summarize.is_empty()
        && turn_prefix_messages.is_empty()
        && (previous_summary.is_none() || !allow_summary_only)
    {
        return None;
    }

    let mut file_ops = extract_file_operations(
        &messages_to_summarize,
        path_entries,
        previous_compaction_index,
    );
    if cut_point.is_split_turn {
        for message in &turn_prefix_messages {
            extract_file_ops_from_message(message, &mut file_ops);
        }
    }

    Some(CompactionPreparation {
        first_kept_entry_id,
        messages_to_summarize,
        source_messages,
        turn_prefix_messages,
        turn_prefix_source_messages,
        is_split_turn: cut_point.is_split_turn,
        tokens_before,
        previous_summary,
        file_ops,
        settings: *settings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn usage(input: u64, output: u64, total: u64) -> Value {
        json!({
            "input": input,
            "output": output,
            "cacheRead": 0,
            "cacheWrite": 0,
            "totalTokens": total,
            "cost": { "input": 0.0, "output": 0.0, "cacheRead": 0.0, "cacheWrite": 0.0, "total": 0.0 }
        })
    }

    fn message_entry(id: &str, parent: Option<&str>, message: Value) -> Value {
        json!({ "id": id, "parentId": parent, "type": "message", "message": message })
    }

    fn user(text: &str) -> Value {
        json!({ "role": "user", "content": text, "timestamp": 0 })
    }

    fn assistant_text(text: &str) -> Value {
        json!({ "role": "assistant", "content": [{ "type": "text", "text": text }], "stopReason": "stop", "usage": usage(10, 10, 20), "timestamp": 0 })
    }

    fn assistant_response(stop_reason: &str, error_message: Option<&str>) -> AssistantMessage {
        let value = json!({
            "content": [],
            "api": "anthropic-messages",
            "provider": "anthropic",
            "model": "claude",
            "usage": usage(1, 1, 2),
            "stopReason": stop_reason,
            "errorMessage": error_message,
            "timestamp": 0
        });
        serde_json::from_value(value).expect("assistant message")
    }

    #[test]
    fn request_context_usage_overrides_cumulative_billing_and_preserves_unknown() {
        let mut usage = Usage { total_tokens: 99999, input: 90000, ..Usage::default() };
        assert_eq!(calculate_context_tokens(&usage), 99999);
        usage.context_usage = Some(Some(maho_ai::types::RequestContextUsage { input: 100, output: 20 }));
        assert_eq!(calculate_context_tokens(&usage), 120);
        let parsed: Usage = serde_json::from_value(serde_json::to_value(usage).expect("serialize")).expect("parse");
        assert_eq!(calculate_context_tokens(&parsed), 120);
        usage.context_usage = Some(None);
        let parsed: Usage = serde_json::from_value(serde_json::to_value(usage).expect("serialize")).expect("parse");
        assert_eq!(calculate_context_tokens(&parsed), 0);
    }

    #[test]
    fn context_tokens_prefer_the_native_total() {
        let native = serde_json::from_value::<Usage>(usage(1, 2, 99)).expect("usage");
        assert_eq!(calculate_context_tokens(&native), 99);
        let computed = serde_json::from_value::<Usage>(usage(1, 2, 0)).expect("usage");
        assert_eq!(calculate_context_tokens(&computed), 3);
    }

    #[test]
    fn the_threshold_prefers_the_larger_side_unless_usage_is_implausible() {
        assert_eq!(resolve_threshold_context_tokens(100.0, 200.0), 200.0);
        assert_eq!(resolve_threshold_context_tokens(0.0, 0.0), 0.0);
        assert_eq!(
            resolve_threshold_context_tokens(1_500_000.0, 150_000.0),
            150_000.0
        );
        assert_eq!(
            resolve_threshold_context_tokens(900_000.0, 10_000.0),
            900_000.0
        );
        assert_eq!(
            resolve_threshold_context_tokens(400_000.0, 50_000.0),
            400_000.0
        );
    }

    #[test]
    fn usage_is_ignored_for_aborted_error_and_empty_turns() {
        let ok = json!({ "role": "assistant", "content": [], "stopReason": "stop", "usage": usage(1, 1, 2) });
        assert!(get_assistant_usage(&ok).is_some());
        let aborted = json!({ "role": "assistant", "content": [], "stopReason": "aborted", "usage": usage(1, 1, 2) });
        assert!(get_assistant_usage(&aborted).is_none());
        let errored = json!({ "role": "assistant", "content": [], "stopReason": "error", "usage": usage(1, 1, 2) });
        assert!(get_assistant_usage(&errored).is_none());
        let zero = json!({ "role": "assistant", "content": [], "stopReason": "stop", "usage": usage(0, 0, 0) });
        assert!(get_assistant_usage(&zero).is_none());
        assert!(get_assistant_usage(&user("hi")).is_none());
    }

    #[test]
    fn the_last_assistant_usage_is_taken_from_the_newest_message_entry() {
        let entries = vec![
            message_entry("a", None, assistant_text("one")),
            json!({ "id": "b", "parentId": "a", "type": "label", "targetId": "a" }),
        ];
        assert_eq!(
            get_last_assistant_usage(&entries).map(|usage| usage.total_tokens),
            Some(20)
        );
        assert!(get_last_assistant_usage(&[]).is_none());
    }

    #[test]
    fn estimation_uses_the_last_usage_plus_the_trailing_messages() {
        let messages = vec![
            user(&"x".repeat(40)),
            assistant_text("answer"),
            user(&"y".repeat(40)),
        ];
        let estimate = estimate_context_tokens(&messages);
        assert_eq!(estimate.usage_tokens, 20);
        assert_eq!(estimate.trailing_tokens, 10);
        assert_eq!(estimate.tokens, 30);
        assert_eq!(estimate.last_usage_index, Some(1));
    }

    #[test]
    fn estimation_without_usage_sums_every_counted_message() {
        let messages = vec![user(&"x".repeat(40)), user(&"y".repeat(8))];
        let estimate = estimate_context_tokens(&messages);
        assert_eq!(estimate.usage_tokens, 0);
        assert_eq!(estimate.tokens, 12);
        assert_eq!(estimate.last_usage_index, None);
    }

    #[test]
    fn a_failed_assistant_turn_and_its_orphaned_tool_result_are_not_counted() {
        let messages = vec![
            user(&"x".repeat(40)),
            json!({ "role": "assistant", "content": [{ "type": "toolCall", "id": "call_1", "name": "read", "arguments": {} }], "stopReason": "error", "usage": usage(0, 0, 0) }),
            json!({ "role": "toolResult", "toolCallId": "call_1", "content": [{ "type": "text", "text": &"z".repeat(40) }] }),
        ];
        let estimate = estimate_context_tokens(&messages);
        assert_eq!(estimate.tokens, 10);
    }

    #[test]
    fn token_estimation_covers_every_message_shape() {
        assert_eq!(estimate_tokens(&user("abcdefgh")), 2);
        assert_eq!(estimate_tokens(&assistant_text("abcdefgh")), 2);
        assert_eq!(
            estimate_tokens(
                &json!({ "role": "assistant", "content": [{ "type": "thinking", "thinking": "abcd" }, { "type": "toolCall", "name": "read", "arguments": { "path": "/a" } }] })
            ),
            6
        );
        assert_eq!(
            estimate_tokens(
                &json!({ "role": "toolResult", "content": [{ "type": "text", "text": "abcdefgh" }] })
            ),
            2
        );
        assert_eq!(
            estimate_tokens(
                &json!({ "role": "bashExecution", "command": "abcd", "output": "efgh" })
            ),
            2
        );
        assert_eq!(
            estimate_tokens(&json!({ "role": "branchSummary", "summary": "abcdefgh" })),
            2
        );
        assert_eq!(
            estimate_tokens(&json!({ "role": "compactionSummary", "summary": "abcdefgh" })),
            2
        );
        assert_eq!(
            estimate_tokens(&json!({ "role": "configurationUpdate" })),
            0
        );
    }

    #[test]
    fn failed_turns_preserve_original_anchor_indices_and_exclude_orphan_results() {
        for reason in ["error", "aborted"] {
            let anchor = assistant_text("anchor");
            let messages = vec![
                user("first"),
                json!({ "role": "assistant", "stopReason": reason, "usage": usage(100, 100, 200),
                    "content": [{ "type": "toolCall", "id": "failed", "name": "read", "arguments": {} }] }),
                anchor.clone(),
                json!({ "role": "toolResult", "toolCallId": "failed", "content": [{ "type": "text", "text": "x".repeat(4000) }] }),
                user("next"),
            ];
            let estimate = estimate_context_tokens(&messages);
            let baseline = estimate_context_tokens(&[user("first"), anchor, user("next")]);
            assert_eq!(estimate.last_usage_index, Some(2));
            assert_eq!(estimate.usage_tokens, baseline.usage_tokens);
            assert_eq!(estimate.trailing_tokens, baseline.trailing_tokens);
            assert_eq!(estimate.tokens, baseline.tokens);
        }
    }

    #[test]
    fn an_image_block_estimates_at_a_fixed_cost() {
        let message = json!({ "role": "user", "content": [{ "type": "image", "data": "x", "mimeType": "image/png" }] });
        assert_eq!(
            estimate_tokens(&message),
            (ESTIMATED_IMAGE_CHARS as u64).div_ceil(4)
        );
    }

    #[test]
    fn supplementary_text_counts_utf16_units_in_all_message_shapes() {
        let text = "\u{1f600}".repeat(5);
        for message in [
            user(&text),
            assistant_text(&text),
            json!({ "role": "assistant", "content": [{ "type": "thinking", "thinking": text }] }),
            json!({ "role": "toolResult", "content": [{ "type": "text", "text": text }] }),
            json!({ "role": "custom", "content": text }),
            json!({ "role": "bashExecution", "command": text, "output": "" }),
            json!({ "role": "branchSummary", "summary": text }),
            json!({ "role": "compactionSummary", "summary": text }),
        ] {
            assert_eq!(estimate_tokens(&message), 3, "{message}");
        }
        assert_eq!(weighted_chars(&format!("{} {text}", "A".repeat(600))), 2411);
        assert_eq!(
            estimate_tokens(&json!({ "role": "assistant", "content": [
            { "type": "toolCall", "name": text, "arguments": {} }
        ] })),
            3
        );
    }

    #[test]
    fn a_long_base64_run_is_weighted_four_times_over() {
        let run = "A".repeat(600);
        assert_eq!(weighted_chars(&run), 600 * 4);
        assert_eq!(weighted_chars("short text"), 10);
    }

    #[test]
    fn compaction_triggers_above_the_window_minus_the_scaled_reserve() {
        let mut settings = crate::compaction::settings::default_compaction_settings();
        assert!(should_compact(200_000.0, 200_000.0, &settings));
        assert!(!should_compact(100_000.0, 200_000.0, &settings));
        settings.enabled = false;
        assert!(!should_compact(200_000.0, 200_000.0, &settings));
    }

    #[test]
    fn the_reserve_scales_with_the_window_unless_scaling_is_disabled() {
        assert_eq!(resolve_reserve_tokens(200_000.0, 16_384.0), 16_384.0);
        assert_eq!(resolve_reserve_tokens(2_000_000.0, 16_384.0), 49_152.0);
        assert_eq!(
            resolve_effective_reserve_tokens(2_000_000.0, 16_384.0, Some(false)),
            16_384.0
        );
        assert_eq!(
            resolve_effective_reserve_tokens(2_000_000.0, 16_384.0, None),
            49_152.0
        );
    }

    #[test]
    fn cut_points_and_turn_starts_follow_the_message_roles() {
        assert!(is_cut_point_message(&user("hi")));
        assert!(is_cut_point_message(&assistant_text("hi")));
        assert!(!is_cut_point_message(
            &json!({ "role": "toolResult", "content": [] })
        ));
        assert!(is_turn_start_message(&user("hi")));
        assert!(!is_turn_start_message(&assistant_text("hi")));
        assert!(!is_turn_start_message(
            &json!({ "role": "toolResult", "content": [] })
        ));
    }

    #[test]
    fn the_cut_point_keeps_the_recent_tail_and_reports_a_split_turn() {
        let entries = vec![
            message_entry("a", None, user("one")),
            message_entry("b", Some("a"), assistant_text("two")),
            message_entry("c", Some("b"), user("three")),
            message_entry("d", Some("c"), assistant_text("four")),
        ];
        assert_eq!(find_valid_cut_points(&entries, 0, 4), vec![0, 1, 2, 3]);

        let cut = find_cut_point(&entries, 0, 4, 1);
        assert_eq!(cut.first_kept_entry_index, 3);
        assert_eq!(cut.turn_start_index, 2);
        assert!(cut.is_split_turn);

        let whole = find_cut_point(&entries, 0, 4, 1_000_000);
        assert_eq!(whole.first_kept_entry_index, 0);
        assert!(!whole.is_split_turn);
    }

    #[test]
    fn a_cut_that_lands_on_a_user_message_is_not_a_split_turn() {
        let entries = vec![
            message_entry("a", None, user("one")),
            message_entry("b", Some("a"), assistant_text("two")),
            message_entry("c", Some("b"), user("three")),
        ];
        let cut = find_cut_point(&entries, 0, 3, 1);
        assert_eq!(cut.first_kept_entry_index, 2);
        assert_eq!(cut.turn_start_index, -1);
        assert!(!cut.is_split_turn);
    }

    #[test]
    fn custom_message_budget_and_overflow_after_last_cut_point_keep_valid_boundaries() {
        let entries = vec![
            message_entry("a", None, user("hi")),
            message_entry("b", Some("a"), assistant_text("hello")),
            json!({ "id": "custom", "parentId": "b", "type": "custom_message", "customType": "fixture",
                "content": "x".repeat(4000), "display": true, "timestamp": "2026-01-01T00:00:00.000Z" }),
            message_entry("c", Some("custom"), assistant_text("ok")),
        ];
        for (budget, index, split, turn) in [(1, 3, true, 2), (2, 2, false, -1)] {
            let cut = find_cut_point(&entries, 0, entries.len(), budget);
            assert_eq!(
                (
                    cut.first_kept_entry_index,
                    cut.is_split_turn,
                    cut.turn_start_index
                ),
                (index, split, turn)
            );
        }
        let entries = vec![
            message_entry("a", None, user("hi")),
            message_entry("b", Some("a"), assistant_text("hello")),
            message_entry(
                "result",
                Some("b"),
                json!({ "role": "toolResult", "toolCallId": "call",
                "content": [{ "type": "text", "text": "x".repeat(8000) }] }),
            ),
        ];
        assert_eq!(
            find_cut_point(&entries, 0, entries.len(), 100).first_kept_entry_index,
            1
        );
    }

    #[test]
    fn an_empty_cut_point_range_keeps_everything() {
        let entries = vec![json!({ "id": "a", "type": "label", "targetId": "a" })];
        let cut = find_cut_point(&entries, 0, 1, 1);
        assert_eq!(cut.first_kept_entry_index, 0);
        assert!(!cut.is_split_turn);
    }

    #[test]
    fn turn_start_lookup_walks_backwards_and_reports_misses() {
        let entries = vec![
            message_entry("a", None, user("one")),
            message_entry("b", Some("a"), assistant_text("two")),
        ];
        assert_eq!(find_turn_start_index(&entries, 1, 0), 0);
        assert_eq!(find_turn_start_index(&entries, 0, 0), 0);
        assert_eq!(find_turn_start_index(&entries, 1, 1), -1);
    }

    #[test]
    fn summarization_failures_name_the_label_and_the_cause() {
        let response = assistant_response("error", Some("boom"));
        assert_eq!(
            get_summarization_failure(&response, "Compaction"),
            Some("Compaction failed: boom".to_string())
        );
        let response = assistant_response("length", None);
        assert_eq!(
            get_summarization_failure(&response, "Compaction"),
            Some(
                "Compaction failed: generation hit the token cap and the summary is incomplete"
                    .to_string()
            )
        );
        let response = assistant_response("stop", None);
        assert_eq!(get_summarization_failure(&response, "Compaction"), None);
    }

    #[test]
    fn usage_combination_adds_every_component() {
        let first = serde_json::from_value::<Usage>(usage(1, 2, 3)).expect("usage");
        let second = serde_json::from_value::<Usage>(usage(4, 5, 6)).expect("usage");
        let combined = combine_usage(&first, &second);
        assert_eq!(combined.input, 5);
        assert_eq!(combined.output, 7);
        assert_eq!(combined.total_tokens, 9);
        assert_eq!(combined.cache_write_1h, None);
        assert_eq!(combined.reasoning, None);
    }

    #[test]
    fn the_update_prompt_embeds_the_shared_instructions() {
        let prompt = update_summarization_prompt();
        assert!(prompt.starts_with("The messages above are NEW conversation messages to incorporate into the existing summary provided in <previous-summary> tags."));
        assert!(prompt.ends_with(UPDATE_SUMMARIZATION_INSTRUCTIONS));
        assert!(
            SOURCE_CONTEXT_UPDATE_SUMMARIZATION_PROMPT.ends_with(UPDATE_SUMMARIZATION_INSTRUCTIONS)
        );
    }

    #[test]
    fn preparation_splits_a_turn_and_collects_file_operations() {
        let entries = vec![
            message_entry("a", None, user("one")),
            message_entry(
                "b",
                Some("a"),
                json!({ "role": "assistant", "content": [{ "type": "toolCall", "name": "edit", "arguments": { "path": "/edited" } }], "stopReason": "toolUse", "usage": usage(1, 1, 2), "timestamp": 0 }),
            ),
            message_entry("c", Some("b"), user("three")),
            message_entry("d", Some("c"), assistant_text("four")),
        ];
        let settings = {
            let mut settings = crate::compaction::settings::default_compaction_settings();
            settings.keep_recent_tokens = 1;
            settings
        };
        let preparation =
            prepare_compaction(&entries, &settings, false, false).expect("preparation");
        assert_eq!(preparation.first_kept_entry_id, "d");
        assert!(preparation.is_split_turn);
        assert_eq!(preparation.messages_to_summarize.len(), 2);
        assert_eq!(preparation.turn_prefix_messages.len(), 1);
        assert!(preparation.file_ops.edited.contains("/edited"));
        assert!(preparation.tokens_before > 0);
        assert_eq!(preparation.previous_summary, None);
    }

    #[test]
    fn preparation_stops_when_nothing_would_be_summarized() {
        let entries = vec![message_entry("a", None, user("one"))];
        let settings = crate::compaction::settings::default_compaction_settings();
        assert!(prepare_compaction(&entries, &settings, false, false).is_none());
    }

    #[test]
    fn preparation_does_not_repeat_a_summary_when_the_retained_window_still_fits() {
        let entries = vec![
            message_entry("a", None, user("old")),
            message_entry("b", Some("a"), user("retained")),
            message_entry("c", Some("b"), assistant_text("retained response")),
            json!({ "id": "summary", "parentId": "c", "type": "compaction", "summary": "digest",
                "firstKeptEntryId": "b", "tokensBefore": 100, "timestamp": "2026-01-01T00:00:00.000Z" }),
            message_entry("d", Some("summary"), user("new")),
        ];
        let settings = crate::compaction::settings::default_compaction_settings();
        assert!(prepare_compaction(&entries, &settings, false, false).is_none());
    }

    #[test]
    fn source_prefix_keeps_older_summary_after_the_latest_summary_without_duplicating_it() {
        let entries = vec![
            message_entry("a", None, user("first")),
            json!({ "id": "old", "parentId": "a", "type": "compaction", "summary": "older",
                "firstKeptEntryId": "a", "tokensBefore": 100, "timestamp": "2026-01-01T00:00:00.000Z" }),
            message_entry("b", Some("old"), user("second")),
            json!({ "id": "latest", "parentId": "b", "type": "compaction", "summary": "newer",
                "firstKeptEntryId": "a", "tokensBefore": 100, "timestamp": "2026-01-01T00:00:00.000Z" }),
        ];
        let messages = collect_source_messages(&entries, 0, entries.len(), Some(3));
        let summaries = messages
            .iter()
            .filter(|message| {
                message.get("role").and_then(Value::as_str) == Some("compactionSummary")
            })
            .map(|message| message.get("summary").and_then(Value::as_str))
            .collect::<Vec<_>>();
        assert_eq!(summaries, [Some("newer"), Some("older")]);
        assert_eq!(messages.last(), Some(&user("second")));
    }

    #[test]
    fn preparation_ignores_a_path_that_already_ends_in_a_compaction() {
        let entries = vec![
            message_entry("a", None, user("one")),
            json!({ "id": "c", "parentId": "a", "type": "compaction", "summary": "digest", "firstKeptEntryId": "a", "tokensBefore": 1, "timestamp": "2026-01-01T00:00:00.000Z" }),
        ];
        let settings = crate::compaction::settings::default_compaction_settings();
        assert!(prepare_compaction(&entries, &settings, false, false).is_none());
    }

    fn summary_model() -> Model {
        serde_json::from_value(json!({
            "id": "claude", "name": "claude", "api": "anthropic-messages", "provider": "anthropic",
            "baseUrl": "", "reasoning": true, "input": [], "contextWindow": 200000, "maxTokens": 4096,
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
            "compat": { "allowedFallbackModels": ["claude-fallback"] }
        })).expect("model")
    }

    type Captured = Mutex<Vec<(Context, SimpleStreamOptions)>>;

    fn canned_stream(
        captured: &Captured,
        texts: &Mutex<Vec<&'static str>>,
        context: &Context,
        options: SimpleStreamOptions,
    ) -> AssistantMessageEventStream {
        captured
            .lock()
            .expect("captured")
            .push((context.clone(), options));
        let mut message = assistant_response("stop", None);
        message.content = vec![ContentBlock::Text(TextContent {
            text: texts.lock().expect("texts").remove(0).to_owned(),
            ..Default::default()
        })];
        let stream = AssistantMessageEventStream::assistant();
        stream.push(maho_ai::types::AssistantMessageEvent::Done {
            reason: maho_ai::types::DoneReason::Stop,
            message,
        });
        stream.end(None);
        stream
    }

    fn last_prompt(context: &Context) -> String {
        match context.messages.last() {
            Some(Message::User(UserMessage {
                content: UserContent::Blocks(blocks),
                ..
            })) => match blocks.first() {
                Some(ContentBlock::Text(text)) => text.text.clone(),
                _ => String::new(),
            },
            _ => String::new(),
        }
    }

    #[test]
    fn summarization_options_carry_reasoning_fallback_and_affinity() {
        let model = summary_model();
        let mut request = SummarizationRequest {
            model: &model,
            api_key: Some("key".into()),
            headers: None,
            env: None,
            signal: None,
            extra_body: None,
            thinking_level: Some(ModelThinkingLevel::High),
            stream_fn: None,
            transform_context: None,
            retry: None,
            callbacks: None,
            session_id: Some("session".into()),
            summarization_max_duration_ms: None,
        };
        let options = create_summarization_options(&model, 100, &request, None, None);
        assert_eq!(options.reasoning, Some(ThinkingLevel::High));
        assert_eq!(
            options.refusal_fallbacks,
            Some(AnthropicRefusalFallback::Models(vec![
                RefusalFallbackModel {
                    model: "claude-fallback".into()
                },
            ]))
        );
        assert_eq!(
            options.stream.request.affinity_session_id.as_deref(),
            Some("session")
        );
        assert_eq!(options.stream.max_tokens, Some(100));
        request.thinking_level = Some(ModelThinkingLevel::Off);
        assert_eq!(
            create_summarization_options(&model, 100, &request, None, None).reasoning,
            None
        );
    }

    #[tokio::test]
    async fn summary_and_turn_prefix_retry_overflow_with_less_history() {
        let mut model = summary_model();
        model.context_window = 4000;
        for prefix in [false, true] {
            let captured: Captured = Mutex::new(Vec::new());
            let count = std::sync::atomic::AtomicUsize::new(0);
            let stream_fn = |_: &Model, context: &Context, options: SimpleStreamOptions| {
                captured.lock().expect("captured").push((context.clone(), options));
                let first = count.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0;
                let mut message = assistant_response(if first { "error" } else { "stop" }, if first { Some("maximum context length exceeded") } else { None });
                message.content = vec![ContentBlock::Text(TextContent { text: "digest".into(), ..Default::default() })];
                let stream = AssistantMessageEventStream::assistant();
                if first { stream.push(maho_ai::types::AssistantMessageEvent::Error { reason: maho_ai::types::ErrorReason::Error, error: message }); }
                else { stream.push(maho_ai::types::AssistantMessageEvent::Done { reason: maho_ai::types::DoneReason::Stop, message }); }
                stream.end(None);
                stream
            };
            let request = SummarizationRequest {
                model: &model, api_key: None, headers: None, env: None, signal: None, extra_body: None, thinking_level: None,
                stream_fn: Some(&stream_fn as &SummarizationStreamFn<'_>), transform_context: None, retry: None, callbacks: None,
                session_id: None, summarization_max_duration_ms: None,
            };
            let messages: Vec<Value> = (0..30).map(|_| assistant_text(&"x".repeat(1000))).collect();
            let result = if prefix { generate_turn_prefix_summary(&messages, 1000, None, None, &request).await }
                else { generate_summary_with_usage(&messages, 1000, None, None, None, None, &request).await };
            assert!(result.is_ok(), "{result:?}");
            let calls = captured.lock().expect("captured");
            assert_eq!(calls.len(), 2);
            assert!(last_prompt(&calls[1].0).len() < last_prompt(&calls[0].0).len());
            assert!(last_prompt(&calls[0].0).contains("<omitted-history messages="));
        }
    }

    #[tokio::test]
    async fn split_turn_compaction_merges_history_and_cache_friendly_prefix_summaries() {
        let model = summary_model();
        let captured: Captured = Mutex::new(Vec::new());
        let texts = Mutex::new(vec!["history digest", "prefix digest"]);
        let stream_fn = |_: &Model, context: &Context, options: SimpleStreamOptions| {
            canned_stream(&captured, &texts, context, options)
        };
        let request = SummarizationRequest {
            model: &model,
            api_key: None,
            headers: None,
            env: None,
            signal: None,
            extra_body: None,
            thinking_level: None,
            stream_fn: Some(&stream_fn as &SummarizationStreamFn<'_>),
            transform_context: None,
            retry: None,
            callbacks: None,
            session_id: None,
            summarization_max_duration_ms: None,
        };
        let preparation = CompactionPreparation {
            first_kept_entry_id: "kept".into(),
            messages_to_summarize: vec![user("old task")],
            source_messages: Vec::new(),
            turn_prefix_messages: vec![user("turn start")],
            turn_prefix_source_messages: Vec::new(),
            is_split_turn: true,
            tokens_before: 10,
            previous_summary: None,
            file_ops: create_file_ops(),
            settings: crate::compaction::settings::default_compaction_settings(),
        };
        let prefix_source = Context {
            active_tool_names: None,
            system_prompt: Some("system".into()),
            messages: Vec::new(),
            tools: None,
        };
        let cache_friendly = CacheFriendlySummaryOptions {
            source_context: None,
            turn_prefix_source_context: Some(prefix_source),
            request_options: Some(SummaryRequestOptions {
                session_id: Some("agent-session".into()),
                ..Default::default()
            }),
        };
        let result = compact(&preparation, None, Some(&cache_friendly), &request)
            .await
            .expect("compaction");
        assert!(result.summary.starts_with(
            "history digest\n\n---\n\n**Turn Context (split turn):**\n\nprefix digest"
        ));
        let calls = captured.lock().expect("captured");
        assert_eq!(calls.len(), 2);
        assert!(last_prompt(&calls[0].0).starts_with("<conversation>"));
        assert_eq!(
            calls[0].1.stream.cache_retention,
            Some(CacheRetention::None)
        );
        assert_eq!(calls[1].0.system_prompt.as_deref(), Some("system"));
        assert_eq!(
            last_prompt(&calls[1].0),
            SOURCE_CONTEXT_TURN_PREFIX_SUMMARIZATION_PROMPT
        );
        assert_eq!(
            calls[1].1.stream.cache_retention,
            Some(CacheRetention::Short)
        );
        assert_eq!(
            calls[1].1.stream.request.affinity_session_id.as_deref(),
            Some("agent-session")
        );
    }

    #[tokio::test]
    async fn transformed_previous_summary_reaches_the_update_prompt() {
        let model = summary_model();
        let captured: Captured = Mutex::new(Vec::new());
        let texts = Mutex::new(vec!["updated digest"]);
        let stream_fn = |_: &Model, context: &Context, options: SimpleStreamOptions| {
            canned_stream(&captured, &texts, context, options)
        };
        let transform: SummaryTransformContext = Arc::new(
            |messages: Vec<Value>,
             _: Option<AbortSignal>|
             -> BoxFuture<'static, Result<Vec<Value>, String>> {
                Box::pin(async move {
                    Ok(messages
                        .into_iter()
                        .map(|mut message| {
                            if let Some(text) = message.pointer_mut("/content/0/text") {
                                *text = json!("redacted summary");
                            }
                            message
                        })
                        .collect())
                })
            },
        );
        let request = SummarizationRequest {
            model: &model,
            api_key: None,
            headers: None,
            env: None,
            signal: None,
            extra_body: None,
            thinking_level: None,
            stream_fn: Some(&stream_fn as &SummarizationStreamFn<'_>),
            transform_context: Some(transform),
            retry: None,
            callbacks: None,
            session_id: None,
            summarization_max_duration_ms: None,
        };
        let summary = generate_summary(
            &[user("new work")],
            1000,
            None,
            Some("secret summary"),
            None,
            &request,
        )
        .await
        .expect("summary");
        assert_eq!(summary, "updated digest");
        let prompt = last_prompt(&captured.lock().expect("captured")[0].0);
        assert!(
            prompt.contains("<previous-summary>\n[User]: redacted summary\n</previous-summary>"),
            "{prompt}"
        );
        assert!(!prompt.contains("secret summary"));
        assert!(prompt.contains("NEW conversation messages"));
    }
}
