//! Port of senpi packages/ai/src/tool-call-middleware/protocols/kimi-xtml/thinking-recovery.ts.

use crate::tool_call_middleware::recovery_code_mask::create_recovery_code_mask;
use crate::types::{AssistantMessage, AssistantMessageDiagnostic, ContentBlock, ThinkingContent};

use super::markers::match_xtml_channel_marker;

enum OutputChannel {
    Thinking,
    Response,
}

struct RecoveredThinking {
    thinking: String,
    response: String,
    changed: bool,
    recovered_response: bool,
}

/// `/^<\|(open|close)\|>([a-zA-Z_][a-zA-Z0-9_]*)?/`.
fn named_marker_action_and_name(marker: &str) -> (Option<&str>, Option<&str>) {
    let Some(rest) = marker.strip_prefix("<|") else { return (None, None) };
    let (action, rest) = if let Some(rest) = rest.strip_prefix("open|>") {
        ("open", rest)
    } else if let Some(rest) = rest.strip_prefix("close|>") {
        ("close", rest)
    } else {
        return (None, None);
    };
    let name_end = rest.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).unwrap_or(rest.len());
    let name = if name_end == 0 { None } else { Some(&rest[..name_end]) };
    (Some(action), name)
}

fn recover_thinking_content(input: &str) -> RecoveredThinking {
    let mut mask = create_recovery_code_mask();
    let mut channel = OutputChannel::Thinking;
    let mut thinking = String::new();
    let mut response = String::new();
    let mut changed = false;
    let mut recovered_response = false;

    let mut append = |channel: &OutputChannel, text: &str, thinking: &mut String, response: &mut String| match channel {
        OutputChannel::Response => response.push_str(text),
        OutputChannel::Thinking => thinking.push_str(text),
    };

    let mut scan = |text: &str| {
        let mut offset = 0usize;
        loop {
            let Some(marker) = match_xtml_channel_marker(&text[offset..]) else { break };
            if marker.is_empty() {
                break;
            }
            append(&channel, &text[offset..offset], &mut thinking, &mut response);
            let (action, name) = named_marker_action_and_name(&marker);
            changed = true;
            match (action, name) {
                (Some("open"), Some("response")) => {
                    channel = OutputChannel::Response;
                    recovered_response = true;
                }
                (Some("open"), Some("think")) => channel = OutputChannel::Thinking,
                (Some("close"), Some("response")) => channel = OutputChannel::Thinking,
                _ => {}
            }
            offset += marker.len();
        }
        append(&channel, &text[offset..], &mut thinking, &mut response);
    };

    let segments: Vec<_> = mask.feed(input, None).into_iter().chain(mask.finish()).collect();
    for segment in segments {
        if segment.scan {
            scan(&segment.text);
        } else {
            append(&channel, &segment.text, &mut thinking, &mut response);
        }
    }

    RecoveredThinking { thinking, response, changed, recovered_response }
}

fn stripped_visible_text(input: &str) -> (String, bool) {
    let mut mask = create_recovery_code_mask();
    let mut text = String::new();
    let mut changed = false;

    let segments: Vec<_> = mask.feed(input, None).into_iter().chain(mask.finish()).collect();
    for segment in segments {
        if !segment.scan {
            text.push_str(&segment.text);
            continue;
        }
        let mut stripped = String::new();
        let mut rest = segment.text.as_str();
        while !rest.is_empty() {
            if let Some(marker) = match_xtml_channel_marker(rest) {
                if marker.is_empty() {
                    stripped.push_str(rest);
                    break;
                }
                rest = &rest[marker.len()..];
                continue;
            }
            let mut chars = rest.chars();
            let head = chars.next().expect("rest is non-empty");
            stripped.push(head);
            rest = chars.as_str();
        }
        if stripped != segment.text {
            changed = true;
        }
        text.push_str(&stripped);
    }

    (text, changed)
}

