//! Transcript provider over the senpi session JSONL store.
//!
//! Spec: `docs/cleanroom/specs/memory-session-provider.md`. Sessions live at `<sessions dir>/<project>/<id>.jsonl`
//! (or flat at `<sessions dir>/<id>.jsonl`). This reads them into the conversation shape
//! [`crate::transcript_search::search_transcripts`] ranks, so the ranking stays testable without touching disk.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::transcript_search::{
    SearchDocument, SearchToolCall, TranscriptConversation, TranscriptProvider,
};

const ARCHIVED_SUFFIX: &str = ".archived";
const SESSION_SUFFIX: &str = ".jsonl";

/// Memory machinery rather than conversation: searching these would let recall match its own past injections.
const EXCLUDED_CUSTOM_TYPES: &[&str] = &[
    "omo-kibitzer:recall",
    "omo-memorian:recall",
    "omo-memory:notice",
];

pub type IsHiddenFn = Box<dyn Fn(&SenpiHiddenCandidate) -> bool + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionHeader {
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

#[derive(Debug, Clone)]
pub struct SenpiHiddenCandidate {
    pub file_path: PathBuf,
    pub directory_name: String,
    pub header: Option<SessionHeader>,
}

pub struct SessionProviderOptions {
    pub sessions_dir: PathBuf,
    pub excluded_dirs: Option<Vec<String>>,
    pub hidden_marker_file: Option<String>,
    pub is_hidden: Option<IsHiddenFn>,
}

pub struct SessionProvider {
    options: SessionProviderOptions,
}

impl SessionProvider {
    pub fn new(options: SessionProviderOptions) -> Self {
        Self { options }
    }

    fn is_hidden(&self, candidate: &SenpiHiddenCandidate) -> bool {
        // A session is archived by the presence of a sibling `.archived` file, not by renaming it, so a single
        // check here covers the case where the JSONL itself is still readable.
        let mut archived = candidate.file_path.clone().into_os_string();
        archived.push(ARCHIVED_SUFFIX);
        if Path::new(&archived).exists() {
            return true;
        }
        if let Some(excluded) = &self.options.excluded_dirs
            && excluded.iter().any(|dir| dir == &candidate.directory_name)
        {
            return true;
        }
        if let Some(marker) = &self.options.hidden_marker_file
            && let Some(parent) = candidate.file_path.parent()
            && parent.join(marker).exists()
        {
            return true;
        }
        if let Some(custom) = &self.options.is_hidden
            && custom(candidate)
        {
            return true;
        }
        false
    }
}

fn parse_header(value: &serde_json::Value) -> Option<SessionHeader> {
    let object = value.as_object()?;
    if object.get("type")?.as_str()? != "session" {
        return None;
    }
    Some(SessionHeader {
        entry_type: "session".to_string(),
        id: object.get("id")?.as_str()?.to_string(),
        timestamp: object
            .get("timestamp")
            .and_then(|value| value.as_str())
            .map(str::to_string),
        cwd: object
            .get("cwd")
            .and_then(|value| value.as_str())
            .map(str::to_string),
        version: object.get("version").and_then(serde_json::Value::as_u64),
        parent_session: object
            .get("parentSession")
            .or_else(|| object.get("parent_session"))
            .and_then(|value| value.as_str())
            .map(str::to_string),
    })
}

struct MappedContent {
    content: String,
    reasoning: String,
    tool_calls: Vec<SearchToolCall>,
}

/// Flattens a message body. A string body is content; an array body splits into text, thinking, and tool calls,
/// so a query can match what a tool was asked to do even when the visible text says nothing about it.
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
                let Some(object) = part.as_object() else {
                    continue;
                };
                let Some(part_type) = object.get("type").and_then(|value| value.as_str()) else {
                    continue;
                };
                match part_type {
                    "text" => {
                        if let Some(text) = object.get("text").and_then(|value| value.as_str())
                            && !text.is_empty()
                        {
                            texts.push(text.to_string());
                        }
                    }
                    "thinking" => {
                        if let Some(thinking) =
                            object.get("thinking").and_then(|value| value.as_str())
                            && !thinking.is_empty()
                        {
                            thoughts.push(thinking.to_string());
                        }
                    }
                    "toolCall" => {
                        let name = object
                            .get("name")
                            .and_then(|value| value.as_str())
                            .filter(|name| !name.is_empty())
                            .map(str::to_string);
                        let arguments =
                            object
                                .get("arguments")
                                .and_then(|arguments| match arguments {
                                    serde_json::Value::Null => None,
                                    serde_json::Value::String(text) => Some(text.clone()),
                                    other => Some(serde_json::to_string(other).unwrap_or_default()),
                                });
                        tool_calls.push(SearchToolCall { name, arguments });
                    }
                    _ => {}
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
    if role == "custom"
        && message
            .get("customType")
            .and_then(|value| value.as_str())
            .is_some_and(|kind| EXCLUDED_CUSTOM_TYPES.contains(&kind))
    {
        return None;
    }

    let mapped = map_content(message.get("content").unwrap_or(&serde_json::Value::Null));
    let mut tool_calls = mapped.tool_calls;
    if let Some(name) = message.get("toolName").and_then(|value| value.as_str()) {
        tool_calls.push(SearchToolCall {
            name: Some(name.to_string()),
            arguments: None,
        });
    }

    // A tool result is output, not something the assistant said: keeping it in `content` would make it read as a
    // statement of the agent rather than a result it received.
    let is_tool_result = role == "toolResult";
    let content = if is_tool_result || mapped.content.is_empty() {
        None
    } else {
        Some(serde_json::Value::String(mapped.content.clone()))
    };
    let tool_return = if is_tool_result && !mapped.content.is_empty() {
        Some(serde_json::Value::String(mapped.content))
    } else {
        None
    };

    Some(SearchDocument {
        id,
        conversation_id: conversation_id.to_string(),
        date: entry
            .get("timestamp")
            .and_then(|value| value.as_str())
            .map(str::to_string),
        message_type: Some(role),
        content,
        reasoning: (!mapped.reasoning.is_empty()).then_some(mapped.reasoning),
        summary: None,
        tool_calls: (!tool_calls.is_empty()).then_some(tool_calls),
        tool_return,
        func_response: None,
    })
}

/// Documents come back with an empty `conversation_id`: a header line can appear after message lines, so the id is
/// only known once the whole file has been read, and filling it per line would silently mislabel early messages.
fn read_session_file(file_path: &Path) -> (Option<SessionHeader>, Vec<SearchDocument>) {
    let Ok(raw) = fs::read_to_string(file_path) else {
        return (None, Vec::new());
    };
    let mut header = None;
    let mut documents = Vec::new();
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
        if let Some(object) = parsed.as_object()
            && let Some(document) = to_document(object, "")
        {
            documents.push(document);
        }
    }
    (header, documents)
}

