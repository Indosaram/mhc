use std::collections::BTreeMap;
use serde_json::{Value,json};
use crate::{content_blocks::append_sdk_content_blocks,tools::map_pi_tool_name};
pub fn content_to_text(content:&[Value],custom:&BTreeMap<String,String>)->String {
    content.iter().map(|block|match block["type"].as_str() {Some("text")=>block["text"].as_str().unwrap_or_default().into(),Some("thinking")=>block["thinking"].as_str().unwrap_or_default().into(),Some("toolCall")=>format!("Historical tool call (non-executable): {} args={}",map_pi_tool_name(block["name"].as_str().expect("tool name"),custom),block["arguments"]),Some(kind)=>format!("[{kind}]"),None=>"[undefined]".into()}).collect::<Vec<String>>().join("\n")
}
fn push_text(blocks:&mut Vec<Value>,text:impl Into<String>) {blocks.push(json!({"type":"text","text":text.into()}));}
pub fn build_prompt_blocks(messages:&[Value],custom:&BTreeMap<String,String>,tool_watch_note:Option<&str>)->Vec<Value> {
    let mut blocks=Vec::new();let final_user=messages.last().filter(|m|m["role"]=="user");let history=if final_user.is_some() {&messages[..messages.len()-1]}else {messages};
    if !history.is_empty() {
        push_text(&mut blocks,"<conversation_history>\n");let mut previous=false;
        for message in history {
            let role=message["role"].as_str().expect("message role");if role=="configurationUpdate" {continue;}
            let label=match role {"user"=>"USER:".into(),"assistant"=>"ASSISTANT:".into(),_=>format!("TOOL RESULT (historical {}, id={}):",map_pi_tool_name(message["toolName"].as_str().expect("tool name"),custom),message["toolCallId"].as_str().expect("tool id"))};
            push_text(&mut blocks,format!("{}{label}\n",if previous {"\n\n"}else {""}));previous=true;
            if role=="assistant" {let text=content_to_text(message["content"].as_array().expect("assistant content"),custom);if !text.is_empty() {push_text(&mut blocks,text);}}
            else if !append_sdk_content_blocks(&mut blocks,&message["content"]) {push_text(&mut blocks,"(see attached image)");}
        }push_text(&mut blocks,"\n</conversation_history>");
    }
    if let Some(note)=tool_watch_note.map(str::trim).filter(|s|!s.is_empty()) {push_text(&mut blocks,"<recovered_tool_results>\n");push_text(&mut blocks,note);push_text(&mut blocks,"\n</recovered_tool_results>");}
    push_text(&mut blocks,"The above is the conversation history so far, provided as context. Respond as the assistant to the user message below only. Never emit \"USER:\" or \"ASSISTANT:\" labels or continue the transcript.");
    if let Some(user)=final_user&& !append_sdk_content_blocks(&mut blocks,&user["content"]) {push_text(&mut blocks,"(see attached image)");}blocks
}
pub fn prompt_message(blocks:Vec<Value>)->Value {json!({"type":"user","message":{"role":"user","content":blocks},"parent_tool_use_id":null,"session_id":"prompt"})}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preserves_images_and_custom_tool_history_as_non_executable() {
        let messages=[json!({"role":"assistant","content":[{"type":"toolCall","name":"repoSearch","arguments":{"query":"needle"}}]}),json!({"role":"toolResult","toolName":"repoSearch","toolCallId":"call","content":[{"type":"text","text":"match"}]}),json!({"role":"user","content":[{"type":"image","mimeType":"image/png","data":"aW1hZ2U="}]})];
        let custom=[("repoSearch".into(),"mcp__custom-tools__repoSearch".into())].into();let blocks=build_prompt_blocks(&messages,&custom,None);
        assert_eq!(blocks.iter().filter(|b|b["type"]=="image").count(),1);assert!(blocks.iter().all(|b|b["type"]!="tool_use"));assert!(blocks[2]["text"].as_str().expect("history").contains("mcp__custom-tools__repoSearch"));let message=prompt_message(blocks);assert_eq!(message["message"]["content"].as_array().expect("blocks").last().expect("placeholder")["text"],"(see attached image)");assert!(message["parent_tool_use_id"].is_null());
    }
    #[test]
    fn omits_config_updates_and_places_recovered_note_before_final_user() {
        let blocks=build_prompt_blocks(&[json!({"role":"configurationUpdate"}),json!({"role":"user","content":"continue"})],&BTreeMap::new(),Some(" recovered "));
        assert!(!blocks.iter().any(|b|b["text"]=="USER:\n"));assert_eq!(blocks.last().expect("final")["text"],"continue");assert!(blocks.iter().any(|b|b["text"]=="recovered"));
    }
}
