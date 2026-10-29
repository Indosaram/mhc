//! JSONL transcript record decoding (upstream `transcript-records.ts`).

use serde_json::{Map, Value, json};

use super::types::AgentSessionSource;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TranscriptContentBlock {
    pub kind: Option<String>,
    pub text: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TranscriptContent {
    Text(String),
    Blocks(Vec<TranscriptContentBlock>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranscriptToolCall {
    pub name: String,
    pub changed_files: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TranscriptMessage {
    pub role: Option<String>,
    pub content: Option<TranscriptContent>,
    pub model: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TranscriptPayload {
    pub id: Option<String>,
    pub session_id: Option<String>,
    pub kind: Option<String>,
    pub role: Option<String>,
    pub message: Option<String>,
    pub model: Option<String>,
    pub cwd: Option<String>,
    pub content: Option<TranscriptContent>,
    pub internal_chat_message_metadata_passthrough: Option<Value>,
}

/// The fields the desktop `TranscriptRecord` schema selected, plus the tool calls a digest needs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TranscriptRecord {
    pub kind: Option<String>,
    pub timestamp: Option<String>,
    pub cwd: Option<String>,
    pub session_id: Option<String>,
    pub ai_title: Option<String>,
    pub is_sidechain: Option<bool>,
    pub is_meta: Option<bool>,
    pub is_compact_summary: Option<bool>,
    pub message: Option<TranscriptMessage>,
    pub payload: Option<TranscriptPayload>,
    pub tools: Option<Vec<TranscriptToolCall>>,
}

struct Invalid;

type Fields = Map<String, Value>;

fn pick_string(source: &Fields, key: &str) -> Result<Option<String>, Invalid> {
    match source.get(key) {
        None => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(_) => Err(Invalid),
    }
}

fn pick_bool(source: &Fields, key: &str) -> Result<Option<bool>, Invalid> {
    match source.get(key) {
        None => Ok(None),
        Some(Value::Bool(flag)) => Ok(Some(*flag)),
        Some(_) => Err(Invalid),
    }
}

fn decode_blocks(value: &Value) -> Result<Vec<TranscriptContentBlock>, Invalid> {
    let Value::Array(items) = value else {
        return Err(Invalid);
    };
    items
        .iter()
        .map(|block| {
            let Value::Object(fields) = block else {
                return Err(Invalid);
            };
            Ok(TranscriptContentBlock {
                kind: pick_string(fields, "type")?,
                text: pick_string(fields, "text")?,
            })
        })
        .collect()
}

fn decode_message(value: &Value) -> Result<TranscriptMessage, Invalid> {
    let Value::Object(fields) = value else {
        return Err(Invalid);
    };
    let content = match fields.get("content") {
        None => None,
        Some(Value::String(text)) => Some(TranscriptContent::Text(text.clone())),
        Some(other) => Some(TranscriptContent::Blocks(decode_blocks(other)?)),
    };
    Ok(TranscriptMessage {
        role: pick_string(fields, "role")?,
        model: pick_string(fields, "model")?,
        content,
    })
}

fn decode_payload(value: &Value) -> Result<TranscriptPayload, Invalid> {
    let Value::Object(fields) = value else {
        return Err(Invalid);
    };
    Ok(TranscriptPayload {
        id: pick_string(fields, "id")?,
        session_id: pick_string(fields, "session_id")?,
        kind: pick_string(fields, "type")?,
        role: pick_string(fields, "role")?,
        message: pick_string(fields, "message")?,
        model: pick_string(fields, "model")?,
        cwd: pick_string(fields, "cwd")?,
        content: match fields.get("content") {
            None => None,
            Some(other) => Some(TranscriptContent::Blocks(decode_blocks(other)?)),
        },
        internal_chat_message_metadata_passthrough: fields
            .get("internal_chat_message_metadata_passthrough")
            .cloned(),
    })
}

const WRITE_TOOLS: [&str; 6] = [
    "Write",
    "Edit",
    "MultiEdit",
    "NotebookEdit",
    "write",
    "edit",
];

/// Paths named by `*** Update File:`, `*** Add File:`, `*** Delete File:` and `*** Move to:` lines.
pub fn files_from_apply_patch(patch: &str) -> Vec<String> {
    const PREFIXES: [&str; 4] = [
        "*** Update File: ",
        "*** Add File: ",
        "*** Delete File: ",
        "*** Move to: ",
    ];
    patch
        .split('\n')
        .filter_map(|line| {
            let line = line.strip_suffix('\r').unwrap_or(line);
            PREFIXES
                .iter()
                .find_map(|prefix| line.strip_prefix(prefix))
                .map(|rest| rest.trim().to_owned())
        })
        .filter(|path| !path.is_empty())
        .collect()
}

fn claude_tool_calls(message: Option<&Value>) -> Vec<TranscriptToolCall> {
    let Some(Value::Object(message)) = message else {
        return Vec::new();
    };
    let Some(Value::Array(content)) = message.get("content") else {
        return Vec::new();
    };
    content
        .iter()
        .filter_map(|block| {
            let block = block.as_object()?;
            if block.get("type")?.as_str()? != "tool_use" {
                return None;
            }
            let name = block.get("name")?.as_str()?;
            let empty = Map::new();
            let input = block
                .get("input")
                .and_then(Value::as_object)
                .unwrap_or(&empty);
            let target = ["file_path", "notebook_path", "path"]
                .iter()
                .filter_map(|key| input.get(*key))
                .find(|value| !value.is_null());
            let changed_files = match target.and_then(Value::as_str) {
                Some(path) if WRITE_TOOLS.contains(&name) => vec![path.to_owned()],
                _ => Vec::new(),
            };
            Some(TranscriptToolCall {
                name: name.to_owned(),
                changed_files,
            })
        })
        .collect()
}

fn patch_input(payload: &Fields) -> Option<String> {
    if let Some(Value::String(input)) = payload.get("input") {
        return Some(input.clone());
    }
    let Some(Value::String(arguments)) = payload.get("arguments") else {
        return None;
    };
    match serde_json::from_str::<Value>(arguments) {
        Err(_) => Some(arguments.clone()),
        Ok(Value::Object(parsed)) => {
            if let Some(Value::String(input)) = parsed.get("input") {
                return Some(input.clone());
            }
            if let Some(Value::Array(command)) = parsed.get("command") {
                return Some(
                    command
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join("\n"),
                );
            }
            None
        }
        Ok(_) => None,
    }
}

fn codex_tool_calls(payload: Option<&Value>) -> Vec<TranscriptToolCall> {
    let Some(Value::Object(payload)) = payload else {
        return Vec::new();
    };
    let kind = payload.get("type").and_then(Value::as_str);
    if !matches!(
        kind,
        Some("function_call" | "custom_tool_call" | "local_shell_call")
    ) {
        return Vec::new();
    }
    let name = payload
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("shell")
        .to_owned();
    let changed_files = match patch_input(payload) {
        Some(input) if input.contains("*** Begin Patch") => files_from_apply_patch(&input),
        _ => Vec::new(),
    };
    vec![TranscriptToolCall {
        name,
        changed_files,
    }]
}

fn decode_fields(fields: &Fields, source: AgentSessionSource) -> Result<TranscriptRecord, Invalid> {
    let message = fields.get("message");
    let payload = fields.get("payload");
    let tools = if source == AgentSessionSource::ClaudeAgent {
        claude_tool_calls(message)
    } else {
        codex_tool_calls(payload)
    };
    Ok(TranscriptRecord {
        kind: pick_string(fields, "type")?,
        timestamp: pick_string(fields, "timestamp")?,
        cwd: pick_string(fields, "cwd")?,
        session_id: pick_string(fields, "sessionId")?,
        ai_title: pick_string(fields, "aiTitle")?,
        is_sidechain: pick_bool(fields, "isSidechain")?,
        is_meta: pick_bool(fields, "isMeta")?,
        is_compact_summary: pick_bool(fields, "isCompactSummary")?,
        message: message.map(decode_message).transpose()?,
        payload: payload.map(decode_payload).transpose()?,
        tools: (!tools.is_empty()).then_some(tools),
    })
}

/// Mirrors the desktop schema decode: a field of the wrong type rejects the whole record.
pub fn decode_transcript_record(
    value: &Value,
    source: AgentSessionSource,
) -> Option<TranscriptRecord> {
    let Value::Object(fields) = value else {
        return None;
    };
    decode_fields(fields, source).ok()
}

pub fn decode_transcript_line(line: &str, source: AgentSessionSource) -> Option<TranscriptRecord> {
    decode_transcript_record(&serde_json::from_str::<Value>(line).ok()?, source)
}

pub fn extract_decoded_cwd(record: &TranscriptRecord) -> Option<String> {
    let direct = record
        .cwd
        .as_deref()
        .map(str::trim)
        .filter(|cwd| !cwd.is_empty());
    let nested = record
        .payload
        .as_ref()
        .and_then(|payload| payload.cwd.as_deref())
        .map(str::trim)
        .filter(|cwd| !cwd.is_empty());
    direct.or(nested).map(str::to_owned)
}

pub fn should_retain_decoded_record(source: AgentSessionSource, record: &TranscriptRecord) -> bool {
    if extract_decoded_cwd(record).is_some() || record.tools.is_some() {
        return true;
    }
    let kind = record.kind.as_deref();
    if source == AgentSessionSource::ClaudeAgent {
        return matches!(kind, Some("user" | "assistant"))
            || record.session_id.is_some()
            || record.ai_title.is_some()
            || record
                .message
                .as_ref()
                .is_some_and(|message| message.model.is_some());
    }
    let payload = record.payload.as_ref();
    let payload_type = payload.and_then(|p| p.kind.as_deref());
    matches!(kind, Some("session_meta" | "turn_context"))
        || (kind == Some("event_msg") && payload_type == Some("user_message"))
        || (kind == Some("response_item")
            && payload_type == Some("message")
            && matches!(
                payload.and_then(|p| p.role.as_deref()),
                Some("user" | "assistant")
            ))
}

/// Extract `cwd` from a session-meta record, tolerating the shapes each CLI writes.
pub fn extract_cwd_from_line(line: &str) -> Option<String> {
    let Value::Object(parsed) = serde_json::from_str::<Value>(line).ok()? else {
        return None;
    };
    if let Some(cwd) = parsed.get("cwd").and_then(Value::as_str)
        && !cwd.trim().is_empty()
    {
        return Some(cwd.to_owned());
    }
    let cwd = parsed.get("payload")?.as_object()?.get("cwd")?.as_str()?;
    (!cwd.trim().is_empty()).then(|| cwd.to_owned())
}

impl TranscriptRecord {
    /// Approximate JSON form, used to bound the retained history size.
    pub fn to_json(&self) -> Value {
        fn content(value: &TranscriptContent) -> Value {
            match value {
                TranscriptContent::Text(text) => json!(text),
                TranscriptContent::Blocks(blocks) => Value::Array(
                    blocks
                        .iter()
                        .map(|b| json!({"type": b.kind, "text": b.text}))
                        .collect(),
                ),
            }
        }
        let mut out = Map::new();
        let mut put = |key: &str, value: Value| {
            out.insert(key.to_owned(), value);
        };
        put("type", json!(self.kind));
        put("timestamp", json!(self.timestamp));
        put("cwd", json!(self.cwd));
        put("sessionId", json!(self.session_id));
        put("aiTitle", json!(self.ai_title));
        if let Some(message) = &self.message {
            put(
                "message",
                json!({"role": message.role, "model": message.model,
                       "content": message.content.as_ref().map(content)}),
            );
        }
        if let Some(payload) = &self.payload {
            put(
                "payload",
                json!({"id": payload.id, "session_id": payload.session_id, "type": payload.kind,
                       "role": payload.role, "message": payload.message, "model": payload.model,
                       "cwd": payload.cwd, "content": payload.content.as_ref().map(content),
                       "internal_chat_message_metadata_passthrough": payload.internal_chat_message_metadata_passthrough}),
            );
        }
        if let Some(tools) = &self.tools {
            put(
                "tools",
                Value::Array(
                    tools
                        .iter()
                        .map(|t| json!({"name": t.name, "changedFiles": t.changed_files}))
                        .collect(),
                ),
            );
        }
        Value::Object(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrong_typed_field_rejects_the_whole_record() {
        assert!(
            decode_transcript_line(
                r#"{"type":"user","cwd":5}"#,
                AgentSessionSource::ClaudeAgent
            )
            .is_none()
        );
        assert!(
            decode_transcript_line(
                r#"{"type":"user","isMeta":"yes"}"#,
                AgentSessionSource::ClaudeAgent
            )
            .is_none()
        );
        assert!(decode_transcript_line("not json", AgentSessionSource::Codex).is_none());
    }

    #[test]
    fn claude_write_tools_report_changed_files() {
        let line = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"Edit","input":{"file_path":"/a/b.rs"}},{"type":"tool_use","name":"Read","input":{"file_path":"/a/c.rs"}}]}}"#;
        let record = decode_transcript_line(line, AgentSessionSource::ClaudeAgent);
        let tools = record.and_then(|r| r.tools).unwrap_or_default();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].changed_files, vec!["/a/b.rs".to_owned()]);
        assert!(tools[1].changed_files.is_empty());
    }

    #[test]
    fn codex_apply_patch_files_are_extracted() {
        let patch = "*** Begin Patch\n*** Update File: src/a.rs\n@@\n*** Add File: src/b.rs\n*** Move to: src/c.rs\n*** End Patch";
        assert_eq!(
            files_from_apply_patch(patch),
            vec!["src/a.rs", "src/b.rs", "src/c.rs"]
        );
        let line = serde_json::json!({"type":"response_item","payload":{"type":"custom_tool_call","name":"apply_patch","input":patch}}).to_string();
        let tools = decode_transcript_line(&line, AgentSessionSource::Codex)
            .and_then(|r| r.tools)
            .unwrap_or_default();
        assert_eq!(tools[0].name, "apply_patch");
        assert_eq!(tools[0].changed_files.len(), 3);
    }

    #[test]
    fn cwd_extraction_reads_top_level_or_payload() {
        assert_eq!(
            extract_cwd_from_line(r#"{"cwd":"/x"}"#).as_deref(),
            Some("/x")
        );
        assert_eq!(
            extract_cwd_from_line(r#"{"payload":{"cwd":"/y"}}"#).as_deref(),
            Some("/y")
        );
        assert_eq!(extract_cwd_from_line(r#"{"cwd":"  "}"#), None);
    }
}