fn list_session_files(sessions_dir: &Path) -> Vec<(PathBuf, String)> {
    if !sessions_dir.exists() {
        return Vec::new();
    }
    let Ok(entries) = fs::read_dir(sessions_dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .collect();
    paths.sort();

    let mut files = Vec::new();
    for path in paths {
        if path.is_dir() {
            let directory_name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let Ok(sub_entries) = fs::read_dir(&path) else {
                continue;
            };
            let mut sub_paths: Vec<PathBuf> = sub_entries
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .collect();
            sub_paths.sort();
            for sub_path in sub_paths {
                if is_session_file(&sub_path) {
                    files.push((sub_path, directory_name.clone()));
                }
            }
        } else if is_session_file(&path) {
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

fn is_session_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(SESSION_SUFFIX))
}

impl TranscriptProvider for SessionProvider {
    fn list_conversations(&self) -> Vec<TranscriptConversation> {
        let mut conversations = Vec::new();
        for (file_path, directory_name) in list_session_files(&self.options.sessions_dir) {
            let (header, documents) = read_session_file(&file_path);
            let id = match &header {
                Some(header) => header.id.clone(),
                // A file with no readable header still has messages worth searching; its stem is the best id
                // available, and dropping the conversation would hide them.
                None => file_path
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string(),
            };

            // A duplicated line must not become a duplicated hit.
            let mut seen = HashSet::new();
            let mut messages = Vec::new();
            for mut document in documents {
                if seen.insert(document.id.clone()) {
                    document.conversation_id.clone_from(&id);
                    messages.push(document);
                }
            }
            if messages.is_empty() {
                continue;
            }

            let hidden = self.is_hidden(&SenpiHiddenCandidate {
                file_path,
                directory_name,
                header,
            });
            conversations.push(TranscriptConversation {
                id,
                messages,
                hidden: hidden.then_some(true),
            });
        }
        conversations
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, lines: &[&str]) -> PathBuf {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("mkdir");
        }
        fs::write(&path, lines.join("\n")).expect("write");
        path
    }

    fn provider(dir: &Path) -> SessionProvider {
        SessionProvider::new(SessionProviderOptions {
            sessions_dir: dir.to_path_buf(),
            excluded_dirs: None,
            hidden_marker_file: None,
            is_hidden: None,
        })
    }

    #[test]
    fn a_session_becomes_one_conversation_with_its_header_id() {
        let dir = tempfile::tempdir().expect("dir");
        write(
            dir.path(),
            "proj/s1.jsonl",
            &[
                r#"{"type":"session","id":"s1","timestamp":"2026-01-01T00:00:00Z"}"#,
                r#"{"type":"message","id":"m1","timestamp":"2026-01-01T00:00:01Z","message":{"role":"user","content":"hello zebra"}}"#,
            ],
        );
        let conversations = provider(dir.path()).list_conversations();
        assert_eq!(conversations.len(), 1);
        assert_eq!(conversations[0].id, "s1");
        assert_eq!(conversations[0].messages.len(), 1);
        assert_eq!(conversations[0].messages[0].conversation_id, "s1");
    }

    #[test]
    fn a_file_without_a_header_uses_its_stem() {
        let dir = tempfile::tempdir().expect("dir");
        write(
            dir.path(),
            "proj/bare.jsonl",
            &[r#"{"type":"message","id":"m1","message":{"role":"user","content":"zebra"}}"#],
        );
        let conversations = provider(dir.path()).list_conversations();
        assert_eq!(conversations[0].id, "bare");
    }

    #[test]
    fn a_duplicate_message_id_is_kept_once() {
        let dir = tempfile::tempdir().expect("dir");
        let line = r#"{"type":"message","id":"m1","message":{"role":"user","content":"zebra"}}"#;
        write(dir.path(), "proj/s1.jsonl", &[line, line]);
        let conversations = provider(dir.path()).list_conversations();
        assert_eq!(conversations[0].messages.len(), 1);
    }

    #[test]
    fn a_malformed_line_is_skipped_not_fatal() {
        let dir = tempfile::tempdir().expect("dir");
        write(
            dir.path(),
            "proj/s1.jsonl",
            &[
                "not json at all",
                r#"{"type":"message","id":"m1","message":{"role":"user","content":"zebra"}}"#,
            ],
        );
        let conversations = provider(dir.path()).list_conversations();
        assert_eq!(conversations[0].messages.len(), 1);
    }

    #[test]
    fn an_archived_session_is_hidden() {
        let dir = tempfile::tempdir().expect("dir");
        let path = write(
            dir.path(),
            "proj/s1.jsonl",
            &[r#"{"type":"message","id":"m1","message":{"role":"user","content":"zebra"}}"#],
        );
        fs::write(path.with_extension("jsonl.archived"), "").expect("archive");
        assert_eq!(
            provider(dir.path()).list_conversations()[0].hidden,
            Some(true)
        );
    }

    #[test]
    fn an_excluded_directory_is_hidden() {
        let dir = tempfile::tempdir().expect("dir");
        write(
            dir.path(),
            "proj/s1.jsonl",
            &[r#"{"type":"message","id":"m1","message":{"role":"user","content":"zebra"}}"#],
        );
        let provider = SessionProvider::new(SessionProviderOptions {
            sessions_dir: dir.path().to_path_buf(),
            excluded_dirs: Some(vec!["proj".into()]),
            hidden_marker_file: None,
            is_hidden: None,
        });
        assert_eq!(provider.list_conversations()[0].hidden, Some(true));
    }

    #[test]
    fn a_marker_file_hides_the_sessions_beside_it() {
        let dir = tempfile::tempdir().expect("dir");
        write(
            dir.path(),
            "proj/s1.jsonl",
            &[r#"{"type":"message","id":"m1","message":{"role":"user","content":"zebra"}}"#],
        );
        fs::write(dir.path().join("proj/.hidden"), "").expect("marker");
        let provider = SessionProvider::new(SessionProviderOptions {
            sessions_dir: dir.path().to_path_buf(),
            excluded_dirs: None,
            hidden_marker_file: Some(".hidden".into()),
            is_hidden: None,
        });
        assert_eq!(provider.list_conversations()[0].hidden, Some(true));
    }

    #[test]
    fn a_tool_result_becomes_the_return_not_the_content() {
        let dir = tempfile::tempdir().expect("dir");
        write(
            dir.path(),
            "proj/s1.jsonl",
            &[r#"{"type":"message","id":"m1","message":{"role":"toolResult","content":"exit 0"}}"#],
        );
        let document = &provider(dir.path()).list_conversations()[0].messages[0];
        assert!(document.content.is_none());
        assert_eq!(
            document.tool_return,
            Some(serde_json::Value::String("exit 0".into()))
        );
    }

    #[test]
    fn a_tool_call_part_keeps_its_name_and_arguments() {
        let dir = tempfile::tempdir().expect("dir");
        write(
            dir.path(),
            "proj/s1.jsonl",
            &[
                r#"{"type":"message","id":"m1","message":{"role":"assistant","content":[{"type":"toolCall","name":"bash","arguments":{"command":"ls"}}]}}"#,
            ],
        );
        let document = &provider(dir.path()).list_conversations()[0].messages[0];
        let calls = document.tool_calls.as_ref().expect("calls");
        assert_eq!(calls[0].name.as_deref(), Some("bash"));
        assert_eq!(calls[0].arguments.as_deref(), Some(r#"{"command":"ls"}"#));
    }

    #[test]
    fn a_memory_injection_is_not_searchable() {
        let dir = tempfile::tempdir().expect("dir");
        write(
            dir.path(),
            "proj/s1.jsonl",
            &[
                r#"{"type":"message","id":"m1","message":{"role":"custom","customType":"omo-memory:notice","content":"<memories>zebra</memories>"}}"#,
                r#"{"type":"message","id":"m2","message":{"role":"user","content":"zebra"}}"#,
            ],
        );
        let conversations = provider(dir.path()).list_conversations();
        assert_eq!(conversations[0].messages.len(), 1);
        assert_eq!(conversations[0].messages[0].id, "m2");
    }

    #[test]
    fn a_file_with_no_messages_is_dropped() {
        let dir = tempfile::tempdir().expect("dir");
        write(
            dir.path(),
            "proj/s1.jsonl",
            &[r#"{"type":"session","id":"s1"}"#],
        );
        assert!(provider(dir.path()).list_conversations().is_empty());
    }

    #[test]
    fn a_missing_directory_is_empty_not_an_error() {
        let dir = tempfile::tempdir().expect("dir");
        let provider = provider(&dir.path().join("does-not-exist"));
        assert!(provider.list_conversations().is_empty());
    }

    #[test]
    fn a_flat_session_file_is_read() {
        let dir = tempfile::tempdir().expect("dir");
        write(
            dir.path(),
            "s1.jsonl",
            &[r#"{"type":"message","id":"m1","message":{"role":"user","content":"zebra"}}"#],
        );
        assert_eq!(provider(dir.path()).list_conversations().len(), 1);
    }
}
