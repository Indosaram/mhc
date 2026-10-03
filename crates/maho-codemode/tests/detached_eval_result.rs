use maho_codemode::tool::{detached_cell_contract::EvalDetachedCellState, detached_eval_result::*};
use maho_ext_api::AgentToolResult;
use serde_json::json;

#[test]
fn live_duration_replaces_only_first_cell_and_preserves_source() {
    let mut result = AgentToolResult::text("output");
    result.details = json!({"durationMs":2,"toolCalls":[],"cells":[{"durationMs":2,"queuedBehind":["old"]},{"durationMs":5}]});
    let next = result_for_detached_state(&result, EvalDetachedCellState::Detached, 20.0, Some(&["A".into()]));
    assert_eq!(next.details["durationMs"], 20.0);
    assert_eq!(next.details["cells"][0]["status"], "queued");
    assert_eq!(next.details["cells"][0]["queuedBehind"], json!(["A"]));
    assert_eq!(next.details["cells"][1]["durationMs"], 5);
    assert_eq!(result.details["cells"][0]["queuedBehind"], json!(["old"]));
}

#[test]
fn terminal_duration_survives_and_stale_queue_is_removed() {
    let mut result = AgentToolResult::text("output");
    result.details = json!({"durationMs":7,"cells":[{"durationMs":6,"queuedBehind":["A"]}]});
    let next = result_for_detached_state(&result, EvalDetachedCellState::Completed, 100.0, None);
    assert_eq!(next.details["durationMs"], 7);
    assert_eq!(next.details["cells"][0]["durationMs"], 6);
    assert_eq!(next.details["cells"][0]["status"], "complete");
    assert!(next.details["cells"][0].get("queuedBehind").is_none());
}

#[test]
fn capacity_error_has_machine_consumed_code() {
    let error = EvalBackgroundCapacityError::new(15, "cell", 30_000.0, &["other".into()]);
    assert_eq!(error.code, "eval_background_capacity_reached");
}

#[test]
fn empty_list_has_control_metadata() {
    let result = create_eval_list_result(&[], &[]);
    assert_eq!(result.details, json!({"action":"list","cells":[]}));
}

#[tokio::test]
async fn list_preview_preserves_javascript_whitespace_boundaries() {
    use maho_codemode::tool::{detached_cell_manager::{EvalDetachedCellManager,DetachedCellManagerOptions},types::{EvalToolInput,EvalLanguage}};
    let mut manager=EvalDetachedCellManager::new(DetachedCellManagerOptions::default());
    let cell=manager.create("preview".into(),EvalToolInput {language:EvalLanguage::Js,code:"42".into(),summary:"unused".into(),action:None,timeout:None,on_timeout:None,reset:None}).unwrap();
    manager.mark_running(&cell);
    let mut result=AgentToolResult::text("");
    result.details=json!({"summary":"\u{feff}  alpha\u{0085}beta\t ","durationMs":0,"cells":[]});
    assert!(manager.complete(&cell,result));
    let listed=create_eval_list_result(&[],&manager.list().1);
    let maho_ext_api::ContentBlock::Text(text)=&listed.content[0] else {panic!("list text")};
    assert!(text.text.ends_with(" -  alpha\u{0085}beta "),"{}",text.text);
}
