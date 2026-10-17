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
async fn read_accepts_shell_quoted_paths() {
    let dir = tempfile::tempdir().unwrap(); std::fs::write(dir.path().join("file name"),"contents").unwrap();
    let tool = create_read_tool_definition(dir.path().into(),ReadToolOptions { folder: None, ..Default::default() });
    assert_eq!(text(&execute(&tool,json!({"path":"\"file name\""})).await.unwrap()),"contents");
}
#[tokio::test]
async fn read_line_cap_and_model_only_notice() {
    let dir = tempfile::tempdir().unwrap(); std::fs::write(dir.path().join("a"),(1..=2500).map(|n| format!("Line {n}")).collect::<Vec<_>>().join("\n")).unwrap();
    let tool = create_read_tool_definition(dir.path().into(),ReadToolOptions::default());
    let result = execute(&tool,json!({"path":"a"})).await.unwrap();
    assert!(text(&result).contains("Line 2000")); assert!(!text(&result).contains("Line 2001"));
    assert!(maho_tools::model_only_text::is_model_only_text(&result.content[1]));
}
fn large_json() -> String {
    serde_json::to_string_pretty(&(0..25).map(|n| json!({"n":n,"a":[1,2,3,4,5],"s":"escaped \" } {"})).collect::<Vec<_>>()).expect("serialize json fixture")
}
#[tokio::test]
async fn read_default_folds_large_structured_file() {
    let dir = tempfile::tempdir().unwrap(); let source = large_json(); std::fs::write(dir.path().join("data.json"),&source).unwrap();
    let tool = create_read_tool_definition(dir.path().into(),ReadToolOptions::default());
    let output = text(&execute(&tool,json!({"path":"data.json"})).await.unwrap());
    assert!(output.split('\n').any(|line| line == "\u{2026}"));
    assert!(output.contains("Reread source before editing; markers are not source."));
    assert!(output.len() < source.len());
}
#[tokio::test]
async fn read_without_folder_stays_verbatim() {
    let dir = tempfile::tempdir().unwrap(); let source = large_json(); std::fs::write(dir.path().join("data.json"),&source).unwrap();
    let tool = create_read_tool_definition(dir.path().into(),ReadToolOptions { folder: None, ..Default::default() });
    assert_eq!(text(&execute(&tool,json!({"path":"data.json"})).await.unwrap()),source);
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
async fn list_sorts_like_locale_compare() {
    let dir = tempfile::tempdir().unwrap();
    for name in ["b","ab","a.b","1x","_x","A"] { std::fs::write(dir.path().join(name),"").unwrap(); }
    let tool = create_ls_tool_definition(dir.path().into(),LsToolOptions::default());
    let result = execute(&tool,json!({})).await.unwrap(); assert_eq!(text(&result).trim_end(),"_x\n1x\nA\na.b\nab\nb");
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
    let result = execute(&tool,json!({"command":"printf problem; exit 7"})).await.unwrap();
    assert!(text(&result).contains("problem")); assert!(text(&result).contains("Command exited with code 7"));
    assert_eq!(result.structured_content.as_ref().unwrap()["exit_code"],7);
}
#[tokio::test]
async fn bash_exposes_structured_result_for_nonzero_exit() {
    let dir = tempfile::tempdir().unwrap(); let tool = create_bash_tool_definition(dir.path().into(),BashToolOptions::default());
    let result = execute(&tool,json!({"command":"printf captured; exit 7"})).await.unwrap();
    assert_eq!(result.structured_content.as_ref().unwrap()["output"],"captured");
    assert_eq!(result.structured_content.as_ref().unwrap()["exit_code"],7);
    assert!(text(&result).contains("Command exited with code 7"));
}
#[test]
fn output_schema_survives_definition_wrapper() {
    let definition = create_bash_tool_definition(std::path::PathBuf::from("."),BashToolOptions::default());
    assert!(definition.output_schema.is_some());
    let wrapped = maho_tools::tool_definition_wrapper::wrap_tool_definition(definition,None);
    assert!(wrapped.tool.output_schema.is_some());
}
#[test]
fn middle_full_output_is_bounded_and_utf8_safe() {
    let mut output = OutputAccumulator::new(OutputAccumulatorOptions { max_lines:1,max_bytes:4,temp_file_prefix:"maho-full-output".into() });
    output.append_text("begin-한글-end").unwrap(); output.finish().unwrap();
    let full = output.read_full_output(8).unwrap();
    assert!(full.truncated); assert!(full.content.starts_with("begi")); assert!(full.content.ends_with("-end"));
    assert!(full.content.contains("bytes omitted")); output.remove_temp_file().unwrap();
}
#[test]
fn glob_is_registered_in_all_tool_index() {
    assert!(maho_tools::index::ALL_TOOL_NAMES.contains(&"glob"));
    let tools = maho_tools::index::create_all_tool_definitions(std::path::Path::new("."),maho_tools::index::ToolsOptions::default());
    assert_eq!(tools["glob"].exposure,Some(ToolExposure::Eval));
}
async fn exec_local(command: &str, env: std::collections::BTreeMap<String,String>) -> (Result<BashExit,ToolError>, String) {
    let output = std::sync::Arc::new(std::sync::Mutex::new(Vec::new())); let captured = std::sync::Arc::clone(&output);
    let result = create_local_bash_operations(None).exec(command, std::path::Path::new("."), BashExecOptions {
        on_data: std::sync::Arc::new(move |data| { captured.lock().expect("output lock").extend_from_slice(data); Ok(()) }),
        signal: AbortSignal::default(), timeout: None, env,
    }).await;
    let text = String::from_utf8_lossy(&output.lock().expect("output lock")).into_owned(); (result, text)
}
#[tokio::test]
async fn local_bash_uses_pipes_and_exactly_the_given_env() {
    let env = std::env::vars().filter(|(key,_)| key == "PATH").chain([("ONLY_VAR".to_owned(),"set".to_owned())]).collect();
    let (result, text) = exec_local("[ -t 1 ] && printf tty || printf pipe; printf ' %s %s' \"${ONLY_VAR-unset}\" \"${HOME-unset}\"", env).await;
    assert_eq!(result.unwrap().exit_code, Some(0)); assert_eq!(text, "pipe set unset");
}
#[cfg(unix)]
#[tokio::test]
async fn local_bash_tracks_backgrounded_process_group_until_killed() {
    let (result, _) = exec_local("sleep 30 >/dev/null 2>&1 &", std::env::vars().collect()).await;
    assert_eq!(result.unwrap().exit_code, Some(0));
    let tracked = maho_tools::shell::list_tracked_detached_children();
    assert_eq!(tracked.len(), 1); assert!(tracked[0].leader_exited);
    maho_tools::shell::kill_tracked_detached_children();
    assert!(maho_tools::shell::list_tracked_detached_children().is_empty());
}
#[test]
fn shell_config_rejects_missing_custom_path_and_classifies_kinds() {
    assert_eq!(maho_tools::shell::get_shell_config(Some("/definitely/missing/shell")).unwrap_err(), "Custom shell path not found: /definitely/missing/shell");
    assert_eq!(maho_tools::shell::shell_config_for_path("C:/Windows/System32/bash.exe").command_transport, Some("stdin"));
    assert_eq!(maho_tools::shell::shell_config_for_path("/usr/bin/pwsh").args, ["-NoProfile", "-Command"]);
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
