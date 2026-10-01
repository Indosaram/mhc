use std::sync::{Arc,Mutex};
use maho_ai::types::BoxFuture;
use maho_agent::harness::context::{Context,BACKGROUND_CONTEXT};
use maho_agent::harness::events::{HarnessEvent,HarnessEventBus};
use maho_agent::harness::runtime::types::{RuntimeLane,LaneState};
use maho_agent::harness::runtime::restore::restore_lane;
use maho_agent::harness::runtime::transcript::watch_lane;
use maho_agent::harness::session::memory::MemoryStorage;
use maho_agent::harness::session::session::{StorageBackedSession,StorageBackedSessionOptions};
use maho_agent::harness::session::types::*;
use maho_agent::harness::session::values::*;
use maho_agent::harness::session::commit::insert_entry;
use serde_json::{Value,json};

fn tool_state()->Value {
    let mut state=scope("tools");
    state["batch"]=json!({"assistantEntryId":"assistant","configuration":configuration(),"turnId":"turn","calls":[{"status":"outcome_ready","sourceIndex":0,"resultEntryId":"result","terminate":false}]});
    state
}
fn assistant_entry()->NewEntry {
    let message=maho_ai::providers::faux::faux_assistant_message(maho_ai::providers::faux::faux_tool_call("read",serde_json::from_value(json!({"path":"file"})).expect("args"),Some("call")),Default::default());
    NewEntry::message("assistant",None,maho_ai::types::Message::Assistant(Box::new(message)).into())
}

#[tokio::test]
async fn faults_missing_or_mismatched_staged_results() {
    for mismatched in [false,true] {
        let lane=fixture().await;
        let mut writes=vec![insert_entry(assistant_entry()),Write::Value(set_value(&branch_tip("main"),json!("assistant")))];
        if mismatched {writes.push(Write::Value(set_value(&pending_entry("result"),json!({"type":"message","payload":{"role":"toolResult","toolCallId":"other-call","toolName":"read","content":[],"isError":false,"timestamp":2}}))));}
        install(&lane,tool_state(),writes).await;
        let result=watch_lane(lane,HarnessEventBus::new(),&BACKGROUND_CONTEXT,false).await;
        let error=match result {Ok(_)=>panic!("corrupt result accepted"),Err(error)=>error};
        assert!(error.message.contains(if mismatched {"mismatched staged result"}else{"missing its staged result"}));
    }
}

