use super::{approval_types::SendToThreadSubscribers,user_input_types::{UserInputProtocolError,read_user_input_result}};
use maho_ext_api::{QuestionRequest,QuestionResponse,QuestionStatus,QuestionAnswer,QuestionOptions,QuestionDraft};
use maho_ext_ask_user::pending::PendingQuestion;
use serde_json::{Value,json};
use std::{collections::BTreeMap,sync::{Arc,Mutex}};
use tokio::sync::{oneshot,watch};

struct PendingInput {
    thread_id: String,
    canonical: QuestionRequest,
    request: Value,
    state: PendingQuestion,
    draft: (BTreeMap<String,QuestionAnswer>,Option<String>),
    on_progress: Option<Arc<dyn Fn(QuestionDraft)+Send+Sync>>,
    wake: watch::Sender<u64>,
    resolve: oneshot::Sender<QuestionResponse>,
}
pub struct UserInputBridge {
    next_id: u64,
    pending: BTreeMap<String,PendingInput>,
    send: SendToThreadSubscribers,
}
fn now_ms() -> u64 {chrono::Utc::now().timestamp_millis().max(0) as u64}
fn draft(request: &QuestionRequest,result: &Value) -> (BTreeMap<String,QuestionAnswer>,Option<String>) {
    let mut answers = BTreeMap::new();
    for question in &request.questions {
        let Some(values) = result["answers"][&question.id]["answers"].as_array() else {continue;};
        let mut selected = Vec::new();let mut text = Vec::new();
        for value in values.iter().filter_map(Value::as_str) {
            if question.options.iter().any(|option|option.label == value) {selected.push(value.into());} else {text.push(value);}
        }
        let text = text.join("\n");
        answers.insert(question.id.clone(),QuestionAnswer {selected,text:(!text.is_empty()).then_some(text)});
    }
    (answers,result["comment"].as_str().map(str::to_owned))
}
impl UserInputBridge {
    pub fn new(send: SendToThreadSubscribers) -> Self {Self {next_id:0,pending:BTreeMap::new(),send}}
    pub fn pending_count(&self) -> usize {self.pending.len()}
    pub fn request_user_input(bridge: &Arc<Mutex<Self>>,thread_id: &str,turn_id: &str,item_id: &str,canonical: QuestionRequest,options: QuestionOptions) -> oneshot::Receiver<QuestionResponse> {
        let mut guard = bridge.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let id = format!("user-input-{}",guard.next_id);guard.next_id += 1;
        let timeout = options.dialog.timeout_ms.unwrap_or(canonical.timeout_ms);
        let questions = canonical.questions.iter().map(|question|json!({"id":question.id,"header":question.header,"question":question.question,"multiSelect":question.multi_select,"isOther":true,"isSecret":false,"options":if question.options.is_empty() {Value::Null} else {json!(question.options.iter().map(|option|json!({"label":option.label,"description":option.description.as_deref().unwrap_or_default()})).collect::<Vec<_>>())}})).collect::<Vec<_>>();
        let request = json!({"id":id,"method":"item/tool/requestUserInput","params":{"threadId":thread_id,"turnId":turn_id,"itemId":item_id,"autoResolutionMs":null,"timeoutMs":timeout,"waitForAnswer":canonical.wait_for_answer,"questions":questions}});
        let state = PendingQuestion::new(canonical.clone(),now_ms(),timeout,None);
        let (wake,mut updates) = watch::channel(state.deadline_at_ms);
        let (resolve,response) = oneshot::channel();
        guard.pending.insert(id.clone(),PendingInput {thread_id:thread_id.into(),canonical,request:request.clone(),state,draft:(BTreeMap::new(),None),on_progress:options.on_progress,wake,resolve});
        if options.dialog.signal.as_ref().is_some_and(maho_ext_api::AbortSignal::is_aborted) {guard.cancel(&id,QuestionStatus::Cancelled);return response;}
        if timeout == 0 {guard.timeout(&id);return response;}
        if (guard.send)(thread_id,request) == 0 {guard.cancel(&id,QuestionStatus::Unavailable);return response;}
        drop(guard);
        let weak = Arc::downgrade(bridge);
        tokio::spawn(async move {
            loop {
                let deadline = *updates.borrow_and_update();
                tokio::select! {
                    changed = updates.changed()=>if changed.is_err() {break;},
                    _ = tokio::time::sleep(std::time::Duration::from_millis(deadline.saturating_sub(now_ms())))=>{
                        if let Some(bridge) = weak.upgrade() {
                            let mut bridge = bridge.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                            if bridge.pending.get(&id).is_some_and(|pending|pending.state.deadline_at_ms > now_ms()) {continue;}
                            bridge.timeout(&id);
                        }
                        break;
                    },
                    _ = async {match &options.dialog.signal {Some(signal)=>signal.cancelled().await,None=>std::future::pending().await}}=>{
                        if let Some(bridge) = weak.upgrade() {bridge.lock().unwrap_or_else(std::sync::PoisonError::into_inner).cancel(&id,QuestionStatus::Cancelled);}
                        break;
                    },
                }
            }
        });
        response
    }
    fn finish(&mut self,id: &str,response: QuestionResponse) {
        if let Some(pending) = self.pending.remove(id) {
            (self.send)(&pending.thread_id,json!({"method":"serverRequest/resolved","params":{"threadId":pending.thread_id,"requestId":id}}));
            let _delivery = pending.resolve.send(response);
        }
    }
    fn cancel(&mut self,id: &str,status: QuestionStatus) {
        if let Some(pending) = self.pending.get_mut(id) {let response = pending.state.cancel(status);self.finish(id,response);}
    }
    fn timeout(&mut self,id: &str) {
        if let Some(pending) = self.pending.get_mut(id) {let response = pending.state.timeout(now_ms());self.finish(id,response);}
    }
    pub fn resolve_response(&mut self,response: &Value) -> Result<bool,UserInputProtocolError> {
        let Some(id) = response["id"].as_str() else {return Ok(false);};
        let Some(pending) = self.pending.get_mut(id) else {return Ok(false);};
        if response.get("error").is_some() {self.cancel(id,QuestionStatus::Cancelled);return Ok(true);}
        let result = read_user_input_result(&response["result"])?;
        let (answers,comment) = draft(&pending.canonical,&result);
        pending.state.touch(now_ms(),Some((answers.clone(),comment.clone())));
        let response = if result["cancelled"] == true {pending.state.cancel(QuestionStatus::Cancelled)} else if let Some(response) = pending.state.submit(answers.clone(),comment) {response} else {
            let mut cancelled = pending.state.cancel(QuestionStatus::Cancelled);
            if answers.values().any(|answer|!answer.selected.is_empty() || answer.text.as_ref().is_some_and(|text|!maho_ai::utils::js::trim(text).is_empty())) {cancelled.status = QuestionStatus::Answered;}
            cancelled
        };
        self.finish(id,response);Ok(true)
    }
    pub fn progress(&mut self,params: &Value) -> Result<bool,UserInputProtocolError> {
        let Some(id) = params["requestId"].as_str() else {return Ok(false);};
        let Some(pending) = self.pending.get_mut(id) else {return Ok(false);};
        let result = read_user_input_result(params)?;let next = draft(&pending.canonical,&result);
        if params.get("answers").is_some() {pending.draft.0 = next.0;}
        if params.get("comment").is_some() {pending.draft.1 = next.1;}
        pending.state.touch(now_ms(),Some(pending.draft.clone()));
        pending.wake.send_replace(pending.state.deadline_at_ms);
        if let Some(progress) = &pending.on_progress {progress(QuestionDraft {answers:Some(pending.draft.0.clone()),comment:pending.draft.1.clone()});}
        Ok(true)
    }
    pub fn replay_pending_for_thread(&self,id: &str) -> usize {
        let pending = self.pending.values().filter(|pending|pending.thread_id == id).collect::<Vec<_>>();
        for pending in &pending {(self.send)(id,pending.request.clone());}pending.len()
    }
    pub fn cancel_pending_for_thread(&mut self,id: &str) -> usize {
        let ids = self.pending.iter().filter(|(_,pending)|pending.thread_id == id).map(|(id,_)|id.clone()).collect::<Vec<_>>();
        for id in &ids {self.cancel(id,QuestionStatus::Cancelled);}ids.len()
    }
}
