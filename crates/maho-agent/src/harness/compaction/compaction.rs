//! Port of senpi packages/agent/src/harness/compaction/compaction.ts.


use maho_ai::models::{Models, ModelsRequestTransforms};
use maho_ai::types::{
    AssistantMessage, BoxFuture, CacheRetention, ContentBlock, Context as AiContext, Message, SimpleStreamOptions,
    StopReason, Usage, UserContent,
};
use maho_ai::utils::lazy::setup_error_message;
use maho_ai::utils::retry::{RetryCallbacks, RetryPolicy, retry_assistant_call};
use maho_ai::utils::uuid::uuidv7;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::harness::context::Context;
use crate::harness::messages::{convert_to_llm, create_branch_summary_message, create_compaction_summary_message};
use crate::harness::session::context::{build_context_entries, session_entry_to_context_messages};
use crate::harness::session::types::{Entry, EntryKind, JsonValue};
use crate::harness::utils::usage::add_usage;
use crate::types::{AgentMessage, CustomAgentMessage};

pub use super::utils::serialize_conversation;
use super::utils::{
    FileOperations, compute_file_lists, content_text_for_summary, create_file_ops, extract_file_ops_from_message,
    format_file_operations,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionErrorCode {
    Aborted,
    SummarizationFailed,
}

impl CompactionErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            CompactionErrorCode::Aborted => "aborted",
            CompactionErrorCode::SummarizationFailed => "summarization_failed",
        }
    }
}

/// Error returned by compaction helpers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionError {
    pub code: CompactionErrorCode,
    pub message: String,
}

impl CompactionError {
    pub fn new(code: CompactionErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for CompactionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for CompactionError {}

/// File-operation details stored on generated compaction entries.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionDetails {
    pub read_files: Vec<String>,
    pub modified_files: Vec<String>,
}

fn safe_json_stringify(value: &Value) -> String {
    value.to_string()
}

fn extract_file_operations(
    messages: &[AgentMessage],
    entries: &[Entry],
    prev_compaction_index: Option<usize>,
) -> FileOperations {
    let mut file_ops = create_file_ops();
    if let Some(index) = prev_compaction_index
        && let EntryKind::Compaction { details, .. } = &entries[index].kind
            && let Some(details) = details {
                if let Some(read) = details.get("readFiles").and_then(Value::as_array) {
                    for path in read {
                        if let Some(path) = path.as_str() {
                            file_ops.read.insert(path.to_owned());
                        }
                    }
                }
                if let Some(modified) = details.get("modifiedFiles").and_then(Value::as_array) {
                    for path in modified {
                        if let Some(path) = path.as_str() {
                            file_ops.edited.insert(path.to_owned());
                        }
                    }
                }
            }
    for message in messages {
        extract_file_ops_from_message(message, &mut file_ops);
    }
    file_ops
}

fn get_message_from_entry(entry: &Entry) -> Option<AgentMessage> {
    match &entry.kind {
        EntryKind::Message { message, .. } => Some(message.clone()),
        EntryKind::BranchSummary { from_id, summary, .. } => Some(AgentMessage::Custom(
            CustomAgentMessage::BranchSummary(create_branch_summary_message(
                summary.clone(),
                from_id.clone(),
                entry.timestamp,
            )),
        )),
        EntryKind::Compaction {
            summary,
            tokens_before,
            ..
        } => Some(AgentMessage::Custom(CustomAgentMessage::CompactionSummary(
            create_compaction_summary_message(summary.clone(), *tokens_before, entry.timestamp),
        ))),
        EntryKind::Custom { .. } => None,
    }
}

fn get_message_from_entry_for_compaction(entry: &Entry) -> Option<AgentMessage> {
    if matches!(entry.kind, EntryKind::Compaction { .. }) {
        return None;
    }
    get_message_from_entry(entry)
}

/// Generated compaction data ready to be persisted as a compaction entry.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactResult {
    pub summary: String,
    pub tokens_before: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    pub retained_tail: Vec<AgentMessage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<JsonValue>,
}

/// One summarization provider request owned by the caller.
pub trait SummaryRequest: Send + Sync {
    fn request<'a>(
        &'a self,
        ai_context: &'a AiContext,
        options: SimpleStreamOptions,
        context: &'a Context,
    ) -> BoxFuture<'a, AssistantMessage>;
}

