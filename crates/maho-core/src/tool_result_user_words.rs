//! Port of senpi `packages/coding-agent/src/core/tool-result-user-words.ts`.
//!
//! Words the user typed that a tool result carries in `details.userWords` instead of its content. A
//! model trained to distrust tool results may ignore user text inside one (senpi#2920), so each request
//! delivers them as a user turn after the tool results, every word after its own label.

use maho_ai::context_provenance::{
    CONTEXT_PROVENANCE_FIELD, context_provenance_fingerprint, get_context_provenance,
};
use serde_json::{Map, Value, json};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolResultUserWord {
    pub label: String,
    pub text: String,
}

pub fn tool_result_user_words(message: &Value) -> Vec<ToolResultUserWord> {
    let Some(words) = message
        .get("details")
        .and_then(|details| details.get("userWords"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    words
        .iter()
        .filter_map(|word| {
            Some(ToolResultUserWord {
                label: word.get("label")?.as_str()?.to_owned(),
                text: word.get("text")?.as_str()?.to_owned(),
            })
        })
        .collect()
}

/// The label block a reference names, then the words, for each word in order.
pub fn user_word_blocks(words: &[ToolResultUserWord]) -> Vec<Value> {
    words
        .iter()
        .flat_map(|word| {
            [
                json!({"type": "text", "text": format!("[{}]", word.label)}),
                json!({"type": "text", "text": word.text}),
            ]
        })
        .collect()
}

fn is_tool_result(message: &Value) -> bool {
    message.get("role").and_then(Value::as_str) == Some("toolResult")
}

/// Inserts one user message after each contiguous run of tool results whose details carry user words.
/// It is derived only from persisted tool results, so live requests, resumed sessions and compaction all
/// see the same messages, and no queue operation can drop them.
pub fn append_tool_result_user_words(messages: Vec<Value>) -> Vec<Value> {
    if !messages
        .iter()
        .any(|message| is_tool_result(message) && !tool_result_user_words(message).is_empty())
    {
        return messages;
    }
    let mut result: Vec<Value> = Vec::with_capacity(messages.len() + 1);
    let mut pending: Vec<Value> = Vec::new();
    let mut source: Option<Value> = None;
    let flush = |result: &mut Vec<Value>,
                 pending: &mut Vec<Value>,
                 source: &mut Option<Value>,
                 next: Option<&Value>| {
        if let Some(from) = source.take()
            && !starts_with_blocks(next, pending)
        {
            result.push(word_message(std::mem::take(pending), &from));
        }
        pending.clear();
    };
    for message in messages {
        if !is_tool_result(&message) {
            flush(&mut result, &mut pending, &mut source, Some(&message));
            result.push(message);
            continue;
        }
        let blocks = user_word_blocks(&tool_result_user_words(&message));
        if !blocks.is_empty() {
            pending.extend(blocks);
            source = Some(message.clone());
        }
        result.push(message);
    }
    flush(&mut result, &mut pending, &mut source, None);
    result
}

/// The word message belongs to its tool result: it carries its own copy of that result's request-local
/// provenance. A sealed result passes its seal on only while the result still matches it; the copy is
/// then sealed with the word message's own fingerprint.
fn word_message(content: Vec<Value>, source: &Value) -> Value {
    let mut message = json!({"role": "user", "content": content, "timestamp": source.get("timestamp").cloned().unwrap_or(Value::Null)});
    let Some(provenance) = get_context_provenance(source) else {
        return message;
    };
    let mut owned: Map<String, Value> = provenance.clone();
    let integrity = owned.remove("integrity");
    let proven = match &integrity {
        None => true,
        Some(seal) => {
            seal.as_str().is_some()
                && seal.as_str().map(str::to_owned) == context_provenance_fingerprint(source)
        }
    };
    if !proven {
        return message;
    }
    if integrity.is_some()
        && let Some(fingerprint) = context_provenance_fingerprint(&message)
    {
        owned.insert("integrity".to_owned(), Value::String(fingerprint));
    }
    if let Some(object) = message.as_object_mut() {
        object.insert(CONTEXT_PROVENANCE_FIELD.to_owned(), Value::Object(owned));
    }
    message
}

/// Already converted output carries the word message right after the run; converting again keeps one.
fn starts_with_blocks(message: Option<&Value>, blocks: &[Value]) -> bool {
    let Some(message) = message else {
        return false;
    };
    if message.get("role").and_then(Value::as_str) != Some("user") {
        return false;
    }
    let Some(content) = message.get("content").and_then(Value::as_array) else {
        return false;
    };
    blocks.iter().enumerate().all(|(index, block)| {
        content.get(index).is_some_and(|candidate| {
            candidate.get("type").and_then(Value::as_str) == Some("text")
                && candidate.get("text") == block.get("text")
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(id: &str, words: Value) -> Value {
        json!({"role":"toolResult","toolCallId":id,"content":[],"timestamp":5,"details":{"userWords":words}})
    }

    #[test]
    fn words_follow_their_run_of_tool_results() {
        let messages = vec![
            json!({"role":"assistant","content":[]}),
            result("a", json!([{"label":"Answer 1","text":"yes"}])),
            result("b", json!([{"label":"Answer 2","text":"no"}, {"label": 3}])),
            json!({"role":"assistant","content":[]}),
        ];
        let out = append_tool_result_user_words(messages);
        assert_eq!(out.len(), 5);
        assert_eq!(out[3]["role"], "user");
        let texts: Vec<&str> = out[3]["content"]
            .as_array()
            .map(|blocks| blocks.iter().filter_map(|b| b["text"].as_str()).collect())
            .unwrap_or_default();
        assert_eq!(texts, vec!["[Answer 1]", "yes", "[Answer 2]", "no"]);
        assert_eq!(out[3]["timestamp"], 5);
    }

    #[test]
    fn converting_twice_keeps_one_word_message() {
        let once =
            append_tool_result_user_words(vec![result("a", json!([{"label":"L","text":"t"}]))]);
        assert_eq!(once.len(), 2);
        let twice = append_tool_result_user_words(once.clone());
        assert_eq!(twice, once);
    }

    #[test]
    fn results_without_words_are_untouched() {
        let messages = vec![json!({"role":"toolResult","content":[],"details":{}})];
        assert_eq!(append_tool_result_user_words(messages.clone()), messages);
        assert!(tool_result_user_words(&json!({"details": null})).is_empty());
    }

    #[test]
    fn unsealed_provenance_is_copied_and_a_broken_seal_is_not() {
        let mut source = result("a", json!([{"label":"L","text":"t"}]));
        source[CONTEXT_PROVENANCE_FIELD] = json!({"origin":"remote"});
        let out = append_tool_result_user_words(vec![source.clone()]);
        assert_eq!(out[1][CONTEXT_PROVENANCE_FIELD], json!({"origin":"remote"}));
        source[CONTEXT_PROVENANCE_FIELD] = json!({"origin":"remote","integrity":"stale"});
        let out = append_tool_result_user_words(vec![source]);
        assert!(out[1].get(CONTEXT_PROVENANCE_FIELD).is_none());
    }
}
