//! Port of senpi packages/agent/src/harness/compaction/branch-summarization.ts.


use maho_ai::models::Models;
use maho_ai::types::{BoxFuture, ContentBlock, Message, SimpleStreamOptions, StopReason, Usage, UserContent};
use maho_ai::utils::retry::{RetryCallbacks, RetryPolicy};

use crate::harness::context::Context;
use crate::harness::messages::convert_to_llm;
use crate::harness::session::types::{Branch, Entry, EntryKind, Session};

use super::compaction::{
    ModelSummaryRequest, SummaryRequest, create_summary_request_options, estimate_tokens,
    SUMMARIZATION_SYSTEM_PROMPT,
};
use super::utils::{
    FileOperations, compute_file_lists, content_text_for_summary, create_file_ops, extract_file_ops_from_message,
    format_file_operations, serialize_conversation,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BranchSummaryErrorCode {
    Aborted,
    SummarizationFailed,
}

impl BranchSummaryErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            BranchSummaryErrorCode::Aborted => "aborted",
            BranchSummaryErrorCode::SummarizationFailed => "summarization_failed",
        }
    }
}

/// Error returned by branch summarization helpers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchSummaryError {
    pub code: BranchSummaryErrorCode,
    pub message: String,
}

impl BranchSummaryError {
    pub fn new(code: BranchSummaryErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for BranchSummaryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for BranchSummaryError {}

/// Generated branch summary data ready to be persisted as a branch-summary entry.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchSummaryResult {
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    pub read_files: Vec<String>,
    pub modified_files: Vec<String>,
}

/// File-operation details stored on generated branch summary entries.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchSummaryDetails {
    pub read_files: Vec<String>,
    pub modified_files: Vec<String>,
}

/// Prepared branch content for summarization.
#[derive(Debug, Clone)]
pub struct BranchPreparation {
    pub messages: Vec<crate::types::AgentMessage>,
    pub file_ops: FileOperations,
    pub total_tokens: i64,
}

/// Entries selected for branch summarization.
#[derive(Debug, Clone, PartialEq)]
pub struct CollectEntriesResult {
    pub entries: Vec<Entry>,
    pub common_ancestor_id: Option<String>,
}

/// Options for generating a branch summary.
pub struct GenerateBranchSummaryOptions {
    pub models: Models,
    pub model: maho_ai::model::Model,
    pub custom_instructions: Option<String>,
    pub replace_instructions: bool,
    pub reserve_tokens: u64,
    pub retry: Option<RetryPolicy>,
    pub callbacks: Option<RetryCallbacks<'static>>,
}

/// Collect entries that should be summarized before navigating to a different session tree entry.
pub async fn collect_entries_for_branch_summary(
    branch: &dyn Branch,
    session: &dyn Session,
    old_tip_id: Option<&str>,
    target_id: &str,
    context: &Context,
) -> Result<CollectEntriesResult, crate::harness::session::SessionError> {
    let Some(old_tip_id) = old_tip_id else {
        return Ok(CollectEntriesResult {
            entries: Vec::new(),
            common_ancestor_id: None,
        });
    };
    let old_path: std::collections::HashSet<String> = branch
        .find_entries(
            Some(crate::harness::session::types::BranchScan {
                start: Some(old_tip_id.to_owned()),
                ..Default::default()
            }),
            context,
        )
        .await?
        .into_iter()
        .map(|entry| entry.id)
        .collect();
    let target_path = branch
        .find_entries(
            Some(crate::harness::session::types::BranchScan {
                start: Some(target_id.to_owned()),
                ..Default::default()
            }),
            context,
        )
        .await?;
    let mut common_ancestor_id: Option<String> = None;
    for entry in &target_path {
        if old_path.contains(&entry.id) {
            common_ancestor_id = Some(entry.id.clone());
            break;
        }
    }
    let mut entries: Vec<Entry> = Vec::new();
    let mut current = Some(old_tip_id.to_owned());

    while let Some(id) = current.clone() {
        if Some(id.clone()) == common_ancestor_id {
            break;
        }
        let Some(entry) = session.get_entry(&id, context).await? else {
            return Err(crate::harness::session::session_invariant_error(format!(
                "Corrupt session: entry {id} not found"
            )));
        };
        entries.push(entry.clone());
        current = entry.parent_id;
    }
    entries.reverse();

    Ok(CollectEntriesResult {
        entries,
        common_ancestor_id,
    })
}