/// `completeSimpleWithRetries` as a request: the harness's own summarization boundary.
pub struct ModelSummaryRequest<'a> {
    pub models: &'a Models,
    pub model: &'a maho_ai::model::Model,
    pub retry: Option<&'a RetryPolicy>,
    pub callbacks: Option<&'a RetryCallbacks<'a>>,
}

impl SummaryRequest for ModelSummaryRequest<'_> {
    fn request<'a>(
        &'a self,
        ai_context: &'a AiContext,
        options: SimpleStreamOptions,
        context: &'a Context,
    ) -> BoxFuture<'a, AssistantMessage> {
        Box::pin(async move {
            complete_simple_with_retries(
                self.models,
                self.model,
                ai_context,
                options,
                self.retry,
                self.callbacks,
                context,
            )
            .await
        })
    }
}

/// Adapter for callers and tests that supply a closure instead of a provider collection.
pub struct SummaryRequestFn<F>(pub F);

impl<F> SummaryRequest for SummaryRequestFn<F>
where
    F: Fn(&AiContext, SimpleStreamOptions, &Context) -> BoxFuture<'static, AssistantMessage> + Send + Sync,
{
    fn request<'a>(
        &'a self,
        ai_context: &'a AiContext,
        options: SimpleStreamOptions,
        context: &'a Context,
    ) -> BoxFuture<'a, AssistantMessage> {
        (self.0)(ai_context, options, context)
    }
}

pub fn create_summary_request_options(options: SimpleStreamOptions, context: &Context) -> SimpleStreamOptions {
    let mut options = options;
    options.stream.request.signal = context.abort_signal();
    options.stream.request.telemetry_context = None;
    options.stream.cache_retention = Some(CacheRetention::None);
    if options.stream.session_id.is_none() {
        options.stream.session_id = uuidv7(None).ok();
    }
    options
}

/// Summaries are standalone requests, so isolate routing and avoid cache writes that cannot be reused.
pub async fn complete_simple_with_retries(
    models: &Models,
    model: &maho_ai::model::Model,
    ai_context: &AiContext,
    options: SimpleStreamOptions,
    retry: Option<&RetryPolicy>,
    callbacks: Option<&RetryCallbacks<'_>>,
    context: &Context,
) -> AssistantMessage {
    let request_options = create_summary_request_options(options, context);
    let signal = request_options.stream.request.signal.clone();
    retry_assistant_call(
        || async {
            match models
                .complete_simple(model, ai_context, Some(request_options.clone()), ModelsRequestTransforms::default())
                .await
            {
                Ok(message) => message,
                Err(error) => {
                    let mut message = setup_error_message(model, "");
                    message.stop_reason = StopReason::Error;
                    message.error_message = Some(error.to_string());
                    message
                }
            }
        },
        retry,
        signal.as_ref(),
        callbacks,
    )
    .await
}

/// Compaction thresholds and retention settings.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionSettings {
    pub enabled: bool,
    pub reserve_tokens: u64,
    pub keep_recent_tokens: u64,
}

/// Default compaction settings used by the harness.
pub const DEFAULT_COMPACTION_SETTINGS: CompactionSettings = CompactionSettings {
    enabled: true,
    reserve_tokens: 16384,
    keep_recent_tokens: 20000,
};

/// Calculate total context tokens from provider usage.
pub fn calculate_context_tokens(usage: &Usage) -> u64 {
    if usage.total_tokens > 0 {
        usage.total_tokens
    } else {
        usage.input + usage.output + usage.cache_read + usage.cache_write
    }
}

fn get_assistant_usage(message: &AgentMessage) -> Option<Usage> {
    let AgentMessage::Llm(Message::Assistant(assistant)) = message else {
        return None;
    };
    if assistant.stop_reason != StopReason::Aborted
        && assistant.stop_reason != StopReason::Error
        && calculate_context_tokens(&assistant.usage) > 0
    {
        Some(assistant.usage)
    } else {
        None
    }
}

/// Return usage from the last valid assistant message in session entries.
pub fn get_last_assistant_usage(entries: &[Entry]) -> Option<Usage> {
    for entry in entries.iter().rev() {
        if let EntryKind::Message { message, .. } = &entry.kind
            && let Some(usage) = get_assistant_usage(message) {
                return Some(usage);
            }
    }
    None
}

/// Estimated context-token usage for a message list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextUsageEstimate {
    pub tokens: u64,
    pub usage_tokens: u64,
    pub trailing_tokens: u64,
    pub last_usage_index: Option<usize>,
}

