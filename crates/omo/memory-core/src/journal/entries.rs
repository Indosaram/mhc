//! Transcript entry projection and normalization for memory journal.

use serde::{Deserialize, Serialize};

/// Maximum characters retained for tool arguments in journal entries.
pub const TOOL_ARGS_TRUNCATE_LIMIT: usize = 300;

/// Redacted reasoning placeholder.
pub const REDACTED_REASONING_TEXT: &str = "[REDACTED REASONING]";

/// A text transcript entry (user, assistant, reasoning, error).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextTranscriptEntry {
    pub kind: String,
    pub text: String,
    pub captured_at: String,
    pub source_line_id: String,
    pub source_message_id: String,
}

impl TextTranscriptEntry {
    /// Create a new text transcript entry.
    pub fn new(
        kind: impl Into<String>,
        text: impl Into<String>,
        captured_at: impl Into<String>,
        source_line_id: impl Into<String>,
        source_message_id: impl Into<String>,
    ) -> Self {
        Self {
            kind: kind.into(),
            text: text.into(),
            captured_at: captured_at.into(),
            source_line_id: source_line_id.into(),
            source_message_id: source_message_id.into(),
        }
    }
}

/// A tool call transcript entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallTranscriptEntry {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(rename = "argsText", default, skip_serializing_if = "Option::is_none")]
    pub args_text: Option<String>,
    #[serde(
        rename = "resultText",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub result_text: Option<String>,
    #[serde(rename = "resultOk", default, skip_serializing_if = "Option::is_none")]
    pub result_ok: Option<bool>,
    pub captured_at: String,
    pub source_line_id: String,
    pub source_message_id: String,
}

impl ToolCallTranscriptEntry {
    /// Create a new tool call transcript entry.
    pub fn new(
        name: Option<String>,
        args_text: Option<String>,
        result_text: Option<String>,
        result_ok: Option<bool>,
        captured_at: impl Into<String>,
        source_line_id: impl Into<String>,
        source_message_id: impl Into<String>,
    ) -> Self {
        Self {
            kind: "tool_call".to_string(),
            name,
            args_text,
            result_text,
            result_ok,
            captured_at: captured_at.into(),
            source_line_id: source_line_id.into(),
            source_message_id: source_message_id.into(),
        }
    }
}

/// An entry in the transcript journal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TranscriptEntry {
    Text(TextTranscriptEntry),
    ToolCall(ToolCallTranscriptEntry),
}

impl TranscriptEntry {
    /// The entry kind.
    pub fn kind(&self) -> &str {
        match self {
            Self::Text(entry) => &entry.kind,
            Self::ToolCall(entry) => &entry.kind,
        }
    }

    /// When the entry was captured (ISO-8601).
    pub fn captured_at(&self) -> &str {
        match self {
            Self::Text(entry) => &entry.captured_at,
            Self::ToolCall(entry) => &entry.captured_at,
        }
    }

    /// Deduplication identifier for this line.
    pub fn source_line_id(&self) -> &str {
        match self {
            Self::Text(entry) => &entry.source_line_id,
            Self::ToolCall(entry) => &entry.source_line_id,
        }
    }

    /// The originating message identifier.
    pub fn source_message_id(&self) -> &str {
        match self {
            Self::Text(entry) => &entry.source_message_id,
            Self::ToolCall(entry) => &entry.source_message_id,
        }
    }

    /// Text content of this entry, or `None` if it is a tool call.
    pub fn text(&self) -> Option<&str> {
        match self {
            Self::Text(entry) => Some(&entry.text),
            Self::ToolCall(_) => None,
        }
    }
}

impl From<TextTranscriptEntry> for TranscriptEntry {
    fn from(entry: TextTranscriptEntry) -> Self {
        Self::Text(entry)
    }
}

impl From<ToolCallTranscriptEntry> for TranscriptEntry {
    fn from(entry: ToolCallTranscriptEntry) -> Self {
        Self::ToolCall(entry)
    }
}

/// A tool call projection from a transcript message.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectedToolCall {
    #[serde(rename = "callId")]
    pub call_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(rename = "argsText", default, skip_serializing_if = "Option::is_none")]
    pub args_text: Option<String>,
    #[serde(
        rename = "resultText",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub result_text: Option<String>,
    #[serde(rename = "resultOk", default, skip_serializing_if = "Option::is_none")]
    pub result_ok: Option<bool>,
}

/// Projected reasoning block: either verbatim text or redacted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ProjectedReasoning {
    Text(String),
    Redacted { redacted: bool },
}

impl ProjectedReasoning {
    /// Create a verbatim reasoning block.
    pub fn text(s: impl Into<String>) -> Self {
        Self::Text(s.into())
    }

    /// Create a redacted reasoning block.
    pub fn redacted() -> Self {
        Self::Redacted { redacted: true }
    }

    /// Extract text representation, resolving redactions to [`REDACTED_REASONING_TEXT`].
    pub fn as_str(&self) -> &str {
        match self {
            Self::Text(s) => s.as_str(),
            Self::Redacted { .. } => REDACTED_REASONING_TEXT,
        }
    }
}

impl From<&str> for ProjectedReasoning {
    fn from(s: &str) -> Self {
        Self::Text(s.to_string())
    }
}

impl From<String> for ProjectedReasoning {
    fn from(s: String) -> Self {
        Self::Text(s)
    }
}

