use std::collections::BTreeMap;
use crate::transport::{TransportError,MAX_CURSOR_CLI_PROMPT_BYTES};
pub const CONTEXT_RECAP_MAX_BYTES:usize=8192;
pub const CONTEXT_RECAP_BEGIN:&str="===== senpi context recap =====";
pub const CONTEXT_RECAP_END:&str="===== end senpi context recap =====";
pub struct RecapExchange { pub role:ExchangeRole,pub text:String }
pub enum ExchangeRole { User,Assistant }
pub fn build_context_recap(model:Option<&str>,exchanges:&[RecapExchange],max_bytes:usize) -> Option<String> {
    let header=format!("{CONTEXT_RECAP_BEGIN}\n({}; recent conversation from senpi's own records follows)",model.map_or_else(||"chat restarted".into(),|m|format!("model switched to '{m}'")));
    let overhead=header.len()+1+CONTEXT_RECAP_END.len()+1;
    if overhead>=max_bytes { return None; }
    let budget=max_bytes-overhead;let mut selected=Vec::new();let mut used=0;
    for exchange in exchanges.iter().rev().filter(|e|!e.text.is_empty()) {
        let role=match exchange.role { ExchangeRole::User=>"user",ExchangeRole::Assistant=>"assistant" };
        let mut line=format!("{role}: {}",exchange.text);
        let cost=line.len()+usize::from(!selected.is_empty());
        if used+cost<=budget { selected.push(line);used+=cost;continue; }
        if selected.is_empty() {
            let mut end=budget.min(line.len());while !line.is_char_boundary(end) { end-=1; }
            line.truncate(end);selected.push(line);
        }
        break;
    }
    if selected.is_empty() { return None; }selected.reverse();
    Some(format!("{header}\n{}\n{CONTEXT_RECAP_END}",selected.join("\n")))
}
#[derive(Clone)]
pub struct SessionRecord { pub account_name:String,pub chat_id:String,pub last_model:String,pub last_used_at:i64 }
pub struct SessionPolicy { pub resume:bool,pub recap_on_model_switch:bool,pub max_recap_bytes:usize,pub prompt_ceiling_bytes:usize }
impl Default for SessionPolicy {
    fn default()->Self { Self {resume:true,recap_on_model_switch:true,max_recap_bytes:CONTEXT_RECAP_MAX_BYTES,prompt_ceiling_bytes:MAX_CURSOR_CLI_PROMPT_BYTES} }
}
pub struct TurnPlan { pub resume_chat_id:Option<String>,pub prompt:String,pub context_recap:Option<String>,pub model_switch:bool,pub recap_dropped_for_ceiling:bool }
pub struct TurnInput<'a> { pub session:&'a str,pub account:&'a str,pub prompt:&'a str,pub model:Option<&'a str>,pub recent:&'a [RecapExchange],pub policy:&'a SessionPolicy }
pub struct SessionAttempt { pub prompt:String,pub resume_chat_id:Option<String> }
pub type AttemptReceiver=tokio::sync::mpsc::UnboundedReceiver<Result<serde_json::Value,serde_json::Value>>;
impl SessionRouter {
    pub async fn run_shared_turn<F,Fut,N,C,O>(router:&tokio::sync::Mutex<Self>,input:TurnInput<'_>,run:F,now:N,classify:C,emit:O)->Result<(),serde_json::Value>
    where F:FnMut(SessionAttempt)->Fut,Fut:std::future::Future<Output=Result<AttemptReceiver,serde_json::Value>>,
        N:Fn()->i64,C:Fn(&crate::errors::CursorCliErrorInput)->crate::errors::CursorCliErrorKind,O:FnMut(serde_json::Value) {
        let mut turn_router=router.lock().await.clone();
        turn_router.run_turn(input,run,now,classify,emit).await
    }
    pub async fn run_turn<F,Fut,N,C,O>(&mut self,input:TurnInput<'_>,mut run:F,now:N,classify:C,mut emit:O)->Result<(),serde_json::Value>
    where F:FnMut(SessionAttempt)->Fut,Fut:std::future::Future<Output=Result<AttemptReceiver,serde_json::Value>>,
        N:Fn()->i64,C:Fn(&crate::errors::CursorCliErrorInput)->crate::errors::CursorCliErrorKind,O:FnMut(serde_json::Value) {
        use serde_json::json;
        use crate::errors::{CursorCliErrorInput,CursorCliErrorKind};
        let session_lock={let mut locks=self.turn_locks.lock().expect("router turn locks");locks.entry(input.session.into()).or_default().clone()};
        let _turn=session_lock.lock().await;
        let plan=self.plan_turn(input.session,input.account,input.prompt,input.model,input.recent,input.policy).map_err(|e|json!({"kind":"prompt_too_large","message":e.to_string()}))?;
        let previous=plan.resume_chat_id;
        let mut attempt=SessionAttempt {prompt:plan.prompt,resume_chat_id:previous.clone()};let mut fell_back=false;
        loop {
            let mut visible=false;
            let failure=match run(attempt).await {
                Err(error)=>error,
                Ok(mut events)=> {
                    let mut failure=None;
                    while let Some(event)=events.recv().await {
                        let event=match event {Ok(e)=>e,Err(e)=>{failure=Some(e);break;}};
                        if event["type"]=="malformed_stream" {failure=Some(json!({"thrown":event}));break;}
                        if event["type"]=="result" && (event["is_error"]==true || event["subtype"]=="error") {failure=Some(json!({"resultEvent":event}));break;}
                        if event["type"]=="system" && event["subtype"]=="init"
                            && let (Some(chat),Some(model))=(event["session_id"].as_str(),event["model"].as_str())
                            && !chat.is_empty() && !model.is_empty() {self.observe_init(input.session,input.account,chat,model,now());}
                        visible|=(event["type"]=="assistant" && event["message"]["content"].as_array().is_some_and(|blocks|blocks.iter().any(|b|b["type"]=="text" && b["text"].as_str().is_some_and(|s|!s.is_empty()))))
                            || ((event["type"]=="assistant_delta" || event["type"]=="text_delta") && event["delta"].as_str().is_some_and(|s|!s.is_empty()));
                        emit(event);
                    }
                    match failure {Some(error)=>error,None if visible=>return Ok(()),None=>json!({"message":"Cursor CLI attempt completed without visible assistant text"})}
                },
            };
            if failure["type"]=="aborted" || failure["kind"]=="aborted" || fell_back || previous.is_none() || visible {return Err(failure);}
            let error=if ["exitCode","stderr","resultEvent","thrown"].iter().any(|k|failure.get(*k).is_some()) {
                CursorCliErrorInput {exit_code:failure["exitCode"].clone(),stderr:failure["stderr"].clone(),result_event:failure["resultEvent"].clone(),thrown:failure["thrown"].clone()}
            } else {CursorCliErrorInput {thrown:failure.clone(),..Default::default()}};
            let kind=classify(&error);if !matches!(kind,CursorCliErrorKind::Other|CursorCliErrorKind::ContextOverflow) {return Err(failure);}
            let recap=plan.context_recap.clone().or_else(||build_context_recap(None,input.recent,input.policy.max_recap_bytes));
            let composed=recap.as_ref().map(|r|format!("{r}\n\n{}",input.prompt));
            let reinjected=composed.as_ref().is_some_and(|p|p.len()<=input.policy.prompt_ceiling_bytes);
            let prompt=if reinjected {composed.unwrap_or_default()} else {input.prompt.into()};
            let reason=if kind==CursorCliErrorKind::ContextOverflow {"context_overflow"} else {"resume_failed"};
            let cause=if kind==CursorCliErrorKind::ContextOverflow {"context overflow"} else {"resume failure"};
            let chat=previous.as_deref().unwrap_or_default();
            emit(json!({"type":"cursor_chat_restarted","previousChatId":chat,"reason":reason,"message":format!("Cursor chat '{chat}' could not continue after a {cause}; a fresh chat was started{}.",if reinjected {" and senpi's recent context was re-injected"} else {""})}));
            attempt=SessionAttempt {prompt,resume_chat_id:None};fell_back=true;
        }
    }
}
#[derive(Clone,Default)]
pub struct SessionRouter {
    records:std::sync::Arc<std::sync::Mutex<BTreeMap<String,SessionRecord>>>,
    turn_locks:std::sync::Arc<std::sync::Mutex<BTreeMap<String,std::sync::Arc<tokio::sync::Mutex<()>>>>>,
}
impl SessionRouter {
    pub fn get_record(&self,session:&str)->Option<SessionRecord> { self.records.lock().expect("router records").get(session).cloned() }
    pub fn clear(&mut self,session:&str) { self.records.lock().expect("router records").remove(session); }
    pub fn observe_init(&mut self,session:&str,account:&str,chat:&str,model:&str,at:i64) {
        self.records.lock().expect("router records").insert(session.into(),SessionRecord {account_name:account.into(),chat_id:chat.into(),last_model:model.into(),last_used_at:at});
    }
    pub fn plan_turn(&self,session:&str,account:&str,prompt:&str,model:Option<&str>,recent:&[RecapExchange],policy:&SessionPolicy)->Result<TurnPlan,TransportError> {
        let bound=self.get_record(session).filter(|b|policy.resume&&b.account_name==account);
        let model_switch=bound.as_ref().is_some_and(|b|Some(b.last_model.as_str())!=model);
        let mut recap=if model_switch&&policy.recap_on_model_switch { build_context_recap(model,recent,policy.max_recap_bytes) } else { None };
        if prompt.len()>policy.prompt_ceiling_bytes { return Err(TransportError::PromptTooLarge {actual_bytes:prompt.len(),limit_bytes:policy.prompt_ceiling_bytes}); }
        let composed=recap.as_ref().map(|r|format!("{r}\n\n{prompt}"));
        let dropped=composed.as_ref().is_some_and(|p|p.len()>policy.prompt_ceiling_bytes);
        if dropped { recap=None; }
        Ok(TurnPlan {resume_chat_id:bound.map(|b|b.chat_id.clone()),prompt:if dropped {prompt.into()} else {composed.unwrap_or_else(||prompt.into())},context_recap:recap,model_switch,recap_dropped_for_ceiling:dropped})
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn retry_once_before_text_and_record_init() {
        use serde_json::json;
        let mut router=SessionRouter::default();router.observe_init("s","a","old","m",1);
        let policy=SessionPolicy::default();let mut attempts=Vec::new();let mut output=Vec::new();
        router.run_turn(TurnInput {session:"s",account:"a",prompt:"next",model:Some("m"),recent:&[],policy:&policy},|attempt| {
            attempts.push(attempt);let (sender,receiver)=tokio::sync::mpsc::unbounded_channel();
            if attempts.len()==1 {sender.send(Err(json!({"stderr":"session not found"}))).expect("failure");}
            else {sender.send(Ok(json!({"type":"system","subtype":"init","session_id":"new","model":"m"}))).expect("init");sender.send(Ok(json!({"type":"text_delta","delta":"hello"}))).expect("text");}
            async move {Ok(receiver)}
        },||2,|_|crate::errors::CursorCliErrorKind::Other,|e|output.push(e)).await.expect("retry succeeds");
        assert_eq!(attempts.len(),2);assert_eq!(attempts[0].resume_chat_id.as_deref(),Some("old"));assert!(attempts[1].resume_chat_id.is_none());
        assert_eq!(output[0]["type"],"cursor_chat_restarted");assert_eq!(router.get_record("s").expect("binding").chat_id,"new");
    }
    #[tokio::test]
    async fn abort_and_post_text_failure_never_retry() {
        use serde_json::json;
        for after_text in [false,true] {
            let mut router=SessionRouter::default();router.observe_init("s","a","old","m",1);let policy=SessionPolicy::default();let mut count=0;
            let error=if after_text {json!({"stderr":"session not found"})} else {json!({"kind":"aborted"})};
            let result=router.run_turn(TurnInput {session:"s",account:"a",prompt:"next",model:Some("m"),recent:&[],policy:&policy},|_| {
                count+=1;let (sender,receiver)=tokio::sync::mpsc::unbounded_channel();if after_text {sender.send(Ok(json!({"type":"text_delta","delta":"hello"}))).expect("text");}sender.send(Err(error.clone())).expect("error");async move {Ok(receiver)}
            },||2,|_|crate::errors::CursorCliErrorKind::Other,|_|{}).await;
            assert_eq!(result,Err(error));assert_eq!(count,1);assert_eq!(router.get_record("s").expect("binding").chat_id,"old");
        }
    }
    fn exchanges()->Vec<RecapExchange> { (0..40).map(|i|RecapExchange {role:ExchangeRole::User,text:format!("hist-{i}: {}","한".repeat(1024))}).collect() }
    #[test]
    fn recap_keeps_newest_and_truncates_utf8() {
        let recap=build_context_recap(Some("b"),&exchanges(),8192).expect("recap");
        assert!(recap.len()<=8192);assert!(recap.contains("hist-39"));assert!(!recap.contains("hist-0"));assert!(recap.ends_with(CONTEXT_RECAP_END));
        let huge=[RecapExchange {role:ExchangeRole::User,text:"한".repeat(64000)}];assert!(build_context_recap(None,&huge,8192).expect("huge recap").len()<=8192);
        assert!(build_context_recap(None,&[],8192).is_none());assert!(build_context_recap(None,&huge,1).is_none());
    }
    #[test]
    fn sticky_account_and_model_switch() {
        let mut router=SessionRouter::default();let policy=SessionPolicy::default();
        assert!(router.plan_turn("s","a","first",Some("m"),&[],&policy).expect("fresh").resume_chat_id.is_none());
        router.observe_init("s","a","chat","m",1);
        let plan=router.plan_turn("s","a","next",Some("n"),&exchanges(),&policy).expect("switch");assert_eq!(plan.resume_chat_id.as_deref(),Some("chat"));assert!(plan.context_recap.is_some());
        assert!(router.plan_turn("s","other","next",Some("n"),&[],&policy).expect("different account").resume_chat_id.is_none());
        router.observe_init("s","a","new-chat","n",2);assert!(!router.plan_turn("s","a","next",Some("n"),&exchanges(),&policy).expect("settled").model_switch);
    }
    #[test]
    fn drop_recap_but_reject_raw_oversize() {
        let mut router=SessionRouter::default();router.observe_init("s","a","chat","m",1);
        let policy=SessionPolicy::default();let raw="p".repeat(129500);
        let plan=router.plan_turn("s","a",&raw,Some("n"),&exchanges(),&policy).expect("drop recap");assert_eq!(plan.prompt,raw);assert!(plan.recap_dropped_for_ceiling);
        assert!(router.plan_turn("s","a",&"q".repeat(130001),Some("n"),&[],&policy).is_err());
    }
}