fn is_failed_stop(stop_reason: StopReason) -> bool {
    matches!(stop_reason, StopReason::Error | StopReason::Aborted)
}

/// The same set the next provider request will carry: convertToLlm drops failed (error/aborted)
/// assistant turns and their orphaned tool results.
fn counted_mask(messages: &[AgentMessage]) -> Vec<bool> {
    let mut kept_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut failed_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    for message in messages {
        if let AgentMessage::Llm(Message::Assistant(assistant)) = message {
            let declared = if is_failed_stop(assistant.stop_reason) {
                &mut failed_ids
            } else {
                &mut kept_ids
            };
            for block in &assistant.content {
                if let ContentBlock::ToolCall(call) = block {
                    declared.insert(call.id.clone());
                }
            }
        }
    }
    for id in &kept_ids {
        failed_ids.remove(id);
    }
    messages
        .iter()
        .map(|message| match message {
            AgentMessage::Llm(Message::Assistant(assistant)) => !is_failed_stop(assistant.stop_reason),
            AgentMessage::Llm(Message::ToolResult(result)) => !failed_ids.contains(&result.tool_call_id),
            _ => true,
        })
        .collect()
}

fn get_last_assistant_usage_info(
    messages: &[AgentMessage],
    counted: &[bool],
) -> Option<(Usage, usize)> {
    for (index, message) in messages.iter().enumerate().rev() {
        if !counted[index] {
            continue;
        }
        if let Some(usage) = get_assistant_usage(message) {
            return Some((usage, index));
        }
    }
    None
}

/// Estimate context tokens for messages using provider usage when available.
pub fn estimate_context_tokens(messages: &[AgentMessage]) -> ContextUsageEstimate {
    let counted = counted_mask(messages);
    let usage_info = get_last_assistant_usage_info(messages, &counted);

    let Some((usage, index)) = usage_info else {
        let mut estimated = 0;
        for (position, message) in messages.iter().enumerate() {
            if counted[position] {
                estimated += estimate_tokens(message);
            }
        }
        return ContextUsageEstimate {
            tokens: estimated,
            usage_tokens: 0,
            trailing_tokens: estimated,
            last_usage_index: None,
        };
    };

    let usage_tokens = calculate_context_tokens(&usage);
    let mut trailing_tokens = 0;
    for position in index + 1..messages.len() {
        if counted[position] {
            trailing_tokens += estimate_tokens(&messages[position]);
        }
    }

    ContextUsageEstimate {
        tokens: usage_tokens + trailing_tokens,
        usage_tokens,
        trailing_tokens,
        last_usage_index: Some(index),
    }
}

/// Return whether context usage exceeds the configured compaction threshold.
pub fn should_compact(context_tokens: u64, context_window: u64, settings: &CompactionSettings) -> bool {
    if !settings.enabled {
        return false;
    }
    context_tokens > context_window.saturating_sub(settings.reserve_tokens)
}

const ESTIMATED_IMAGE_CHARS: usize = 4800;

fn estimate_text_and_image_content_chars(content: &UserContent) -> usize {
    match content {
        UserContent::Text(text) => text.chars().count(),
        UserContent::Blocks(blocks) => blocks
            .iter()
            .map(|block| match block {
                ContentBlock::Text(text) => text.text.chars().count(),
                ContentBlock::Image(_) => ESTIMATED_IMAGE_CHARS,
                _ => 0,
            })
            .sum(),
    }
}

fn estimate_blocks_chars(blocks: &[ContentBlock]) -> usize {
    blocks
        .iter()
        .map(|block| match block {
            ContentBlock::Text(text) => text.text.chars().count(),
            ContentBlock::Image(_) => ESTIMATED_IMAGE_CHARS,
            _ => 0,
        })
        .sum()
}

