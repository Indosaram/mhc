//! Streaming wire events omit cumulative snapshots while preserving constant-size metadata.
use maho_ai::types::ContentBlock;
use maho_ai::utils::assistant_capture::{AssistantCapture, utf16_len};
use maho_ai::utils::assistant_seed_frontier::seed_next_offset;
use serde_json::{Value, json};
use std::borrow::Cow;

#[derive(Debug, thiserror::Error)]
pub enum JsonEventError {
    #[error("message_update message is not an assistant message")]
    NotAssistant,
    #[error("toolcall_start content at index {0} is not a tool call")]
    NotToolCall(Value),
    #[error(transparent)]
    SeedFrontier(#[from] maho_ai::utils::assistant_seed_frontier::SeedFrontierError),
}

pub fn to_json_event(event: &Value) -> Result<Cow<'_, Value>, JsonEventError> {
    if event.get("type").and_then(Value::as_str) != Some("message_update") {
        return Ok(Cow::Borrowed(event));
    }
    let message = &event["message"];
    if message["role"].as_str() != Some("assistant") {
        return Err(JsonEventError::NotAssistant);
    }
    let source = &event["assistantMessageEvent"];
    // Copy every entry except the cumulative `partial` snapshot (kept as a cheap null so the
    // following `remove` reorders keys exactly as before).
    let mut delta = match source.as_object() {
        Some(object) => Value::Object(
            object
                .iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        if key == "partial" {
                            Value::Null
                        } else {
                            value.clone()
                        },
                    )
                })
                .collect(),
        ),
        None => source.clone(),
    };
    if delta["type"].as_str() == Some("toolcall_start") {
        let index = delta["contentIndex"]
            .as_u64()
            .and_then(|n| usize::try_from(n).ok());
        let tool = index.and_then(|i| source["partial"]["content"].get(i));
        let Some(tool) = tool.filter(|tool| tool["type"].as_str() == Some("toolCall")) else {
            return Err(JsonEventError::NotToolCall(delta["contentIndex"].clone()));
        };
        let id = tool.get("id").cloned();
        let name = tool.get("name").cloned();
        if let Some(object) = delta.as_object_mut() {
            object.remove("partial");
            if let Some(id) = id {
                object.insert("id".into(), id);
            }
            if let Some(name) = name {
                object.insert("toolName".into(), name);
            }
        }
    } else if let Some(object) = delta.as_object_mut() {
        object.remove("partial");
    }
    let mut result =
        json!({"type":"message_update","usage":message["usage"],"assistantMessageEvent":delta});
    if let Some(name) = event.get("resolvedToolName") {
        result["resolvedToolName"] = name.clone();
    }
    Ok(Cow::Owned(result))
}

/// The engine-staged block-seed projection (senpi#4272, R3.C6.2; `toJsonEvent(event, { blockSeeds: 1 })`); no
/// public destination selects it yet. `capture` is the producer-time capture the event carries
/// (`getAssistantCapture(event)`, which Rust hands over beside the value). `Ok(None)` is TS `undefined`: an
/// assistant record with no capture has no seeded form, because any seed would be read from the live partial
/// and could already hold later bytes. Identity is never invented.
pub fn to_seeded_json_event(
    event: &Value,
    capture: Option<&AssistantCapture>,
) -> Result<Option<Value>, JsonEventError> {
    let kind = event.get("type").and_then(Value::as_str);
    if matches!(kind, Some("message_start" | "message_end"))
        && event["message"]["role"].as_str() == Some("assistant")
    {
        if let Some(message) = capture.and_then(|capture| capture.message.as_deref()) {
            let mut seeded = event.clone();
            seeded["message"] = to_value(message);
            return Ok(Some(seeded));
        }
        // The terminal message is complete when emitted; only an uncaptured start may already hold later bytes.
        return Ok((kind == Some("message_end")).then(|| event.clone()));
    }
    if kind != Some("message_update") {
        return Ok(Some(event.clone()));
    }
    let Some(capture) = capture else {
        return Ok(None);
    };
    let Some(assistant_event) =
        to_seeded_assistant_event(&event["assistantMessageEvent"], capture)?
    else {
        return Ok(None);
    };
    let mut outer = event.as_object().cloned().unwrap_or_default();
    outer.remove("message");
    outer.remove("assistantMessageEvent");
    outer.insert("usage".into(), to_value(&capture.usage));
    if let Some(metadata) = capture
        .metadata
        .as_deref()
        .filter(|metadata| !metadata.is_empty())
    {
        outer.insert("messageMetadata".into(), Value::Object(metadata.clone()));
    }
    outer.insert("assistantMessageEvent".into(), assistant_event);
    Ok(Some(Value::Object(outer)))
}

fn to_value<T: serde::Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

