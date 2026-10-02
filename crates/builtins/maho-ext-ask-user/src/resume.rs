use serde_json::Value;
use std::collections::HashSet;
use crate::{schema::{AskUserVariant,to_canonical,DEFAULT_ASK_USER_TIMEOUT_MS},format::parse_ask_user_answer_frame};
pub struct DanglingQuestion{pub tool_call_id:String,pub variant:AskUserVariant,pub args:Value}
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
