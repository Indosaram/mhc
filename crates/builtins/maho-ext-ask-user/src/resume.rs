use serde_json::Value;
use std::collections::HashSet;
use crate::{schema::{AskUserVariant,to_canonical,DEFAULT_ASK_USER_TIMEOUT_MS},format::parse_ask_user_answer_frame};
pub struct DanglingQuestion{pub tool_call_id:String,pub variant:AskUserVariant,pub args:Value}
pub const ASK_USER_RESUMED_ENTRY: &str = "ask-user:resumed";
pub(crate) async fn resume_dangling_questions(sender:std::sync::Arc<maho_ext_api::ExtensionApi>,ctx:&maho_ext_api::ExtensionContext)->Result<(),maho_ext_api::ExtensionFailure>{
    let entries=ctx.session_manager.get_branch().into_iter().map(|entry|{
        entry.data
    }).collect::<Vec<_>>();
    let mut pending=crate::registry::get_pending_questions(ctx.session_manager.session_id()).into_iter().map(|entry|entry.request.request_id.clone()).collect::<HashSet<_>>();
    let timeout=(ctx.get_ask_user_settings()?.timeout_minutes*60_000.0) as u64;
    for dangling in find_dangling_questions(&entries){
        if !pending.insert(dangling.tool_call_id.clone()){continue;}
        sender.append_entry(ASK_USER_RESUMED_ENTRY,Some(serde_json::json!({"toolCallId":dangling.tool_call_id})))?;
        let request=to_canonical(dangling.variant,&dangling.args,dangling.tool_call_id.clone(),Some(timeout)).unwrap_or_else(|_|maho_ext_api::QuestionRequest{request_id:dangling.tool_call_id,questions:vec![],wait_for_answer:false,timeout_ms:timeout});
        crate::tool::start_question(sender.clone(),ctx.clone(),request,ctx.signal.clone(),std::sync::Arc::new(std::sync::Mutex::new(crate::tool::AskUserState::default())),dangling.variant,true).await?;
    }
    Ok(())
}
pub fn find_dangling_questions(entries:&[Value])->Vec<DanglingQuestion>{
    let mut results=HashSet::new();let mut accepted=HashSet::new();let mut resumed=HashSet::new();let mut settled=HashSet::new();
    for entry in entries{
        if entry["type"]=="message"&&entry["message"]["role"]=="user"{
            let content=&entry["message"]["content"];let texts=if let Some(text)=content.as_str(){vec![text]}else{content.as_array().map_or(Vec::new(),|parts|parts.iter().filter(|part|part["type"]=="text").filter_map(|part|part["text"].as_str()).collect())};
            for text in texts{if let Some((id,_))=parse_ask_user_answer_frame(text){settled.insert(id.to_owned());}}
        }
        if entry["type"]=="custom"&&entry["customType"]=="ask-user:resumed"&&let Some(id)=entry["data"]["toolCallId"].as_str(){resumed.insert(id.to_owned());}
        if entry["type"]=="custom"&&entry["customType"]=="ask-user:settlement"&&let Some(id)=entry["data"]["requestId"].as_str(){settled.insert(id.to_owned());}
        if entry["type"]=="message"&&entry["message"]["role"]=="toolResult"&&let Some(id)=entry["message"]["toolCallId"].as_str(){results.insert(id.to_owned());let message=&entry["message"];if message["isError"]!=true&&message["details"]["accepted"]==true&&message["details"]["status"]=="pending"{accepted.insert(id.to_owned());}}
    }
    let mut dangling=Vec::new();
    for entry in entries.iter().rev(){
        if entry["type"]!="message"||entry["message"]["role"]!="assistant"{continue;}
        let Some(blocks)=entry["message"]["content"].as_array()else{continue;};
        for block in blocks.iter().rev(){
            if block["type"]!="toolCall"||block["incomplete"]==true{continue;}
            let variant=match block["name"].as_str(){Some("request_user_input")=>AskUserVariant::Codex,Some("ask_user_question")=>AskUserVariant::Claude,_=>continue};
            let Some(id)=block["id"].as_str()else{continue;};if settled.contains(id)||resumed.contains(id){continue;}
            let wait=to_canonical(variant,&block["arguments"],id.into(),Some(DEFAULT_ASK_USER_TIMEOUT_MS)).is_ok_and(|request|request.wait_for_answer);
            if results.contains(id)&&(wait||!accepted.contains(id)){continue;}
            dangling.push(DanglingQuestion{tool_call_id:id.into(),variant,args:block["arguments"].clone()});
        }
    }
    dangling
}