#[tokio::test]
async fn reduces_frames_and_projects_running_settled_tools_with_full_indexes() {
    let lane=fixture().await;
    let partial=maho_ai::providers::faux::faux_assistant_message("",maho_ai::providers::faux::FauxAssistantMessageOptions{stop_reason:Some(maho_ai::types::StopReason::Pending),..Default::default()});
    install(&lane,effect(),vec![Write::List(append_list(&pending_assistant_frames("op","response"),json!({"type":"start","partial":partial}))),Write::List(append_list(&pending_assistant_frames("op","response"),json!({"type":"text_start","contentIndex":0,"content":{"type":"text","text":""}}))),Write::List(append_list(&pending_assistant_frames("op","response"),json!({"type":"text_delta","contentIndex":0,"delta":"partial"})))]).await;
    let watch=watch_lane(lane,HarnessEventBus::new(),&BACKGROUND_CONTEXT,false).await.expect("frame watch");
    assert_eq!(watch.snapshot().expect("snapshot")["operation"]["streamingMessage"]["content"],json!([{"type":"text","text":"partial"}]));watch.unsubscribe();
    let lane=fixture().await;
    let mut state=tool_state();
    state["batch"]["calls"]=json!([{"status":"completed","sourceIndex":1,"resultEntryId":"completed","terminate":false},{"status":"effect_pending","sourceIndex":2,"resultEntryId":"result","replay":"safe"},{"status":"effect_pending","sourceIndex":3,"resultEntryId":"without-checkpoint","replay":"never"},{"status":"outcome_ready","sourceIndex":4,"resultEntryId":"ready","terminate":true},{"status":"outcome_ready","sourceIndex":5,"resultEntryId":"synthetic","terminate":false},{"status":"planned","sourceIndex":6,"resultEntryId":"planned"}]);
    let mut content=vec![maho_ai::providers::faux::faux_text("before")];
    for (id,name,source) in [("call-completed","completed","completed"),("call-running","read","running"),("call-without-checkpoint","write","without-checkpoint"),("call-ready","real","real"),("call-synthetic","missing","synthetic"),("call-planned","planned","planned")] {content.push(maho_ai::providers::faux::faux_tool_call(name,serde_json::from_value(json!({"source":source})).expect("source args"),Some(id)));}
    let assistant=NewEntry::message("assistant",None,maho_ai::types::Message::Assistant(Box::new(maho_ai::providers::faux::faux_assistant_message(content,Default::default()))).into());
    let completed:NewEntry=serde_json::from_value(json!({"id":"completed","parentId":"assistant","type":"message","message":{"role":"toolResult","toolCallId":"call-completed","toolName":"completed","content":[{"type":"text","text":"completed"}],"isError":false,"timestamp":2}})).expect("completed entry");
    let usage=json!({"input":1,"output":2,"cacheRead":3,"cacheWrite":4,"totalTokens":10,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}});
    install(&lane,state,vec![insert_entry(assistant),insert_entry(completed),Write::Value(set_value(&branch_tip("main"),json!("completed"))),Write::Value(set_value(&operation_tool_args("op","turn",2),json!({"path":"file"}))),Write::Value(set_value(&operation_tool_args("op","turn",3),json!({"path":"output"}))),Write::Value(set_value(&operation_tool_args("op","turn",4),json!({"path":"settled"}))),Write::Value(set_value(&pending_tool_output("op","result"),json!({"content":[{"type":"text","text":"partial"}],"details":{"bytes":1}}))),Write::Value(set_value(&pending_entry("ready"),json!({"type":"message","payload":{"role":"toolResult","toolCallId":"call-ready","toolName":"real","content":[{"type":"text","text":"settled"}],"details":{"kind":"real"},"usage":usage,"addedToolNames":["later"],"isError":false,"timestamp":3}}))),Write::Value(set_value(&pending_entry("synthetic"),json!({"type":"message","payload":{"role":"toolResult","toolCallId":"call-synthetic","toolName":"missing","content":[{"type":"text","text":"unavailable"}],"isError":true,"timestamp":4}})))]).await;
    let watch=watch_lane(lane,HarnessEventBus::new(),&BACKGROUND_CONTEXT,false).await.expect("tools watch");let snapshot=watch.snapshot().expect("snapshot");
    assert_eq!(snapshot["transcript"].as_array().expect("transcript").iter().map(|e|e["id"].as_str().expect("id")).collect::<Vec<_>>(),["assistant","completed"]);
    assert_eq!(snapshot["operation"]["runningTools"],json!([
        {"status":"running","toolCallId":"call-running","toolName":"read","args":{"path":"file"},"result":{"content":[{"type":"text","text":"partial"}],"details":{"bytes":1}}},
        {"status":"running","toolCallId":"call-without-checkpoint","toolName":"write","args":{"path":"output"}},
        {"status":"settled","toolCallId":"call-ready","toolName":"real","args":{"path":"settled"},"result":{"content":[{"type":"text","text":"settled"}],"details":{"kind":"real"},"usage":usage,"addedToolNames":["later"],"terminate":true},"isError":false},
        {"status":"settled","toolCallId":"call-synthetic","toolName":"missing","args":{"source":"synthetic"},"result":{"content":[{"type":"text","text":"unavailable"}]},"isError":true}
    ]));watch.unsubscribe();
}

