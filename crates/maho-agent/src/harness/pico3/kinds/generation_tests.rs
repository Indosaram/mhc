use super::*;

fn policy()->RetryPolicy {RetryPolicy {enabled:true,max_retries:3,base_delay_ms:2000,max_agent_delay_ms:Some(60000)}}
fn prep()->Prep {
    Prep {cutoff:12,system:Some(11),model:ModelRef {provider:"faux".into(),model_id:"one".into()},thinking_level:ThinkingLevel::Off,tools:vec!["read".into()],retry:policy(),attempt:2}
}

#[test]
fn interrupted_attempt_uses_durable_exponential_deadline() {
    let retry=policy();
    let decision=retry_decision(&retry,3,None,100);
    assert_eq!(decision,RetryDecision::Retry {until_ms:8100});
}
#[test]
fn retry_exhaustion_counts_initial_request_separately() {
    let retry=policy();
    let decision=retry_decision(&retry,4,None,100);
    assert_eq!(decision,RetryDecision::Fail {exhausted:true});
}
#[test]
fn disabled_retry_is_provider_failure_not_exhaustion() {
    let mut retry=policy();retry.enabled=false;
    let decision=retry_decision(&retry,9,None,0);
    assert_eq!(decision,RetryDecision::Fail {exhausted:false});
}
#[test]
fn retry_delay_saturates_before_applying_agent_cap() {
    let mut retry=policy();retry.max_retries=u32::MAX;retry.base_delay_ms=u64::MAX;
    let decision=retry_decision(&retry,100,None,17);
    assert_eq!(decision,RetryDecision::Retry {until_ms:60017});
}
#[test]
fn deferred_checkpoint_round_trip_preserves_cutoff_and_handle() {
    let cp=GenerationCheckpoint::Deferred {prep:prep(),handle:Stored::from_json(json(&serde_json::json!({"provider":"faux","modelId":"one","api":"faux","id":"request-2","pollAfterMs":123})).expect("handle")),poll_at:500};
    let restored:GenerationCheckpoint=decode(&json(&cp).expect("checkpoint")).expect("restore");
    match restored {GenerationCheckpoint::Deferred {prep,handle,poll_at}=> {
        assert_eq!(prep.cutoff,12);assert_eq!(prep.system,Some(11));assert_eq!(prep.attempt,2);
        assert_eq!(handle.decode().expect("handle").id,"request-2");assert_eq!(poll_at,500);
    },_=>panic!("wrong phase")}
}

pub(crate) async fn fixture()->(std::rc::Rc<super::super::super::session::Session>,std::rc::Rc<super::super::super::memory::MemoryStorage>) {
    use std::{rc::Rc,cell::RefCell,collections::BTreeMap};
    let storage=Rc::new(super::super::super::memory::MemoryStorage::new());
    let writes:Vec<Write>=serde_json::from_value(serde_json::json!([
        {"type":"conversation","conversation":{"id":1}},
        {"type":"entry","entry":{"id":2,"conversationId":1,"kind":"pi.user","model":[{"role":"user","content":"go","timestamp":0}]}},
        {"type":"input","input":{"id":3,"conversationId":1,"status":"placed","entry":2}},
        {"type":"doc","ref":{"doc":"rewindable","conversationId":1},"ops":[["r",{"model":{"provider":"faux","modelId":"one"},"thinkingLevel":"off","selectedTools":[],"profile":"default","threshold":0,"keepRecent":20000,"plugins":{}}]]},
        {"type":"doc","ref":{"doc":"sticky","conversationId":1},"ops":[["r",{"retry":{"enabled":true,"maxRetries":3,"baseDelayMs":2000},"steeringMode":"all","followUpMode":"all","inbox":[],"turn":{"tools":[]},"tasks":{},"plugins":{}}]]}
    ])).expect("writes");
    let ctx=&crate::harness::context::BACKGROUND_CONTEXT;
    storage.commit(&writes,ctx).await.expect("seed");
    let session=super::super::super::session::Session::new(storage.clone(),Rc::new(RefCell::new(BTreeMap::new())),Rc::new(RefCell::new(BTreeMap::new())),Rc::new(||100)).expect("session");
    session.load(ctx).await.expect("load");(session,storage)
}
pub(crate) fn current(kind:&str)->Task {
    serde_json::from_value(serde_json::json!({"id":9,"conversationId":1,"kind":kind,"input":null,"status":"running","after":[],"owns":[]})).expect("task")
}
#[tokio::test]
async fn no_model_failure_settles_input_in_same_commit() {
    let (session,storage)=fixture().await;let current=current("pi.generation");
    let Step::Done(done)=fail_step(vec![3],GenerationFailure::NoModel,"no model configured".into()) else {panic!("terminal closure")};
    let result=session.commit(Invoker::Kernel {conversation_id:Some(1)},move |tx,ctx|Box::pin(async move {done(tx,&current,&ctx).await}),&crate::harness::context::BACKGROUND_CONTEXT,super::super::super::session::CommitOptions {closing:true,..Default::default()}).await.expect("close");
    assert!(matches!(result.value,Completion::Failed {failure:GenerationFailureData {reason:GenerationFailure::NoModel,..}}));
    assert_eq!(result.changes.inputs.len(),1);
    let input=storage.input(3,&crate::harness::context::BACKGROUND_CONTEXT).await.expect("input").expect("present");
    assert_eq!(input.status,InputStatus::Unanswered);assert_eq!(input.reason,Some(InputReason::Failed));
}
#[tokio::test]
async fn recovered_requesting_records_usage_and_retry_checkpoint_atomically() {
    let (session,_)=fixture().await;let current=current("pi.generation");
    let Step::Build(build)=retry_step(vec![3],prep(),None,"interrupted".into(),100).expect("retry") else {panic!("builder")};
    let result=session.commit(Invoker::Kernel {conversation_id:Some(1)},move |tx,ctx|Box::pin(async move {build(tx,&current,&ctx).await}),&crate::harness::context::BACKGROUND_CONTEXT,Default::default()).await.expect("retry commit");
    assert_eq!(result.changes.entries.len(),1);assert_eq!(result.changes.entries[0].kind,"pi.usage");
    match result.value {Transition::Checkpoint(GenerationCheckpoint::Retrying {prep,until_ms,..})=> {assert_eq!(prep.cutoff,12);assert_eq!(prep.attempt,2);assert_eq!(until_ms,4100);},_=>panic!("retry checkpoint")}
}
#[tokio::test]
async fn overflow_without_collapsible_history_settles_input_and_writes_notice() {
    let (session,storage)=fixture().await;let current=current("pi.generation");
    let Step::Done(done)=overflow(vec![3],100) else {panic!("terminal")};
    let result=session.commit(Invoker::Kernel {conversation_id:Some(1)},move |tx,ctx|Box::pin(async move {done(tx,&current,&ctx).await}),&crate::harness::context::BACKGROUND_CONTEXT,super::super::super::session::CommitOptions {closing:true,..Default::default()}).await.expect("close");
    assert!(result.changes.entries.iter().any(|e|e.kind=="pi.notice"));
    assert_eq!(storage.input(3,&crate::harness::context::BACKGROUND_CONTEXT).await.expect("input").expect("present").reason,Some(InputReason::Failed));
}
