use serde::{Deserialize, Serialize};
use super::super::types::*;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryFields {
    pub id: Id,
    pub conversation_id: Id,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub edits: Option<Vec<ContextEdit>>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub by_task_id: Option<Id>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Continuation { pub continuation: True, pub from: Id }
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AssistantData { pub attempt: u64 }
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Truncated { pub bytes: u64, pub lines: u64 }
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ToolResultData {
    #[serde(default, skip_serializing_if = "Option::is_none")] pub details: Option<JsonValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub diagnostics: Option<JsonValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub control: Option<ToolControl>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub truncated: Option<Truncated>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SystemData { pub baseline: bool }
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UsageData {
    pub attempt: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub usage: Option<maho_ai::types::Usage>,
    pub error: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SummaryData { pub through: Id }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UserEntry {
    #[serde(flatten)] pub entry: EntryFields,
    pub model: [Stored<UserMessage>; 1],
    #[serde(default, skip_serializing_if = "Option::is_none")] pub data: Option<Continuation>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AssistantEntry {
    #[serde(flatten)] pub entry: EntryFields,
    pub model: [Stored<AssistantMessage>; 1],
    pub data: AssistantData,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolResultEntry {
    #[serde(flatten)] pub entry: EntryFields,
    pub model: [Stored<ToolResultMessage>; 1],
    pub data: ToolResultData,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SystemEntry {
    #[serde(flatten)] pub entry: EntryFields,
    pub model: [Stored<SystemMessage>; 1],
    pub data: SystemData,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NoticeEntry { #[serde(flatten)] pub entry: EntryFields, pub model: [Stored<UserMessage>; 1] }
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UsageEntry { #[serde(flatten)] pub entry: EntryFields, pub data: UsageData }
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SummaryEntry {
    #[serde(flatten)] pub entry: EntryFields,
    pub model: [Stored<UserMessage>; 1],
    pub data: SummaryData,
    pub head: Id,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HandoffEntry { #[serde(flatten)] pub entry: EntryFields, pub model: [Stored<UserMessage>; 1], pub head: Id }
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResetEntry { #[serde(flatten)] pub entry: EntryFields, pub head: Id }

pub struct Entries {
    pub user: EntryKind<UserEntry>,
    pub assistant: EntryKind<AssistantEntry>,
    pub tool_result: EntryKind<ToolResultEntry>,
    pub system: EntryKind<SystemEntry>,
    pub notice: EntryKind<NoticeEntry>,
    pub usage: EntryKind<UsageEntry>,
    pub summary: EntryKind<SummaryEntry>,
    pub handoff: EntryKind<HandoffEntry>,
    pub reset: EntryKind<ResetEntry>,
}

pub fn entries() -> Entries {
    Entries {
        user: EntryKind::core("pi.user"),
        assistant: EntryKind::core("pi.assistant"),
        tool_result: EntryKind::core("pi.tool_result"),
        system: EntryKind::core("pi.system"),
        notice: EntryKind::core("pi.notice"),
        usage: EntryKind::core("pi.usage"),
        summary: EntryKind::core("pi.summary"),
        handoff: EntryKind::core("pi.handoff"),
        reset: EntryKind::core("pi.reset"),
    }
}

#[cfg(test)]
#[path = "entries_tests.rs"]
mod tests;
