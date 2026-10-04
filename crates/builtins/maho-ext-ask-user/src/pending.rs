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

/// The question owner drives the authoritative idle deadline independently of UI countdowns.
pub struct PendingTimer {
    pending: std::sync::Arc<std::sync::Mutex<PendingQuestion>>,
    changed: tokio::sync::watch::Sender<u64>,
    started: tokio::time::Instant,
    wall_started_ms: u64,
    task: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}
impl PendingTimer {
    pub fn new(request: QuestionRequest, on_timeout: std::sync::Arc<dyn Fn(QuestionResponse) + Send + Sync>) -> Self {
        let timeout = request.timeout_ms;
        let pending = std::sync::Arc::new(std::sync::Mutex::new(PendingQuestion::new(request, 0, timeout, None)));
        let (changed, mut updates) = tokio::sync::watch::channel(0);
        let started = tokio::time::Instant::now();
        let wall_started_ms = maho_ai::utils::diagnostics::now_ms().max(0) as u64;
        let state = pending.clone();
        let task = tokio::spawn(async move {
            loop {
                let deadline = {
                    let question = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    if question.result.is_some() { return; }
                    question.deadline_at_ms
                };
                tokio::select! {
                    biased;
                    update = updates.changed() => if update.is_err() { return; },
                    () = tokio::time::sleep_until(started + std::time::Duration::from_millis(deadline)) => {
                        let response = {
                            let mut question = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                            if question.result.is_some() { return; }
                            let now = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                            if now < question.deadline_at_ms { continue; }
                            question.timeout(now)
                        };
                        on_timeout(response);
                        return;
                    }
                }
            }
        });
        Self { pending, changed, started, wall_started_ms, task: std::sync::Mutex::new(Some(task)) }
    }
    pub fn touch(&self, draft: Option<(BTreeMap<String, QuestionAnswer>, Option<String>)>) {
        let now = u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).touch(now, draft);
        self.changed.send_modify(|generation| *generation = generation.wrapping_add(1));
    }
    pub fn progress(&self,draft:maho_ext_api::QuestionDraft){
        let now=u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let mut pending=self.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let answers=draft.answers.unwrap_or_else(||pending.draft_answers.clone());
        pending.touch(now,Some((answers,draft.comment)));
        drop(pending);
        self.changed.send_modify(|generation|*generation=generation.wrapping_add(1));
    }
    pub fn submit(&self, answers: BTreeMap<String, QuestionAnswer>, comment: Option<String>) -> Option<QuestionResponse> {
        let response = self.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).submit(answers, comment);
        if response.is_some() { self.disarm(); }
        response
    }
    pub fn cancel(&self, reason: QuestionStatus) -> QuestionResponse {
        let response = self.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).cancel(reason);
        self.disarm();
        response
    }
    fn disarm(&self) {
        if let Some(task) = self.task.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_ref() { task.abort(); }
    }
    pub async fn settle(&self) {
        let task = self.task.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
        if let Some(task) = task { let _result = task.await; }
    }
    pub fn deadline_at_ms(&self) -> u64 {
        self.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).deadline_at_ms
    }
    pub fn initial_draft(&self) -> maho_ext_api::QuestionDraft {
        let pending=self.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        maho_ext_api::QuestionDraft{answers:Some(pending.draft_answers.clone()),comment:pending.draft_comment.clone()}
    }
    pub fn hard_deadline_at_ms(&self) -> u64 {
        self.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).hard_deadline_at_ms
    }
    pub fn absolute_deadline_at_ms(&self) -> u64 {
        self.wall_started_ms.saturating_add(self.deadline_at_ms())
    }
    pub fn absolute_hard_deadline_at_ms(&self) -> u64 {
        self.wall_started_ms.saturating_add(self.hard_deadline_at_ms())
    }
    pub fn remaining_ms(&self) -> u64 {
        let now = u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remaining_ms(now)
    }
}
impl Drop for PendingTimer {
    fn drop(&mut self) {
        if let Some(task) = self.task.get_mut().unwrap_or_else(std::sync::PoisonError::into_inner).take() { task.abort(); }
    }
}
