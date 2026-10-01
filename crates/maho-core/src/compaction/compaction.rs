//! Port of senpi packages/coding-agent/src/core/compaction/compaction.ts.
//!
//! The pure core is ported whole: token accounting, cut-point selection, the summarization
//! prompts and prepareCompaction. The provider-calling functions (createSummarizationOptions,
//! completeSummarization, generateSummary, generateSummaryWithUsage, transformSummarySource,
//! cacheFriendlyContextFits) and the compact() driver in compaction-execution.ts stay with the
//! AgentSession wiring (todo 21) and are recorded as partial in the parity ledger.

use std::sync::LazyLock;

use maho_ai::types::{AssistantMessage, StopReason, Usage, UsageCost};
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
    context_messages_for_compaction_entry(entry).into_iter().next()
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
        messages.extend(session_entry_to_context_messages(&entries[previous_compaction_index]));
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
    Usage {
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
    if usage.total_tokens != 0 {
        usage.total_tokens
    } else {
        usage.input + usage.output + usage.cache_read + usage.cache_write
    }
}

/// senpi resolveThresholdContextTokens.
pub fn resolve_threshold_context_tokens(usage_tokens: f64, estimate_tokens: f64) -> f64 {
    let usage = if usage_tokens > 0.0 { usage_tokens } else { 0.0 };
    let estimate = if estimate_tokens > 0.0 { estimate_tokens } else { 0.0 };
    if estimate >= 50_000.0 && usage > estimate * 8.0 {
        return estimate;
    }
    usage.max(estimate)
}

fn usage_from_value(value: &Value) -> Option<Usage> {
    serde_json::from_value(value.clone()).ok()
}

