use hex::encode as hex_encode;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::internal_initiator_marker::has_internal_initiator_marker;
use crate::logger::log;

use super::recent_dispatches::get_recent_prompt_dispatch;
use super::types::InternalPromptDispatchResult;

const MAX_PROMPT_DEDUPE_DEPTH: usize = 64;

fn canonicalize_prompt_input_for_dedupe(key: &str, value: &Value, depth: usize) -> Value {
    if key == "signal" {
        return Value::String("[AbortSignal]".to_string());
    }
    if depth > MAX_PROMPT_DEDUPE_DEPTH {
        return Value::String("[MaxDepth]".to_string());
    }
    match value {
        Value::Array(items) => {
            let canonical = items
                .iter()
                .map(|item| canonicalize_prompt_input_for_dedupe("", item, depth + 1))
                .collect();
            Value::Array(canonical)
        }
        Value::Object(map) => {
            let mut sorted_keys: Vec<&String> = map.keys().collect();
            sorted_keys.sort();
            let mut canonical_map = Map::new();
            for entry_key in sorted_keys {
                if let Some(entry_val) = map.get(entry_key) {
                    canonical_map.insert(
                        entry_key.clone(),
                        canonicalize_prompt_input_for_dedupe(entry_key, entry_val, depth + 1),
                    );
                }
            }
            Value::Object(canonical_map)
        }
        other => other.clone(),
    }
}

fn is_continuation_text_part_like(part: &Value) -> bool {
    let Some(part_obj) = part.as_object() else {
        return false;
    };
    if part_obj.get("type").and_then(Value::as_str) != Some("text") {
        return false;
    }
    let Some(text) = part_obj.get("text").and_then(Value::as_str) else {
        return false;
    };
    if !has_internal_initiator_marker(text) {
        return false;
    }
    let Some(metadata) = part_obj.get("metadata").and_then(Value::as_object) else {
        return false;
    };
    metadata.get("compaction_continue").and_then(Value::as_bool) == Some(true)
}

fn has_continuation_prompt_intent(input: &Value) -> bool {
    let Some(body) = input.get("body").and_then(Value::as_object) else {
        return false;
    };
    let Some(parts) = body.get("parts").and_then(Value::as_array) else {
        return false;
    };
    parts.iter().any(is_continuation_text_part_like)
}

pub fn normalize_prompt_input_for_semantic_dedupe(input: &Value) -> Value {
    if has_continuation_prompt_intent(input) {
        return json!({
            "__omo_internal_intent": "continuation"
        });
    }
    canonicalize_prompt_input_for_dedupe("", input, 0)
}

pub fn stringify_prompt_input_for_dedupe(input: &Value) -> String {
    let normalized = normalize_prompt_input_for_semantic_dedupe(input);
    serde_json::to_string(&normalized).unwrap_or_else(|_| input.to_string())
}

pub fn create_semantic_prompt_dedupe_key(input: &Value) -> String {
    let fingerprint = stringify_prompt_input_for_dedupe(input);
    let mut hasher = Sha256::new();
    hasher.update(fingerprint.as_bytes());
    let digest = hex_encode(hasher.finalize());
    format!("semantic:{digest}")
}

pub fn coalesce_recent_semantic_prompt_dispatch(
    session_id: &str,
    dedupe_key: &str,
    source: &str,
    now: u64,
) -> Option<InternalPromptDispatchResult> {
    let recent_dispatch = get_recent_prompt_dispatch(session_id, dedupe_key, now)?;
    log(
        "[prompt-async-gate] prompt coalesced with recent semantic dispatch",
        Some(&json!({
            "sessionID": session_id,
            "source": source,
            "queuedBy": recent_dispatch.source,
        })),
    );
    Some(InternalPromptDispatchResult::Queued {
        queued_by: recent_dispatch.source,
        position: 0,
    })
}