/// Estimate token count for one message using a conservative character heuristic.
pub fn estimate_tokens(message: &AgentMessage) -> u64 {
    let chars = match message {
        AgentMessage::Llm(Message::User(user)) => {
            return ceil_div(estimate_text_and_image_content_chars(&user.content), 4);
        }
        AgentMessage::Llm(Message::Assistant(assistant)) => {
            let mut chars = 0;
            for block in &assistant.content {
                match block {
                    ContentBlock::Text(text) => chars += text.text.chars().count(),
                    ContentBlock::Thinking(thinking) => chars += thinking.thinking.chars().count(),
                    ContentBlock::ToolCall(call) => {
                        chars += call.name.chars().count() + safe_json_stringify(&Value::Object(call.arguments.clone())).chars().count();
                    }
                    _ => {}
                }
            }
            return ceil_div(chars, 4);
        }
        AgentMessage::Llm(Message::ToolResult(result)) => {
            return ceil_div(estimate_blocks_chars(&result.content), 4);
        }
        AgentMessage::Llm(Message::ConfigurationUpdate(_)) => return 0,
        AgentMessage::Custom(CustomAgentMessage::BashExecution(bash)) => {
            bash.command.chars().count() + bash.output.chars().count()
        }
        AgentMessage::Custom(CustomAgentMessage::Custom(custom)) => match &custom.content {
            crate::harness::messages::CustomMessageContent::Text(text) => text.chars().count(),
            crate::harness::messages::CustomMessageContent::Blocks(blocks) => estimate_blocks_chars(blocks),
        },
        AgentMessage::Custom(CustomAgentMessage::BranchSummary(branch)) => branch.summary.chars().count(),
        AgentMessage::Custom(CustomAgentMessage::CompactionSummary(compaction)) => {
            compaction.summary.chars().count()
        }
    };
    ceil_div(chars, 4)
}

fn ceil_div(value: usize, divisor: usize) -> u64 {
    value.div_ceil(divisor) as u64
}

fn find_valid_cut_points(entries: &[Entry], start_index: usize, end_index: usize) -> Vec<usize> {
    let mut cut_points: Vec<usize> = Vec::new();
    for (index, entry) in entries.iter().enumerate().take(end_index).skip(start_index) {
        match &entry.kind {
            EntryKind::Message { message, .. } => {
                let role = message.role();
                if matches!(
                    role,
                    "bashExecution" | "custom" | "branchSummary" | "compactionSummary" | "user" | "assistant"
                ) {
                    cut_points.push(index);
                }
            }
            EntryKind::Compaction { .. } | EntryKind::BranchSummary { .. } | EntryKind::Custom { .. } => {}
        }
        if matches!(entry.kind, EntryKind::BranchSummary { .. }) {
            cut_points.push(index);
        }
    }
    cut_points
}

/// Find the user-visible message that starts the turn containing an entry.
pub fn find_turn_start_index(entries: &[Entry], entry_index: usize, start_index: usize) -> i64 {
    if entry_index < start_index {
        return -1;
    }
    for index in (start_index..=entry_index).rev() {
        let entry = &entries[index];
        if matches!(entry.kind, EntryKind::BranchSummary { .. }) {
            return index as i64;
        }
        if let EntryKind::Message { message, .. } = &entry.kind
            && matches!(message.role(), "user" | "bashExecution") {
                return index as i64;
            }
    }
    -1
}

/// Cut point selected for compaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CutPointResult {
    pub first_kept_entry_index: usize,
    pub turn_start_index: i64,
    pub is_split_turn: bool,
}

/// Find the compaction cut point that keeps approximately the requested recent-token budget.
pub fn find_cut_point(
    entries: &[Entry],
    start_index: usize,
    end_index: usize,
    keep_recent_tokens: u64,
) -> CutPointResult {
    let cut_points = find_valid_cut_points(entries, start_index, end_index);

    if cut_points.is_empty() {
        return CutPointResult {
            first_kept_entry_index: start_index,
            turn_start_index: -1,
            is_split_turn: false,
        };
    }
    let mut accumulated_tokens: u64 = 0;
    let mut cut_index = cut_points[0];

    if end_index > start_index {
        for index in (start_index..end_index).rev() {
            let entry = &entries[index];
            let EntryKind::Message { message, .. } = &entry.kind else {
                continue;
            };
            accumulated_tokens += estimate_tokens(message);
            if accumulated_tokens >= keep_recent_tokens {
                let mut found_cut_point = false;
                for candidate in &cut_points {
                    if *candidate >= index {
                        cut_index = *candidate;
                        found_cut_point = true;
                        break;
                    }
                }
                if !found_cut_point {
                    cut_index = cut_points[cut_points.len() - 1];
                }
                break;
            }
        }
    }
    while cut_index > start_index {
        let prev_entry = &entries[cut_index - 1];
        if matches!(prev_entry.kind, EntryKind::Compaction { .. }) {
            break;
        }
        if matches!(prev_entry.kind, EntryKind::Message { .. }) {
            break;
        }
        cut_index -= 1;
    }
    let cut_entry = &entries[cut_index];
    let is_user_message = matches!(&cut_entry.kind, EntryKind::Message { message, .. } if message.role() == "user");
    let turn_start_index = if is_user_message {
        -1
    } else {
        find_turn_start_index(entries, cut_index, start_index)
    };

    CutPointResult {
        first_kept_entry_index: cut_index,
        turn_start_index,
        is_split_turn: !is_user_message && turn_start_index != -1,
    }
}