fn get_message_from_entry(entry: &Entry) -> Option<crate::types::AgentMessage> {
    match &entry.kind {
        EntryKind::Message { message, .. } => {
            if message.role() == "toolResult" {
                return None;
            }
            Some(message.clone())
        }
        EntryKind::BranchSummary { from_id, summary, .. } => Some(crate::types::AgentMessage::Custom(
            crate::types::CustomAgentMessage::BranchSummary(crate::harness::messages::create_branch_summary_message(
                summary.clone(),
                from_id.clone(),
                entry.timestamp,
            )),
        )),
        EntryKind::Compaction {
            summary,
            tokens_before,
            ..
        } => Some(crate::types::AgentMessage::Custom(
            crate::types::CustomAgentMessage::CompactionSummary(
                crate::harness::messages::create_compaction_summary_message(
                    summary.clone(),
                    *tokens_before,
                    entry.timestamp,
                ),
            ),
        )),
        EntryKind::Custom { .. } => None,
    }
}

/// Prepare branch entries for summarization within an optional token budget.
pub fn prepare_branch_entries(entries: &[Entry], token_budget: u64) -> BranchPreparation {
    let mut messages: Vec<crate::types::AgentMessage> = Vec::new();
    let mut file_ops = create_file_ops();
    let mut total_tokens: u64 = 0;
    for entry in entries {
        if !matches!(entry.kind, EntryKind::BranchSummary { .. }) {
            continue;
        }
        let Some(details) = entry.kind.details() else {
            continue;
        };
        if let Some(read) = details.get("readFiles").and_then(serde_json::Value::as_array) {
            for path in read {
                if let Some(path) = path.as_str() {
                    file_ops.read.insert(path.to_owned());
                }
            }
        }
        if let Some(modified) = details.get("modifiedFiles").and_then(serde_json::Value::as_array) {
            for path in modified {
                if let Some(path) = path.as_str() {
                    file_ops.edited.insert(path.to_owned());
                }
            }
        }
    }
    for entry in entries.iter().rev() {
        let Some(message) = get_message_from_entry(entry) else {
            continue;
        };
        extract_file_ops_from_message(&message, &mut file_ops);

        let tokens = estimate_tokens(&message);
        if token_budget > 0 && total_tokens + tokens > token_budget {
            if matches!(entry.kind, EntryKind::Compaction { .. } | EntryKind::BranchSummary { .. })
                && total_tokens < token_budget * 9 / 10
            {
                messages.insert(0, message);
                total_tokens += tokens;
            }
            break;
        }

        messages.insert(0, message);
        total_tokens += tokens;
    }

    BranchPreparation {
        messages,
        file_ops,
        total_tokens: total_tokens as i64,
    }
}

const BRANCH_SUMMARY_PREAMBLE: &str = "The user explored a different conversation branch before returning here.
Summary of that exploration:

";

const BRANCH_SUMMARY_PROMPT: &str = "Create a structured summary of this conversation branch for context when returning later.

Use this EXACT format:

## Goal
[What was the user trying to accomplish in this branch?]

