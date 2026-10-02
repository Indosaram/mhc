use std::collections::BTreeMap;
use maho_ai::{model::Model, types::{AssistantMessage, AssistantMessageEvent as Event, ContentBlock, TextContent, ThinkingContent, ToolCall}};
use serde_json::Value;
pub struct StreamEventContext {
    pub model:Model, pub output:AssistantMessage, pub custom_tool_name_to_pi:BTreeMap<String,String>,
    indices:Vec<Option<u64>>, partial_json:BTreeMap<usize,String>,
}
impl StreamEventContext {
    pub fn new(model:Model,now:i64,custom:BTreeMap<String,String>)->Self {
        Self {output:crate::stream_protocol::empty_output(&model,now),model,custom_tool_name_to_pi:custom,indices:Vec::new(),partial_json:BTreeMap::new()}
    }
    pub fn apply_stream_event(&mut self,event:&Value)->Option<Event> {
        match event["type"].as_str() {
            Some("message_start")=>{crate::stream_protocol::update_usage(&self.model,&mut self.output,&event["message"]["usage"]);None},
            Some("message_delta")=>{self.output.stop_reason=crate::stream_protocol::map_stop_reason(event["delta"]["stop_reason"].as_str());crate::stream_protocol::update_usage(&self.model,&mut self.output,&event["usage"]);None},
            Some("content_block_start")=>{
                let block=&event["content_block"];let index=self.output.content.len();
                let content=match block["type"].as_str() {
                    Some("text")=>ContentBlock::Text(TextContent::default()),
                    Some("thinking")=>ContentBlock::Thinking(ThinkingContent {thinking_signature:Some(String::new()),..Default::default()}),
                    Some("tool_use")=>{self.partial_json.insert(index,String::new());ContentBlock::ToolCall(ToolCall {id:block["id"].as_str()?.into(),name:crate::tools::map_sdk_tool_name(block["name"].as_str()?,&self.custom_tool_name_to_pi),arguments:block["input"].as_object().cloned().unwrap_or_default(),..Default::default()})},
                    _=>return None,
                };
                let kind=content.type_name();self.output.content.push(content.clone());self.indices.push(event["index"].as_u64());
                match kind {"text"=>Some(Event::TextStart {content_index:index,partial:self.output.clone()}),"thinking"=>Some(Event::ThinkingStart {content_index:index,partial:self.output.clone()}),_=>Some(Event::ToolcallStart {content_index:index,partial:self.output.clone()})}
            },
            Some("content_block_delta")=>{
                let index=self.indices.iter().position(|index|index.is_some()&&*index==event["index"].as_u64())?;let delta=&event["delta"];
                match (delta["type"].as_str(),&mut self.output.content[index]) {
                    (Some("text_delta"),ContentBlock::Text(block))=>{let text=delta["text"].as_str()?;block.text.push_str(text);Some(Event::TextDelta {content_index:index,delta:text.into(),partial:self.output.clone()})},
                    (Some("thinking_delta"),ContentBlock::Thinking(block))=>{let text=delta["thinking"].as_str()?;block.thinking.push_str(text);Some(Event::ThinkingDelta {content_index:index,delta:text.into(),partial:self.output.clone()})},
                    (Some("signature_delta"),ContentBlock::Thinking(block))=>{block.thinking_signature.get_or_insert_default().push_str(delta["signature"].as_str()?);None},
                    (Some("input_json_delta"),ContentBlock::ToolCall(block))=>{let text=delta["partial_json"].as_str()?;let partial=self.partial_json.entry(index).or_default();partial.push_str(text);block.arguments=maho_ai::utils::json_parse::parse_streaming_json(Some(partial)).as_object().cloned().unwrap_or_default();Some(Event::ToolcallDelta {content_index:index,delta:text.into(),partial:self.output.clone()})},
                    _=>None,
                }
            },
            Some("content_block_stop")=>{
                let index=self.indices.iter().position(|index|index.is_some()&&*index==event["index"].as_u64())?;
                let event=match &mut self.output.content[index] {
                    ContentBlock::Text(block)=>Some(Event::TextEnd {content_index:index,content:block.text.clone(),partial:self.output.clone()}),
                    ContentBlock::Thinking(block)=>Some(Event::ThinkingEnd {content_index:index,content:block.thinking.clone(),partial:self.output.clone()}),
                    ContentBlock::ToolCall(block)=>{let partial=self.partial_json.remove(&index).unwrap_or_default();block.arguments=crate::tools::map_tool_args(&block.name,maho_ai::utils::json_parse::parse_streaming_json(Some(&partial)).as_object());Some(Event::ToolcallEnd {content_index:index,tool_call:block.clone(),partial:self.output.clone()})},
                    ContentBlock::Image(_) | ContentBlock::ProviderNative(_)=>None,
                };self.indices[index]=None;event
            },
            _=>None,
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;use serde_json::json;
    fn context()->StreamEventContext {StreamEventContext::new(maho_ai::models_generated::MODELS["anthropic"].values().next().expect("model").clone(),1,BTreeMap::new())}
    #[test]
    fn text_and_thinking_blocks_follow_sdk_indices_and_signatures() {
        let mut context=context();context.apply_stream_event(&json!({"type":"content_block_start","index":4,"content_block":{"type":"thinking"}})).expect("start");
        context.apply_stream_event(&json!({"type":"content_block_delta","index":4,"delta":{"type":"thinking_delta","thinking":"why"}})).expect("delta");
        assert!(context.apply_stream_event(&json!({"type":"content_block_delta","index":4,"delta":{"type":"signature_delta","signature":"sig"}})).is_none());
        context.apply_stream_event(&json!({"type":"content_block_stop","index":4})).expect("stop");
        context.apply_stream_event(&json!({"type":"content_block_start","index":7,"content_block":{"type":"text","text":"ignored"}})).expect("start");
        context.apply_stream_event(&json!({"type":"content_block_delta","index":7,"delta":{"type":"text_delta","text":"hello"}})).expect("delta");
        let event=context.apply_stream_event(&json!({"type":"content_block_stop","index":7})).expect("stop");assert!(matches!(event,Event::TextEnd {content_index:1,content,..} if content=="hello"));
        let ContentBlock::Thinking(block)=&context.output.content[0] else {panic!("thinking");};assert_eq!(block.thinking_signature.as_deref(),Some("sig"));
        assert!(context.apply_stream_event(&json!({"type":"content_block_delta","index":4,"delta":{"type":"thinking_delta","thinking":"late"}})).is_none());
    }
    #[test]
    fn tool_json_is_incremental_and_arguments_are_mapped_on_close() {
        let mut context=context();context.apply_stream_event(&json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"call","name":"Read","input":{}}})).expect("start");
        for partial in ["{\"file_path\":\"","src\"}"] {context.apply_stream_event(&json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":partial}})).expect("delta");}
        let event=context.apply_stream_event(&json!({"type":"content_block_stop","index":0})).expect("stop");let Event::ToolcallEnd {tool_call,..}=event else {panic!("tool");};assert_eq!(tool_call.name,"read");assert_eq!(tool_call.arguments["path"],"src");
    }
}
