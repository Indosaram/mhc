//! Transcript provider over senpi's session JSONL store (~/.senpi/agent/sessions).

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::engine::{TranscriptConversation, TranscriptProvider};
use super::query::{SearchDocument, SearchToolCall};

const ARCHIVED_SUFFIX: &str = ".archived";
const SESSION_SUFFIX: &str = ".jsonl";

/// Predicate deciding whether a candidate session is hidden.
pub type IsHiddenFn = Box<dyn Fn(&SenpiHiddenCandidate) -> bool + Send + Sync>;

/// Header entry at the start of a senpi session file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SenpiSessionHeader {
    #[serde(rename = "type")]
    pub entry_type: String,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session: Option<String>,
}

/// Candidate session evaluated for hidden visibility rules.
#[derive(Debug, Clone)]
pub struct SenpiHiddenCandidate {
    pub file_path: PathBuf,
    pub directory_name: String,
    pub header: Option<SenpiSessionHeader>,
}

/// Configuration options for the Senpi session provider.
pub struct SenpiSessionProviderOptions {
    pub sessions_dir: PathBuf,
    pub excluded_dirs: Option<Vec<String>>,
    pub hidden_marker_file: Option<String>,
    pub is_hidden: Option<IsHiddenFn>,
}

/// Transcript provider reading JSONL session files from a Senpi sessions directory.
pub struct SenpiSessionProvider {
    options: SenpiSessionProviderOptions,
}

impl SenpiSessionProvider {
    /// Creates a new Senpi session provider with the given options.
    pub fn new(options: SenpiSessionProviderOptions) -> Self {
        Self { options }
    }

    fn is_hidden(&self, candidate: &SenpiHiddenCandidate) -> bool {
        let mut archived = candidate.file_path.clone().into_os_string();
        archived.push(ARCHIVED_SUFFIX);
        if Path::new(&archived).exists() {
            return true;
        }

        if let Some(excluded) = &self.options.excluded_dirs
            && excluded.iter().any(|d| d == &candidate.directory_name)
        {
            return true;
        }

        if let Some(marker) = &self.options.hidden_marker_file
            && let Some(parent) = candidate.file_path.parent()
            && parent.join(marker).exists()
        {
            return true;
        }

        if let Some(custom_is_hidden) = &self.options.is_hidden
            && custom_is_hidden(candidate)
        {
            return true;
        }

        false
    }
}

fn parse_header(value: &serde_json::Value) -> Option<SenpiSessionHeader> {
    let obj = value.as_object()?;
    if obj.get("type")?.as_str()? != "session" {
        return None;
    }
    let id = obj.get("id")?.as_str()?.to_string();
    let timestamp = obj
        .get("timestamp")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let cwd = obj.get("cwd").and_then(|v| v.as_str()).map(str::to_string);
    let version = obj.get("version").and_then(|v| v.as_u64());
    let parent_session = obj
        .get("parentSession")
        .or_else(|| obj.get("parent_session"))
        .and_then(|v| v.as_str())
        .map(str::to_string);

    Some(SenpiSessionHeader {
        entry_type: "session".to_string(),
        id,
        timestamp,
        cwd,
        version,
        parent_session,
    })
}

struct MappedContent {
    content: String,
    reasoning: String,
    tool_calls: Vec<SearchToolCall>,
}

fn map_content(content: &serde_json::Value) -> MappedContent {
    match content {
        serde_json::Value::String(text) => MappedContent {
            content: text.clone(),
            reasoning: String::new(),
            tool_calls: Vec::new(),
        },
        serde_json::Value::Array(parts) => {
            let mut texts = Vec::new();
            let mut thoughts = Vec::new();
            let mut tool_calls = Vec::new();

            for part in parts {
                let Some(obj) = part.as_object() else {
                    continue;
                };
                let Some(part_type) = obj.get("type").and_then(|v| v.as_str()) else {
                    continue;
                };
                if part_type == "text" {
                    if let Some(text) = obj.get("text").and_then(|v| v.as_str())
                        && !text.is_empty()
                    {
                        texts.push(text.to_string());
                    }
                    continue;
                }
                if part_type == "thinking" {
                    if let Some(thinking) = obj.get("thinking").and_then(|v| v.as_str())
                        && !thinking.is_empty()
                    {
                        thoughts.push(thinking.to_string());
                    }
                    continue;
                }
                if part_type == "toolCall" {
                    let name = obj
                        .get("name")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .map(str::to_string);
                    let arguments = obj.get("arguments").and_then(|args| match args {
                        serde_json::Value::Null => None,
                        serde_json::Value::String(s) => Some(s.clone()),
                        other => Some(serde_json::to_string(other).unwrap_or_default()),
                    });
                    tool_calls.push(SearchToolCall { name, arguments });
                }
            }

            MappedContent {
                content: texts.join("\n"),
                reasoning: thoughts.join("\n"),
                tool_calls,
            }
        }
        _ => MappedContent {
            content: String::new(),
            reasoning: String::new(),
            tool_calls: Vec::new(),
        },
    }
}

