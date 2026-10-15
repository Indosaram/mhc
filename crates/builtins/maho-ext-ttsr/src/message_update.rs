use maho_ai::types::{AssistantMessageEvent,ContentBlock};
use serde_json::Value;
use crate::types::TtsrStreamSource;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TtsrStreamDelta<'a> { pub source:TtsrStreamSource,pub stream_key:String,pub delta:&'a str,pub tool_name:Option<&'a str> }
/// Reads the stream delta straight from the serialized event without deserializing the accumulated `partial`.
pub fn get_ttsr_stream_delta_from_value(event:&Value)->Option<TtsrStreamDelta<'_>> {
    let (source,prefix)=match event.get("type").and_then(Value::as_str)? { "text_delta"=>(TtsrStreamSource::Text,"text"),"thinking_delta"=>(TtsrStreamSource::Thinking,"thinking"),"toolcall_delta"=>(TtsrStreamSource::Tool,"tool"),_=>return None };
    let content_index=event.get("contentIndex").and_then(Value::as_u64)?;
    let delta=event.get("delta").and_then(Value::as_str)?;
    let tool_name=if source==TtsrStreamSource::Tool { usize::try_from(content_index).ok().and_then(|index|event.get("partial")?.get("content")?.get(index)).filter(|block|block.get("type").and_then(Value::as_str)==Some("toolCall")).and_then(|block|block.get("name")?.as_str()) } else { None };
    Some(TtsrStreamDelta { source,stream_key:format!("{prefix}:{content_index}"),delta,tool_name })
}
pub fn get_ttsr_stream_delta(event:&AssistantMessageEvent)->Option<TtsrStreamDelta<'_>> {
    match event {
        AssistantMessageEvent::TextDelta { content_index,delta,.. }=>Some(TtsrStreamDelta { source:TtsrStreamSource::Text,stream_key:format!("text:{content_index}"),delta,tool_name:None }),
        AssistantMessageEvent::ThinkingDelta { content_index,delta,.. }=>Some(TtsrStreamDelta { source:TtsrStreamSource::Thinking,stream_key:format!("thinking:{content_index}"),delta,tool_name:None }),
        AssistantMessageEvent::ToolcallDelta { content_index,delta,partial }=>Some(TtsrStreamDelta { source:TtsrStreamSource::Tool,stream_key:format!("tool:{content_index}"),delta,tool_name:partial.content.get(*content_index).and_then(|block| match block { ContentBlock::ToolCall(call)=>Some(call.name.as_str()), _=>None }) }),
        AssistantMessageEvent::Start { .. } | AssistantMessageEvent::TextStart { .. } | AssistantMessageEvent::TextEnd { .. } | AssistantMessageEvent::ThinkingStart { .. } | AssistantMessageEvent::ThinkingEnd { .. } | AssistantMessageEvent::ToolcallStart { .. } | AssistantMessageEvent::ToolcallEnd { .. } | AssistantMessageEvent::Done { .. } | AssistantMessageEvent::Error { .. }=>None,
    }
}
#[cfg(test)] mod tests {
    use super::*;
    fn partial(content:serde_json::Value)->maho_ai::types::AssistantMessage { serde_json::from_value(serde_json::json!({"content":content,"api":"faux","provider":"faux","model":"faux","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"stopReason":"stop","timestamp":0})).unwrap() }
    #[test] fn text_delta_preserves_index_and_value() { let event=AssistantMessageEvent::TextDelta { content_index:2,delta:"text".into(),partial:partial(serde_json::json!([])) }; let result=get_ttsr_stream_delta(&event).unwrap(); assert_eq!(result.stream_key,"text:2"); assert_eq!(result.delta,"text"); assert_eq!(result.source,TtsrStreamSource::Text); }
    #[test] fn thinking_delta_has_separate_key() { let event=AssistantMessageEvent::ThinkingDelta { content_index:2,delta:"thought".into(),partial:partial(serde_json::json!([])) }; let result=get_ttsr_stream_delta(&event).unwrap(); assert_eq!(result.stream_key,"thinking:2"); assert_eq!(result.source,TtsrStreamSource::Thinking); }
    #[test] fn tool_delta_projects_tool_name_from_partial() { let event=AssistantMessageEvent::ToolcallDelta { content_index:0,delta:"{}".into(),partial:partial(serde_json::json!([{"type":"toolCall","id":"call","name":"edit","arguments":{}}])) }; let result=get_ttsr_stream_delta(&event).unwrap(); assert_eq!(result.tool_name,Some("edit")); assert_eq!(result.stream_key,"tool:0"); }
    #[test] fn missing_tool_block_keeps_delta_without_name() { let event=AssistantMessageEvent::ToolcallDelta { content_index:4,delta:"{}".into(),partial:partial(serde_json::json!([])) }; let result=get_ttsr_stream_delta(&event).unwrap(); assert!(result.tool_name.is_none()); }
    #[test] fn start_event_does_not_produce_delta() { let event=AssistantMessageEvent::Start { partial:partial(serde_json::json!([])) }; let result=get_ttsr_stream_delta(&event); assert!(result.is_none()); }
    #[test] fn value_projection_matches_typed_projection() {
        let events=[AssistantMessageEvent::TextDelta { content_index:2,delta:"text".into(),partial:partial(serde_json::json!([])) },AssistantMessageEvent::ThinkingDelta { content_index:1,delta:"thought".into(),partial:partial(serde_json::json!([])) },AssistantMessageEvent::ToolcallDelta { content_index:0,delta:"{}".into(),partial:partial(serde_json::json!([{"type":"toolCall","id":"call","name":"edit","arguments":{}}])) },AssistantMessageEvent::ToolcallDelta { content_index:4,delta:"{}".into(),partial:partial(serde_json::json!([])) },AssistantMessageEvent::Start { partial:partial(serde_json::json!([])) }];
        for event in events { let value=serde_json::to_value(&event).unwrap(); assert_eq!(get_ttsr_stream_delta_from_value(&value),get_ttsr_stream_delta(&event)); }
    }
}
