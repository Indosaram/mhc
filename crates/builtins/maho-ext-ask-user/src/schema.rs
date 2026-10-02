use maho_ext_api::{Question,QuestionOption,QuestionRequest};
use serde_json::Value;
pub use crate::params::{claude_params, codex_params};
pub const DEFAULT_ASK_USER_TIMEOUT_MS:u64=1_800_000;
pub const WAIT_FLAG_STEER_TEXT:&str="This call omitted wait_for_answer (or waitForAnswer). Set true to pause here until the user answers, false to keep working and receive the answer later as a user message.";
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum AskUserVariant{Codex,Claude}
fn text(value:&Value,key:&str)->String{value.get(key).and_then(Value::as_str).unwrap_or("").trim().into()}
fn snake_case(id:&str)->bool{id.split('_').all(|part|!part.is_empty()&&part.bytes().all(|byte|byte.is_ascii_lowercase()||byte.is_ascii_digit()))&&id.as_bytes().first().is_some_and(u8::is_ascii_lowercase)}
pub fn to_canonical(variant:AskUserVariant,args:&Value,request_id:String,timeout_ms:Option<u64>)->Result<QuestionRequest,String>{
    let wait_key=if variant==AskUserVariant::Codex{"wait_for_answer"}else{"waitForAnswer"};
    let wait_for_answer=args.get(wait_key).and_then(Value::as_bool).ok_or(WAIT_FLAG_STEER_TEXT)?;
    let max=if variant==AskUserVariant::Codex{3}else{4};
    let raw=args.get("questions").and_then(Value::as_array).filter(|questions|!questions.is_empty()&&questions.len()<=max).ok_or_else(||format!("questions must contain 1 to {max} items"))?;
    let mut questions=Vec::new();
    for (index,raw) in raw.iter().enumerate(){
        if !raw.is_object(){return Err("each question must be an object".into());}
        let header=text(raw,"header");if header.is_empty(){return Err("header must be non-empty".into());}
        if header.encode_utf16().count()>12{return Err("header must be 12 or fewer characters".into());}
        let question=text(raw,"question");if question.is_empty(){return Err("question must be non-empty".into());}
        let mut options=Vec::new();
        if let Some(raw_options)=raw.get("options"){
            let raw_options=raw_options.as_array().ok_or("options must be an array")?;
            if raw_options.len()<2||raw_options.len()>max{return Err(format!("options must contain 2 to {max} items{}",if variant==AskUserVariant::Claude{" when present"}else{""}));}
            for option in raw_options{
                if !option.is_object(){return Err("each option must be an object".into());}
                let label=text(option,"label");if label.is_empty(){return Err("option label must be non-empty".into());}
                let description=text(option,"description");if variant==AskUserVariant::Codex&&description.is_empty(){return Err("option description is required".into());}
                options.push(QuestionOption{label,description:(!description.is_empty()).then_some(description)});
            }
        }else if variant==AskUserVariant::Codex{return Err("options must contain 2 to 3 items".into());}
        let id=if variant==AskUserVariant::Claude{format!("q{}",index+1)}else{let id=text(raw,"id");if !snake_case(&id){return Err("id must be snake_case".into());}id};
        let multi_select=if variant==AskUserVariant::Claude{raw.get("multiSelect").and_then(Value::as_bool).ok_or("multiSelect is required")?}else{raw.get("multiSelect").and_then(Value::as_bool)==Some(true)};
        questions.push(Question{id,header,question,options,multi_select});
    }
    Ok(QuestionRequest{request_id,questions,wait_for_answer,timeout_ms:timeout_ms.unwrap_or(DEFAULT_ASK_USER_TIMEOUT_MS)})
}
