//! Bounded per-session summary (upstream `session-digest.ts`).

use super::constants::MAX_SESSION_DIGEST_BYTES;
use super::transcript_records::TranscriptRecord;
use super::types::{AgentSessionThread, MessageRole, SessionDigest, SessionToolUse};

const FIRST_REQUEST_CHARS: usize = 1500;
const OUTCOME_CHARS: usize = 1000;
const MAX_TOOLS: usize = 24;
const MAX_FILES: usize = 32;
const MAX_PATH_CHARS: usize = 200;

pub fn digest_byte_length(digest: &SessionDigest) -> usize {
    digest.to_json_string().len()
}

/// UTF-16 length, matching the JS `String.length` the upstream limits are written against.
fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

fn slice_utf16(text: &str, limit: usize) -> String {
    let units: Vec<u16> = text.encode_utf16().take(limit).collect();
    String::from_utf16_lossy(&units)
}

struct Clipped {
    text: String,
    clipped: bool,
}

fn clip(text: &str, limit: usize) -> Clipped {
    let trimmed = text.trim();
    if utf16_len(trimmed) <= limit {
        return Clipped {
            text: trimmed.to_owned(),
            clipped: false,
        };
    }
    Clipped {
        text: format!("{}…", slice_utf16(trimmed, limit.saturating_sub(1))),
        clipped: true,
    }
}

fn summarize_tools(records: &[TranscriptRecord]) -> (Vec<SessionToolUse>, Vec<String>) {
    let mut counts: Vec<(String, usize)> = Vec::new();
    let mut files: Vec<String> = Vec::new();
    for tool in records
        .iter()
        .flat_map(|record| record.tools.iter().flatten())
    {
        match counts.iter_mut().find(|(name, _)| *name == tool.name) {
            Some((_, count)) => *count += 1,
            None => counts.push((tool.name.clone(), 1)),
        }
        for file in &tool.changed_files {
            if !files.contains(file) {
                files.push(file.clone());
            }
        }
    }
    let mut tools: Vec<SessionToolUse> = counts
        .into_iter()
        .map(|(name, count)| SessionToolUse { name, count })
        .collect();
    tools.sort_by(|left, right| {
        right
            .count
            .cmp(&left.count)
            .then_with(|| left.name.cmp(&right.name))
    });
    (tools, files)
}

/// A bounded, local-only summary of one session: the first request, the tools used, the files changed and
/// the last assistant answer. Lists are cut first, then the two texts, until the JSON fits the byte cap.
pub fn build_session_digest(
    thread: &AgentSessionThread,
    records: &[TranscriptRecord],
    cwd: &str,
) -> SessionDigest {
    let first_user = thread.messages.iter().find(|m| m.role == MessageRole::User);
    let last_assistant = thread
        .messages
        .iter()
        .rev()
        .find(|m| m.role == MessageRole::Assistant);
    let request = clip(
        first_user.map_or("", |m| m.text.as_str()),
        FIRST_REQUEST_CHARS,
    );
    let outcome = clip(
        last_assistant.map_or("", |m| m.text.as_str()),
        OUTCOME_CHARS,
    );
    let (tools, all_files) = summarize_tools(records);
    let files: Vec<String> = all_files
        .iter()
        .take(MAX_FILES)
        .map(|f| clip(f, MAX_PATH_CHARS).text)
        .collect();
    let truncated = request.clipped
        || outcome.clipped
        || tools.len() > MAX_TOOLS
        || all_files.len() > MAX_FILES;
    let mut digest = SessionDigest {
        session_key: format!("{}:{}", thread.source.as_str(), thread.provider_session_id),
        source: thread.source,
        provider_session_id: thread.provider_session_id.clone(),
        cwd: cwd.to_owned(),
        title: thread.title.clone(),
        model: thread.model.clone(),
        started_at: thread.created_at.clone(),
        last_active_at: thread.updated_at.clone(),
        message_count: thread.messages.len(),
        first_request: request.text,
        tools_used: tools.into_iter().take(MAX_TOOLS).collect(),
        files_changed: files,
        outcome: outcome.text,
        truncated,
    };
    while digest_byte_length(&digest) > MAX_SESSION_DIGEST_BYTES {
        let request_len = utf16_len(&digest.first_request);
        let outcome_len = utf16_len(&digest.outcome);
        if !digest.files_changed.is_empty() {
            digest.files_changed.pop();
        } else if !digest.tools_used.is_empty() {
            digest.tools_used.pop();
        } else if request_len >= outcome_len && request_len > 0 {
            digest.first_request = clip(&digest.first_request, request_len * 9 / 10).text;
        } else if outcome_len > 0 {
            digest.outcome = clip(&digest.outcome, outcome_len * 9 / 10).text;
        } else {
            let title_len = utf16_len(&digest.title);
            digest.title = clip(&digest.title, title_len * 9 / 10).text;
            digest.cwd = clip(&digest.cwd, MAX_PATH_CHARS).text;
        }
        digest.truncated = true;
    }
    digest
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_history::transcript_records::TranscriptToolCall;
    use crate::session_history::types::{AgentSessionSource, AgentSessionThreadMessage};

    fn thread(first: &str, last: &str) -> AgentSessionThread {
        let message = |role, text: &str| AgentSessionThreadMessage {
            role,
            text: text.to_owned(),
            created_at: "2026-01-01T00:00:00.000Z".into(),
        };
        AgentSessionThread {
            source: AgentSessionSource::ClaudeAgent,
            provider_session_id: "s1".into(),
            title: "t".into(),
            model: None,
            created_at: "2026-01-01T00:00:00.000Z".into(),
            updated_at: "2026-01-01T00:00:00.000Z".into(),
            messages: vec![
                message(MessageRole::User, first),
                message(MessageRole::Assistant, last),
            ],
        }
    }

    #[test]
    fn small_digest_is_untruncated_and_keyed() {
        let digest = build_session_digest(&thread("do it", "done"), &[], "/p");
        assert_eq!(digest.session_key, "claudeAgent:s1");
        assert!(!digest.truncated);
        assert_eq!(digest.message_count, 2);
    }

    #[test]
    fn long_text_is_clipped_with_an_ellipsis_and_fits_the_cap() {
        let long = "x".repeat(5000);
        let record = TranscriptRecord {
            tools: Some(
                (0..40)
                    .map(|i| TranscriptToolCall {
                        name: format!("tool{i}"),
                        changed_files: vec![format!("/f/{i}")],
                    })
                    .collect(),
            ),
            ..TranscriptRecord::default()
        };
        let digest = build_session_digest(&thread(&long, &long), &[record], "/p");
        assert!(digest.truncated);
        assert!(digest_byte_length(&digest) <= MAX_SESSION_DIGEST_BYTES);
        assert!(digest.first_request.ends_with('…') || digest.first_request.chars().count() < 1500);
    }
}
