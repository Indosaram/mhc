use maho_tools::{definition::*, read::*, write::*, ls::*, grep::*, bash::*, output_accumulator::*, edit::*};
use serde_json::{Value,json};
async fn execute(tool: &ToolDefinition, params: Value) -> Result<ToolResult,ToolError> {
    (tool.execute)(ToolCall { id:"test",params,signal:AbortSignal::default(),on_update:None,context:None }).await
}
fn text(result: &ToolResult) -> String {
    result.content.iter().filter_map(|c| match c { ToolContent::Text { text,.. } => Some(text.as_str()),ToolContent::Image { .. } => None }).collect::<Vec<_>>().join("\n")
}
#[tokio::test]
async fn read_offset_limit_and_continuation() {
    let dir = tempfile::tempdir().unwrap(); std::fs::write(dir.path().join("a"),"one\ntwo\nthree\nfour").unwrap();
    let tool = create_read_tool_definition(dir.path().into(),ReadToolOptions::default());
    let result = execute(&tool,json!({"path":"a","offset":2,"limit":1})).await.unwrap();
    assert_eq!(text(&result),"two\n\n[2 more lines in file. Use offset=3 to continue.]");
    assert!(execute(&tool,json!({"path":"a","offset":8})).await.is_err());
}
#[tokio::test]
async fn read_line_cap_and_model_only_notice() {
    let dir = tempfile::tempdir().unwrap(); std::fs::write(dir.path().join("a"),(1..=2500).map(|n| format!("Line {n}")).collect::<Vec<_>>().join("\n")).unwrap();
    let tool = create_read_tool_definition(dir.path().into(),ReadToolOptions::default());
    let result = execute(&tool,json!({"path":"a"})).await.unwrap();
    assert!(text(&result).contains("Line 2000")); assert!(!text(&result).contains("Line 2001"));
    assert!(maho_tools::model_only_text::is_model_only_text(&result.content[1]));
}
#[tokio::test]
async fn write_creates_parents_and_reports_update() {
    let dir = tempfile::tempdir().unwrap(); let tool = create_write_tool_definition(dir.path().into(),WriteToolOptions::default());
    let first = execute(&tool,json!({"path":"a/b","content":"one\n"})).await.unwrap(); assert_eq!(first.details.unwrap()["operation"],"add");
    let same = execute(&tool,json!({"path":"a/b","content":"one\n"})).await.unwrap(); assert!(same.details.is_none());
    let updated = execute(&tool,json!({"path":"a/b","content":"two\n"})).await.unwrap(); assert_eq!(updated.details.unwrap()["operation"],"update");
    assert_eq!(std::fs::read_to_string(dir.path().join("a/b")).unwrap(),"two\n");
}
#[tokio::test]
async fn edit_is_atomic_when_one_match_fails() {
    let dir = tempfile::tempdir().unwrap(); std::fs::write(dir.path().join("a"),"first second").unwrap();
    let tool = create_edit_tool_definition(dir.path().into(),EditToolOptions::default());
    assert!(execute(&tool,json!({"path":"a","edits":[{"oldText":"first","newText":"changed"},{"oldText":"missing","newText":"changed"}]})).await.is_err());
    assert_eq!(std::fs::read_to_string(dir.path().join("a")).unwrap(),"first second");
}
#[tokio::test]
async fn edit_preserves_bom_crlf_and_legacy_preparation() {
    let dir = tempfile::tempdir().unwrap(); std::fs::write(dir.path().join("a"),"\u{feff}one\r\ntwo\r\n").unwrap();
    let tool = create_edit_tool_definition(dir.path().into(),EditToolOptions::default());
    let params = prepare_edit_arguments(json!({"path":"a","oldText":"one\n","newText":"ONE\n"})).unwrap();
    execute(&tool,params).await.unwrap(); assert_eq!(std::fs::read_to_string(dir.path().join("a")).unwrap(),"\u{feff}ONE\r\ntwo\r\n");
}
#[tokio::test]
async fn list_includes_dotfiles_and_directory_suffix() {
    let dir = tempfile::tempdir().unwrap(); std::fs::write(dir.path().join(".hidden"),"").unwrap(); std::fs::create_dir(dir.path().join("folder")).unwrap();
    let tool = create_ls_tool_definition(dir.path().into(),LsToolOptions::default());
    let result = execute(&tool,json!({})).await.unwrap(); assert!(text(&result).contains(".hidden")); assert!(text(&result).contains("folder/"));
}
#[tokio::test]
async fn native_grep_searches_file_with_context() {
    let dir = tempfile::tempdir().unwrap(); std::fs::write(dir.path().join("a"),"before\nneedle\nafter\n").unwrap();
    let tool = create_grep_tool_definition(dir.path().into(),GrepToolOptions::default());
    let result = execute(&tool,json!({"pattern":"needle","path":"a","context":1})).await.unwrap();
    let details = result.details.unwrap(); assert_eq!(details["engine"],"native"); assert_eq!(details["matchCount"],1); assert_eq!(details["matches"].as_array().unwrap().len(),3);
}
#[tokio::test]
async fn native_grep_recovers_unmatched_pattern() {
    let dir = tempfile::tempdir().unwrap(); std::fs::write(dir.path().join("a"),"call(foo\n").unwrap();
    let tool = create_grep_tool_definition(dir.path().into(),GrepToolOptions::default());
    let result = execute(&tool,json!({"pattern":"call(","path":"a"})).await.unwrap();
    assert_eq!(result.details.unwrap()["matchCount"],1);
}
#[tokio::test]
async fn bash_runs_real_pty_and_reports_failure() {
    let dir = tempfile::tempdir().unwrap(); let tool = create_bash_tool_definition(dir.path().into(),BashToolOptions::default());
    assert_eq!(text(&execute(&tool,json!({"command":"printf hello"})).await.unwrap()),"hello");
    let error = execute(&tool,json!({"command":"printf problem; exit 7"})).await.unwrap_err();
    assert!(error.to_string().contains("problem")); assert!(error.to_string().contains("Command exited with code 7"));
}
#[test]
fn accumulator_handles_split_utf8_and_spill() {
    let mut output = OutputAccumulator::new(OutputAccumulatorOptions { max_lines:1,max_bytes:10,temp_file_prefix:"maho-tools-test".into() });
    let bytes = "한글\nnext\n".as_bytes();
    for byte in bytes { output.append(&[*byte]).unwrap(); }
    output.finish().unwrap(); let snapshot = output.snapshot(true).unwrap(); output.close_temp_file().unwrap();
    assert_eq!(snapshot.content,"next"); assert_eq!(snapshot.truncation.total_lines,2);
    assert_eq!(std::fs::read(snapshot.full_output_path.unwrap()).unwrap(),bytes); output.remove_temp_file().unwrap();
}
#[tokio::test]
async fn cancellation_is_sticky_for_late_subscribers() {
    let signal = AbortSignal::default(); signal.abort();
    tokio::time::timeout(std::time::Duration::from_secs(1),signal.cancelled()).await.unwrap(); assert!(signal.check().is_err());
}
