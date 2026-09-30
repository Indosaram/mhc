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

struct ThinkingRecoveryState {
    channel: OutputChannel,
    thinking: String,
    response: String,
    changed: bool,
    recovered_response: bool,
}

impl ThinkingRecoveryState {
    fn append(&mut self, text: &str) {
        match self.channel {
            OutputChannel::Response => self.response.push_str(text),
            OutputChannel::Thinking => self.thinking.push_str(text),
        }
    }
}

fn scan_thinking_segment(state: &mut ThinkingRecoveryState, text: &str) {
    let mut offset = 0usize;
    let mut last = 0usize;
    while offset < text.len() {
        if let Some(marker) = match_xtml_channel_marker(&text[offset..])
            && !marker.is_empty()
        {
            state.append(&text[last..offset]);
            let (action, name) = named_marker_action_and_name(&marker);
            state.changed = true;
            match (action, name) {
                (Some("open"), Some("response")) => {
                    state.channel = OutputChannel::Response;
                    state.recovered_response = true;
                }
                (Some("open"), Some("think")) => state.channel = OutputChannel::Thinking,
                (Some("close"), Some("response")) => state.channel = OutputChannel::Thinking,
                _ => {}
            }
            offset += marker.len();
            last = offset;
            continue;
        }
        offset += text[offset..].chars().next().map(char::len_utf8).unwrap_or(1);
    }
    state.append(&text[last..]);
}

fn recover_thinking_content(input: &str) -> RecoveredThinking {
    let mut mask = create_recovery_code_mask();
    let mut state = ThinkingRecoveryState {
        channel: OutputChannel::Thinking,
        thinking: String::new(),
        response: String::new(),
        changed: false,
        recovered_response: false,
    };

    let segments: Vec<_> = mask.feed(input, None).into_iter().chain(mask.finish()).collect();
    for segment in segments {
        if segment.scan {
            scan_thinking_segment(&mut state, &segment.text);
        } else {
            state.append(&segment.text);
        }
    }

    RecoveredThinking {
        thinking: state.thinking,
        response: state.response,
        changed: state.changed,
        recovered_response: state.recovered_response,
    }
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
            timestamp: crate::utils::diagnostics::now_ms(),
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
    use serde_json::Value;

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
    fn thinking_text(message: &AssistantMessage) -> String {
        message.content.iter().filter_map(|block| if let ContentBlock::Thinking(t) = block { Some(t.thinking.as_str()) } else { None }).collect()
    }

    fn visible_text(message: &AssistantMessage) -> String {
        message.content.iter().filter_map(|block| if let ContentBlock::Text(t) = block { Some(t.text.as_str()) } else { None }).collect()
    }

    fn rendered_content(message: &AssistantMessage) -> String {
        message.content.iter().map(|block| serde_json::to_string(block).expect("serializable")).collect()
    }

    #[test]
    fn strips_tools_channel_and_unnamed_markers_from_thinking() {
        let msg = message(vec![ContentBlock::Thinking(ThinkingContent { thinking: "private<|close|>tools<|sep|> reasoning<|close|><|sep|> remains<|sep|>".into(), ..ThinkingContent::default() })]);
        let recovered = recover_kimi_xtml_thinking(&msg);
        assert_eq!(thinking_text(&recovered), "private reasoning remains");
        assert_eq!(visible_text(&recovered), "");
        assert!(!rendered_content(&recovered).contains("<|"));
        let diagnostic = recovered.diagnostics.as_ref().and_then(|d| d.last()).expect("diagnostic");
        assert_eq!(diagnostic.kind, "kimi_xtml_thinking_recovery");
        assert_eq!(diagnostic.details.as_ref().and_then(|d| d.get("recoveredResponse")), Some(&Value::Bool(false)));
    }

    #[test]
    fn still_promotes_response_content_while_stripping_adjacent_tools_markers() {
        let msg = message(vec![ContentBlock::Thinking(ThinkingContent {
            thinking: "private<|close|>think<|sep|><|close|>tools<|sep|><|open|>response<|sep|>visible<|close|>response<|sep|>".into(),
            ..ThinkingContent::default()
        })]);
        let recovered = recover_kimi_xtml_thinking(&msg);
        assert_eq!(thinking_text(&recovered), "private");
        assert_eq!(visible_text(&recovered), "visible");
        assert!(!rendered_content(&recovered).contains("<|"));
    }

    #[test]
    fn preserves_tools_and_unnamed_marker_literals_inside_fenced_code() {
        let literal = "Example:\n\u{60}\u{60}\u{60}text\n<|close|>tools<|sep|><|close|><|sep|>\n\u{60}\u{60}\u{60}";
        let msg = message(vec![ContentBlock::Thinking(ThinkingContent { thinking: literal.into(), ..ThinkingContent::default() })]);
        let recovered = recover_kimi_xtml_thinking(&msg);
        assert_eq!(recovered.content, msg.content);
        assert_eq!(thinking_text(&recovered), literal);
        assert!(recovered.diagnostics.is_none());
    }

}