/// A transcript message projection ready to be converted into journal entries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TranscriptProjection {
    #[serde(rename = "user")]
    User {
        #[serde(rename = "messageId")]
        message_id: String,
        text: String,
    },
    #[serde(rename = "error")]
    Error {
        #[serde(rename = "messageId")]
        message_id: String,
        text: String,
    },
    #[serde(rename = "assistant")]
    Assistant {
        #[serde(rename = "messageId")]
        message_id: String,
        #[serde(rename = "textBlocks", default, skip_serializing_if = "Vec::is_empty")]
        text_blocks: Vec<String>,
        #[serde(
            rename = "reasoningBlocks",
            default,
            skip_serializing_if = "Vec::is_empty"
        )]
        reasoning_blocks: Vec<ProjectedReasoning>,
        #[serde(rename = "toolCalls", default, skip_serializing_if = "Vec::is_empty")]
        tool_calls: Vec<ProjectedToolCall>,
    },
}

fn truncate_str(s: &str, limit: usize) -> &str {
    match s.char_indices().nth(limit) {
        Some((idx, _)) => &s[..idx],
        None => s,
    }
}

/// Project a single transcript message into normalized journal entries.
pub fn project_transcript_entries(
    message: &TranscriptProjection,
    captured_at: &str,
) -> Vec<TranscriptEntry> {
    match message {
        TranscriptProjection::User { message_id, text } => {
            if text.trim().is_empty() {
                return Vec::new();
            }
            vec![TranscriptEntry::Text(TextTranscriptEntry {
                kind: "user".to_string(),
                text: text.clone(),
                captured_at: captured_at.to_string(),
                source_line_id: format!("{message_id}:user"),
                source_message_id: message_id.clone(),
            })]
        }
        TranscriptProjection::Error { message_id, text } => {
            if text.trim().is_empty() {
                return Vec::new();
            }
            vec![TranscriptEntry::Text(TextTranscriptEntry {
                kind: "error".to_string(),
                text: text.clone(),
                captured_at: captured_at.to_string(),
                source_line_id: format!("{message_id}:error"),
                source_message_id: message_id.clone(),
            })]
        }
        TranscriptProjection::Assistant {
            message_id,
            text_blocks,
            reasoning_blocks,
            tool_calls,
        } => {
            let mut entries = Vec::new();
            let assistant_text = text_blocks
                .iter()
                .filter(|t| !t.trim().is_empty())
                .cloned()
                .collect::<Vec<_>>()
                .join("\n");

            if !assistant_text.is_empty() {
                entries.push(TranscriptEntry::Text(TextTranscriptEntry {
                    kind: "assistant".to_string(),
                    text: assistant_text,
                    captured_at: captured_at.to_string(),
                    source_line_id: format!("{message_id}:assistant"),
                    source_message_id: message_id.clone(),
                }));
            }

            for (index, reasoning) in reasoning_blocks.iter().enumerate() {
                let text = reasoning.as_str();
                if text.trim().is_empty() {
                    continue;
                }
                entries.push(TranscriptEntry::Text(TextTranscriptEntry {
                    kind: "reasoning".to_string(),
                    text: text.to_string(),
                    captured_at: captured_at.to_string(),
                    source_line_id: format!("{message_id}:reasoning:{index}"),
                    source_message_id: message_id.clone(),
                }));
            }

            for tool_call in tool_calls {
                let args_text = tool_call
                    .args_text
                    .as_ref()
                    .map(|args| truncate_str(args, TOOL_ARGS_TRUNCATE_LIMIT).to_string());

                entries.push(TranscriptEntry::ToolCall(ToolCallTranscriptEntry {
                    kind: "tool_call".to_string(),
                    name: tool_call.name.clone(),
                    args_text,
                    result_text: tool_call.result_text.clone(),
                    result_ok: tool_call.result_ok,
                    captured_at: captured_at.to_string(),
                    source_line_id: format!("{message_id}:tool:{}", tool_call.call_id),
                    source_message_id: message_id.clone(),
                }));
            }

            entries
        }
    }
}

/// Parse a raw JSON value into a validated [`TranscriptEntry`].
pub fn parse_transcript_entry(
    value: &serde_json::Value,
) -> Result<TranscriptEntry, crate::journal::store::JournalError> {
    let obj = value
        .as_object()
        .ok_or(crate::journal::store::JournalError::InvalidTranscriptJournalRow)?;
    let kind = obj
        .get("kind")
        .and_then(|v| v.as_str())
        .ok_or(crate::journal::store::JournalError::InvalidTranscriptJournalRow)?;
    let captured_at = obj
        .get("captured_at")
        .and_then(|v| v.as_str())
        .ok_or(crate::journal::store::JournalError::InvalidTranscriptJournalRow)?;
    let source_line_id = obj
        .get("source_line_id")
        .and_then(|v| v.as_str())
        .ok_or(crate::journal::store::JournalError::InvalidTranscriptJournalRow)?;
    let source_message_id = obj
        .get("source_message_id")
        .and_then(|v| v.as_str())
        .ok_or(crate::journal::store::JournalError::InvalidTranscriptJournalRow)?;

    let _ = (captured_at, source_line_id, source_message_id);

    if kind == "tool_call" {
        let entry: ToolCallTranscriptEntry = serde_json::from_value(value.clone())
            .map_err(|_| crate::journal::store::JournalError::InvalidTranscriptJournalRow)?;
        Ok(TranscriptEntry::ToolCall(entry))
    } else if matches!(kind, "user" | "assistant" | "reasoning" | "error") {
        if obj.get("text").and_then(|v| v.as_str()).is_none() {
            return Err(crate::journal::store::JournalError::InvalidTranscriptJournalRow);
        }
        let entry: TextTranscriptEntry = serde_json::from_value(value.clone())
            .map_err(|_| crate::journal::store::JournalError::InvalidTranscriptJournalRow)?;
        Ok(TranscriptEntry::Text(entry))
    } else {
        Err(crate::journal::store::JournalError::InvalidTranscriptJournalRow)
    }
}

#[cfg(test)]
#[path = "entries_tests.rs"]
mod tests;
