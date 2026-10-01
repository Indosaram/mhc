//! Port of senpi packages/agent/src/harness/messages.ts.

use maho_ai::types::{ContentBlock, Message, TextContent, UserContent, UserMessage};
use maho_ai::utils::drop_failed_assistant_turns::drop_failed_assistant_turns;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::types::{AgentMessage, CustomAgentMessage};

pub const COMPACTION_SUMMARY_PREFIX: &str = "The conversation history before this point was compacted into the following summary:\n\n<summary>\n";
pub const COMPACTION_SUMMARY_SUFFIX: &str = "\n</summary>";
pub const BRANCH_SUMMARY_PREFIX: &str = "The following is a summary of a branch that this conversation came back from:\n\n<summary>\n";
pub const BRANCH_SUMMARY_SUFFIX: &str = "</summary>";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BashExecutionMessage {
    #[serde(default = "bash_execution_role")]
    pub role: String,
    pub command: String,
    pub output: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i64>,
    pub cancelled: bool,
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_output_path: Option<String>,
    pub timestamp: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclude_from_context: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CustomMessageContent {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomMessage {
    #[serde(default = "custom_role")]
    pub role: String,
    pub custom_type: String,
    pub content: CustomMessageContent,
    pub display: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    pub timestamp: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchSummaryMessage {
    #[serde(default = "branch_summary_role")]
    pub role: String,
    pub summary: String,
    pub from_id: Option<String>,
    pub timestamp: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionSummaryMessage {
    #[serde(default = "compaction_summary_role")]
    pub role: String,
    pub summary: String,
    pub tokens_before: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    pub timestamp: i64,
}

fn bash_execution_role() -> String {
    "bashExecution".to_owned()
}

fn custom_role() -> String {
    "custom".to_owned()
}

fn branch_summary_role() -> String {
    "branchSummary".to_owned()
}

fn compaction_summary_role() -> String {
    "compactionSummary".to_owned()
}

pub fn bash_execution_to_text(message: &BashExecutionMessage) -> String {
    let mut text = format!("Ran `{}`\n", message.command);
    if message.output.is_empty() {
        text.push_str("(no output)");
    } else {
        text.push_str(&format!("```\n{}\n```", message.output));
    }
    if message.cancelled {
        text.push_str("\n\n(command cancelled)");
    } else if message.exit_code.is_some_and(|code| code != 0) {
        text.push_str(&format!(
            "\n\nCommand exited with code {}",
            message.exit_code.unwrap_or_default()
        ));
    }
    if message.truncated
        && let Some(path) = &message.full_output_path {
            text.push_str(&format!("\n\n[Output truncated. Full output: {path}]"));
        }
    text
}

pub fn create_branch_summary_message(
    summary: impl Into<String>,
    from_id: Option<String>,
    timestamp: impl Into<TimestampInput>,
) -> BranchSummaryMessage {
    BranchSummaryMessage {
        role: branch_summary_role(),
        summary: summary.into(),
        from_id,
        timestamp: timestamp.into().to_millis(),
    }
}

pub fn create_compaction_summary_message(
    summary: impl Into<String>,
    tokens_before: i64,
    timestamp: impl Into<TimestampInput>,
) -> CompactionSummaryMessage {
    CompactionSummaryMessage {
        role: compaction_summary_role(),
        summary: summary.into(),
        tokens_before,
        details: None,
        timestamp: timestamp.into().to_millis(),
    }
}

pub fn create_custom_message(
    custom_type: impl Into<String>,
    content: CustomMessageContent,
    display: bool,
    details: Option<Value>,
    timestamp: impl Into<TimestampInput>,
) -> CustomMessage {
    CustomMessage {
        role: custom_role(),
        custom_type: custom_type.into(),
        content,
        display,
        details,
        timestamp: timestamp.into().to_millis(),
    }
}

/// `timestamp: string | number` normalized to milliseconds since the Unix epoch.
#[derive(Debug, Clone)]
pub enum TimestampInput {
    Number(i64),
    IsoString(String),
}

impl TimestampInput {
    fn to_millis(&self) -> i64 {
        match self {
            TimestampInput::Number(value) => *value,
            TimestampInput::IsoString(value) => parse_iso_millis(value),
        }
    }
}

impl From<i64> for TimestampInput {
    fn from(value: i64) -> Self {
        TimestampInput::Number(value)
    }
}

impl From<String> for TimestampInput {
    fn from(value: String) -> Self {
        TimestampInput::IsoString(value)
    }
}

impl From<&str> for TimestampInput {
    fn from(value: &str) -> Self {
        TimestampInput::IsoString(value.to_string())
    }
}

fn parse_iso_millis(value: &str) -> i64 {
    time_from_iso(value).unwrap_or_default()
}

fn time_from_iso(value: &str) -> Option<i64> {
    let trimmed = value.trim();
    let normalized = trimmed.strip_suffix('Z').unwrap_or(trimmed);
    let (date, rest) = match normalized.split_once(['T', 't']) {
        Some(parts) => parts,
        None => (normalized, ""),
    };
    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: i64 = date_parts.next()?.parse().ok()?;
    let day: i64 = date_parts.next()?.parse().ok()?;

    let mut time_parts = rest.split(':');
    let hour: i64 = time_parts.next().unwrap_or("0").parse().ok()?;
    let minute: i64 = time_parts.next().unwrap_or("0").parse().ok()?;
    let second_part = time_parts.next().unwrap_or("0");
    let (second, fraction) = second_part.split_once('.').unwrap_or((second_part, ""));
    let second: i64 = second.parse().ok()?;
    let millis: i64 = {
        let digits: String = fraction.chars().take(3).collect();
        let padded = format!("{digits:0<3}");
        padded.parse().unwrap_or(0)
    };

    let days = days_from_civil(year, month, day);
    Some(((days * 86_400 + hour * 3_600 + minute * 60 + second) * 1_000) + millis)
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

impl BranchSummaryMessage {
    pub fn to_llm(&self) -> Message {
        summary_to_llm(
            &self.summary,
            BRANCH_SUMMARY_PREFIX,
            BRANCH_SUMMARY_SUFFIX,
            self.timestamp,
        )
    }
}

impl CompactionSummaryMessage {
    pub fn to_llm(&self) -> Message {
        summary_to_llm(
            &self.summary,
            COMPACTION_SUMMARY_PREFIX,
            COMPACTION_SUMMARY_SUFFIX,
            self.timestamp,
        )
    }
}

fn summary_to_llm(summary: &str, prefix: &str, suffix: &str, timestamp: i64) -> Message {
    Message::User(UserMessage {
        content: UserContent::Blocks(vec![ContentBlock::text(format!("{prefix}{summary}{suffix}"))]),
        timestamp,
    })
}

fn custom_message_to_llm(message: &CustomMessage) -> Message {
    let content = match &message.content {
        CustomMessageContent::Text(text) => vec![ContentBlock::Text(TextContent {
            text: text.clone(),
            ..TextContent::default()
        })],
        CustomMessageContent::Blocks(blocks) => blocks.clone(),
    };
    Message::User(UserMessage {
        content: UserContent::Blocks(content),
        timestamp: message.timestamp,
    })
}

/// Project one custom app message onto the provider message the model sees.
pub fn custom_agent_message_to_llm(message: &CustomAgentMessage) -> Message {
    match message {
        CustomAgentMessage::BashExecution(bash) => Message::User(UserMessage {
            content: UserContent::Blocks(vec![ContentBlock::text(bash_execution_to_text(bash))]),
            timestamp: bash.timestamp,
        }),
        CustomAgentMessage::Custom(custom) => custom_message_to_llm(custom),
        CustomAgentMessage::BranchSummary(branch) => branch.to_llm(),
        CustomAgentMessage::CompactionSummary(compaction) => compaction.to_llm(),
    }
}

/// Role discriminant of a custom app message.
pub fn custom_agent_message_role(message: &CustomAgentMessage) -> &'static str {
    match message {
        CustomAgentMessage::BashExecution(_) => "bashExecution",
        CustomAgentMessage::Custom(_) => "custom",
        CustomAgentMessage::BranchSummary(_) => "branchSummary",
        CustomAgentMessage::CompactionSummary(_) => "compactionSummary",
    }
}

/// convertToLlm: drop excluded bash-execution messages, then drop failed assistant turns.
pub fn convert_to_llm(messages: Vec<AgentMessage>) -> Vec<Message> {
    let converted: Vec<Message> = messages
        .into_iter()
        .filter_map(|message| match message {
            AgentMessage::Llm(message) => Some(message),
            AgentMessage::Custom(custom) => match custom {
                CustomAgentMessage::BashExecution(bash) if bash.exclude_from_context == Some(true) => None,
                custom => Some(custom_agent_message_to_llm(&custom)),
            },
        })
        .collect();
    drop_failed_assistant_turns(&converted)
}
