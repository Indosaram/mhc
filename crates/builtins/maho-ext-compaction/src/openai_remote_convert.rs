use maho_core::{messages::convert_to_llm,session_manager::session_entry_to_context_messages};
use serde_json::{Value,json};
use crate::openai_remote_schema::get_openai_remote_compaction_details;
fn record_signature(value:Option<&str>)->Option<Value> {let text=value.filter(|s|s.starts_with('{'))?;serde_json::from_str::<Value>(text).ok().filter(Value::is_object)}
pub fn provider_native_item(raw:&Value)->Option<Value> {if raw.is_object() && raw.get("type").is_some_and(Value::is_string) {Some(raw.clone())} else {None}}
fn user_content(content:&Value)->Vec<Value> {
    if let Some(text)=content.as_str() {return vec![json!({"type":"input_text","text":text})];}
    content.as_array().into_iter().flatten().map(|b|if b.get("type").and_then(Value::as_str)==Some("text") {json!({"type":"input_text","text":b["text"]})} else {json!({"type":"input_image","detail":"auto","image_url":format!("data:{};base64,{}",b["mimeType"].as_str().unwrap_or_default(),b["data"].as_str().unwrap_or_default())})}).collect()
}
fn convert_message(message:&Value,index:usize,image_capable:bool)->Vec<Value> {
    match message.get("role").and_then(Value::as_str) {
        Some("user")=>vec![json!({"role":"user","content":user_content(&message["content"])})],
        Some("assistant")=>message["content"].as_array().into_iter().flatten().filter_map(|block|match block.get("type").and_then(Value::as_str) {
            Some("text")=>{
                let signature=block.get("textSignature").and_then(Value::as_str).filter(|s|!s.is_empty());
                let parsed=record_signature(signature).filter(|v|v.get("v").and_then(Value::as_u64)==Some(1) && v.get("id").is_some_and(Value::is_string));
                let id=parsed.as_ref().and_then(|v|v.get("id")).and_then(Value::as_str).or(signature).map(str::to_owned).unwrap_or_else(||format!("msg_{index}"));
                let mut item=json!({"type":"message","role":"assistant","status":"completed","id":id,"content":[{"type":"output_text","text":block["text"],"annotations":[]}]});
                if let Some(phase)=parsed.as_ref().and_then(|v|v.get("phase")).and_then(Value::as_str).filter(|p|matches!(*p,"commentary"|"final_answer")) {item["phase"]=json!(phase);}
                Some(item)
            }
            Some("thinking")=>record_signature(block.get("thinkingSignature").and_then(Value::as_str)).filter(|v|v.get("type").and_then(Value::as_str)==Some("reasoning")),
            Some("toolCall")=>{
                let mut parts=block["id"].as_str().unwrap_or_default().split('|');let call=parts.next().unwrap_or_default();let id=parts.next().filter(|id|id.starts_with("fc"));
                let args=block.get("arguments").filter(|v|!v.is_null()).cloned().unwrap_or_else(||json!({}));
                let mut item=json!({"type":"function_call","call_id":call,"name":block["name"],"arguments":args.to_string()});if let Some(id)=id {item["id"]=json!(id);}Some(item)
            }
            Some("providerNative")=>provider_native_item(&block["raw"]),
            _=>None,
        }).collect(),
        Some("toolResult")=>{
            let blocks=message["content"].as_array().map(Vec::as_slice).unwrap_or_default();
            let text=blocks.iter().filter(|b|b.get("type").and_then(Value::as_str)==Some("text")).filter_map(|b|b.get("text").and_then(Value::as_str)).collect::<Vec<_>>().join("\n");
            let images:Vec<_>=blocks.iter().filter(|b|b.get("type").and_then(Value::as_str)==Some("image")).collect();
            let output=if !images.is_empty() && image_capable {
                let mut content=Vec::new();if !text.is_empty() {content.push(json!({"type":"input_text","text":text}));}
                content.extend(images.iter().map(|b|json!({"type":"input_image","detail":"auto","image_url":format!("data:{};base64,{}",b["mimeType"].as_str().unwrap_or_default(),b["data"].as_str().unwrap_or_default())})));Value::Array(content)
            } else {json!(if !text.is_empty() {text} else if !images.is_empty() {"(see attached image)".into()} else {"(no tool output)".into()})};
            vec![json!({"type":"function_call_output","call_id":message["toolCallId"].as_str().unwrap_or_default().split('|').next().unwrap_or_default(),"output":output})]
        }
        _=>Vec::new(),
    }
}
pub fn convert_pending_messages(messages:&[Value],image_capable:bool)->Vec<Value> {convert_to_llm(messages).iter().enumerate().flat_map(|(i,m)|convert_message(m,i,image_capable)).collect()}
pub fn convert_branch_entries(entries:&[Value],image_capable:bool)->Vec<Value> {
    let mut items=Vec::new();let mut pending=Vec::new();let mut index=0;
    let flush=|pending:&mut Vec<Value>,items:&mut Vec<Value>,index:&mut usize| {for message in convert_to_llm(pending) {items.extend(convert_message(&message,*index,image_capable));*index+=1;}pending.clear();};
    for entry in entries {
        if entry.get("type").and_then(Value::as_str)==Some("compaction") && let Some(details)=entry.get("details").and_then(get_openai_remote_compaction_details) {
            flush(&mut pending,&mut items,&mut index);items.extend(details.replacement_input.into_iter().map(Value::Object));continue;
        }
        pending.extend(session_entry_to_context_messages(entry));
    }
    flush(&mut pending,&mut items,&mut index);items
}
