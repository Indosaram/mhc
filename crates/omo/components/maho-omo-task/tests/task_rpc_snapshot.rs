use maho_omo_task::task_rpc_codec::{task_snapshot,bounded_task_output};
use senpi_task::{state::{TaskRecordInput,create_task_record}, tools::output::types::TaskOutputDetails};
#[test] fn transcript_exact_utf16_boundary_retains_mode_source_and_no_truncation() {
    use senpi_task::tools::output::{snapshot::build_task_snapshot,types::{TranscriptMode,TranscriptSource}};
    let record=create_task_record(TaskRecordInput::default(),Some(1)).expect("record");
    for text in ["x".repeat(32000),"🦀".repeat(16000)] {
        let snapshot=build_task_snapshot(&record,"/tmp",1000);
        let value=bounded_task_output(&TaskOutputDetails::Transcript { mode:TranscriptMode::Full,source:TranscriptSource::SessionJsonl,transcript:text.clone(),truncated:false,snapshot }).expect("output");
        assert_eq!(value["transcript"],text); assert_eq!(value["truncated"],false); assert_eq!(value["mode"],"full"); assert_eq!(value["source"],"session-jsonl");
    }
}
#[test] fn output_snapshot_bounds_complete_text_and_model_matrix() {
    use senpi_task::{state::{ResolvedModelRecord,ResolvedModelSource},tools::output::snapshot::build_task_snapshot};
    let record=create_task_record(TaskRecordInput::default(),Some(1)).expect("record");
    let mut snapshot=build_task_snapshot(&record,"/tmp",1000);
    snapshot.age_ms=999;
    snapshot.task_id="i".repeat(257); snapshot.child_session_id=Some("i".repeat(257)); snapshot.execution_mode="x".repeat(32001); snapshot.model="x".repeat(32001);
    snapshot.name=Some("x".repeat(32001)); snapshot.task_summary=Some("x".repeat(32001)); snapshot.agent_type=Some("x".repeat(32001)); snapshot.category=Some("x".repeat(32001)); snapshot.description=Some("x".repeat(32001)); snapshot.final_response=Some("x".repeat(32001)); snapshot.error_message=Some("x".repeat(32001));
    snapshot.resolved_model=Some(ResolvedModelRecord { provider:"x".repeat(32001),model_id:"x".repeat(32001),display:"x".repeat(32001),source:ResolvedModelSource::Category,variant:Some("x".repeat(32001)),reasoning_effort:Some("x".repeat(32001)),reasoning:Some("x".repeat(32001)) });
    let value=bounded_task_output(&TaskOutputDetails::Status { snapshot }).expect("output"); assert_eq!(value["kind"],"status"); let snapshot=&value["snapshot"];
    for key in ["task_id","child_session_id"] { assert_eq!(snapshot[key].as_str().expect("id").len(),256); }
    for key in ["name","task_summary","execution_mode","model","agent_type","category","description","final_response","error_message"] { assert_eq!(snapshot[key].as_str().expect("text").len(),32000,"{key}"); }
    for key in ["description","final_response","error_message"] { assert_eq!(snapshot[format!("{key}_truncated")],true); }
    for key in ["provider","model_id","display","variant","reasoning_effort","reasoning"] { assert_eq!(snapshot["resolved_model"][key].as_str().expect("model text").len(),32000); }
    assert_eq!(snapshot["resolved_model"]["source"],"category"); assert_eq!(snapshot["age_ms"],999);
}
#[test] fn live_progress_projects_all_optional_fields_including_zero() {
    use senpi_task::progress::{ProgressActivity,ToolProgressDetails};
    let mut details=ToolProgressDetails { progress:ProgressActivity { activity:"working".into(),started_at:12.0 },child_id:"private-child".into(),current_tool:None,last_assistant_line:None,turns:0.0,tool_calls:None,tokens:None,output_tokens:None,tokens_per_second:None };
    let required=serde_json::json!({"activity":"working","started_at":12.0,"turns":0.0});
    assert_eq!(maho_omo_task::task_rpc_codec::live_progress_snapshot(&details),required);
    details.current_tool=Some("read".into()); details.last_assistant_line=Some("line".into()); details.tool_calls=Some(0.0); details.tokens=Some(0.0); details.output_tokens=Some(0.0); details.tokens_per_second=Some(0.0);
    let progress=maho_omo_task::task_rpc_codec::live_progress_snapshot(&details);
    assert_eq!(progress,serde_json::json!({"activity":"working","started_at":12.0,"turns":0.0,"current_tool":"read","last_assistant_line":"line","tool_calls":0.0,"total_tokens":0.0,"output_tokens":0.0,"tokens_per_second":0.0}));
    let record=create_task_record(TaskRecordInput::default(),Some(1)).expect("record");
    assert_eq!(task_snapshot(&record,None,Some(&progress)).expect("snapshot")["live_progress"],progress);
}
#[test] fn task_snapshot_bounds_every_text_field_without_flagging_exact_boundary() {
    let mut record=create_task_record(TaskRecordInput::default(),Some(1)).expect("record");
    record.task_id="i".repeat(257); record.name=Some("x".repeat(32001)); record.task_summary=Some("x".repeat(32001)); record.agent_type=Some("x".repeat(32001)); record.category=Some("x".repeat(32001)); record.model="x".repeat(32001); record.description=Some("x".repeat(32000));
    let value=task_snapshot(&record,None,None).expect("snapshot"); assert_eq!(value["task_id"].as_str().expect("id").len(),256);
    for key in ["name","task_summary","agent_type","category","model","description"] { assert_eq!(value[key].as_str().expect("text").len(),32000,"{key}"); assert!(value.get(format!("{key}_truncated")).is_none(),"{key}"); }
}
#[test] fn task_snapshot_bounds_text_and_sets_truncation_flags() { let mut record = create_task_record(TaskRecordInput::default(),Some(1)).unwrap(); record.description = Some("x".repeat(32001)); record.final_response = Some("x".repeat(32001)); record.error_message = Some("x".repeat(32001)); let snapshot = task_snapshot(&record,None,None).unwrap(); for key in ["description","final_response","error_message"] { assert_eq!(snapshot[key].as_str().unwrap().len(),32000); assert_eq!(snapshot[format!("{key}_truncated")],true); } }
#[test] fn missing_task_output_does_not_disclose_known_tasks() { let value = bounded_task_output(&TaskOutputDetails::NotFound { reason:"private".into(),known_tasks:vec!["private".into()] }).unwrap(); assert_eq!(value,serde_json::json!({"kind":"not_found","reason":"Task not found."})); }
#[test] fn invalid_output_arguments_remain_machine_readable() { let value = bounded_task_output(&TaskOutputDetails::InvalidArguments { reason:"invalid".into() }).unwrap(); assert_eq!(value["reason"],"invalid"); }
#[test] fn task_snapshot_child_identity_is_bounded() { let mut record = create_task_record(TaskRecordInput::default(),Some(1)).unwrap(); record.child_session_id = Some("a".repeat(257)); let snapshot = task_snapshot(&record,None,None).unwrap(); assert_eq!(snapshot["child_session_id"].as_str().unwrap().len(),256); }
#[test] fn transcript_output_bounds_utf16_and_preserves_existing_truncation() {
    use senpi_task::tools::output::{snapshot::build_task_snapshot,types::{TranscriptMode,TranscriptSource}};
    let record=create_task_record(TaskRecordInput::default(),Some(1)).expect("record"); let snapshot=build_task_snapshot(&record,"/tmp",1000);
    let details=TaskOutputDetails::Transcript { mode:TranscriptMode::Full,source:TranscriptSource::EventLog,transcript:"🦀".repeat(16001),truncated:false,snapshot:snapshot.clone() }; let value=bounded_task_output(&details).expect("output"); assert_eq!(value["transcript"].as_str().expect("transcript").encode_utf16().count(),32000); assert_eq!(value["truncated"],true);
    let details=TaskOutputDetails::Transcript { mode:TranscriptMode::Tail,source:TranscriptSource::EventLog,transcript:"short".into(),truncated:true,snapshot }; assert_eq!(bounded_task_output(&details).expect("output")["truncated"],true);
}
#[test] fn output_snapshot_bounds_lineage_and_loss_breadcrumbs_without_discarding_pid() {
    use senpi_task::{state::{TaskStatus,ResidencyState},tools::output::{snapshot::build_task_snapshot,types::SuspendedDetails}};
    let mut record=create_task_record(TaskRecordInput::default(),Some(1)).expect("record"); record.parent_session_id="p".repeat(257); record.root_session_id="r".repeat(257); record.status=TaskStatus::Lost; record.pid=Some(42); record.residency_state=ResidencyState::PersistedOnly; let mut snapshot=build_task_snapshot(&record,"/tmp",1000); let lost=snapshot.lost.as_mut().expect("lost"); lost.explanation="e".repeat(32001); lost.session_dir="s".repeat(32001); snapshot.suspended=Some(SuspendedDetails { explanation:"x".repeat(32001) });
    let value=bounded_task_output(&TaskOutputDetails::Status { snapshot }).expect("output"); for field in ["parent_session_id","root_session_id"] { assert_eq!(value["snapshot"][field].as_str().expect("lineage").len(),256); } for field in ["explanation","session_dir"] { assert_eq!(value["snapshot"]["lost"][field].as_str().expect("breadcrumb").len(),32000); } assert_eq!(value["snapshot"]["lost"]["pid"],42); assert_eq!(value["snapshot"]["suspended"]["explanation"].as_str().expect("suspended").len(),32000);
}
#[test] fn absent_optional_snapshot_fields_are_not_invented() { let record=create_task_record(TaskRecordInput::default(),Some(1)).expect("record"); let value=task_snapshot(&record,None,None).expect("snapshot"); for field in ["child_session_id","final_response","error_message","run_stats","live_progress","description_truncated"] { assert!(value.get(field).is_none(),"{field}"); } }
#[test] fn snapshots_preserve_full_durable_stats_and_prefer_live_stats_when_supplied() {
    use senpi_task::state::{TaskRunStats,TaskStatus};
    let mut record=create_task_record(TaskRecordInput::default(),Some(1)).expect("record"); record.status=TaskStatus::Completed; record.child_session_id=Some("child".into()); record.final_response=Some("result".into());
    let stats=TaskRunStats { runtime_ms:2500,turns:3,tool_calls:4,output_tokens:Some(200),total_tokens:Some(1200),generation_ms:Some(1500),tokens_per_second:Some(133.3),cost_usd:Some(0.12),cache_hit_rate_last:Some(0.5),cache_hit_rate_run:Some(0.4) }; record.run_stats=Some(stats.clone());
    let value=task_snapshot(&record,None,None).expect("durable"); assert_eq!(value["run_stats"],serde_json::to_value(&stats).expect("stats")); assert_eq!(value["child_session_id"],"child"); assert_eq!(value["final_response"],"result");
    let live=TaskRunStats { runtime_ms:1000,..Default::default() }; let value=task_snapshot(&record,Some(&live),None).expect("live"); assert_eq!(value["run_stats"],serde_json::to_value(&live).expect("stats"));
    record.status=TaskStatus::Error; record.error_message=Some("failed".into()); assert_eq!(task_snapshot(&record,None,None).expect("error")["error_message"],"failed");
}
