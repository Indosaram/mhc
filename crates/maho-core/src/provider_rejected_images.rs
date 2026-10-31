//! Port of senpi `packages/coding-agent/src/core/provider-rejected-images.ts`.
//!
//! Provider image rejections (senpi#2170). A provider that rejects an image rejects every later request
//! that replays it, so one bad image bricked the session. The failed assistant turn is persisted with
//! its error, so the rejection is derived from history on every request (and after a restart): images
//! no successful response had accepted before an image-rejection failure are replaced by a placeholder
//! naming their source.

use std::collections::{BTreeMap, HashMap};
use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Value, json};

static PROVIDER_IMAGE_REJECTION_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)does not represent a valid image|image_parse_error|invalid_image|unsupported image|could not process image|unable to process input image|image_url'?\.?[^\n]*invalid base64|expected a base64-encoded data url|image was specified using the image/[a-z]+ media type|unsupported media type for base64 image|invalid data url for image")
        .unwrap_or_else(|e| panic!("image rejection: {e}"))
});

struct RejectedImage {
    message_index: usize,
    block_index: usize,
    source: String,
}

pub fn is_provider_image_rejection(error_message: Option<&str>) -> bool {
    error_message.is_some_and(|message| PROVIDER_IMAGE_REJECTION_PATTERN.is_match(message))
}

pub fn rejected_image_placeholder(source: &str) -> String {
    format!(
        "[Image omitted: the provider rejected this image ({source}) as invalid, so it is no longer sent with the conversation.]"
    )
}

fn role(message: &Value) -> &str {
    message
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

fn content_blocks(message: &Value) -> &[Value] {
    message
        .get("content")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
}

fn tool_call_source(block: &Value) -> Option<String> {
    let args = block.get("arguments")?.as_object()?;
    ["path", "file_path", "filePath"].iter().find_map(|key| {
        args.get(*key)
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned)
    })
}

fn image_source(message: &Value, tool_call_sources: &HashMap<String, String>) -> String {
    if role(message) != "toolResult" {
        return "an attached image".to_owned();
    }
    if let Some(known) = message
        .get("toolCallId")
        .and_then(Value::as_str)
        .and_then(|id| tool_call_sources.get(id))
        .filter(|known| !known.is_empty())
    {
        return known.clone();
    }
    match message.get("toolName").and_then(Value::as_str) {
        Some(tool_name) => format!("{tool_name} tool result"),
        None => "a tool result".to_owned(),
    }
}

/// Rejected images keyed by the index of the failed assistant turn that rejected them.
fn scan_rejected_images(messages: &[Value]) -> HashMap<usize, Vec<RejectedImage>> {
    let mut tool_call_sources = HashMap::new();
    let mut rejections = HashMap::new();
    let mut unconfirmed: Vec<RejectedImage> = Vec::new();
    for (message_index, message) in messages.iter().enumerate() {
        match role(message) {
            "assistant" => {
                for block in content_blocks(message) {
                    if block.get("type").and_then(Value::as_str) == Some("toolCall")
                        && let (Some(source), Some(id)) = (
                            tool_call_source(block),
                            block.get("id").and_then(Value::as_str),
                        )
                    {
                        tool_call_sources.insert(id.to_owned(), source);
                    }
                }
                let stop_reason = message.get("stopReason").and_then(Value::as_str);
                let error_message = message.get("errorMessage").and_then(Value::as_str);
                if stop_reason == Some("error") && is_provider_image_rejection(error_message) {
                    if !unconfirmed.is_empty() {
                        rejections.insert(message_index, std::mem::take(&mut unconfirmed));
                    }
                    unconfirmed.clear();
                } else if stop_reason != Some("error") && stop_reason != Some("aborted") {
                    unconfirmed.clear();
                }
            }
            "user" | "toolResult" => {
                for (block_index, block) in content_blocks(message).iter().enumerate() {
                    if block.get("type").and_then(Value::as_str) == Some("image") {
                        unconfirmed.push(RejectedImage {
                            message_index,
                            block_index,
                            source: image_source(message, &tool_call_sources),
                        });
                    }
                }
            }
            _ => {}
        }
    }
    rejections
}

/// The sources of the images the failed turn at `failed_index` rejected.
pub fn rejected_image_sources(messages: &[Value], failed_index: usize) -> Vec<String> {
    scan_rejected_images(messages)
        .remove(&failed_index)
        .unwrap_or_default()
        .into_iter()
        .map(|image| image.source)
        .collect()
}

pub fn omit_provider_rejected_images(messages: Vec<Value>) -> Vec<Value> {
    let mut rejected: BTreeMap<usize, BTreeMap<usize, String>> = BTreeMap::new();
    for images in scan_rejected_images(&messages).into_values() {
        for image in images {
            rejected
                .entry(image.message_index)
                .or_default()
                .insert(image.block_index, image.source);
        }
    }
    if rejected.is_empty() {
        return messages;
    }
    messages
        .into_iter()
        .enumerate()
        .map(|(message_index, mut message)| {
            let Some(blocks) = rejected.get(&message_index) else {
                return message;
            };
            let content: Vec<Value> = content_blocks(&message)
                .iter()
                .enumerate()
                .map(|(block_index, block)| match blocks.get(&block_index) {
                    Some(source) => {
                        json!({"type": "text", "text": rejected_image_placeholder(source)})
                    }
                    None => block.clone(),
                })
                .collect();
            message["content"] = Value::Array(content);
            message
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image() -> Value {
        json!({"type":"image","data":"AAAA","mimeType":"image/png"})
    }

    fn failed(error: &str) -> Value {
        json!({"role":"assistant","content":[],"stopReason":"error","errorMessage":error})
    }

    #[test]
    fn images_before_a_rejection_are_replaced_with_their_source() {
        let messages = vec![
            json!({"role":"user","content":[{"type":"text","text":"look"}, image()]}),
            json!({"role":"assistant","content":[{"type":"toolCall","id":"c1","name":"read","arguments":{"path":"/a.png"}}],"stopReason":"toolUse"}),
            json!({"role":"toolResult","toolCallId":"c1","toolName":"read","content":[image()]}),
            failed("Image does not represent a valid image"),
        ];
        assert_eq!(
            rejected_image_sources(&messages, 3),
            vec!["/a.png".to_owned()]
        );
        let omitted = omit_provider_rejected_images(messages);
        assert_eq!(omitted[0]["content"][1], image());
        assert_eq!(
            omitted[2]["content"][0]["text"],
            rejected_image_placeholder("/a.png")
        );
    }

    #[test]
    fn user_images_and_unknown_tool_results_name_a_generic_source() {
        let messages = vec![
            json!({"role":"user","content":[image()]}),
            json!({"role":"toolResult","toolCallId":"x","content":[image()]}),
            failed("invalid_image"),
        ];
        assert_eq!(
            rejected_image_sources(&messages, 2),
            vec!["an attached image".to_owned(), "a tool result".to_owned()]
        );
    }

    #[test]
    fn other_failures_and_success_leave_history_alone() {
        let messages = vec![
            json!({"role":"user","content":[image()]}),
            failed("rate limited"),
        ];
        assert_eq!(omit_provider_rejected_images(messages.clone()), messages);
        assert!(!is_provider_image_rejection(None));
        assert!(is_provider_image_rejection(Some("Could not process image")));
    }
}
