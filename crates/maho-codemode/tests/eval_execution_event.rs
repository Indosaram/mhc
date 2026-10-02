use maho_codemode::tool::{call_capture::EvalToolCallMetric, eval_execution_event::*, types::EvalLanguage};
use maho_ext_api::AgentToolResult;
use serde_json::json;

#[test]
fn aggregates_pending_calls_and_preserves_first_tool_order() {
    let metrics = vec![
        EvalToolCallMetric{name:"z".into(),started_at:10.0,ok:Some(true),duration_ms:Some(11.0)},
        EvalToolCallMetric{name:"a".into(),started_at:14.0,ok:None,duration_ms:None},
        EvalToolCallMetric{name:"z".into(),started_at:15.0,ok:Some(false),duration_ms:Some(3.0)},
    ];
    let mut result = AgentToolResult::text("done"); result.details=json!({"durationMs":99});
    let payload=build_eval_execution_event_payload(BuildEvalExecutionEventOptions{cell_id:"cell",language:EvalLanguage::Js,started_at:10.0,completed_at:20.0,queued_ms:4.0,detached:true,metrics:&metrics,tool_calls:&[json!({"name":"z","ok":true,"args":{"secret":"excluded"}})],state_error:None,outcome:EvalExecutionSettleOutcome::Result(&result)});
    assert_eq!(payload["distinctToolsCalled"],json!(["z","a"]));
    assert_eq!(payload["toolAggregates"]["z"],json!({"count":2,"totalDurationMs":14.0,"okCount":1,"errorCount":1,"pendingCount":0}));
    assert_eq!(payload["pendingToolCallCount"],1);
    assert_eq!(payload["kernelDurationMs"],99);
    let rpc=to_eval_execution_rpc_payload(&payload);
    assert!(rpc["toolCalls"][0].get("args").is_none());
    assert_eq!(rpc["detailLevel"],"metadata");
}

#[test]
fn errors_are_bounded_and_state_error_has_precedence() {
    let error="x".repeat(600);
    let payload=build_eval_execution_event_payload(BuildEvalExecutionEventOptions{cell_id:"cell",language:EvalLanguage::Py,started_at:20.0,completed_at:10.0,queued_ms:0.0,detached:false,metrics:&[],tool_calls:&[],state_error:Some(&error),outcome:EvalExecutionSettleOutcome::Error("other")});
    assert_eq!(payload["ok"],false);
    assert_eq!(payload["durationMs"],0.0);
    assert_eq!(payload["error"].as_str().unwrap().chars().count(),513);
    assert!(to_eval_execution_rpc_payload(&payload).get("error").is_none());
}

#[test]
fn oversized_rpc_collapses_to_total_aggregate() {
    let metrics:Vec<_>=(0..66).map(|i|EvalToolCallMetric{name:format!("tool{i}"),started_at:0.0,ok:Some(true),duration_ms:Some(1.0)}).collect();
    let calls=vec![json!({"name":"x".repeat(40000),"ok":true})];
    let result=AgentToolResult::text("done");
    let payload=build_eval_execution_event_payload(BuildEvalExecutionEventOptions{cell_id:"cell",language:EvalLanguage::Js,started_at:0.0,completed_at:10.0,queued_ms:0.0,detached:false,metrics:&metrics,tool_calls:&calls,state_error:None,outcome:EvalExecutionSettleOutcome::Result(&result)});
    assert_eq!(payload["toolAggregateOverflow"]["count"],2);
    let rpc=to_eval_execution_rpc_payload(&payload);
    assert_eq!(rpc["rpcTruncated"],true);
    assert_eq!(rpc["toolCalls"],json!([]));
    assert_eq!(rpc["toolAggregateOverflow"]["count"],66);
}
