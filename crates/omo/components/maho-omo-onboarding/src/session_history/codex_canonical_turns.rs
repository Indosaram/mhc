//! Codex prompt de-duplication (upstream `codex-canonical-turns.ts`).

use std::collections::HashSet;

use super::transcript_records::{TranscriptContent, TranscriptRecord};

pub fn extract_text(content: Option<&TranscriptContent>) -> String {
    match content {
        None => String::new(),
        Some(TranscriptContent::Text(text)) => text.trim().to_owned(),
        Some(TranscriptContent::Blocks(blocks)) => blocks
            .iter()
            .filter(|block| {
                matches!(
                    block.kind.as_deref(),
                    Some("text" | "input_text" | "output_text")
                )
            })
            .map(|block| block.text.as_deref().map_or("", str::trim))
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn codex_turn_id(metadata: Option<&serde_json::Value>) -> Option<String> {
    let turn_id = metadata?.as_object()?.get("turn_id")?.as_str()?;
    (!turn_id.trim().is_empty()).then(|| turn_id.to_owned())
}

struct ResponseUser {
    index: usize,
    turn_id: String,
    text: String,
}

fn finish_turn(
    indices: &mut HashSet<usize>,
    canonical_texts: &mut HashSet<String>,
    response_users: &mut Vec<ResponseUser>,
) {
    let canonical_turn_ids: HashSet<&str> = response_users
        .iter()
        .filter(|user| canonical_texts.contains(&user.text))
        .map(|user| user.turn_id.as_str())
        .collect();
    for user in response_users.iter() {
        if canonical_turn_ids.contains(user.turn_id.as_str()) {
            indices.insert(user.index);
        }
    }
    canonical_texts.clear();
    response_users.clear();
}

/// A Codex response item can carry generated setup text beside the real prompt. Response-user records are
/// suppressed only when a shared turn id and a verbatim `user_message` event prove which prompt was typed.
pub fn canonical_codex_response_user_indices(records: &[TranscriptRecord]) -> HashSet<usize> {
    let mut indices = HashSet::new();
    let mut canonical_texts = HashSet::new();
    let mut response_users: Vec<ResponseUser> = Vec::new();
    for (index, record) in records.iter().enumerate() {
        let payload = record.payload.as_ref();
        let payload_type = payload.and_then(|p| p.kind.as_deref());
        let role = payload.and_then(|p| p.role.as_deref());
        let record_type = record.kind.as_deref();
        if record_type == Some("response_item")
            && payload_type == Some("message")
            && role == Some("assistant")
        {
            finish_turn(&mut indices, &mut canonical_texts, &mut response_users);
            continue;
        }
        if record_type == Some("event_msg") && payload_type == Some("user_message") {
            let text = payload
                .and_then(|p| p.message.as_deref())
                .map_or("", str::trim);
            if !text.is_empty() {
                canonical_texts.insert(text.to_owned());
            }
            continue;
        }
        if record_type == Some("response_item")
            && payload_type == Some("message")
            && role == Some("user")
        {
            let turn_id = codex_turn_id(
                payload.and_then(|p| p.internal_chat_message_metadata_passthrough.as_ref()),
            );
            let text = extract_text(payload.and_then(|p| p.content.as_ref()));
            if let Some(turn_id) = turn_id
                && !text.is_empty()
            {
                response_users.push(ResponseUser {
                    index,
                    turn_id,
                    text,
                });
            }
        }
    }
    finish_turn(&mut indices, &mut canonical_texts, &mut response_users);
    indices
}
