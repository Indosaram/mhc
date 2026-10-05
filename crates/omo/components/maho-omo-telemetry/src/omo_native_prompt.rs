pub fn occurrence_bucket(count:u64)->&'static str {match count {0|1=>"1",2=>"2",3..=5=>"3_5",_=>"6_plus"}}
pub fn length_bucket(length:usize)->&'static str {match length {0..100=>"lt_100",100..500=>"100_500",500..2000=>"500_2000",_=>"gte_2000"}}
pub fn ordinal_bucket(ordinal:u64)->&'static str {match ordinal {1=>"1",0|2|3=>"2_3",4..=10=>"4_10",11..=25=>"11_25",_=>"26_plus"}}
pub fn queue_mode(disposition:Option<&str>,streaming:Option<&str>)->&'static str {match (disposition,streaming) {(Some("started"),_)=>"immediate",(Some("queued"),Some("followUp"))=>"follow_up",(Some("queued"),Some("steer"))=>"steer",_=>"other"}}
use std::{collections::{HashMap,HashSet},sync::{Arc,Mutex}};
use maho_ext_api::{EventKind,EventResult,ExtensionApi,ExtensionEvent,InputSource,InputDisposition,StreamingBehavior};
use maho_omo_ultrawork::{UltraworkClassification,arming_snapshot,classify_ultrawork_input};
use serde_json::{Value,json};
struct PendingPrompt {classification:UltraworkClassification,source:InputSource,ordinal:u64,length:usize,hash:String,streaming:Option<StreamingBehavior>}
#[derive(Default)]
struct PromptState {pending:indexmap::IndexMap<String,PendingPrompt>,completed:HashSet<String>,ordinals:HashMap<String,u64>}
fn properties(prompt:PendingPrompt,disposition:Option<InputDisposition>)->Value {
    let c=prompt.classification;
    let source=match prompt.source {InputSource::Interactive=>"interactive",InputSource::Rpc=>"rpc",InputSource::Extension=>"extension"};
    let disposition=disposition.map(|d|match d {InputDisposition::Started=>"started",InputDisposition::Queued=>"queued",InputDisposition::Handled=>"handled",InputDisposition::Rejected=>"rejected"});
    let streaming=prompt.streaming.map(|s|match s {StreamingBehavior::Steer=>"steer",StreamingBehavior::FollowUp=>"followUp"});
    json!({"$session_id":prompt.hash,"input_source":source,"invocation_stage":c.stage,"is_effective_ultrawork_invocation":c.effective,"is_real_user_prompt":prompt.source!=InputSource::Extension,"is_turn_start":disposition==Some("started"),"keyword_any":c.matched_ulw||c.matched_ultrawork,"keyword_occurrence_bucket":occurrence_bucket(u64::try_from(c.occurrence_count).unwrap_or(u64::MAX)),"keyword_ultrawork_full":c.matched_ultrawork,"keyword_ulw_abbrev":c.matched_ulw,"keyword_variant":match(c.matched_ulw,c.matched_ultrawork){(true,true)=>"both",(true,false)=>"ulw",(false,true)=>"ultrawork",_=>"none"},"prompt_length_bucket":length_bucket(prompt.length),"queue_mode":queue_mode(disposition,streaming),"real_prompt_ordinal_bucket":ordinal_bucket(prompt.ordinal),"suppression_reason":c.suppression_reason})
}
pub fn register_omo_native_prompt_telemetry(api:&mut ExtensionApi,hash:Arc<dyn Fn(&str)->String+Send+Sync>,capture:crate::omo_native_parallel_summary::SummaryCapture) {
    let state=Arc::new(Mutex::new(PromptState::default()));
    let inputs=Arc::clone(&state);
    api.on(EventKind::Input,Arc::new(move |event,ctx| {
        if let ExtensionEvent::Input(input)=event && !input.input_id.is_empty() {
            let mut state=inputs.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if !state.completed.contains(&input.input_id) && !state.pending.contains_key(&input.input_id) {
                let id=ctx.session_manager.session_id();let id=if id.is_empty(){"anonymous"}else{id};
                let ordinal=state.ordinals.entry(id.into()).or_default();
                if input.source!=InputSource::Extension {*ordinal+=1;}
                let ordinal=(*ordinal).max(1);
                state.pending.insert(input.input_id.clone(),PendingPrompt {classification:classify_ultrawork_input(&input.text,input.source,arming_snapshot(Some(id))),source:input.source,ordinal,length:input.text.encode_utf16().count(),hash:hash(id),streaming:input.streaming_behavior});
            }
        }
        Box::pin(async {Ok(EventResult::Input(maho_ext_api::InputEventResult::Continue))})
    }));
    let dispositions=Arc::clone(&state);let captured=Arc::clone(&capture);
    api.on(EventKind::InputDisposition,Arc::new(move |event,_| {
        if let ExtensionEvent::InputDisposition {input_id,disposition}=event {
            let prompt={let mut state=dispositions.lock().unwrap_or_else(std::sync::PoisonError::into_inner);let prompt=state.pending.shift_remove(input_id);if prompt.is_some(){state.completed.insert(input_id.clone());}prompt};
            if let Some(prompt)=prompt {captured("prompt_submitted",properties(prompt,Some(*disposition)));}
        }
        Box::pin(async {Ok(EventResult::None)})
    }));
    api.on(EventKind::SessionShutdown,Arc::new(move |_,_| {
        let pending={let mut state=state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);let pending=state.pending.drain(..).collect::<Vec<_>>();for (id,_) in &pending {state.completed.insert(id.clone());}pending};
        for (_,prompt) in pending {capture("prompt_submitted",properties(prompt,None));}
        Box::pin(async {Ok(EventResult::None)})
    }));
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry_test_support::*;
    fn harness()->(ExtensionApi,Arc<Mutex<Vec<Value>>>) {let captures=Arc::new(Mutex::new(Vec::new()));let captured=Arc::clone(&captures);let mut api=api();register_omo_native_prompt_telemetry(&mut api,Arc::new(|id|format!("hash:{id}")),Arc::new(move |name,p| {assert_eq!(name,"prompt_submitted");captured.lock().unwrap().push(p);}));(api,captures)}
    async fn input(api:&ExtensionApi,home:&std::path::Path,id:&str,text:&str,session:&str,source:InputSource,streaming_behavior:Option<StreamingBehavior>) {dispatch(api,ExtensionEvent::Input(maho_ext_api::InputEvent {input_id:id.into(),text:text.into(),source,images:None,streaming_behavior}),&context(home,session)).await;}
    async fn disposition(api:&ExtensionApi,home:&std::path::Path,id:&str,value:InputDisposition) {dispatch(api,ExtensionEvent::InputDisposition {input_id:id.into(),disposition:value},&context(home,"unused")).await;}
    #[tokio::test] async fn classifications_track_owner_arming_and_compact() {
        let t=tempfile::tempdir().unwrap();let (api,captures)=harness();let session="prompt-classification-46";
        input(&api,t.path(),"first","ulw ulw ulw plan",session,InputSource::Interactive,None).await;disposition(&api,t.path(),"first",InputDisposition::Started).await;
        maho_omo_ultrawork::shared_session_arming().lock().unwrap().mark_armed(Some(session));
        input(&api,t.path(),"second","ulw continue",session,InputSource::Interactive,None).await;disposition(&api,t.path(),"second",InputDisposition::Started).await;
        maho_omo_ultrawork::shared_session_arming().lock().unwrap().rearm_on_compact(Some(session));
        input(&api,t.path(),"third","ulw after compact",session,InputSource::Interactive,None).await;disposition(&api,t.path(),"third",InputDisposition::Started).await;
        let captures=captures.lock().unwrap();assert_eq!(captures.iter().map(|p|(p["invocation_stage"].as_str().unwrap(),p["keyword_occurrence_bucket"].as_str().unwrap())).collect::<Vec<_>>(),vec![("first_arm","3_5"),("remention","1"),("post_compact_rearm","1")]);assert!(captures.iter().all(|p|p["is_effective_ultrawork_invocation"]==true && p["keyword_variant"]=="ulw"));
    }
    #[tokio::test] async fn suppression_and_extension_sources() {let t=tempfile::tempdir().unwrap();let (api,captures)=harness();input(&api,t.path(),"skill","ulw-plan","suppression-46",InputSource::Interactive,None).await;disposition(&api,t.path(),"skill",InputDisposition::Started).await;input(&api,t.path(),"extension","ultrawork internal","suppression-46",InputSource::Extension,None).await;disposition(&api,t.path(),"extension",InputDisposition::Handled).await;let captures=captures.lock().unwrap();assert_eq!(captures[0]["keyword_any"],false);assert_eq!(captures[0]["suppression_reason"],"no_keyword");assert_eq!(captures[1]["is_real_user_prompt"],false);assert_eq!(captures[1]["suppression_reason"],"extension_source");assert_eq!(captures[1]["real_prompt_ordinal_bucket"],"1");}
    #[tokio::test] async fn queued_duplicate_dispositions_emit_once() {let t=tempfile::tempdir().unwrap();let (api,captures)=harness();input(&api,t.path(),"queued","please continue","queue-46",InputSource::Interactive,Some(StreamingBehavior::Steer)).await;disposition(&api,t.path(),"queued",InputDisposition::Queued).await;disposition(&api,t.path(),"queued",InputDisposition::Started).await;disposition(&api,t.path(),"unknown",InputDisposition::Rejected).await;let captures=captures.lock().unwrap();assert_eq!(captures.len(),1);assert_eq!(captures[0]["queue_mode"],"steer");assert_eq!(captures[0]["is_turn_start"],false);}
    #[tokio::test] async fn snapshot_precedes_mutating_handler() {let t=tempfile::tempdir().unwrap();let (api,captures)=harness();input(&api,t.path(),"ordered","ulw ship it","ordered-46",InputSource::Interactive,None).await;maho_omo_ultrawork::shared_session_arming().lock().unwrap().mark_armed(Some("ordered-46"));disposition(&api,t.path(),"ordered",InputDisposition::Started).await;assert_eq!(captures.lock().unwrap()[0]["invocation_stage"],"first_arm");}
    #[tokio::test] async fn every_disposition_length_and_per_session_ordinal() {let t=tempfile::tempdir().unwrap();let (api,captures)=harness();for (id,len,session,d,streaming) in [("a1",1,"sizes-a-46",InputDisposition::Started,None),("a2",100,"sizes-a-46",InputDisposition::Queued,Some(StreamingBehavior::FollowUp)),("a3",500,"sizes-a-46",InputDisposition::Handled,None),("a4",2000,"sizes-a-46",InputDisposition::Rejected,None),("b1",1,"sizes-b-46",InputDisposition::Started,None)] {input(&api,t.path(),id,&"x".repeat(len),session,InputSource::Interactive,streaming).await;disposition(&api,t.path(),id,d).await;}let captures=captures.lock().unwrap();assert_eq!(captures.iter().map(|p|(p["queue_mode"].as_str().unwrap(),p["prompt_length_bucket"].as_str().unwrap(),p["real_prompt_ordinal_bucket"].as_str().unwrap())).collect::<Vec<_>>(),vec![("immediate","lt_100","1"),("follow_up","100_500","2_3"),("other","500_2000","2_3"),("other","gte_2000","4_10"),("immediate","lt_100","1")]);}
    #[tokio::test] async fn shutdown_flushes_before_late_disposition() {let t=tempfile::tempdir().unwrap();let (api,captures)=harness();input(&api,t.path(),"cancelled","ulw resume later","shutdown-46",InputSource::Interactive,None).await;dispatch(&api,ExtensionEvent::SessionShutdown(maho_ext_api::SessionShutdownEvent {reason:maho_ext_api::SessionReason::Quit,target_session_file:None,signal:None}),&context(t.path(),"shutdown-46")).await;disposition(&api,t.path(),"cancelled",InputDisposition::Started).await;let captures=captures.lock().unwrap();assert_eq!(captures.len(),1);assert_eq!(captures[0]["queue_mode"],"other");assert_eq!(captures[0]["is_turn_start"],false);}
    #[tokio::test] async fn adversarial_input_privacy_and_utf16_length() {let t=tempfile::tempdir().unwrap();let (api,captures)=harness();for (index,text) in [String::new(),"x".repeat(100_000),"[](){}.*+?^$|\\".into(),"안녕하세요 ulw 부탁해".into(),"😀".repeat(50)].iter().enumerate() {let id=format!("edge-{index}");input(&api,t.path(),&id,text,"edge-46",InputSource::Rpc,None).await;disposition(&api,t.path(),&id,InputDisposition::Started).await;}input(&api,t.path(),"","ulw","edge-46",InputSource::Interactive,None).await;disposition(&api,t.path(),"missing",InputDisposition::Started).await;let captures=captures.lock().unwrap();assert_eq!(captures.len(),5);assert_eq!(captures[4]["prompt_length_bucket"],"100_500");for capture in captures.iter() {assert_eq!(capture.as_object().unwrap().len(),15);assert!(capture.get("text").is_none());assert!(!capture.to_string().contains("100000"));}}
    #[test] fn occurrences() {for (n,want) in [(0,"1"),(1,"1"),(2,"2"),(3,"3_5"),(5,"3_5"),(6,"6_plus")] {assert_eq!(occurrence_bucket(n),want);}}
    #[test] fn lengths() {for (n,want) in [(99,"lt_100"),(100,"100_500"),(499,"100_500"),(500,"500_2000"),(1999,"500_2000"),(2000,"gte_2000")] {assert_eq!(length_bucket(n),want);}}
    #[test] fn ordinals() {for (n,want) in [(1,"1"),(2,"2_3"),(3,"2_3"),(4,"4_10"),(10,"4_10"),(11,"11_25"),(25,"11_25"),(26,"26_plus")] {assert_eq!(ordinal_bucket(n),want);}}
    #[test] fn queues() {assert_eq!(queue_mode(Some("started"),None),"immediate");assert_eq!(queue_mode(Some("queued"),Some("steer")),"steer");assert_eq!(queue_mode(Some("queued"),Some("followUp")),"follow_up");assert_eq!(queue_mode(Some("rejected"),Some("steer")),"other");}
}