## Constraints & Preferences
- [Any constraints, preferences, or requirements mentioned]
- [Or \"(none)\" if none were mentioned]

## Progress
### Done
- [x] [Completed tasks/changes]

### In Progress
- [ ] [Work that was started but not finished]

### Blocked
- [Issues preventing progress, if any]

## Key Decisions
- **[Decision]**: [Brief rationale]

## Next Steps
1. [What should happen next to continue this work]

Keep each section concise. Preserve exact file paths, function names, and error messages.";

/// Generate a summary for abandoned branch entries.
pub async fn generate_branch_summary(
    entries: &[Entry],
    options: &GenerateBranchSummaryOptions,
    context: &Context,
) -> Result<BranchSummaryResult, BranchSummaryError> {
    let context_window = if options.model.context_window > 0 {
        options.model.context_window
    } else {
        128000
    };
    let preparation = prepare_branch_entries(entries, context_window.saturating_sub(options.reserve_tokens));
    let request = ModelSummaryRequest {
        models: &options.models,
        model: &options.model,
        retry: options.retry.as_ref(),
        callbacks: options.callbacks.as_ref(),
    };
    generate_branch_summary_with_request(
        &preparation,
        &PreparedBranchSummaryOptions {
            custom_instructions: options.custom_instructions.clone(),
            replace_instructions: options.replace_instructions,
        },
        &request,
        context,
    )
    .await
}

#[derive(Debug, Clone, Default)]
pub struct PreparedBranchSummaryOptions {
    pub custom_instructions: Option<String>,
    pub replace_instructions: bool,
}

/// Generate a prepared branch summary through a caller-owned one-request boundary.
pub async fn generate_branch_summary_with_request(
    preparation: &BranchPreparation,
    options: &PreparedBranchSummaryOptions,
    request: &dyn SummaryRequest,
    context: &Context,
) -> Result<BranchSummaryResult, BranchSummaryError> {
    let BranchPreparation {
        messages, file_ops, ..
    } = preparation;
    if messages.is_empty() {
        return Ok(BranchSummaryResult {
            summary: "No content to summarize".to_owned(),
            usage: None,
            read_files: Vec::new(),
            modified_files: Vec::new(),
        });
    }
    let llm_messages = convert_to_llm(messages.clone());
    let conversation_text = serialize_conversation(&llm_messages);
    let instructions = match (&options.custom_instructions, options.replace_instructions) {
        (Some(custom_instructions), true) => custom_instructions.clone(),
        (Some(custom_instructions), false) => {
            format!("{BRANCH_SUMMARY_PROMPT}\n\nAdditional focus: {custom_instructions}")
        }
        (None, _) => BRANCH_SUMMARY_PROMPT.to_owned(),
    };
    let prompt_text = format!("<conversation>\n{conversation_text}\n</conversation>\n\n{instructions}");

    let summarization_messages = vec![Message::User(maho_ai::types::UserMessage {
        content: UserContent::Blocks(vec![ContentBlock::text(prompt_text)]),
        timestamp: now_ms(),
    })];
    let mut completion_options = SimpleStreamOptions::default();
    completion_options.stream.max_tokens = Some(2048);
    let response = request
        .request(
        &maho_ai::types::Context {
            system_prompt: Some(SUMMARIZATION_SYSTEM_PROMPT.to_owned()),
            messages: summarization_messages,
            tools: None,
        },
        create_summary_request_options(completion_options, context),
        context,
        )
        .await;
    if response.stop_reason == StopReason::Aborted {
        return Err(BranchSummaryError::new(
            BranchSummaryErrorCode::Aborted,
            non_empty(response.error_message.as_deref(), "Branch summary aborted"),
        ));
    }
    if response.stop_reason == StopReason::Error {
        return Err(BranchSummaryError::new(
            BranchSummaryErrorCode::SummarizationFailed,
            format!(
                "Branch summary failed: {}",
                non_empty(response.error_message.as_deref(), "Unknown error")
            ),
        ));
    }

    let mut summary = format!(
        "{BRANCH_SUMMARY_PREAMBLE}{}",
        content_text_for_summary(&response.content, "\n")
    );
    let (read_files, modified_files) = compute_file_lists(file_ops);
    summary.push_str(&format_file_operations(&read_files, &modified_files));

    Ok(BranchSummaryResult {
        summary: if summary.is_empty() {
            "No summary generated".to_owned()
        } else {
            summary
        },
        usage: Some(response.usage),
        read_files,
        modified_files,
    })
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

pub type BranchSummaryRequest = dyn SummaryRequest;
pub type BranchSummaryFuture = BoxFuture<'static, ()>;
