use maho_ext_pi_ast_grep::cli::{RunSgOptions, run_sg};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let binary = std::env::args_os().nth(1).ok_or("binary path required")?;
    let fixture = tempfile::tempdir()?;
    let file = fixture.path().join("fixture.ts");
    std::fs::write(&file, "console.log(value);\n")?;
    let mut options = RunSgOptions { pattern: "console.log($MSG)".into(), lang: "typescript".into(), paths: vec![file.to_string_lossy().into_owned()], rewrite: Some("logger.info($MSG)".into()), ..Default::default() };
    let binary = std::path::Path::new(&binary);
    let function_file = fixture.path().join("function.ts");
    std::fs::write(&function_file, "function hello(value) { return value; }\n")?;
    let function = run_sg(&RunSgOptions { pattern: "function $NAME($$$) { $$$ }".into(), lang: "typescript".into(), paths: vec![function_file.to_string_lossy().into_owned()], ..Default::default() }, binary).await;
    if function.matches.is_empty() || function.error.is_some() { return Err("function structural search failed".into()); }
    let glob = run_sg(&RunSgOptions { pattern: "console.log($MSG)".into(), lang: "typescript".into(), paths: vec![fixture.path().to_string_lossy().into_owned()], globs: vec!["*.ts".into()], ..Default::default() }, binary).await;
    if glob.matches.is_empty() || glob.error.is_some() || glob.matches.iter().any(|found| !found.file.ends_with(".ts")) { return Err("glob search failed".into()); }
    let absent = run_sg(&RunSgOptions { pattern: "thisIdentifierDoesNotExistAnywhere($$$)".into(), lang: "typescript".into(), paths: vec![file.to_string_lossy().into_owned()], ..Default::default() }, binary).await;
    if !absent.matches.is_empty() || absent.error.is_some() || absent.total_matches != 0 { return Err("no-match search failed".into()); }
    let preview = run_sg(&options, binary).await;
    if preview.matches.len() != 1 || preview.error.is_some() { return Err(format!("preview failed: {preview:?}").into()); }
    if std::fs::read_to_string(&file)? != "console.log(value);\n" { return Err("preview modified fixture".into()); }
    options.update_all = true;
    let applied = run_sg(&options, binary).await;
    if applied.matches.len() != 1 || applied.error.is_some() { return Err(format!("apply failed: {applied:?}").into()); }
    if std::fs::read_to_string(&file)? != "logger.info(value);\n" { return Err("apply did not rewrite fixture".into()); }
    let definition = maho_ext_pi_ast_grep::tools::tool(false, binary.to_owned());
    let result = (definition.execute)(maho_ext_api::ToolCall {
        id: "fixture-call", params: serde_json::json!({"pattern":"logger.info($MSG)","lang":"typescript","paths":[file]}),
        signal: Default::default(), on_update: None, context: None,
    }).await?;
    if result.details.as_ref().and_then(|details| details.get("totalMatches")).and_then(serde_json::Value::as_u64) != Some(1) { return Err("native tool executor did not expose match details".into()); }
    println!("preview_matches=1 preview_unchanged=true applied_matches=1 rewrite_verified=true");
    println!("native_tool_executor_matches=1");
    println!("function_search=true glob_filter=true no_match_without_error=true");
    let agent_result = serde_json::from_value(serde_json::json!({"content":[{"type":"text","text":"native result"}],"details":result.details}))?;
    let output = maho_ext_pi_ast_grep::render::result_text(&agent_result,true,false,false);
    if !output.contains("1 match") || !output.contains("logger.info(value)") { return Err("native renderer omitted real match".into()); }
    println!("expanded_renderer:\n{output}");
    Ok(())
}