pub fn recover_kimi_xtml_thinking(message: &AssistantMessage) -> AssistantMessage {
    let mut changed = false;
    let mut thinking_changed = false;
    let mut recovered_response = false;
    let mut content: Vec<ContentBlock> = Vec::new();

    for block in &message.content {
        match block {
            ContentBlock::Text(text_block) => {
                let (stripped_text, stripped_changed) = stripped_visible_text(&text_block.text);
                changed = changed || stripped_changed;
                if stripped_changed {
                    content.push(ContentBlock::Text(crate::types::TextContent { text: stripped_text, ..text_block.clone() }));
                } else {
                    content.push(block.clone());
                }
            }
            ContentBlock::Thinking(thinking_block) => {
                let recovered = recover_thinking_content(&thinking_block.thinking);
                changed = changed || recovered.changed;
                thinking_changed = thinking_changed || recovered.changed;
                recovered_response = recovered_response || recovered.recovered_response;
                content.push(ContentBlock::Thinking(ThinkingContent { thinking: recovered.thinking, ..thinking_block.clone() }));
                if !recovered.response.is_empty() {
                    content.push(ContentBlock::Text(crate::types::TextContent { text: recovered.response, audience: None, text_signature: None }));
                }
            }
            other => content.push(other.clone()),
        }
    }

    if !changed {
        return message.clone();
    }

    let mut result = message.clone();
    result.content = content;
    if thinking_changed {
        let mut diagnostics = result.diagnostics.unwrap_or_default();
        diagnostics.push(AssistantMessageDiagnostic {
            kind: "kimi_xtml_thinking_recovery".to_string(),
            timestamp: crate::utils::now_millis(),
            error: None,
            details: Some(serde_json::Map::from_iter([("recoveredResponse".to_string(), serde_json::Value::Bool(recovered_response))])),
        });
        result.diagnostics = Some(diagnostics);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{StopReason, TextContent, Usage};

    fn message(content: Vec<ContentBlock>) -> AssistantMessage {
        AssistantMessage {
            content,
            api: "openai-completions".into(),
            provider: "openai".into(),
            model: "m".into(),
            response_model: None,
            response_id: None,
            provider_thinking_level: None,
            diagnostics: None,
            usage: Usage::default(),
            stop_reason: StopReason::Stop,
            stop_details: None,
            deferred: None,
            error_message: None,
            abort_source: None,
            raw_stop_reason: None,
            end_turn: None,
            timestamp: 0,
        }
    }

    #[test]
    fn returns_message_unchanged_when_no_markers_are_present() {
        let msg = message(vec![ContentBlock::Thinking(ThinkingContent { thinking: "plain thoughts".into(), ..ThinkingContent::default() })]);
        let recovered = recover_kimi_xtml_thinking(&msg);
        assert_eq!(recovered.content, msg.content);
        assert!(recovered.diagnostics.is_none());
    }

    #[test]
    fn splits_a_leaked_response_marker_out_of_thinking_into_a_text_block() {
        let msg = message(vec![ContentBlock::Thinking(ThinkingContent { thinking: "reasoning<|open|>response<|sep|>the answer".into(), ..ThinkingContent::default() })]);
        let recovered = recover_kimi_xtml_thinking(&msg);
        assert_eq!(recovered.content.len(), 2);
        match &recovered.content[0] {
            ContentBlock::Thinking(t) => assert_eq!(t.thinking, "reasoning"),
            other => panic!("expected thinking block, got {other:?}"),
        }
        match &recovered.content[1] {
            ContentBlock::Text(t) => assert_eq!(t.text, "the answer"),
            other => panic!("expected text block, got {other:?}"),
        }
        assert!(recovered.diagnostics.is_some());
    }

    #[test]
    fn strips_leaked_markers_from_plain_text_blocks() {
        let msg = message(vec![ContentBlock::Text(TextContent { text: "hello<|sep|>world".into(), audience: None, text_signature: None })]);
        let recovered = recover_kimi_xtml_thinking(&msg);
        match &recovered.content[0] {
            ContentBlock::Text(t) => assert_eq!(t.text, "helloworld"),
            other => panic!("expected text block, got {other:?}"),
        }
    }
}