pub const SUMMARIZATION_SYSTEM_PROMPT: &str = "You are a context summarization assistant. Your task is to read a conversation between a user and an AI assistant, then produce a structured summary following the exact format specified.

Do NOT continue the conversation. Do NOT respond to any questions in the conversation. ONLY output the structured summary.";

const SUMMARIZATION_PROMPT: &str = "The messages above are a conversation to summarize. Create a structured context checkpoint summary that another LLM will use to continue the work.

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

const UPDATE_SUMMARIZATION_PROMPT: &str = "The messages above are NEW conversation messages to incorporate into the existing summary provided in <previous-summary> tags.

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

/// Generate or update a conversation summary for compaction.
#[allow(clippy::too_many_arguments)]
pub async fn generate_summary(
    current_messages: &[AgentMessage],
    models: &Models,
    model: &maho_ai::model::Model,
    reserve_tokens: u64,
    custom_instructions: Option<&str>,
    previous_summary: Option<&str>,
    thinking_level: Option<maho_ai::types::ThinkingLevel>,
    retry: Option<&RetryPolicy>,
    callbacks: Option<&RetryCallbacks<'_>>,
    context: &Context,
) -> Result<String, CompactionError> {
    let result = generate_summary_with_usage(
        current_messages,
        models,
        model,
        reserve_tokens,
        custom_instructions,
        previous_summary,
        thinking_level,
        retry,
        callbacks,
        context,
    )
    .await?;
    Ok(result.0)
}

/// Generate or update a conversation summary and return its provider usage.
#[allow(clippy::too_many_arguments)]
pub async fn generate_summary_with_usage(
    current_messages: &[AgentMessage],
    models: &Models,
    model: &maho_ai::model::Model,
    reserve_tokens: u64,
    custom_instructions: Option<&str>,
    previous_summary: Option<&str>,
    thinking_level: Option<maho_ai::types::ThinkingLevel>,
    retry: Option<&RetryPolicy>,
    callbacks: Option<&RetryCallbacks<'_>>,
    context: &Context,
) -> Result<(String, Usage), CompactionError> {
    let options = SummaryGenerationOptions {
        model: model.clone(),
        reserve_tokens,
        custom_instructions: custom_instructions.map(str::to_owned),
        previous_summary: previous_summary.map(str::to_owned),
        thinking_level,
    };
    let request = ModelSummaryRequest {
        models,
        model,
        retry,
        callbacks,
    };
    generate_summary_with_request(current_messages, &options, &request, context).await
}

#[derive(Debug, Clone)]
pub struct SummaryGenerationOptions {
    pub model: maho_ai::model::Model,
    pub reserve_tokens: u64,
    pub custom_instructions: Option<String>,
    pub previous_summary: Option<String>,
    pub thinking_level: Option<maho_ai::types::ThinkingLevel>,
}