/// senpi getAssistantUsage.
pub fn get_assistant_usage(message: &Value) -> Option<Usage> {
    if message_role(message) != Some("assistant") {
        return None;
    }
    let stop_reason = message.get("stopReason").and_then(Value::as_str).unwrap_or_default();
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
        let failed = matches!(message.get("stopReason").and_then(Value::as_str), Some("error") | Some("aborted"));
        let declared = if failed { &mut failed_call_ids } else { &mut kept_call_ids };
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
            Some("assistant") => !matches!(message.get("stopReason").and_then(Value::as_str), Some("error") | Some("aborted")),
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
        return ContextUsageEstimate { tokens: estimated, usage_tokens: 0, trailing_tokens: estimated, last_usage_index: None };
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

/// senpi policy.ts RESERVE_WINDOW_FRACTION.
pub const RESERVE_WINDOW_FRACTION: f64 = 0.04;
/// senpi policy.ts MAX_SCALED_RESERVE_TOKENS.
pub const MAX_SCALED_RESERVE_TOKENS: f64 = 49_152.0;

/// senpi policy.ts resolveReserveTokens.
pub fn resolve_reserve_tokens(context_window: f64, configured_reserve: f64) -> f64 {
    configured_reserve.max((RESERVE_WINDOW_FRACTION * context_window).floor().min(MAX_SCALED_RESERVE_TOKENS))
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
pub fn should_compact(context_tokens: f64, context_window: f64, settings: &CompactionSettings) -> bool {
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
    let mut chars = text.chars().count();
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
                Some("text") => block.get("text").and_then(Value::as_str).map(weighted_chars).unwrap_or(0),
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
        Some("user") => estimate_text_and_image_content_chars(message.get("content").unwrap_or(&Value::Null)),
        Some("assistant") => {
            let mut chars = 0usize;
            if let Some(blocks) = message.get("content").and_then(Value::as_array) {
                for block in blocks {
                    match block.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            chars += block.get("text").and_then(Value::as_str).map(|text| text.chars().count()).unwrap_or(0);
                        }
                        Some("thinking") => {
                            chars += block.get("thinking").and_then(Value::as_str).map(|text| text.chars().count()).unwrap_or(0);
                        }
                        Some("toolCall") => {
                            let name = block.get("name").and_then(Value::as_str).unwrap_or_default();
                            let arguments = block.get("arguments").map(|arguments| serde_json::to_string(arguments).unwrap_or_default()).unwrap_or_default();
                            chars += name.chars().count() + weighted_chars(&arguments);
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
            let command = message.get("command").and_then(Value::as_str).unwrap_or_default();
            let output = message.get("output").and_then(Value::as_str).unwrap_or_default();
            command.chars().count() + weighted_chars(output)
        }
        Some("branchSummary") | Some("compactionSummary") => {
            message.get("summary").and_then(Value::as_str).map(|summary| summary.chars().count()).unwrap_or(0)
        }
        _ => 0,
    };
    chars.div_ceil(4) as u64
}

/// senpi isCutPointMessage.
pub fn is_cut_point_message(message: &Value) -> bool {
    matches!(
        message_role(message),
        Some("user") | Some("assistant") | Some("bashExecution") | Some("custom") | Some("branchSummary") | Some("compactionSummary")
    )
}

/// senpi isTurnStartMessage.
pub fn is_turn_start_message(message: &Value) -> bool {
    matches!(
        message_role(message),
        Some("user") | Some("bashExecution") | Some("custom") | Some("branchSummary") | Some("compactionSummary")
    )
}

fn is_turn_start_entry(entry: &Value) -> bool {
    if entry_type(entry) == Some("compaction") {
        return false;
    }
    context_messages_for_compaction_entry(entry).iter().any(is_turn_start_message)
}

/// senpi findValidCutPoints.
pub fn find_valid_cut_points(entries: &[Value], start_index: usize, end_index: usize) -> Vec<usize> {
    let mut cut_points: Vec<usize> = Vec::new();
    for (index, entry) in entries.iter().enumerate().take(end_index.min(entries.len())).skip(start_index) {
        if entry_type(entry) == Some("compaction") {
            continue;
        }
        if context_messages_for_compaction_entry(entry).iter().any(is_cut_point_message) {
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
pub fn find_cut_point(entries: &[Value], start_index: usize, end_index: usize, keep_recent_tokens: i64) -> CutPointResult {
    let cut_points = find_valid_cut_points(entries, start_index, end_index);

    if cut_points.is_empty() {
        return CutPointResult { first_kept_entry_index: start_index, turn_start_index: -1, is_split_turn: false };
    }

    let mut accumulated_tokens: i64 = 0;
    let mut cut_index = cut_points[0];

    let mut index = end_index as i64 - 1;
    while index >= start_index as i64 {
        let entry = &entries[index as usize];
        let message_tokens: i64 = context_messages_for_compaction_entry(entry).iter().map(|message| estimate_tokens(message) as i64).sum();
        if message_tokens != 0 {
            accumulated_tokens += message_tokens;
            if accumulated_tokens >= keep_recent_tokens {
                match cut_points.iter().find(|cut_point| **cut_point >= index as usize) {
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
        if entry_type(previous) == Some("compaction") || !context_messages_for_compaction_entry(previous).is_empty() {
            break;
        }
        if entry_type(previous) == Some("custom_message")
            && previous.get("customType").and_then(Value::as_str).is_some_and(is_context_excluded_custom_message)
        {
            break;
        }
        cut_index -= 1;
    }

    let cut_entry = &entries[cut_index];
    let starts_turn = is_turn_start_entry(cut_entry);
    let turn_start_index = if starts_turn { -1 } else { find_turn_start_index(entries, cut_index, start_index) };

    CutPointResult { first_kept_entry_index: cut_index, turn_start_index, is_split_turn: !starts_turn && turn_start_index != -1 }
}

/// senpi getSummarizationFailure.
pub fn get_summarization_failure(response: &AssistantMessage, label: &str) -> Option<String> {
    if response.stop_reason == StopReason::Error {
        let message = response.error_message.clone().unwrap_or_else(|| "Unknown error".to_string());
        return Some(format!("{label} failed: {message}"));
    }
    if response.stop_reason == StopReason::Length {
        return Some(format!("{label} failed: generation hit the token cap and the summary is incomplete"));
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
pub const UPDATE_SUMMARIZATION_INSTRUCTIONS: &str = "Update the existing structured summary with new information. RULES:
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

fn extract_file_operations(messages: &[Value], entries: &[Value], previous_compaction_index: Option<usize>) -> FileOperations {
    let mut file_ops = create_file_ops();

    if let Some(previous_compaction_index) = previous_compaction_index {
        let previous_compaction = &entries[previous_compaction_index];
        let from_hook = previous_compaction.get("fromHook").and_then(Value::as_bool).unwrap_or(false);
        if !from_hook
            && let Some(details) = previous_compaction.get("details")
        {
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

    let previous_compaction_index = path_entries.iter().rposition(|entry| entry_type(entry) == Some("compaction"));

    let mut previous_summary: Option<String> = None;
    let mut boundary_start = 0usize;
    if let Some(previous_compaction_index) = previous_compaction_index {
        let previous_compaction = &path_entries[previous_compaction_index];
        previous_summary = previous_compaction.get("summary").and_then(Value::as_str).map(str::to_string);
        let first_kept_entry_id = previous_compaction.get("firstKeptEntryId").and_then(Value::as_str);
        let first_kept_entry_index = first_kept_entry_id
            .and_then(|id| path_entries.iter().position(|entry| entry_id(entry).as_deref() == Some(id)));
        boundary_start = first_kept_entry_index.unwrap_or(previous_compaction_index + 1);
    }
    let boundary_end = path_entries.len();

    let tokens_before = estimate_context_tokens(&filter_context_excluded_messages(
        build_session_context(path_entries, None).messages,
    ))
    .tokens as i64;

    let mut cut_point = find_cut_point(path_entries, boundary_start, boundary_end, settings.keep_recent_tokens);
    if force_progress
        && cut_point.first_kept_entry_index == boundary_start
        && let Some(next_cut_point) = find_valid_cut_points(path_entries, boundary_start + 1, boundary_end).first().copied()
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

    let history_end = if cut_point.is_split_turn { cut_point.turn_start_index as usize } else { cut_point.first_kept_entry_index };

    let messages_to_summarize: Vec<Value> = (boundary_start..history_end)
        .filter_map(|index| get_message_from_entry_for_compaction(&path_entries[index]))
        .collect();

    let source_messages = collect_source_messages(path_entries, boundary_start, history_end, previous_compaction_index);

    let mut turn_prefix_messages: Vec<Value> = Vec::new();
    if cut_point.is_split_turn {
        for entry in path_entries.iter().take(cut_point.first_kept_entry_index).skip(cut_point.turn_start_index as usize) {
            if let Some(message) = get_message_from_entry_for_compaction(entry) {
                turn_prefix_messages.push(message);
            }
        }
    }
    let turn_prefix_source_messages = if cut_point.is_split_turn {
        collect_source_messages(path_entries, boundary_start, cut_point.first_kept_entry_index, previous_compaction_index)
    } else {
        Vec::new()
    };

    if messages_to_summarize.is_empty()
        && turn_prefix_messages.is_empty()
        && (previous_summary.is_none() || !allow_summary_only)
    {
        return None;
    }

    let mut file_ops = extract_file_operations(&messages_to_summarize, path_entries, previous_compaction_index);
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
        assert_eq!(resolve_threshold_context_tokens(1_500_000.0, 150_000.0), 150_000.0);
        assert_eq!(resolve_threshold_context_tokens(900_000.0, 10_000.0), 900_000.0);
        assert_eq!(resolve_threshold_context_tokens(400_000.0, 50_000.0), 400_000.0);
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
        assert_eq!(get_last_assistant_usage(&entries).map(|usage| usage.total_tokens), Some(20));
        assert!(get_last_assistant_usage(&[]).is_none());
    }

    #[test]
    fn estimation_uses_the_last_usage_plus_the_trailing_messages() {
        let messages = vec![user(&"x".repeat(40)), assistant_text("answer"), user(&"y".repeat(40))];
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
        assert_eq!(estimate_tokens(&json!({ "role": "assistant", "content": [{ "type": "thinking", "thinking": "abcd" }, { "type": "toolCall", "name": "read", "arguments": { "path": "/a" } }] })), 6);
        assert_eq!(estimate_tokens(&json!({ "role": "toolResult", "content": [{ "type": "text", "text": "abcdefgh" }] })), 2);
        assert_eq!(estimate_tokens(&json!({ "role": "bashExecution", "command": "abcd", "output": "efgh" })), 2);
        assert_eq!(estimate_tokens(&json!({ "role": "branchSummary", "summary": "abcdefgh" })), 2);
        assert_eq!(estimate_tokens(&json!({ "role": "compactionSummary", "summary": "abcdefgh" })), 2);
        assert_eq!(estimate_tokens(&json!({ "role": "configurationUpdate" })), 0);
    }

    #[test]
    fn an_image_block_estimates_at_a_fixed_cost() {
        let message = json!({ "role": "user", "content": [{ "type": "image", "data": "x", "mimeType": "image/png" }] });
        assert_eq!(estimate_tokens(&message), (ESTIMATED_IMAGE_CHARS as u64).div_ceil(4));
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
        assert_eq!(resolve_effective_reserve_tokens(2_000_000.0, 16_384.0, Some(false)), 16_384.0);
        assert_eq!(resolve_effective_reserve_tokens(2_000_000.0, 16_384.0, None), 49_152.0);
    }

    #[test]
    fn cut_points_and_turn_starts_follow_the_message_roles() {
        assert!(is_cut_point_message(&user("hi")));
        assert!(is_cut_point_message(&assistant_text("hi")));
        assert!(!is_cut_point_message(&json!({ "role": "toolResult", "content": [] })));
        assert!(is_turn_start_message(&user("hi")));
        assert!(!is_turn_start_message(&assistant_text("hi")));
        assert!(!is_turn_start_message(&json!({ "role": "toolResult", "content": [] })));
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
        assert_eq!(get_summarization_failure(&response, "Compaction"), Some("Compaction failed: boom".to_string()));
        let response = assistant_response("length", None);
        assert_eq!(
            get_summarization_failure(&response, "Compaction"),
            Some("Compaction failed: generation hit the token cap and the summary is incomplete".to_string())
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
        assert!(SOURCE_CONTEXT_UPDATE_SUMMARIZATION_PROMPT.ends_with(UPDATE_SUMMARIZATION_INSTRUCTIONS));
    }

    #[test]
    fn preparation_splits_a_turn_and_collects_file_operations() {
        let entries = vec![
            message_entry("a", None, user("one")),
            message_entry("b", Some("a"), json!({ "role": "assistant", "content": [{ "type": "toolCall", "name": "edit", "arguments": { "path": "/edited" } }], "stopReason": "toolUse", "usage": usage(1, 1, 2), "timestamp": 0 })),
            message_entry("c", Some("b"), user("three")),
            message_entry("d", Some("c"), assistant_text("four")),
        ];
        let settings = {
            let mut settings = crate::compaction::settings::default_compaction_settings();
            settings.keep_recent_tokens = 1;
            settings
        };
        let preparation = prepare_compaction(&entries, &settings, false, false).expect("preparation");
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
    fn preparation_ignores_a_path_that_already_ends_in_a_compaction() {
        let entries = vec![
            message_entry("a", None, user("one")),
            json!({ "id": "c", "parentId": "a", "type": "compaction", "summary": "digest", "firstKeptEntryId": "a", "tokensBefore": 1, "timestamp": "2026-01-01T00:00:00.000Z" }),
        ];
        let settings = crate::compaction::settings::default_compaction_settings();
        assert!(prepare_compaction(&entries, &settings, false, false).is_none());
    }
}
