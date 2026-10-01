use std::collections::{BTreeMap,BTreeSet};
use maho_ext_api::{QuestionRequest,QuestionResponse,QuestionAnswer,QuestionStatus};
use serde_json::{Value,json};
use tokio::sync::oneshot;
pub async fn degrade_question(ui:&dyn maho_ext_api::ExtensionUi,request:QuestionRequest,options:maho_ext_api::ExtensionUiDialogOptions)->Result<QuestionResponse,maho_ext_api::ExtensionFailure>{
    let mut answers=BTreeMap::new();let other="Other (type an answer)";
    for question in &request.questions{
        let labels=question.options.iter().map(|option|option.label.clone()).chain(std::iter::once(other.into())).collect::<Vec<_>>();
        let selected=ui.select(&question.question,&labels,options.clone()).await;
        if selected.as_deref()==Some(other){if let Some(text)=ui.input(&question.question,None,options.clone()).await.filter(|text|!text.trim().is_empty()){answers.insert(question.id.clone(),QuestionAnswer{selected:vec![],text:Some(text)});}}
        else if let Some(selected)=selected{answers.insert(question.id.clone(),QuestionAnswer{selected:vec![selected],text:None});}
    }
    let comment=ui.input("Anything else? (optional)",None,options).await;
    let unanswered=request.questions.iter().filter(|question|!answers.contains_key(&question.id)).map(|question|question.id.clone()).collect::<Vec<_>>();
    let status=if comment.as_deref().is_some_and(|comment|!comment.trim().is_empty()){QuestionStatus::CommentSubmitted}else if !unanswered.is_empty(){QuestionStatus::Cancelled}else{QuestionStatus::Answered};
    Ok(QuestionResponse{status,answers,comment,unanswered,auto_resolved_after_ms:None})
}
struct Pending{request:QuestionRequest,frame:Value,answers:BTreeMap<String,QuestionAnswer>,comment:Option<String>,asked_at:u64,timeout:u64,sender:oneshot::Sender<QuestionResponse>}
#[derive(Default)]
pub struct ConnectionQuestionBridge{pending:BTreeMap<String,Pending>,resolved:BTreeSet<String>}
#[derive(Debug,PartialEq,Eq)]pub enum QuestionReply{Accepted,Incomplete,AlreadyResolved,Unknown}
fn answers_json(answers:&BTreeMap<String,QuestionAnswer>)->Value{Value::Object(answers.iter().map(|(id,answer)|(id.clone(),json!({"selected":answer.selected,"text":answer.text}))).collect())}
fn parse_answers(value:&Value)->BTreeMap<String,QuestionAnswer>{value.as_object().into_iter().flatten().map(|(id,answer)|(id.clone(),QuestionAnswer{selected:answer.get("selected").and_then(Value::as_array).map(|selected|selected.iter().filter_map(Value::as_str).map(str::to_owned).collect()).unwrap_or_default(),text:answer.get("text").and_then(Value::as_str).map(str::to_owned)})).collect()}
impl ConnectionQuestionBridge{
    pub fn ask(&mut self,request:QuestionRequest,timeout:Option<u64>,now:u64)->(Value,oneshot::Receiver<QuestionResponse>){
        let id=uuid::Uuid::new_v4().to_string();let timeout=timeout.unwrap_or(request.timeout_ms);
        let questions=request.questions.iter().map(|question|json!({"id":question.id,"header":question.header,"question":question.question,"multiSelect":question.multi_select,"options":question.options.iter().map(|option|json!({"label":option.label,"description":option.description})).collect::<Vec<_>>()})).collect::<Vec<_>>();
        let frame=json!({"type":"extension_ui_request","method":"question","id":id,"requestId":request.request_id,"toolCallId":request.request_id,"waitForAnswer":request.wait_for_answer,"questions":questions,"timeout":timeout,"askedAtMs":now,"deadlineAtMs":now+timeout,"remainingMs":timeout});
        let (sender,receiver)=oneshot::channel();self.pending.insert(id,Pending{request,frame:frame.clone(),answers:BTreeMap::new(),comment:None,asked_at:now,timeout,sender});(frame,receiver)
    }
    pub fn pending_questions(&self,now:u64)->Vec<Value>{self.pending.values().map(|pending|{let mut frame=pending.frame.clone();frame["remainingMs"]=frame["deadlineAtMs"].as_u64().unwrap_or(now).saturating_sub(now).into();frame}).collect()}
    fn finish(&mut self,id:&str,status:QuestionStatus,now:u64)->Option<Value>{
        let pending=self.pending.remove(id)?;self.resolved.insert(id.into());
        let unanswered=pending.request.questions.iter().filter(|question|pending.answers.get(&question.id).is_none_or(|answer|answer.selected.is_empty()&&answer.text.as_deref().is_none_or(|text|text.trim().is_empty()))).map(|question|question.id.clone()).collect::<Vec<_>>();
        let outcome=match status{QuestionStatus::Answered=>"answered",QuestionStatus::CommentSubmitted=>"comment-submitted",QuestionStatus::TimedOut=>"timed_out",QuestionStatus::Cancelled=>"cancelled",QuestionStatus::OrphanedAfterRestart=>"orphaned_after_restart",QuestionStatus::Unavailable=>"unavailable"};
        let record=json!({"type":"question_resolved","id":id,"requestId":pending.request.request_id,"toolCallId":pending.request.request_id,"outcome":outcome,"answers":answers_json(&pending.answers),"comment":pending.comment,"unanswered":unanswered,"deadlineAtMs":pending.frame["deadlineAtMs"]});
        let result=QuestionResponse{status,answers:pending.answers,comment:pending.comment,unanswered,auto_resolved_after_ms:(status==QuestionStatus::TimedOut).then_some(now.saturating_sub(pending.asked_at))};let _=pending.sender.send(result);Some(record)
    }
    pub fn respond(&mut self,response:&Value,now:u64)->(QuestionReply,Option<Value>){
        let Some(id)=response.get("id").and_then(Value::as_str)else{return (QuestionReply::Unknown,None);};
        if !self.pending.contains_key(id){return (if self.resolved.contains(id){QuestionReply::AlreadyResolved}else{QuestionReply::Unknown},None);}
        if response.get("cancelled").is_some(){return (QuestionReply::Accepted,self.finish(id,QuestionStatus::Cancelled,now));}
        let Some(answers)=response.get("answers")else{return (QuestionReply::Incomplete,None);};
        let pending=self.pending.get_mut(id).expect("pending question exists");pending.answers=parse_answers(answers);pending.comment=response.get("comment").and_then(Value::as_str).map(str::to_owned);
        let comment=pending.comment.as_deref().is_some_and(|comment|!comment.trim().is_empty());
        if !comment&&pending.answers.is_empty(){return (QuestionReply::Incomplete,None);}
        (QuestionReply::Accepted,self.finish(id,if comment{QuestionStatus::CommentSubmitted}else{QuestionStatus::Answered},now))
    }
    pub fn progress(&mut self,draft:&Value,now:u64)->Option<Value>{
        let id=draft.get("id")?.as_str()?;let pending=self.pending.get_mut(id)?;
        if let Some(answers)=draft.get("answers"){pending.answers=parse_answers(answers);}
        if let Some(comment)=draft.get("comment").and_then(Value::as_str){pending.comment=Some(comment.into());}
        pending.frame["deadlineAtMs"]=(now+pending.timeout).into();Some(json!({"type":"question_updated","id":id,"deadlineAtMs":now+pending.timeout,"remainingMs":pending.timeout}))
    }
    pub fn expire(&mut self,now:u64)->Vec<Value>{let ids=self.pending.iter().filter(|(_,pending)|pending.frame["deadlineAtMs"].as_u64().is_some_and(|deadline|now>=deadline)).map(|(id,_)|id.clone()).collect::<Vec<_>>();ids.iter().filter_map(|id|self.finish(id,QuestionStatus::TimedOut,now)).collect()}
    pub fn cancel_all(&mut self,now:u64)->Vec<Value>{let ids=self.pending.keys().cloned().collect::<Vec<_>>();ids.iter().filter_map(|id|self.finish(id,QuestionStatus::Cancelled,now)).collect()}
}
#[cfg(test)]mod tests{
    use super::*;
    fn request()->QuestionRequest{QuestionRequest{request_id:"tool".into(),questions:vec![maho_ext_api::Question{id:"q".into(),header:"h".into(),question:"prompt".into(),options:vec![],multi_select:false}],wait_for_answer:true,timeout_ms:100}}
    #[tokio::test]async fn typed_extension_request_settles_from_rpc_response(){let mut bridge=ConnectionQuestionBridge::default();let (frame,receiver)=bridge.ask(request(),None,0);let id=frame["id"].as_str().unwrap();assert_eq!(bridge.respond(&json!({"id":id,"answers":{}}),1).0,QuestionReply::Incomplete);assert_eq!(bridge.respond(&json!({"id":id,"answers":{"q":{"selected":["yes"]}}}),2).0,QuestionReply::Accepted);assert_eq!(receiver.await.unwrap().status,QuestionStatus::Answered);assert_eq!(bridge.respond(&json!({"id":id}),3).0,QuestionReply::AlreadyResolved);}
    #[tokio::test]async fn progress_rearms_idle_timeout_and_retains_draft(){let mut bridge=ConnectionQuestionBridge::default();let (frame,receiver)=bridge.ask(request(),None,0);bridge.progress(&json!({"id":frame["id"],"answers":{"q":{"selected":[],"text":"draft"}}}),99);assert!(bridge.expire(100).is_empty());assert_eq!(bridge.pending_questions(100)[0]["remainingMs"],99);assert_eq!(bridge.expire(199).len(),1);let response=receiver.await.unwrap();assert_eq!(response.status,QuestionStatus::TimedOut);assert!(response.unanswered.is_empty());assert_eq!(response.auto_resolved_after_ms,Some(199));}
}
