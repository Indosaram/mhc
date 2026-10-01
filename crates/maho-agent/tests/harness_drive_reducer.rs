use maho_agent::harness::runtime::reducer::{reduce_lane_snapshot, LaneSnapshotReduction};
use serde_json::{Value,json};

fn snapshot() -> Value { json!({"lane":"main","tipId":null,"configuration":{"model":{"provider":"test","modelId":"model"},"thinkingLevel":"off","activeToolNames":[]},"operation":null,"lastResult":null,"queues":[],"transcript":[],"stats":{"messageCount":0,"usage":{}},"faulted":false}) }
fn fold(snapshot: &mut Value, events: Vec<Value>) { for event in events { assert_eq!(reduce_lane_snapshot(snapshot,&event),None); } }
#[test]
fn folds_ordinary_run_to_authoritative_snapshot() { let mut s=snapshot(); fold(&mut s,vec![json!({"type":"run_start","lane":"main","runId":"run","startedAt":1}),json!({"type":"entry_added","lane":"main","entry":{"id":"answer","type":"message","message":{"role":"assistant"}}}),json!({"type":"run_end","lane":"main","runId":"run","status":"completed","fromTipId":null,"tipId":"answer","endedAt":2})]); assert_eq!(s["operation"],Value::Null); assert_eq!(s["tipId"],"answer"); assert_eq!(s["lastResult"],json!({"operationId":"run","kind":"run","status":"completed","fromTipId":null,"tipId":"answer","startedAt":1,"endedAt":2})); assert_eq!(s["stats"]["messageCount"],1); }
#[test]
fn folds_suspend_resume_without_closing_operation() { let mut s=snapshot(); fold(&mut s,vec![json!({"type":"run_start","runId":"run","startedAt":1}),json!({"type":"run_suspend","runId":"run","deferred":{"id":"handle"},"poll":0})]); assert_eq!(s["operation"]["deferred"],json!({"handle":{"id":"handle"},"poll":0})); fold(&mut s,vec![json!({"type":"run_resume","runId":"run"})]); assert!(s["operation"].get("deferred").is_none()); assert_eq!(s["operation"]["id"],"run"); }
#[test]
fn folds_standalone_compaction_and_segment_semantics() { let mut s=snapshot(); s["transcript"]=json!([{"id":"old"}]); fold(&mut s,vec![json!({"type":"compaction_start","runId":"compact","startedAt":1}),json!({"type":"entry_added","entry":{"id":"summary","type":"compaction"}}),json!({"type":"compaction_end","runId":"compact","status":"completed","endedAt":2})]); assert_eq!(s["transcript"],json!([{"id":"summary","type":"compaction"}])); assert!(s["operation"].is_null()); assert_eq!(s["lastResult"]["kind"],"compaction"); }
#[test]
fn replicates_globally_ordered_queue_changes() { let mut s=snapshot(); fold(&mut s,vec![json!({"type":"queue_update","lane":"main","queues":[{"entryId":"next","kind":"nextRun"},{"entryId":"steer","kind":"steer"},{"entryId":"follow","kind":"followUp"}]}),json!({"type":"queue_update","lane":"main","queues":[{"entryId":"next","kind":"nextRun"},{"entryId":"follow","kind":"followUp"}]})]); assert_eq!(s["queues"],json!([{"entryId":"next","kind":"nextRun"},{"entryId":"follow","kind":"followUp"}])); }
#[test]
fn keeps_in_run_compaction_inside_open_run() { let mut s=snapshot(); fold(&mut s,vec![json!({"type":"run_start","runId":"run","startedAt":1}),json!({"type":"compaction_start","runId":"run","reason":"threshold","startedAt":2}),json!({"type":"compaction_end","runId":"run","status":"declined","endedAt":3})]); assert_eq!(s["operation"]["kind"],"run"); assert_eq!(s["operation"]["startedAt"],1); }
#[test]
fn retains_settled_parallel_tools_until_source_ordered_placement() {
    let mut s=snapshot(); fold(&mut s,vec![json!({"type":"run_start","runId":"run","startedAt":1})]);
    let start=|i| json!({"type":"tool_start","runId":"run","toolCallId":format!("call-{i}"),"toolName":format!("tool-{i}"),"args":{"index":i}});
    let end=|i| json!({"type":"tool_end","runId":"run","toolCallId":format!("call-{i}"),"toolName":format!("tool-{i}"),"result":{"content":[{"type":"text","text":format!("done-{i}")}]},"isError":false});
    let entry=|i| json!({"type":"entry_added","entry":{"id":format!("result-{i}"),"type":"message","message":{"role":"toolResult","toolCallId":format!("call-{i}")}}});
    fold(&mut s,vec![start(0),start(1),start(2),start(0),json!({"type":"tool_update","runId":"stale","toolCallId":"call-1","partialResult":{}}),end(2),end(0)]);
    assert_eq!(s["operation"]["runningTools"].as_array().unwrap().len(),3); assert!(s["operation"]["runningTools"][1].get("result").is_none()); assert_eq!(s["operation"]["runningTools"][2]["args"],json!({"index":2}));
    fold(&mut s,vec![entry(0),end(1),entry(1),entry(2)]); assert_eq!(s["operation"]["runningTools"],json!([])); assert_eq!(s["transcript"].as_array().unwrap().iter().map(|e|e["id"].as_str().unwrap()).collect::<Vec<_>>(),["result-0","result-1","result-2"]);
}
#[test]
fn navigation_completion_requires_rebase() { let mut s=snapshot(); assert_eq!(reduce_lane_snapshot(&mut s,&json!({"type":"navigation_end","lane":"main","runId":"nav"})),Some(LaneSnapshotReduction::Rebase)); }
