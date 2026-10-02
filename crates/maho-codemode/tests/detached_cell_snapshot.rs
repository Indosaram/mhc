use maho_codemode::tool::{detached_cell_snapshot::*,detached_cell_contract::EvalDetachedCellState,types::*};
use std::sync::Arc;
use serde_json::json;

fn cell() -> DetachedCellResultSource {
    DetachedCellResultSource { cell_id:"B".into(),input:EvalToolInput { language:EvalLanguage::Js,code:"42".into(),summary:"test".into(),action:None,timeout:None,on_timeout:None,reset:None },started_at_ms:100.0,run_started_at_ms:None,detached:true,state:EvalDetachedCellState::Queued,queue_snapshot:None,state_retained:None,interrupt_note:None,live_result:None,terminal_result:None,hard_limited:false,hard_limit_seconds:Some(1800.0),run_budget_exhausted:false,run_budget_seconds:Some(300.0) }
}
#[test] fn predecessor_order_excludes_target_and_later_cells() {
    let mut cell=cell();cell.queue_snapshot=Some(Arc::new(|| (Some("A".into()),vec!["X".into(),"B".into(),"C".into()])));
    assert_eq!(queued_behind_cell(&cell),Some(vec!["A".into(),"X".into()]));
    cell.state=EvalDetachedCellState::Running;assert_eq!(queued_behind_cell(&cell),None);
}

#[test] fn footer_clock_uses_activation_while_wake_clock_uses_submission() {
    use maho_codemode::tool::detached_cell_status::*;
    let mut cell=cell();cell.state=EvalDetachedCellState::Running;cell.run_started_at_ms=Some(500.0);
    assert_eq!(detached_status_entries(&[&cell])[0].started_at_ms,500.0);
    let wake=detached_wake_source_state(&[&cell]);
    assert_eq!(wake.active_count,1);assert_eq!(wake.items.unwrap()[0].started_at_ms,100.0);
}
#[test] fn queued_snapshot_has_zero_duration_and_detached_projection() {
    let value=snapshot_detached_cell(&cell(),5000.0);
    assert_eq!(value.state,EvalDetachedCellState::Detached);
    assert_eq!(value.result.details["durationMs"],0.0);
    assert_eq!(value.result.details["cells"][0]["status"],"queued");
    assert_eq!(value.output_tail,"");assert!(value.hard_limit_seconds.is_none());
}
#[test] fn terminal_result_overrides_provider_and_error_keeps_output() {
    let mut cell=cell();cell.terminal_result=Some(maho_ext_api::AgentToolResult::text("terminal"));
    cell.live_result=Some(Arc::new(|| panic!("terminal result must win")));
    let result=detached_error_result(&cell,"failed",Some("eval_kernel_reset_refused"));
    assert_eq!(result.details["isError"],true);assert_eq!(result.details["code"],"eval_kernel_reset_refused");
    assert_eq!(result.content[0],maho_ext_api::ContentBlock::text("terminal\nfailed"));
    cell.terminal_result.as_mut().unwrap().details=json!({"cells":[{"output":"specific"}]});
    assert_eq!(snapshot_detached_cell(&cell,500.0).output_tail,"specific");
}