fn to_document(
    entry: &serde_json::Map<String, serde_json::Value>,
    conversation_id: &str,
) -> Option<SearchDocument> {
    if entry.get("type")?.as_str()? != "message" {
        return None;
    }
    let id = entry.get("id")?.as_str()?.to_string();
    let message = entry.get("message")?.as_object()?;
    let role = message.get("role")?.as_str()?.to_string();

    let mapped = map_content(message.get("content").unwrap_or(&serde_json::Value::Null));
    let tool_name = message.get("toolName").and_then(|v| v.as_str());

    let mut tool_calls = mapped.tool_calls;
    if let Some(name) = tool_name {
        tool_calls.push(SearchToolCall {
            name: Some(name.to_string()),
            arguments: None,
        });
    }

    let is_tool_result = role == "toolResult";
    let date = entry
        .get("timestamp")
        .and_then(|v| v.as_str())
        .map(str::to_string);

    let content = if is_tool_result {
        None
    } else if !mapped.content.is_empty() {
        Some(serde_json::Value::String(mapped.content.clone()))
    } else {
        None
    };

    let tool_return = if is_tool_result && !mapped.content.is_empty() {
        Some(serde_json::Value::String(mapped.content))
    } else {
        None
    };

    let reasoning = if !mapped.reasoning.is_empty() {
        Some(mapped.reasoning)
    } else {
        None
    };

    let tool_calls_opt = if !tool_calls.is_empty() {
        Some(tool_calls)
    } else {
        None
    };

    Some(SearchDocument {
        id,
        conversation_id: conversation_id.to_string(),
        date,
        message_type: Some(role),
        content,
        reasoning,
        summary: None,
        tool_calls: tool_calls_opt,
        tool_return,
        func_response: None,
    })
}

fn read_session_file(file_path: &Path) -> (Option<SenpiSessionHeader>, Vec<serde_json::Value>) {
    let Ok(raw) = fs::read_to_string(file_path) else {
        return (None, Vec::new());
    };

    let mut header = None;
    let mut lines = Vec::new();

    for line in raw.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(parsed) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };

        if header.is_none()
            && let Some(candidate) = parse_header(&parsed)
        {
            header = Some(candidate);
            continue;
        }

        if parsed.is_object() {
            lines.push(parsed);
        }
    }

    (header, lines)
}

fn list_session_files(sessions_dir: &Path) -> Vec<(PathBuf, String)> {
    if !sessions_dir.exists() {
        return Vec::new();
    }
    let Ok(entries) = fs::read_dir(sessions_dir) else {
        return Vec::new();
    };

    let mut files = Vec::new();
    let mut sorted_entries: Vec<PathBuf> =
        entries.filter_map(|e| e.ok().map(|de| de.path())).collect();
    sorted_entries.sort();

    for path in sorted_entries {
        if path.is_dir() {
            let directory_name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            if let Ok(sub_entries) = fs::read_dir(&path) {
                let mut sub_paths: Vec<PathBuf> = sub_entries
                    .filter_map(|e| e.ok().map(|de| de.path()))
                    .collect();
                sub_paths.sort();
                for sub_path in sub_paths {
                    if sub_path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.ends_with(SESSION_SUFFIX))
                    {
                        files.push((sub_path, directory_name.clone()));
                    }
                }
            }
        } else if path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.ends_with(SESSION_SUFFIX))
        {
            let directory_name = sessions_dir
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            files.push((path, directory_name));
        }
    }

    files
}

impl TranscriptProvider for SenpiSessionProvider {
    fn list_conversations(&self) -> Vec<TranscriptConversation> {
        let mut conversations = Vec::new();

        for (file_path, directory_name) in list_session_files(&self.options.sessions_dir) {
            let (header, lines) = read_session_file(&file_path);
            let id = match &header {
                Some(h) => h.id.clone(),
                None => {
                    let stem = file_path.file_stem().unwrap_or_default().to_string_lossy();
                    stem.to_string()
                }
            };

            let mut seen = HashSet::new();
            let mut messages = Vec::new();

            for entry in &lines {
                let Some(obj) = entry.as_object() else {
                    continue;
                };
                let Some(document) = to_document(obj, &id) else {
                    continue;
                };
                if seen.insert(document.id.clone()) {
                    messages.push(document);
                }
            }

            if messages.is_empty() {
                continue;
            }

            let candidate = SenpiHiddenCandidate {
                file_path: file_path.clone(),
                directory_name,
                header,
            };
            let hidden = self.is_hidden(&candidate);

            conversations.push(TranscriptConversation {
                id,
                messages,
                hidden: if hidden { Some(true) } else { None },
            });
        }

        conversations
    }
}

#[cfg(test)]
#[path = "senpi_session_provider_tests.rs"]
mod tests;
