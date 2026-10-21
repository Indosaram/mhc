//! Port of senpi api/anthropic-web-search-expiry.ts.
use serde_json::{Map, Value, json};
use std::collections::HashSet;

pub fn is_expired_web_search_content_error(status: Option<u16>, message: &str) -> bool {
    status == Some(400)
        && message
            .to_ascii_lowercase()
            .contains("invalid `encrypted_content` in `search_result` block")
}

pub fn expired_web_search_replay_ids(params: &Map<String, Value>, error: &str) -> Vec<String> {
    let Some(messages) = params.get("messages").and_then(Value::as_array) else {
        return Vec::new();
    };
    let lower = error.to_ascii_lowercase();
    let last = lower
        .match_indices("messages.")
        .find_map(|(index, _)| {
            let tail = &lower[index + "messages.".len()..];
            let (number, tail) = tail.split_once(".content.")?;
            let (_, suffix) = tail.split_once(": invalid `encrypted_content`")?;
            let _ = suffix;
            let content_index = tail.split(':').next()?;
            content_index.parse::<usize>().ok()?;
            number.parse::<usize>().ok()
        })
        .unwrap_or_else(|| messages.len().saturating_sub(1));
    messages
        .iter()
        .take(last.saturating_add(1))
        .filter(|message| message["role"] == "assistant")
        .filter_map(|message| message["content"].as_array())
        .flatten()
        .filter(|block| block["type"] == "web_search_tool_result")
        .filter_map(|block| block["tool_use_id"].as_str().map(str::to_owned))
        .collect()
}

pub fn demote_expired_web_search_replay(
    params: &mut Map<String, Value>,
    expired: &HashSet<String>,
) {
    if expired.is_empty() {
        return;
    }
    let Some(messages) = params.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };
    for message in messages {
        if message["role"] != "assistant" {
            continue;
        }
        let Some(content) = message.get_mut("content").and_then(Value::as_array_mut) else {
            continue;
        };
        for block in content {
            if block["type"] == "server_tool_use"
                && block["name"] == "web_search"
                && block["id"].as_str().is_some_and(|id| expired.contains(id))
            {
                let text = block["input"]["query"].as_str().map_or_else(
                    || "Searched the web.".to_owned(),
                    |query| format!("Searched the web for: {query}"),
                );
                *block = json!({"type":"text", "text":text});
            } else if block["type"] == "web_search_tool_result"
                && block["tool_use_id"]
                    .as_str()
                    .is_some_and(|id| expired.contains(id))
            {
                let lines: Vec<String> = block["content"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|result| result["type"] == "web_search_result")
                    .map(|result| {
                        format!(
                            "- {} ({})",
                            result["title"].as_str().unwrap_or_default(),
                            result["url"].as_str().unwrap_or_default()
                        )
                    })
                    .collect();
                let text = if lines.is_empty() {
                    "Web search returned no results.".to_owned()
                } else {
                    format!(
                        "Web search results (page contents no longer available):\n{}",
                        lines.join("\n")
                    )
                };
                *block = json!({"type":"text", "text":text});
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn demotion_preserves_newer_searches_when_rejection_names_older_message() {
        // Given two independently encrypted search results.
        let mut params = json!({"messages":[
            {"role":"assistant","content":[{"type":"server_tool_use","name":"web_search","id":"old","input":{"query":"q"}},{"type":"web_search_tool_result","tool_use_id":"old","content":[]}]},
            {"role":"assistant","content":[{"type":"web_search_tool_result","tool_use_id":"new","content":[]}]}
        ]}).as_object().cloned().unwrap_or_default();
        let error = "messages.0.content.1: Invalid `encrypted_content` in `search_result` block";
        // When only results at or before the rejected message are demoted.
        let expired = expired_web_search_replay_ids(&params, error)
            .into_iter()
            .collect();
        demote_expired_web_search_replay(&mut params, &expired);
        // Then the newer opaque result still replays natively.
        assert_eq!(params["messages"][0]["content"][0]["type"], "text");
        assert_eq!(params["messages"][0]["content"][1]["type"], "text");
        assert_eq!(
            params["messages"][1]["content"][0]["type"],
            "web_search_tool_result"
        );
        assert!(is_expired_web_search_content_error(Some(400), error));
        assert!(!is_expired_web_search_content_error(Some(500), error));
    }
}