/// Generate one summary through a caller-owned one-request boundary.
pub async fn generate_summary_with_request(
    current_messages: &[AgentMessage],
    options: &SummaryGenerationOptions,
    request: &dyn SummaryRequest,
    context: &Context,
) -> Result<(String, Usage), CompactionError> {
    let max_tokens = (options.reserve_tokens * 4 / 5).min(if options.model.max_tokens > 0 {
        options.model.max_tokens
    } else {
        u64::MAX
    });
    let mut base_prompt = if options.previous_summary.is_some() {
        UPDATE_SUMMARIZATION_PROMPT
    } else {
        SUMMARIZATION_PROMPT
    };
    let custom_prompt;
    if let Some(custom_instructions) = &options.custom_instructions {
        custom_prompt = format!("{base_prompt}\n\nAdditional focus: {custom_instructions}");
        base_prompt = &custom_prompt;
    }
    let llm_messages = convert_to_llm(current_messages.to_vec());
    let conversation_text = serialize_conversation(&llm_messages);
    let mut prompt_text = format!("<conversation>\n{conversation_text}\n</conversation>\n\n");
    if let Some(previous_summary) = &options.previous_summary {
        prompt_text.push_str(&format!(
            "<previous-summary>\n{previous_summary}\n</previous-summary>\n\n"
        ));
    }
    prompt_text.push_str(base_prompt);

    let summarization_messages = vec![Message::User(maho_ai::types::UserMessage {
        content: UserContent::Blocks(vec![ContentBlock::text(prompt_text)]),
        timestamp: now_ms(),
    })];

    let mut completion_options = SimpleStreamOptions::default();
    completion_options.stream.max_tokens = Some(max_tokens);
    if options.model.reasoning
        && let Some(thinking_level) = options.thinking_level {
            completion_options.reasoning = Some(thinking_level);
        }

    let response = request
        .request(
        &AiContext {
            system_prompt: Some(SUMMARIZATION_SYSTEM_PROMPT.to_owned()),
            messages: summarization_messages,
            tools: None,
        },
        create_summary_request_options(completion_options, context),
        context,
        )
        .await;

    if response.stop_reason == StopReason::Aborted {
        return Err(CompactionError::new(
            CompactionErrorCode::Aborted,
            non_empty(response.error_message.as_deref(), "Summarization aborted"),
        ));
    }
    if response.stop_reason == StopReason::Error {
        return Err(CompactionError::new(
            CompactionErrorCode::SummarizationFailed,
            format!(
                "Summarization failed: {}",
                non_empty(response.error_message.as_deref(), "Unknown error")
            ),
        ));
    }

    let text = content_text_for_summary(&response.content, "\n");

    Ok((text, response.usage))
}

