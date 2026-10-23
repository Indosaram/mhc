//! Transcript records into a visible thread (upstream `transcript-parser.ts`).

use std::collections::VecDeque;

use super::codex_canonical_turns::{canonical_codex_response_user_indices, extract_text};
use super::constants::{MAX_IMPORT_RECORDS, MAX_IMPORTED_MESSAGES};
use super::transcript_records::{TranscriptRecord, decode_transcript_line};
use super::types::{
    AgentSessionSource, AgentSessionThread, AgentSessionThreadMessage, MessageRole,
};

#[derive(Clone, Debug)]
pub struct TranscriptMetadata {
    pub source: AgentSessionSource,
    pub fallback_session_id: String,
    pub last_active_at_ms: i64,
}

#[derive(Clone)]
struct RetainedMessage {
    message: AgentSessionThreadMessage,
    codex_response_user: bool,
    id: u64,
}

pub fn iso_from_ms(ms: i64) -> String {
    jiff::Timestamp::from_millisecond(ms)
        .map(|ts| ts.strftime("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
        .unwrap_or_else(|_| "1970-01-01T00:00:00.000Z".to_owned())
}

fn normalize_timestamp(value: Option<&str>, fallback: &str) -> String {
    let Some(value) = value else {
        return fallback.to_owned();
    };
    match value.parse::<jiff::Timestamp>() {
        Ok(ts) => iso_from_ms(ts.as_millisecond()),
        Err(_) => fallback.to_owned(),
    }
}

/// Keep visible user and assistant text while ignoring tools, reasoning, and malformed records.
pub fn parse_agent_session_transcript(
    input: &TranscriptMetadata,
    contents: &str,
) -> Option<AgentSessionThread> {
    let records = contents.strip_suffix('\n').unwrap_or(contents);
    let lines: Vec<&str> = records.split('\n').take(MAX_IMPORT_RECORDS + 1).collect();
    if lines.len() > MAX_IMPORT_RECORDS {
        return None;
    }
    let decoded: Vec<TranscriptRecord> = lines
        .iter()
        .filter_map(|line| decode_transcript_line(line, input.source))
        .collect();
    parse_agent_session_records(input, &decoded)
}

#[derive(Default)]
struct MessageWindow {
    messages: VecDeque<RetainedMessage>,
    first: Option<RetainedMessage>,
    next_id: u64,
}

impl MessageWindow {
    fn retain(&mut self, message: AgentSessionThreadMessage, codex_response_user: bool) {
        self.next_id += 1;
        let retained = RetainedMessage {
            message,
            codex_response_user,
            id: self.next_id,
        };
        if self.first.is_none() && retained.message.role == MessageRole::User {
            self.first = Some(retained.clone());
        }
        self.messages.push_back(retained);
        if self.messages.len() > MAX_IMPORTED_MESSAGES {
            self.messages.pop_front();
        }
    }

    /// Codex writes one prompt as both a response item and an event; drop only the matching response copy.
    fn drop_response_copy(&mut self, text: &str) {
        for index in (0..self.messages.len()).rev() {
            let message = &self.messages[index];
            if message.message.role == MessageRole::Assistant {
                return;
            }
            if message.codex_response_user && message.message.text.trim() == text.trim() {
                if self
                    .first
                    .as_ref()
                    .is_some_and(|first| first.id == message.id)
                {
                    self.first = None;
                }
                self.messages.remove(index);
                return;
            }
        }
    }

    fn has_matching_event_in_turn(&self, text: &str) -> bool {
        let comparison = text.trim();
        for message in self.messages.iter().rev() {
            if message.message.role == MessageRole::Assistant {
                return false;
            }
            if message.message.role == MessageRole::User
                && !message.codex_response_user
                && message.message.text.trim() == comparison
            {
                return true;
            }
        }
        false
    }
}

fn non_empty_trim(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|v| !v.is_empty())
}

pub fn parse_agent_session_records(
    input: &TranscriptMetadata,
    records: &[TranscriptRecord],
) -> Option<AgentSessionThread> {
    let fallback_timestamp = iso_from_ms(input.last_active_at_ms);
    // Claude filenames are session ids; Codex rollout names are not, so only metadata can supply one.
    let mut provider_session_id = if input.source == AgentSessionSource::Codex {
        String::new()
    } else {
        input.fallback_session_id.clone()
    };
    let mut title: Option<String> = None;
    let mut model: Option<String> = None;
    let mut has_codex_session_id = false;
    let mut window = MessageWindow::default();
    let canonical = if input.source == AgentSessionSource::Codex {
        canonical_codex_response_user_indices(records)
    } else {
        Default::default()
    };
    let created_at = |record: &TranscriptRecord| {
        normalize_timestamp(record.timestamp.as_deref(), &fallback_timestamp)
    };

    for (record_index, record) in records.iter().enumerate() {
        let kind = record.kind.as_deref();
        if input.source == AgentSessionSource::ClaudeAgent {
            if record.is_sidechain == Some(true)
                || record.is_meta == Some(true)
                || record.is_compact_summary == Some(true)
            {
                continue;
            }
            if let Some(id) = non_empty_trim(record.session_id.as_deref()) {
                provider_session_id = id.to_owned();
            }
            if let Some(t) = non_empty_trim(record.ai_title.as_deref()) {
                title = Some(t.to_owned());
            }
            let message_model =
                non_empty_trim(record.message.as_ref().and_then(|m| m.model.as_deref()));
            // Claude uses `<synthetic>` for local error responses; it is not a resumable model id.
            if let Some(m) = message_model
                && m != "<synthetic>"
            {
                model = Some(m.to_owned());
            }
            let role = match kind {
                Some("user") => MessageRole::User,
                Some("assistant") => MessageRole::Assistant,
                _ => continue,
            };
            let text = extract_text(record.message.as_ref().and_then(|m| m.content.as_ref()));
            if !text.is_empty() {
                window.retain(
                    AgentSessionThreadMessage {
                        role,
                        text,
                        created_at: created_at(record),
                    },
                    false,
                );
            }
            continue;
        }
        let payload = record.payload.as_ref();
        let payload_type = payload.and_then(|p| p.kind.as_deref());
        if kind == Some("session_meta") {
            let session_id = non_empty_trim(payload.and_then(|p| p.id.as_deref()))
                .or_else(|| non_empty_trim(payload.and_then(|p| p.session_id.as_deref())));
            if !has_codex_session_id && let Some(id) = session_id {
                provider_session_id = id.to_owned();
                has_codex_session_id = true;
            }
            continue;
        }
        if kind == Some("turn_context")
            && let Some(m) = non_empty_trim(payload.and_then(|p| p.model.as_deref()))
        {
            model = Some(m.to_owned());
            continue;
        }
        if kind == Some("event_msg") && payload_type == Some("user_message") {
            let text = payload.and_then(|p| p.message.as_deref()).unwrap_or("");
            if text.trim().is_empty() {
                continue;
            }
            window.drop_response_copy(text);
            window.retain(
                AgentSessionThreadMessage {
                    role: MessageRole::User,
                    text: text.to_owned(),
                    created_at: created_at(record),
                },
                false,
            );
            continue;
        }
        if kind != Some("response_item") || payload_type != Some("message") {
            continue;
        }
        let role = match payload.and_then(|p| p.role.as_deref()) {
            Some("user") => MessageRole::User,
            Some("assistant") => MessageRole::Assistant,
            _ => continue,
        };
        let text = extract_text(payload.and_then(|p| p.content.as_ref()));
        if text.is_empty() {
            continue;
        }
        if role == MessageRole::User
            && (canonical.contains(&record_index) || window.has_matching_event_in_turn(&text))
        {
            continue;
        }
        window.retain(
            AgentSessionThreadMessage {
                role,
                text,
                created_at: created_at(record),
            },
            role == MessageRole::User,
        );
    }

    let first = window.first.clone()?;
    if provider_session_id.trim().is_empty() {
        return None;
    }
    let visible: Vec<AgentSessionThreadMessage> =
        window.messages.iter().map(|m| m.message.clone()).collect();
    let first_retained = window.messages.iter().any(|m| m.id == first.id);
    let retained: Vec<AgentSessionThreadMessage> = if first_retained {
        visible
    } else {
        let keep = MAX_IMPORTED_MESSAGES - 1;
        let tail_start = visible.len().saturating_sub(keep);
        std::iter::once(first.message.clone())
            .chain(visible[tail_start..].iter().cloned())
            .collect()
    };
    let first_line = first.message.text.trim().split('\n').next().unwrap_or("");
    let sliced: Vec<u16> = first_line.encode_utf16().take(100).collect();
    let derived_title = String::from_utf16_lossy(&sliced).trim().to_owned();
    let created = retained
        .first()
        .map_or_else(|| fallback_timestamp.clone(), |m| m.created_at.clone());
    Some(AgentSessionThread {
        source: input.source,
        provider_session_id,
        title: title.unwrap_or_else(|| {
            if derived_title.is_empty() {
                "Imported thread".to_owned()
            } else {
                derived_title
            }
        }),
        model,
        created_at: created,
        updated_at: fallback_timestamp,
        messages: retained,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(source: AgentSessionSource) -> TranscriptMetadata {
        TranscriptMetadata {
            source,
            fallback_session_id: "file-id".into(),
            last_active_at_ms: 1_700_000_000_000,
        }
    }

    #[test]
    fn claude_skips_sidechains_and_ignores_synthetic_model() {
        let contents = [
            r#"{"type":"user","message":{"role":"user","content":"first ask"},"sessionId":"S1","timestamp":"2026-01-01T00:00:00Z"}"#,
            r#"{"type":"assistant","isSidechain":true,"message":{"role":"assistant","content":"hidden"}}"#,
            r#"{"type":"assistant","message":{"role":"assistant","model":"<synthetic>","content":[{"type":"text","text":"err"}]}}"#,
            r#"{"type":"assistant","message":{"role":"assistant","model":"claude-x","content":[{"type":"text","text":"done"}]}}"#,
        ]
        .join("\n");
        let thread =
            parse_agent_session_transcript(&meta(AgentSessionSource::ClaudeAgent), &contents);
        let thread = thread.expect("thread");
        assert_eq!(thread.provider_session_id, "S1");
        assert_eq!(thread.model.as_deref(), Some("claude-x"));
        assert_eq!(thread.messages.len(), 3);
        assert_eq!(thread.title, "first ask");
        assert_eq!(thread.created_at, "2026-01-01T00:00:00.000Z");
    }

    #[test]
    fn codex_dedups_response_copy_against_event_and_needs_a_session_id() {
        let contents = [
            r#"{"type":"session_meta","payload":{"id":"C1","cwd":"/p"}}"#,
            r#"{"type":"turn_context","payload":{"model":"gpt-x"}}"#,
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"hi there"}]}}"#,
            r#"{"type":"event_msg","payload":{"type":"user_message","message":"hi there"}}"#,
            r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"yo"}]}}"#,
        ]
        .join("\n");
        let thread = parse_agent_session_transcript(&meta(AgentSessionSource::Codex), &contents)
            .expect("thread");
        assert_eq!(thread.provider_session_id, "C1");
        assert_eq!(thread.model.as_deref(), Some("gpt-x"));
        assert_eq!(thread.messages.len(), 2);
        let no_id = r#"{"type":"event_msg","payload":{"type":"user_message","message":"hi"}}"#;
        assert!(parse_agent_session_transcript(&meta(AgentSessionSource::Codex), no_id).is_none());
    }

    #[test]
    fn keeps_first_user_message_when_window_overflows() {
        let mut lines =
            vec![r#"{"type":"user","message":{"role":"user","content":"opening"}}"#.to_owned()];
        for i in 0..250 {
            lines.push(format!(
                r#"{{"type":"assistant","message":{{"role":"assistant","content":"a{i}"}}}}"#
            ));
        }
        let thread = parse_agent_session_transcript(
            &meta(AgentSessionSource::ClaudeAgent),
            &lines.join("\n"),
        )
        .expect("thread");
        assert_eq!(thread.messages.len(), MAX_IMPORTED_MESSAGES);
        assert_eq!(thread.messages[0].text, "opening");
        assert_eq!(thread.messages[199].text, "a249");
    }

    #[test]
    fn too_many_records_yields_none() {
        let contents = "{}\n".repeat(MAX_IMPORT_RECORDS + 1);
        assert!(
            parse_agent_session_transcript(&meta(AgentSessionSource::Codex), &contents).is_none()
        );
    }
}
