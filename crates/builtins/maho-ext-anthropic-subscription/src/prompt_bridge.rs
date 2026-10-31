use std::collections::BTreeMap;
use serde_json::{Value,json};
use crate::{content_blocks::append_sdk_content_blocks,tools::map_pi_tool_name};
pub fn content_to_text(content:&[Value],custom:&BTreeMap<String,String>)->String {
    content.iter().map(|block|match block["type"].as_str() {Some("text")=>block["text"].as_str().unwrap_or_default().into(),Some("thinking")=>block["thinking"].as_str().unwrap_or_default().into(),Some("toolCall")=>format!("Historical tool call (non-executable): {} args={}",map_pi_tool_name(block["name"].as_str().expect("tool name"),custom),block["arguments"]),Some(kind)=>format!("[{kind}]"),None=>"[undefined]".into()}).collect::<Vec<String>>().join("\n")
}
pub const RESPOND_TO_USER_INSTRUCTION:&str="The above is the conversation history so far, provided as context. Respond as the assistant to the user message below only. Never emit \"USER:\" or \"ASSISTANT:\" labels or continue the transcript.";
/// A replay that ends with tool results (a model switch or fallback mid-turn) has no new user message, so the
/// instruction must not call the last tool output a user message (senpi#2768).
pub const CONTINUE_TURN_INSTRUCTION:&str="The above is the conversation history so far, provided as context. There is no new user message: the last entries are results of tool calls you made in your current turn, not anything the user sent. Continue that turn as the assistant. Never emit \"USER:\" or \"ASSISTANT:\" labels or continue the transcript.";
fn push_text(blocks:&mut Vec<Value>,text:impl Into<String>) {blocks.push(json!({"type":"text","text":text.into()}));}
/// Text that closes the replayed history. Everything after it changes every turn, so it stays after the cache
/// breakpoint.
pub const CONVERSATION_HISTORY_CLOSER:&str="\n</conversation_history>";
pub fn build_prompt_blocks(messages:&[Value],custom:&BTreeMap<String,String>,tool_watch_note:Option<&str>)->Vec<Value> {
    build_prompt_blocks_with(messages,custom,tool_watch_note,None)
}
/// senpi `markCacheBreakpoint`: Claude Code drops a breakpoint set on an image, so the last text block of the history
/// carries it.
fn mark_cache_breakpoint(blocks:&mut [Value],history_start:usize,ttl:crate::prompt_cache_ttl::PromptCacheTtl) {
    if let Some(block)=blocks[history_start..].iter_mut().rev().find(|block|block["type"]=="text") {
        block["cache_control"]=json!({"type":"ephemeral","ttl":ttl.as_str()});
    }
}
/// [`build_prompt_blocks`] with senpi `PromptBlockOptions.cacheBreakpoint`: the last history text block becomes a
/// prompt-cache breakpoint with the lifetime Claude Code is pinned to. History only grows by appending, so the next
/// rebuilt prompt reads it from cache (senpi#2982). Only for one-shot queries.
pub fn build_prompt_blocks_with(messages:&[Value],custom:&BTreeMap<String,String>,tool_watch_note:Option<&str>,cache_breakpoint:Option<crate::prompt_cache_ttl::PromptCacheTtl>)->Vec<Value> {
    let mut blocks=Vec::new();let final_user=messages.last().filter(|m|m["role"]=="user");let history=if final_user.is_some() {&messages[..messages.len()-1]}else {messages};
    let continues_turn=final_user.is_none()&&history.last().is_some_and(|m|m["role"]=="toolResult");
    let replayed_images=crate::cold_seed_images::replay_history_images(history,custom,continues_turn);
    if !history.is_empty() {
        let history_start=blocks.len();
        push_text(&mut blocks,"<conversation_history>\n");let mut previous=false;
        for (message_index,message) in history.iter().enumerate() {
            let role=message["role"].as_str().expect("message role");if role=="configurationUpdate"||role=="system" {continue;}
            let label=match role {"user"=>"USER:".into(),"assistant"=>"ASSISTANT:".into(),_=>format!("TOOL RESULT (historical {}, id={}):",map_pi_tool_name(message["toolName"].as_str().expect("tool name"),custom),message["toolCallId"].as_str().expect("tool id"))};
            push_text(&mut blocks,format!("{}{label}\n",if previous {"\n\n"}else {""}));previous=true;
            if role=="assistant" {let text=content_to_text(message["content"].as_array().expect("assistant content"),custom);if !text.is_empty() {push_text(&mut blocks,text);}}
            else if !append_sdk_content_blocks(&mut blocks,replayed_images.get(&message_index).unwrap_or(&message["content"])) {push_text(&mut blocks,"(see attached image)");}
        }
        if let Some(ttl)=cache_breakpoint {mark_cache_breakpoint(&mut blocks,history_start,ttl);}
        push_text(&mut blocks,CONVERSATION_HISTORY_CLOSER);
    }
    if let Some(note)=tool_watch_note.map(str::trim).filter(|s|!s.is_empty()) {push_text(&mut blocks,"<recovered_tool_results>\n");push_text(&mut blocks,note);push_text(&mut blocks,"\n</recovered_tool_results>");}
    push_text(&mut blocks,if continues_turn {CONTINUE_TURN_INSTRUCTION}else {RESPOND_TO_USER_INSTRUCTION});
    if let Some(user)=final_user&& !append_sdk_content_blocks(&mut blocks,&user["content"]) {push_text(&mut blocks,"(see attached image)");}blocks
}
pub fn prompt_message(blocks:Vec<Value>)->Value {json!({"type":"user","message":{"role":"user","content":blocks},"parent_tool_use_id":null,"session_id":"prompt"})}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn one_shot_history_carries_a_cache_breakpoint_before_the_closer() {
        let messages=[json!({"role":"user","content":"earlier"}),json!({"role":"assistant","content":[{"type":"text","text":"answer"}]}),json!({"role":"user","content":"now"})];
        let blocks=build_prompt_blocks_with(&messages,&BTreeMap::new(),None,Some(crate::prompt_cache_ttl::PromptCacheTtl::OneHour));
        let closer=blocks.iter().position(|block|block["text"]==CONVERSATION_HISTORY_CLOSER).expect("closer");
        let marked:Vec<_>=blocks.iter().enumerate().filter(|(_,block)|block.get("cache_control").is_some()).map(|(index,_)|index).collect();
        assert_eq!(marked,[closer-1]);
        assert_eq!(blocks[closer-1]["cache_control"],json!({"type":"ephemeral","ttl":"1h"}));
        assert!(build_prompt_blocks(&messages,&BTreeMap::new(),None).iter().all(|block|block.get("cache_control").is_none()));
    }
    #[test]
    fn preserves_images_and_custom_tool_history_as_non_executable() {
        let messages=[json!({"role":"assistant","content":[{"type":"toolCall","name":"repoSearch","arguments":{"query":"needle"}}]}),json!({"role":"toolResult","toolName":"repoSearch","toolCallId":"call","content":[{"type":"text","text":"match"}]}),json!({"role":"user","content":[{"type":"image","mimeType":"image/png","data":"aW1hZ2U="}]})];
        let custom=[("repoSearch".into(),"mcp__custom-tools__repoSearch".into())].into();let blocks=build_prompt_blocks(&messages,&custom,None);
        assert_eq!(blocks.iter().filter(|b|b["type"]=="image").count(),1);assert!(blocks.iter().all(|b|b["type"]!="tool_use"));assert!(blocks[2]["text"].as_str().expect("history").contains("mcp__custom-tools__repoSearch"));let message=prompt_message(blocks);assert_eq!(message["message"]["content"].as_array().expect("blocks").last().expect("placeholder")["text"],"(see attached image)");assert!(message["parent_tool_use_id"].is_null());
    }
    #[test]
    fn a_replay_ending_in_tool_results_continues_the_turn_issue_2768() {
        let history=[json!({"role":"user","content":"draw"}),json!({"role":"system","content":"hidden"}),json!({"role":"assistant","content":[{"type":"toolCall","name":"bash","arguments":{}}]}),json!({"role":"toolResult","toolName":"bash","toolCallId":"call","content":[{"type":"text","text":"done"}]})];
        let blocks=build_prompt_blocks(&history,&BTreeMap::new(),None);
        assert_eq!(blocks.last().expect("instruction")["text"],CONTINUE_TURN_INSTRUCTION);
        assert!(!blocks.iter().any(|block|block["text"]=="hidden"));
        let mut asked=history.to_vec();asked.push(json!({"role":"user","content":"next"}));
        let blocks=build_prompt_blocks(&asked,&BTreeMap::new(),None);
        assert!(blocks.iter().any(|block|block["text"]==RESPOND_TO_USER_INSTRUCTION));
        assert!(!blocks.iter().any(|block|block["text"]==CONTINUE_TURN_INSTRUCTION));
    }
    #[test]
    fn omits_config_updates_and_places_recovered_note_before_final_user() {
        let blocks=build_prompt_blocks(&[json!({"role":"configurationUpdate"}),json!({"role":"user","content":"continue"})],&BTreeMap::new(),Some(" recovered "));
        assert!(!blocks.iter().any(|b|b["text"]=="USER:\n"));assert_eq!(blocks.last().expect("final")["text"],"continue");assert!(blocks.iter().any(|b|b["text"]=="recovered"));
    }
}