fn non_empty(value: Option<&str>, fallback: &str) -> String {
    match value {
        Some(value) if !value.is_empty() => value.to_owned(),
        _ => fallback.to_owned(),
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or_default()
}

/// Prepared inputs for a compaction run.
#[derive(Debug, Clone)]
pub struct CompactionPreparation {
    pub messages_to_summarize: Vec<AgentMessage>,
    pub turn_prefix_messages: Vec<AgentMessage>,
    pub retained_tail: Vec<AgentMessage>,
    pub is_split_turn: bool,
    pub tokens_before: i64,
    pub previous_summary: Option<String>,
    pub file_ops: FileOperations,
    pub settings: CompactionSettings,
}

/// Prepare session entries for compaction, or return None when compaction is not applicable.
pub fn prepare_compaction(
    path_entries: &[Entry],
    settings: CompactionSettings,
) -> Result<Option<CompactionPreparation>, CompactionError> {
    if path_entries.is_empty()
        || matches!(
            path_entries[path_entries.len() - 1].kind,
            EntryKind::Compaction { .. }
        )
    {
        return Ok(None);
    }

    let mut prev_compaction_index: Option<usize> = None;
    for index in (0..path_entries.len()).rev() {
        if matches!(path_entries[index].kind, EntryKind::Compaction { .. }) {
            prev_compaction_index = Some(index);
            break;
        }
    }

    let mut previous_summary: Option<String> = None;
    let mut compactable_entries: Vec<Entry> = path_entries.to_vec();
    if let Some(index) = prev_compaction_index {
        let prev_compaction = &path_entries[index];
        if let EntryKind::Compaction {
            summary, retained_tail, ..
        } = &prev_compaction.kind
        {
            previous_summary = Some(summary.clone());
            let mut virtual_retained: Vec<Entry> = Vec::new();
            for (position, message) in retained_tail.iter().enumerate() {
                virtual_retained.push(Entry {
                    id: format!("{}:retained:{position}", prev_compaction.id),
                    parent_id: Some(if position == 0 {
                        prev_compaction.id.clone()
                    } else {
                        format!("{}:retained:{}", prev_compaction.id, position - 1)
                    }),
                    seq: prev_compaction.seq,
                    timestamp: message_timestamp(message),
                    kind: EntryKind::Message {
                        message: message.clone(),
                        terminate: None,
                    },
                });
            }
            compactable_entries = virtual_retained;
            compactable_entries.extend(path_entries[index + 1..].iter().cloned());
        }
    }
    let boundary_end = compactable_entries.len();

    let context_messages: Vec<AgentMessage> = build_context_entries(path_entries)
        .iter()
        .flat_map(session_entry_to_context_messages)
        .collect();
    let tokens_before = estimate_context_tokens(&context_messages).tokens as i64;

    let cut_point = find_cut_point(&compactable_entries, 0, boundary_end, settings.keep_recent_tokens);
    let history_end = if cut_point.is_split_turn {
        cut_point.turn_start_index.max(0) as usize
    } else {
        cut_point.first_kept_entry_index
    };
    let mut messages_to_summarize: Vec<AgentMessage> = Vec::new();
    for entry in compactable_entries.iter().take(history_end) {
        if let Some(message) = get_message_from_entry_for_compaction(entry) {
            messages_to_summarize.push(message);
        }
    }
    let mut turn_prefix_messages: Vec<AgentMessage> = Vec::new();
    if cut_point.is_split_turn {
        for entry in compactable_entries
            .iter()
            .take(cut_point.first_kept_entry_index)
            .skip(cut_point.turn_start_index.max(0) as usize)
        {
            if let Some(message) = get_message_from_entry_for_compaction(entry) {
                turn_prefix_messages.push(message);
            }
        }
    }
    let mut retained_tail: Vec<AgentMessage> = Vec::new();
    for entry in compactable_entries
        .iter()
        .take(boundary_end)
        .skip(cut_point.first_kept_entry_index)
    {
        if let Some(message) = get_message_from_entry_for_compaction(entry) {
            retained_tail.push(message);
        }
    }
    let mut file_ops = extract_file_operations(&messages_to_summarize, path_entries, prev_compaction_index);
    if cut_point.is_split_turn {
        for message in &turn_prefix_messages {
            extract_file_ops_from_message(message, &mut file_ops);
        }
    }

    Ok(Some(CompactionPreparation {
        messages_to_summarize,
        turn_prefix_messages,
        retained_tail,
        is_split_turn: cut_point.is_split_turn,
        tokens_before,
        previous_summary,
        file_ops,
        settings,
    }))
}

fn message_timestamp(message: &AgentMessage) -> i64 {
    match message {
        AgentMessage::Llm(Message::User(user)) => user.timestamp,
        AgentMessage::Llm(Message::Assistant(assistant)) => assistant.timestamp,
        AgentMessage::Llm(Message::ToolResult(result)) => result.timestamp,
        AgentMessage::Llm(Message::ConfigurationUpdate(update)) => update.timestamp,
        AgentMessage::Custom(CustomAgentMessage::BashExecution(bash)) => bash.timestamp,
        AgentMessage::Custom(CustomAgentMessage::Custom(custom)) => custom.timestamp,
        AgentMessage::Custom(CustomAgentMessage::BranchSummary(branch)) => branch.timestamp,
        AgentMessage::Custom(CustomAgentMessage::CompactionSummary(compaction)) => compaction.timestamp,
    }
}

const TURN_PREFIX_SUMMARIZATION_PROMPT: &str = "This is the PREFIX of a turn that was too large to keep. The SUFFIX (recent work) is retained.

Summarize the prefix to provide context for the retained suffix:

## Original Request
[What did the user ask for in this turn?]

## Early Progress
- [Key decisions and work done in the prefix]

## Context for Suffix
- [Information needed to understand the retained recent work]

Be concise. Focus on what's needed to understand the kept suffix.";

/// Generate compaction summary data from prepared session history.
#[allow(clippy::too_many_arguments)]
pub async fn compact(
    preparation: &CompactionPreparation,
    models: &Models,
    model: &maho_ai::model::Model,
    custom_instructions: Option<&str>,
    thinking_level: Option<maho_ai::types::ThinkingLevel>,
    retry: Option<&RetryPolicy>,
    callbacks: Option<&RetryCallbacks<'_>>,
    context: &Context,
) -> Result<CompactResult, CompactionError> {
    let request = ModelSummaryRequest {
        models,
        model,
        retry,
        callbacks,
    };
    compact_with_request(
        preparation,
        &CompactGenerationOptions {
            model: model.clone(),
            custom_instructions: custom_instructions.map(str::to_owned),
            thinking_level,
        },
        &request,
        context,
    )
    .await
}

#[derive(Debug, Clone)]
pub struct CompactGenerationOptions {
    pub model: maho_ai::model::Model,
    pub custom_instructions: Option<String>,
    pub thinking_level: Option<maho_ai::types::ThinkingLevel>,
}

/// Generate compaction data through a caller-owned boundary for each provider request.
pub async fn compact_with_request(
    preparation: &CompactionPreparation,
    options: &CompactGenerationOptions,
    request: &dyn SummaryRequest,
    context: &Context,
) -> Result<CompactResult, CompactionError> {
    let CompactionPreparation {
        messages_to_summarize,
        turn_prefix_messages,
        retained_tail,
        is_split_turn,
        tokens_before,
        previous_summary,
        file_ops,
        settings,
    } = preparation;

    let summary: String;
    let summary_usage: Usage;

    if *is_split_turn && !turn_prefix_messages.is_empty() {
        let mut history_text = "No prior history.".to_owned();
        let mut history_usage: Option<Usage> = None;
        if !messages_to_summarize.is_empty() {
            let history_result = generate_summary_with_request(
                messages_to_summarize,
                &SummaryGenerationOptions {
                    model: options.model.clone(),
                    reserve_tokens: settings.reserve_tokens,
                    custom_instructions: options.custom_instructions.clone(),
                    previous_summary: previous_summary.clone(),
                    thinking_level: options.thinking_level,
                },
                request,
                context,
            )
            .await?;
            history_text = history_result.0;
            history_usage = Some(history_result.1);
        }
        let turn_prefix_result = generate_turn_prefix_summary(
            turn_prefix_messages,
            &options.model,
            settings.reserve_tokens,
            options.thinking_level,
            request,
            context,
        )
        .await?;
        summary = format!(
            "{history_text}\n\n---\n\n**Turn Context (split turn):**\n\n{}",
            turn_prefix_result.0
        );
        summary_usage = match history_usage {
            Some(history_usage) => add_usage(&history_usage, &turn_prefix_result.1),
            None => turn_prefix_result.1,
        };
    } else {
        let summary_result = generate_summary_with_request(
            messages_to_summarize,
            &SummaryGenerationOptions {
                model: options.model.clone(),
                reserve_tokens: settings.reserve_tokens,
                custom_instructions: options.custom_instructions.clone(),
                previous_summary: previous_summary.clone(),
                thinking_level: options.thinking_level,
            },
            request,
            context,
        )
        .await?;
        summary = summary_result.0;
        summary_usage = summary_result.1;
    }

    let (read_files, modified_files) = compute_file_lists(file_ops);
    let mut summary = summary;
    summary.push_str(&format_file_operations(&read_files, &modified_files));
    let details = serde_json::json!({
        "readFiles": read_files,
        "modifiedFiles": modified_files,
    });

    Ok(CompactResult {
        summary,
        tokens_before: *tokens_before,
        usage: Some(summary_usage),
        retained_tail: retained_tail.clone(),
        details: Some(details),
    })
}

async fn generate_turn_prefix_summary(
    messages: &[AgentMessage],
    model: &maho_ai::model::Model,
    reserve_tokens: u64,
    thinking_level: Option<maho_ai::types::ThinkingLevel>,
    request: &dyn SummaryRequest,
    context: &Context,
) -> Result<(String, Usage), CompactionError> {
    let max_tokens = (reserve_tokens / 2).min(if model.max_tokens > 0 {
        model.max_tokens
    } else {
        u64::MAX
    });
    let llm_messages = convert_to_llm(messages.to_vec());
    let conversation_text = serialize_conversation(&llm_messages);
    let prompt_text = format!("<conversation>\n{conversation_text}\n</conversation>\n\n{TURN_PREFIX_SUMMARIZATION_PROMPT}");
    let summarization_messages = vec![Message::User(maho_ai::types::UserMessage {
        content: UserContent::Blocks(vec![ContentBlock::text(prompt_text)]),
        timestamp: now_ms(),
    })];

    let mut completion_options = SimpleStreamOptions::default();
    completion_options.stream.max_tokens = Some(max_tokens);
    if model.reasoning
        && let Some(thinking_level) = thinking_level {
            completion_options.reasoning = Some(thinking_level);
        }
    let response = request
        .request(
        &AiContext {
            system_prompt: Some(SUMMARIZATION_SYSTEM_PROMPT.to_owned()),
            messages: summarization_messages,
            tools: None,
        },
        create_summary_request_options(completion_options, context),
        context,
        )
        .await;
    if response.stop_reason == StopReason::Aborted {
        return Err(CompactionError::new(
            CompactionErrorCode::Aborted,
            non_empty(response.error_message.as_deref(), "Turn prefix summarization aborted"),
        ));
    }
    if response.stop_reason == StopReason::Error {
        return Err(CompactionError::new(
            CompactionErrorCode::SummarizationFailed,
            format!(
                "Turn prefix summarization failed: {}",
                non_empty(response.error_message.as_deref(), "Unknown error")
            ),
        ));
    }

    Ok((content_text_for_summary(&response.content, "\n"), response.usage))
}