struct Lane {session:StorageBackedSession,state:Mutex<LaneState>}
impl RuntimeLane for Lane {
    fn name(&self)->&str{"main"}
    fn session(&self)->&dyn Session{&self.session}
    fn state(&self)->LaneState{self.state.lock().expect("state lock").clone()}
    fn publish_state(&self,state:LaneState){*self.state.lock().expect("state lock")=state;}
    fn emit<'a>(&'a self,_:Vec<HarnessEvent>,_:&'a Context)->BoxFuture<'a,()>{Box::pin(async {})}
}
async fn commit(lane:&Lane,writes:Vec<Write>){let mutation=lane.session.begin_mutation(&BACKGROUND_CONTEXT).await.expect("mutation");mutation.commit(writes,&BACKGROUND_CONTEXT).await.expect("commit");mutation.end(&BACKGROUND_CONTEXT).await;lane.publish_state(restore_lane(&lane.session,"main",&BACKGROUND_CONTEXT).await.expect("restore"));}
fn configuration()->Value{json!({"model":{"provider":"configured","modelId":"model"},"thinkingLevel":"off","activeToolNames":[]})}
fn scope(at:&str)->Value{json!({"at":at,"control":{"status":"running"},"settings":{"compaction":{"enabled":true,"reserveTokens":1000,"keepRecentTokens":2000},"steeringMode":"all","followUpMode":"all","toolExecution":"parallel"},"latestAssistantEntryId":null})}
fn effect()->Value{let mut s=scope("assistant.effect_pending");s["generationContext"]=json!({"stepId":"step","triggerEntryId":"trigger","configuration":configuration(),"streamOptions":{},"retryPolicy":{"maxAttempts":2,"baseDelayMs":0,"maxAgentDelayMs":30000},"overflowRecoveryUsed":false});s["attempt"]=json!(1);s["responseEntryId"]=json!("response");s["usageId"]=json!("usage");s["intendedOutputLimit"]=json!(100);s["contextWindow"]=json!(1000);s}
async fn fixture()->Arc<Lane>{let session=StorageBackedSession::new(SessionMetadata{id:"watch".into(),created_at:1,storage_version:1,cwd:None,parent_session_id:None,legacy_parent_session_path:None},Arc::new(MemoryStorage::new(Default::default())),StorageBackedSessionOptions::default());let mutation=session.begin_mutation(&BACKGROUND_CONTEXT).await.expect("setup mutation");mutation.commit(vec![Write::Value(set_value(&branch_tip("main"),Value::Null)),Write::Value(set_value(&lane_config("main"),configuration())),Write::Value(set_value(&lane_state("main"),json!({"currentOperationId":null,"lastOperationId":null,"inbox":[]})))],&BACKGROUND_CONTEXT).await.expect("setup");mutation.end(&BACKGROUND_CONTEXT).await;let state=restore_lane(&session,"main",&BACKGROUND_CONTEXT).await.expect("restore");Arc::new(Lane{session,state:Mutex::new(state)})}
async fn install(lane:&Lane,state:Value,mut writes:Vec<Write>){writes.extend([Write::Value(set_value(&operation_meta("op"),json!({"operationId":"op","lane":"main","sourceTipId":null,"startedAt":1,"intent":{"kind":"run","promptEntryIds":[]}}))),Write::Value(set_value(&operation_state("op"),state)),Write::Value(set_value(&lane_state("main"),json!({"currentOperationId":"op","lastOperationId":null,"inbox":[]})))]);commit(lane,writes).await;}
fn user(id:&str,parent:Option<String>)->NewEntry{NewEntry::message(id,parent,maho_ai::types::Message::User(maho_ai::types::UserMessage{content:maho_ai::types::UserContent::Text(id.into()),timestamp:2}).into())}

#[tokio::test]
async fn captures_compaction_bounded_transcript_and_isolates_snapshot(){let lane=fixture().await;let compact:NewEntry=serde_json::from_value(json!({"id":"compact","parentId":"root","type":"compaction","summary":"summary","retainedTail":[],"tokensBefore":10,"fromHook":false})).expect("compaction");let root:NewEntry=serde_json::from_value(json!({"id":"root","parentId":null,"type":"custom","customType":"root"})).expect("root");commit(&lane,vec![insert_entry(root),insert_entry(compact),insert_entry(user("after",Some("compact".into()))),Write::Value(set_value(&branch_tip("main"),json!("after")))]).await;let bus=HarnessEventBus::new();let first=watch_lane(lane.clone(),bus.clone(),&BACKGROUND_CONTEXT,false).await.expect("watch");let mut snapshot=first.snapshot().expect("snapshot");assert_eq!(snapshot["transcript"].as_array().expect("transcript").iter().map(|e|e["id"].as_str().expect("id")).collect::<Vec<_>>(),["compact","after"]);assert_eq!(snapshot["configuration"],configuration());assert_eq!(snapshot["stats"]["messageCount"],1);assert_eq!(snapshot["operation"],Value::Null);assert_eq!(snapshot["queues"],json!([]));assert_eq!(snapshot["faulted"],false);snapshot["transcript"]=json!([]);snapshot["tipId"]=Value::Null;let second=watch_lane(lane,bus,&BACKGROUND_CONTEXT,false).await.expect("second watch");assert_eq!(second.snapshot().expect("snapshot")["tipId"],"after");assert_eq!(second.snapshot().expect("snapshot")["transcript"].as_array().expect("transcript").len(),2);first.unsubscribe();second.unsubscribe();}

