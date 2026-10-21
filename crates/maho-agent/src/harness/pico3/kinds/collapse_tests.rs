use super::*;

fn history(id:Id,message:serde_json::Value)->Entry {
    serde_json::from_value(serde_json::json!({"id":id,"conversationId":1,"kind":"test","model":[message]})).expect("entry")
}
fn user_entry(id:Id)->Entry {history(id,serde_json::json!({"role":"user","content":"a".repeat(80),"timestamp":0}))}
fn assistant_entry(id:Id)->Entry {history(id,serde_json::json!({"role":"assistant","content":[{"type":"toolCall","id":"c","name":"x","arguments":{}}],"api":"faux","provider":"faux","model":"one","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"stopReason":"toolUse","timestamp":0}))}
fn tool_entry(id:Id)->Entry {history(id,serde_json::json!({"role":"toolResult","toolCallId":"c","toolName":"x","content":[{"type":"text","text":"a".repeat(80)}],"isError":false,"timestamp":0}))}
fn base()->Base {Base {expected_head:None,instructions:None,model:ModelRef {provider:"faux".into(),model_id:"one".into()},thinking_level:ThinkingLevel::Off,retry:RetryPolicy {enabled:true,max_retries:3,base_delay_ms:2000,max_agent_delay_ms:None},attempt:1}}

#[test]
fn choose_through_keeps_entire_newest_exchange_when_it_exceeds_budget() {
    let entries=vec![user_entry(1),assistant_entry(2),tool_entry(3),tool_entry(4)];
    let through=choose_through(&entries,0).expect("choose");
    assert_eq!(through,Some(1));
}
#[test]
fn choose_through_collapses_prior_exchange_only_at_last_result() {
    let entries=vec![assistant_entry(1),tool_entry(2),tool_entry(3),user_entry(4)];
    let through=choose_through(&entries,0).expect("choose");
    assert_eq!(through,Some(3));
}
#[test]
fn choose_through_returns_none_when_every_exchange_fits() {
    let entries=vec![user_entry(1),assistant_entry(2),tool_entry(3)];
    let through=choose_through(&entries,u64::MAX).expect("choose");
    assert_eq!(through,None);
}
#[test]
fn choose_through_never_discards_only_exchange() {
    let entries=vec![assistant_entry(1),tool_entry(2)];
    let through=choose_through(&entries,0).expect("choose");
    assert_eq!(through,None);
}
#[test]
fn prepared_summary_round_trip_retains_expected_head_and_attempt() {
    let mut base=base();base.expected_head=Some(7);base.attempt=3;
    let saved=json(&CollapseCheckpoint::Prepared {base,summary:"".into()}).expect("save");
    let restored:CollapseCheckpoint=decode(&saved).expect("restore");
    match restored {CollapseCheckpoint::Prepared {base,summary}=> {assert_eq!(base.expected_head,Some(7));assert_eq!(base.attempt,3);assert!(summary.is_empty());},_=>panic!("prepared")}
}
#[test]
fn interrupted_summary_obeys_disabled_retry_policy() {
    let mut base=base();base.retry.enabled=false;
    let step=after_failure(base,None,"interrupted".into(),100);
    assert!(matches!(step,Step::Done(_)));
}
#[tokio::test]
async fn prepared_summary_commits_self_head_when_nothing_is_retained() {
    let (session,_)=super::super::generation::tests::fixture().await;
    let current=super::super::generation::tests::current("pi.collapse");
    let Step::Done(done)=prepared(base(),"summary".into(),2,100) else {panic!("terminal")};
    let result=session.commit(Invoker::Kernel {conversation_id:Some(1)},move |tx,ctx|Box::pin(async move {done(tx,&current,&ctx).await}),&crate::harness::context::BACKGROUND_CONTEXT,super::super::super::session::CommitOptions {closing:true,..Default::default()}).await.expect("summary commit");
    assert!(matches!(result.value,Completion::Completed {..}));
    let summary=&result.changes.entries[0];assert_eq!(summary.kind,"pi.summary");assert_eq!(summary.head,Some(summary.id));
}
#[tokio::test]
async fn prepared_summary_preserves_first_entry_after_through_as_head() {
    let (session,_)=super::super::generation::tests::fixture().await;
    let current=super::super::generation::tests::current("pi.collapse");
    let Step::Done(done)=prepared(base(),"summary".into(),1,100) else {panic!("terminal")};
    let result=session.commit(Invoker::Kernel {conversation_id:Some(1)},move |tx,ctx|Box::pin(async move {done(tx,&current,&ctx).await}),&crate::harness::context::BACKGROUND_CONTEXT,super::super::super::session::CommitOptions {closing:true,..Default::default()}).await.expect("summary commit");
    assert_eq!(result.changes.entries[0].head,Some(2));
}
#[tokio::test]
async fn stale_prepared_summary_writes_no_entry() {
    let (session,_)=super::super::generation::tests::fixture().await;
    let current=super::super::generation::tests::current("pi.collapse");let mut base=base();base.expected_head=Some(90);
    let Step::Done(done)=prepared(base,"summary".into(),2,100) else {panic!("terminal")};
    let result=session.commit(Invoker::Kernel {conversation_id:Some(1)},move |tx,ctx|Box::pin(async move {done(tx,&current,&ctx).await}),&crate::harness::context::BACKGROUND_CONTEXT,super::super::super::session::CommitOptions {closing:true,..Default::default()}).await.expect("stale commit");
    assert!(result.changes.entries.is_empty());assert!(matches!(result.value,Completion::Failed {failure:CollapseFailureData {reason:CompactionFailure::Stale,..}}));
}
