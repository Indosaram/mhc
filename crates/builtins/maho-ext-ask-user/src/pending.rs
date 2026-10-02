use maho_ext_api::{QuestionRequest,QuestionResponse,QuestionStatus,QuestionAnswer};
use std::collections::BTreeMap;
pub struct PendingQuestion{
    request:QuestionRequest,created_at_ms:u64,idle_timeout_ms:u64,hard_deadline_at_ms:u64,
    pub deadline_at_ms:u64,draft_answers:BTreeMap<String,QuestionAnswer>,draft_comment:Option<String>,pub result:Option<QuestionResponse>,
}
impl PendingQuestion{
    pub fn new(request:QuestionRequest,now:u64,idle_timeout_ms:u64,hard_cap_ms:Option<u64>)->Self{
        let hard_deadline_at_ms=now.saturating_add(hard_cap_ms.unwrap_or(7_200_000));
        Self{request,created_at_ms:now,idle_timeout_ms,hard_deadline_at_ms,deadline_at_ms:now.saturating_add(idle_timeout_ms).min(hard_deadline_at_ms),draft_answers:BTreeMap::new(),draft_comment:None,result:None}
    }
    pub fn touch(&mut self,now:u64,draft:Option<(BTreeMap<String,QuestionAnswer>,Option<String>)>){
        if self.result.is_some(){return;}
        if let Some((answers,comment))=draft{self.draft_answers=answers;self.draft_comment=comment;}
        self.deadline_at_ms=now.saturating_add(self.idle_timeout_ms).min(self.hard_deadline_at_ms);
    }
    fn response(&self,status:QuestionStatus,answers:BTreeMap<String,QuestionAnswer>,comment:Option<String>,elapsed:Option<u64>)->QuestionResponse{
        let unanswered=self.request.questions.iter().filter(|question|!answers.get(&question.id).is_some_and(|answer|!answer.selected.is_empty()||answer.text.as_ref().is_some_and(|text|!text.trim().is_empty()))).map(|question|question.id.clone()).collect();
        QuestionResponse{status,answers,comment:comment.filter(|comment|!comment.trim().is_empty()),unanswered,auto_resolved_after_ms:elapsed}
    }
    pub fn submit(&mut self,answers:BTreeMap<String,QuestionAnswer>,comment:Option<String>)->Option<QuestionResponse>{
        if let Some(result)=&self.result{return Some(result.clone());}
        let status=if comment.as_ref().is_some_and(|comment|!comment.trim().is_empty()){QuestionStatus::CommentSubmitted}else{QuestionStatus::Answered};
        let response=self.response(status,answers,comment,None);
        if status==QuestionStatus::Answered&&!response.unanswered.is_empty()&&response.answers.is_empty(){return None;}
        self.result=Some(response.clone());Some(response)
    }
    pub fn cancel(&mut self,reason:QuestionStatus)->QuestionResponse{
        if let Some(result)=&self.result{return result.clone();}
        let response=self.response(reason,self.draft_answers.clone(),self.draft_comment.clone(),None);self.result=Some(response.clone());response
    }
    pub fn timeout(&mut self,now:u64)->QuestionResponse{
        if let Some(result)=&self.result{return result.clone();}
        let response=self.response(QuestionStatus::TimedOut,self.draft_answers.clone(),self.draft_comment.clone(),Some(now.saturating_sub(self.created_at_ms)));self.result=Some(response.clone());response
    }
    pub fn remaining_ms(&self,now:u64)->u64{self.deadline_at_ms.saturating_sub(now)}
}