#[tokio::test]
async fn omits_streaming_presentation_without_frames(){let lane=fixture().await;install(&lane,effect(),vec![]).await;let watch=watch_lane(lane,HarnessEventBus::new(),&BACKGROUND_CONTEXT,false).await.expect("watch");assert!(watch.snapshot().expect("snapshot")["operation"].get("streamingMessage").is_none());watch.unsubscribe();}

#[tokio::test]
async fn dereferences_queues_pending_writes_and_deferred_handles(){let lane=fixture().await;let handle=json!({"provider":"provider","modelId":"model","api":"test","id":"deferred"});let mut message=maho_ai::providers::faux::faux_assistant_message("",Default::default());message.stop_reason=maho_ai::types::StopReason::Deferred;message.deferred=Some(serde_json::from_value(handle.clone()).expect("handle"));let mut state=scope("deferred.suspended");state["control"]=json!({"status":"cancel_requested","requestedAt":2});state["stepId"]=json!("step");state["sourceEntryId"]=json!("source");state["poll"]=json!(3);state["configuration"]=configuration();state["streamOptions"]=json!({});install(&lane,state,vec![insert_entry(NewEntry::message("source",None,maho_ai::types::Message::Assistant(Box::new(message)).into())),Write::Value(set_value(&branch_tip("main"),json!("source")))]).await;let mut writes=Vec::new();let mut inbox=Vec::new();for(id,kind)in[("next","nextRun"),("steer","steer"),("follow","followUp"),("write","write")]{inbox.push(json!({"entryId":id,"kind":kind}));let payload=if id=="write"{json!({"type":"custom","customType":"note","payload":{"id":id}})}else{json!({"type":"message","payload":{"role":"user","content":id,"timestamp":1}})};writes.push(Write::Value(set_value(&pending_entry(id),payload)));}writes.push(Write::Value(set_value(&lane_state("main"),json!({"currentOperationId":"op","lastOperationId":null,"inbox":inbox}))));commit(&lane,writes).await;let watch=watch_lane(lane,HarnessEventBus::new(),&BACKGROUND_CONTEXT,false).await.expect("watch");let snapshot=watch.snapshot().expect("snapshot");assert_eq!(snapshot["queues"].as_array().expect("queues").len(),4);for(i,id)in["next","steer","follow"].iter().enumerate(){assert_eq!(snapshot["queues"][i]["message"]["content"],*id);}assert_eq!(snapshot["queues"][3],json!({"entryId":"write","kind":"write","type":"custom","customType":"note","data":{"id":"write"}}));assert_eq!(snapshot["operation"],json!({"id":"op","kind":"run","startedAt":1,"fromTipId":null,"status":"aborting","deferred":{"handle":handle,"poll":3},"runningTools":[]}));watch.unsubscribe();}

#[tokio::test]
async fn faults_required_payload_corruption_and_unsubscribes(){let lane=fixture().await;commit(&lane,vec![Write::Value(set_value(&lane_state("main"),json!({"currentOperationId":null,"lastOperationId":null,"inbox":[{"entryId":"missing","kind":"nextRun"}]})))]).await;assert!(watch_lane(lane,HarnessEventBus::new(),&BACKGROUND_CONTEXT,false).await.is_err());}

#[tokio::test]
async fn returns_snapshot_after_without_replay(){let lane=fixture().await;commit(&lane,vec![insert_entry(user("existing",None)),Write::Value(set_value(&branch_tip("main"),json!("existing")))]).await;let watch=watch_lane(lane,HarnessEventBus::new(),&BACKGROUND_CONTEXT,false).await.expect("watch");let count=Arc::new(std::sync::atomic::AtomicUsize::new(0));let captured=count.clone();watch.start(Arc::new(move|_,_|{captured.fetch_add(1,std::sync::atomic::Ordering::SeqCst);Box::pin(async{})})).await;assert_eq!(watch.snapshot().expect("snapshot")["transcript"][0]["id"],"existing");assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst),0);watch.unsubscribe();}
