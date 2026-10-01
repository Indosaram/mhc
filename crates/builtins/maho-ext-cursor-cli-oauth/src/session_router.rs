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
pub struct SessionRecord { pub account_name:String,pub chat_id:String,pub last_model:String,pub last_used_at:i64 }
pub struct SessionPolicy { pub resume:bool,pub recap_on_model_switch:bool,pub max_recap_bytes:usize,pub prompt_ceiling_bytes:usize }
impl Default for SessionPolicy {
    fn default()->Self { Self {resume:true,recap_on_model_switch:true,max_recap_bytes:CONTEXT_RECAP_MAX_BYTES,prompt_ceiling_bytes:MAX_CURSOR_CLI_PROMPT_BYTES} }
}
pub struct TurnPlan { pub resume_chat_id:Option<String>,pub prompt:String,pub context_recap:Option<String>,pub model_switch:bool,pub recap_dropped_for_ceiling:bool }
#[derive(Default)]
pub struct SessionRouter { records:BTreeMap<String,SessionRecord> }
impl SessionRouter {
    pub fn get_record(&self,session:&str)->Option<&SessionRecord> { self.records.get(session) }
    pub fn clear(&mut self,session:&str) { self.records.remove(session); }
    pub fn observe_init(&mut self,session:&str,account:&str,chat:&str,model:&str,at:i64) {
        self.records.insert(session.into(),SessionRecord {account_name:account.into(),chat_id:chat.into(),last_model:model.into(),last_used_at:at});
    }
    pub fn plan_turn(&self,session:&str,account:&str,prompt:&str,model:Option<&str>,recent:&[RecapExchange],policy:&SessionPolicy)->Result<TurnPlan,TransportError> {
        let bound=self.records.get(session).filter(|b|policy.resume&&b.account_name==account);
        let model_switch=bound.is_some_and(|b|Some(b.last_model.as_str())!=model);
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