/// Builds a seeded assistant event from the producer-time capture only: an ordinary delta reads its index, its
/// delta string and the capture, never the shared partial, the block or the parsed arguments.
fn to_seeded_assistant_event(
    event: &Value,
    capture: &AssistantCapture,
) -> Result<Option<Value>, JsonEventError> {
    let kind = event["type"].as_str().unwrap_or_default();
    match kind {
        "start" => {
            return Ok(capture
                .message
                .as_deref()
                .map(|message| json!({"type": "start", "seed": {"message": to_value(message)}})));
        }
        "done" => {
            return Ok(Some(
                json!({"type": "done", "reason": event["reason"], "message": event["message"]}),
            ));
        }
        "error" => {
            return Ok(Some(
                json!({"type": "error", "reason": event["reason"], "error": event["error"]}),
            ));
        }
        _ => {}
    }
    let content_index = capture
        .content_index
        .map_or_else(|| event["contentIndex"].clone(), Value::from);
    if matches!(kind, "text_delta" | "thinking_delta" | "toolcall_delta") {
        let Some(offset) = capture.offset else {
            return Ok(None);
        };
        let delta = &event["delta"];
        if kind != "toolcall_delta" {
            return Ok(Some(
                json!({"type": kind, "contentIndex": content_index, "delta": delta, "offset": offset}),
            ));
        }
        return Ok(capture.parsed_through.map(|parsed_through| {
            json!({"type": kind, "contentIndex": content_index, "delta": delta, "offset": offset, "parsedThrough": parsed_through})
        }));
    }
    let block = capture.block.as_deref();
    match (kind, block) {
        ("toolcall_end", Some(block @ ContentBlock::ToolCall(_))) => {
            return Ok(Some(
                json!({"type": kind, "contentIndex": content_index, "toolCall": to_value(block)}),
            ));
        }
        ("text_end", Some(block @ ContentBlock::Text(_)))
        | ("thinking_end", Some(block @ ContentBlock::Thinking(_))) => {
            return Ok(Some(
                json!({"type": kind, "contentIndex": content_index, "content": event["content"], "block": to_value(block)}),
            ));
        }
        ("toolcall_end" | "text_end" | "thinking_end", _) => return Ok(None),
        _ => {}
    }
    let Some(covered_code_units) = capture.covered_code_units else {
        return Ok(None);
    };
    let seed_length = match block {
        Some(ContentBlock::Text(text)) => utf16_len(&text.text),
        Some(ContentBlock::Thinking(thinking)) => utf16_len(&thinking.thinking),
        _ => capture.raw_argument_prefix.as_deref().map_or(0, utf16_len),
    };
    let next_offset = seed_next_offset(
        capture
            .next_offset
            .and_then(|offset| i64::try_from(offset).ok()),
        seed_length,
    )?;
    match (kind, block) {
        ("text_start", Some(block @ ContentBlock::Text(_)))
        | ("thinking_start", Some(block @ ContentBlock::Thinking(_))) => Ok(Some(json!({
            "type": kind,
            "contentIndex": content_index,
            "seed": {"block": to_value(block), "coveredCodeUnits": covered_code_units, "nextOffset": next_offset},
        }))),
        ("toolcall_start", Some(block @ ContentBlock::ToolCall(call))) => {
            let (Some(raw_argument_prefix), Some(parsed_through)) = (
                capture.raw_argument_prefix.as_deref(),
                capture.parsed_through,
            ) else {
                return Ok(None);
            };
            Ok(Some(json!({
                "type": kind,
                "contentIndex": content_index,
                "id": call.id,
                "toolName": call.name,
                "seed": {
                    "block": to_value(block),
                    "rawArgumentPrefix": raw_argument_prefix,
                    "parsedThrough": parsed_through,
                    "coveredCodeUnits": covered_code_units,
                    "nextOffset": next_offset,
                },
            })))
        }
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn removes_snapshots_and_preserves_usage() {
        let event = json!({"type":"message_update","message":{"role":"assistant","usage":{"input":2}},"assistantMessageEvent":{"type":"text_delta","delta":"hello","partial":{"content":[]}},"resolvedToolName":"bash"});
        assert_eq!(
            to_json_event(&event).unwrap().as_ref(),
            &json!({"type":"message_update","usage":{"input":2},"assistantMessageEvent":{"type":"text_delta","delta":"hello"},"resolvedToolName":"bash"})
        );
    }
    #[test]
    fn tool_start_retains_id_and_name() {
        let event = json!({"type":"message_update","message":{"role":"assistant","usage":{}},"assistantMessageEvent":{"type":"toolcall_start","contentIndex":0,"partial":{"content":[{"type":"toolCall","id":"call1","name":"bash"}]}}});
        assert_eq!(
            to_json_event(&event).unwrap()["assistantMessageEvent"],
            json!({"type":"toolcall_start","contentIndex":0,"id":"call1","toolName":"bash"})
        );
    }
    #[test]
    fn refuses_invalid_tool_start() {
        let event = json!({"type":"message_update","message":{"role":"assistant","usage":{}},"assistantMessageEvent":{"type":"toolcall_start","contentIndex":0,"partial":{"content":[{"type":"text","text":"oops"}]}}});
        assert!(matches!(
            to_json_event(&event),
            Err(JsonEventError::NotToolCall(_))
        ));
    }
    #[test]
    fn unrelated_event_is_borrowed() {
        assert!(matches!(
            to_json_event(&json!({"type":"agent_end"})).unwrap(),
            Cow::Borrowed(_)
        ));
    }

    // Port of senpi test/suite/regressions/4272-seeded-json-event.test.ts (v2026.10.27-2). The producer side
    // (`AssistantMessageEventStream { captureBlocks }`) is maho-ai's; these captures are what it attaches.
    mod seeded {
        use super::super::*;
        use std::sync::Arc;

        fn usage() -> Value {
            json!({"input":1,"output":2,"cacheRead":0,"cacheWrite":0,"totalTokens":3,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}})
        }
        fn assistant() -> Value {
            json!({"role":"assistant","api":"faux","provider":"faux","model":"fixture","content":[],"timestamp":1,"stopReason":"stop","usage":usage()})
        }
        /// The wire form of the typed message, so expectations do not depend on serde defaults.
        fn message_wire() -> Value {
            to_value(
                &serde_json::from_value::<maho_ai::types::AssistantMessage>(assistant())
                    .expect("message"),
            )
        }
        fn usage_wire() -> Value {
            to_value(&capture().usage)
        }
        fn block_wire(value: Value) -> Value {
            to_value(&*block(value))
        }
        fn block(value: Value) -> Arc<ContentBlock> {
            Arc::new(serde_json::from_value(value).expect("block"))
        }
        fn capture() -> AssistantCapture {
            AssistantCapture {
                usage: serde_json::from_value(usage()).expect("usage"),
                ..AssistantCapture::default()
            }
        }
        fn update(event: Value) -> Value {
            json!({"type":"message_update","message":assistant(),"assistantMessageEvent":event,"resolvedToolName":"read"})
        }

        #[test]
        fn projects_every_block_kind_from_the_capture_not_the_partial() {
            let start_capture = AssistantCapture {
                message: Some(Arc::new(
                    serde_json::from_value(assistant()).expect("message"),
                )),
                ..capture()
            };
            let start = to_seeded_json_event(&update(json!({"type":"start","partial":{"content":[{"type":"text","text":"mutated"}]}})), Some(&start_capture)).unwrap().unwrap();
            assert_eq!(
                start["assistantMessageEvent"],
                json!({"type":"start","seed":{"message":message_wire()}})
            );

            let text_start = AssistantCapture {
                block: Some(block(json!({"type":"text","text":"Hé"}))),
                covered_code_units: Some(2),
                next_offset: Some(2),
                ..capture()
            };
            assert_eq!(
                to_seeded_json_event(
                    &update(json!({"type":"text_start","contentIndex":0,"partial":{}})),
                    Some(&text_start)
                )
                .unwrap()
                .unwrap(),
                json!({"type":"message_update","resolvedToolName":"read","usage":usage_wire(),"assistantMessageEvent":{"type":"text_start","contentIndex":0,"seed":{"block":block_wire(json!({"type":"text","text":"Hé"})),"coveredCodeUnits":2,"nextOffset":2}}})
            );
            let delta = AssistantCapture {
                offset: Some(5),
                ..capture()
            };
            assert_eq!(
                to_seeded_json_event(
                    &update(json!({"type":"text_delta","contentIndex":0,"delta":"!","partial":{}})),
                    Some(&delta)
                )
                .unwrap()
                .unwrap()["assistantMessageEvent"],
                json!({"type":"text_delta","contentIndex":0,"delta":"!","offset":5})
            );
            let mut metadata = serde_json::Map::new();
            metadata.insert("responseId".into(), json!("resp_1"));
            let text_end = AssistantCapture {
                block: Some(block(json!({"type":"text","text":"Hé 😀!"}))),
                metadata: Some(Arc::new(metadata)),
                ..capture()
            };
            let end = to_seeded_json_event(
                &update(
                    json!({"type":"text_end","contentIndex":0,"content":"Hé 😀!","partial":{}}),
                ),
                Some(&text_end),
            )
            .unwrap()
            .unwrap();
            assert_eq!(end["messageMetadata"], json!({"responseId":"resp_1"}));
            assert_eq!(
                end["assistantMessageEvent"],
                json!({"type":"text_end","contentIndex":0,"content":"Hé 😀!","block":block_wire(json!({"type":"text","text":"Hé 😀!"}))})
            );

            let raw = "{ \"path\" :\t\"a\\u0041";
            let raw_len = raw.encode_utf16().count();
            let call = json!({"type":"toolCall","id":"call_1","name":"mcp__fs__Read","arguments":{"path":"a"}});
            let tool_start = AssistantCapture {
                block: Some(block(call.clone())),
                raw_argument_prefix: Some(raw.into()),
                parsed_through: Some(0),
                covered_code_units: Some(raw_len),
                next_offset: Some(raw_len),
                ..capture()
            };
            assert_eq!(
                to_seeded_json_event(
                    &update(json!({"type":"toolcall_start","contentIndex":2,"partial":{}})),
                    Some(&tool_start)
                )
                .unwrap()
                .unwrap()["assistantMessageEvent"],
                json!({"type":"toolcall_start","contentIndex":2,"id":"call_1","toolName":"mcp__fs__Read","seed":{"block":block_wire(call.clone()),"rawArgumentPrefix":raw,"parsedThrough":0,"coveredCodeUnits":raw_len,"nextOffset":raw_len}})
            );
            let tool_delta = AssistantCapture {
                offset: Some(raw_len),
                parsed_through: Some(raw_len + 3),
                ..capture()
            };
            assert_eq!(
                to_seeded_json_event(&update(json!({"type":"toolcall_delta","contentIndex":2,"delta":"\" }","partial":{}})), Some(&tool_delta)).unwrap().unwrap()["assistantMessageEvent"],
                json!({"type":"toolcall_delta","contentIndex":2,"delta":"\" }","offset":raw_len,"parsedThrough":raw_len + 3})
            );
            // The block's own index wins over the event's public one.
            let remapped = AssistantCapture {
                content_index: Some(4),
                offset: Some(0),
                ..capture()
            };
            assert_eq!(
                to_seeded_json_event(
                    &update(
                        json!({"type":"thinking_delta","contentIndex":1,"delta":"x","partial":{}})
                    ),
                    Some(&remapped)
                )
                .unwrap()
                .unwrap()["assistantMessageEvent"]["contentIndex"],
                4
            );
            // A frontier other than full replay or full continuation is refused.
            let bad = AssistantCapture {
                next_offset: Some(1),
                ..text_start
            };
            assert!(matches!(
                to_seeded_json_event(
                    &update(json!({"type":"text_start","contentIndex":0,"partial":{}})),
                    Some(&bad)
                ),
                Err(JsonEventError::SeedFrontier(_))
            ));
        }

        #[test]
        fn projects_a_direct_error_and_leaves_non_assistant_records_untouched() {
            let failed = json!({"role":"assistant","stopReason":"error","errorMessage":"boom"});
            let seeded = to_seeded_json_event(
                &update(json!({"type":"error","reason":"error","error":failed})),
                Some(&capture()),
            )
            .unwrap()
            .unwrap();
            assert_eq!(
                seeded["assistantMessageEvent"],
                json!({"type":"error","reason":"error","error":failed})
            );
            let user = json!({"type":"message_start","message":{"role":"user","content":[{"type":"text","text":"hi"}],"timestamp":1}});
            assert_eq!(to_seeded_json_event(&user, None).unwrap(), Some(user));
            let turn = json!({"type":"turn_start"});
            assert_eq!(to_seeded_json_event(&turn, None).unwrap(), Some(turn));
        }

        #[test]
        fn an_uncaptured_assistant_update_or_start_has_no_seeded_form() {
            let partial = json!({"role":"assistant","content":[{"type":"text","text":"late"}]});
            let uncaptured = json!({"type":"message_update","assistantMessageEvent":{"type":"text_start","contentIndex":0,"partial":partial},"message":partial});
            assert_eq!(to_seeded_json_event(&uncaptured, None).unwrap(), None);
            assert_eq!(
                to_seeded_json_event(&json!({"type":"message_start","message":partial}), None)
                    .unwrap(),
                None
            );
            let end = json!({"type":"message_end","message":partial});
            assert_eq!(to_seeded_json_event(&end, None).unwrap(), Some(end));
            let captured_start = AssistantCapture {
                message: Some(Arc::new(
                    serde_json::from_value(assistant()).expect("message"),
                )),
                ..capture()
            };
            assert_eq!(
                to_seeded_json_event(
                    &json!({"type":"message_start","message":partial,"clientMessageId":"m1"}),
                    Some(&captured_start)
                )
                .unwrap()
                .unwrap(),
                json!({"type":"message_start","message":message_wire(),"clientMessageId":"m1"})
            );
        }
    }
}
