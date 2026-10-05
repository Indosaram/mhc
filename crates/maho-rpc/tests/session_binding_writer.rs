#[tokio::test]
async fn binding_commits_attribution_and_routes_records_through_stdio_actor(){
    use tokio::io::AsyncReadExt;
    use maho_rpc::{session_binding::BindingRecords,session_attribution::SessionActivityRegistry,session_event_fanout::SessionEventFanout,session_event_writer::SessionWriterActor};
    let activity=SessionActivityRegistry::default();let mut binding=BindingRecords::new("s".into(),activity.clone());
    let mut fanout=SessionEventFanout::default();let(output,mut reader)=tokio::io::duplex(1);let writer=SessionWriterActor::new(output);
    binding.deliver_records("s","{\"type\":\"tool_execution_start\",\"toolCallId\":\"call\",\"toolName\":\"bash\"}\n",&mut fanout,&writer).unwrap();
    assert_eq!(activity.since(activity.mark()).unwrap().tool.as_deref(),Some("bash"));
    let expected=maho_rpc::jsonl::serialize_json_line(&serde_json::json!({"type":"tool_execution_start","toolCallId":"call","toolName":"bash","sessionId":"s"})).unwrap();
    let read=async{let mut bytes=vec![0;expected.len()];reader.read_exact(&mut bytes).await.unwrap();assert_eq!(bytes,expected.as_bytes());};
    let(result,())=tokio::time::timeout(std::time::Duration::from_secs(2),async{tokio::join!(writer.flush(),read)}).await.unwrap();result.unwrap();
    binding.dispose();assert!(activity.since(activity.mark()).is_none());
}

#[test]
fn typed_session_events_preserve_optional_fields_and_flattened_budget(){
    use maho_rpc::session_binding::session_event_record;
    use maho_ext_api::{AgentSessionEvent as E,CompactionReason,CompactionRejectionCause,ToolHookLifecycleEvent,ToolHookName,ToolHookPhase,ToolHookStatus};
    use serde_json::json;
    let event=E::CompactionEnd{reason:CompactionReason::Manual,result:None,aborted:false,will_retry:false,request_id:Some("request".into()),accepted:Some(false),rejection_cause:Some(CompactionRejectionCause::ExternalOwner),error_message:None};
    assert_eq!(session_event_record(&event).unwrap(),json!({"type":"compaction_end","reason":"manual","aborted":false,"willRetry":false,"requestId":"request","accepted":false,"rejectionCause":"external-owner"}));
    assert_eq!(session_event_record(&E::ServiceTierChanged{tier:None,fast_mode:false}).unwrap(),json!({"type":"service_tier_changed","fastMode":false}));
    let event=E::ToolHookStatus(ToolHookLifecycleEvent{hook_run_id:"run".into(),hook_name:ToolHookName::PreToolUse,tool_name:"bash".into(),tool_call_id:"call".into(),extension_path:"native.rs".into(),status_message:"done".into(),started_at:1,phase:ToolHookPhase::End{completed_at:2,status:ToolHookStatus::Completed,error_message:None}});
    assert_eq!(session_event_record(&event).unwrap(),json!({"type":"tool_hook_status","hookRunId":"run","hookName":"PreToolUse","toolName":"bash","toolCallId":"call","extensionPath":"native.rs","statusMessage":"done","startedAt":1,"phase":"end","completedAt":2,"status":"completed"}));
    assert_eq!(session_event_record(&E::SettingsSourceSelected{selection:json!({"scope":"project"})}).unwrap(),json!({"type":"settings_source_selected","scope":"project"}));
}
