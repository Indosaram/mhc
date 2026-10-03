use crate::{types::{ApplyPatchResult,ApplyPatchPreview,ApplyPatchToolDetails},preview_format::truncate_preview,apply::compact_apply_patch_result,recovery::build_partial_failure_text};
pub const APPLY_PATCH_RESULT_PATCH_MAX_BYTES:usize=16*1024;
pub fn retained_patch(patch:Option<&str>)->Option<String> { patch.filter(|patch|patch.len()<=APPLY_PATCH_RESULT_PATCH_MAX_BYTES).map(String::from) }
pub fn applied_preview(result:&ApplyPatchResult)->Option<ApplyPatchPreview> {
    let mut operations:Vec<_>=result.details.applied_operations.iter().collect(); operations.sort_by_key(|operation|operation.operation_index);
    if operations.is_empty() { return None; }
    let files:Vec<_>=operations.into_iter().map(|operation|{ let mut file=operation.preview.clone(); file.diff=truncate_preview(&file.diff); file.patch=retained_patch(file.patch.as_deref()); file }).collect();
    Some(ApplyPatchPreview{added:files.iter().map(|file|file.added).sum(),removed:files.iter().map(|file|file.removed).sum(),files})
}
pub fn execution_result(result:ApplyPatchResult)->(String,ApplyPatchToolDetails) {
    let preview=applied_preview(&result); let text=if result.failures.is_empty() { result.summaries.join("\n") } else { build_partial_failure_text(&result) };
    (text,ApplyPatchToolDetails{preview,result:Some(compact_apply_patch_result(result)),progress:None})
}
pub fn create_apply_patch_tool()->maho_tools::definition::ToolDefinition {
    create_apply_patch_tool_variant(crate::types::ApplyPatchWireMode::Freeform)
}
pub fn create_apply_patch_tool_variant(variant:crate::types::ApplyPatchWireMode)->maho_tools::definition::ToolDefinition {
    use std::sync::Arc;
    use maho_tools::definition::{ToolDefinition,ToolError,ToolResult,ToolContent};
    let mut tool=ToolDefinition::new("apply_patch",crate::constants::APPLY_PATCH_FREEFORM_DESCRIPTION,crate::constants::apply_patch_params(),Arc::new(|call|Box::pin(async move {
        let input=crate::params::normalize_apply_patch_arguments(&call.params).input;
        if input.is_empty() { return Err(ToolError::Message("input is required".into())); }
        let cwd=call.context.ok_or_else(||ToolError::Message("Missing tool execution context".into()))?.cwd();
        let total=crate::parser::parse_patch(&input).map_or(0,|hunks|hunks.len());
        let progress=(total>0).then_some(crate::types::ApplyPatchProgress{applied:0,failed:0,total});
        let (text,details)=crate::preview::create_pending_patch_update(cwd,&input,progress,None).await;
        let preview=details.as_ref().and_then(|details|details.preview.clone());
        if let Some(update)=&call.on_update { update(ToolResult{content:vec![ToolContent::text(text)],details:details.map(serde_json::to_value).transpose()?})?; }
        let on_update=call.on_update.clone(); let owned_cwd=cwd.to_path_buf(); let progress_input=input.clone();
        let callback=move |progress| { let update=on_update.clone(); let cwd=owned_cwd.clone(); let input=progress_input.clone(); let preview=preview.clone(); Box::pin(async move {
            let (text,details)=crate::preview::create_pending_patch_update(&cwd,&input,Some(progress),preview).await;
            if let Some(update)=update { update(ToolResult{content:vec![ToolContent::text(text)],details:details.map(serde_json::to_value).transpose().map_err(|error|error.to_string())?}).map_err(|error|error.to_string())?; } Ok(())
        }) as std::pin::Pin<Box<dyn std::future::Future<Output=Result<(),String>>+Send>> };
        let result=crate::apply::apply_patch_detailed_with_progress(cwd,&input,Some(&callback)).await.map_err(ToolError::Message)?;
        let (text,details)=execution_result(result); Ok(ToolResult{content:vec![ToolContent::text(text)],details:Some(serde_json::to_value(details)?)})
    })));
    tool.label="ApplyPatch".into();
    if variant==crate::types::ApplyPatchWireMode::Json { tool.description=crate::constants::apply_patch_json_description(); }
    else { tool.freeform=Some(maho_ai::types::FreeformToolFormat{kind:"grammar".into(),syntax:"lark".into(),definition:crate::constants::APPLY_PATCH_LARK_GRAMMAR.into()}); }
    tool.prepare_arguments=Some(Arc::new(|value|Ok(serde_json::json!({"input":crate::params::normalize_apply_patch_arguments(&value).input}))));
    tool.prompt_snippet=Some("Apply Codex-format file patches with apply_patch".into());
    tool.prompt_guidelines=Some(vec!["Use apply_patch for file edits instead of mutating files through bash, Python scripts, heredocs, or shell redirection.".into(),"After apply_patch succeeds, do not re-read the edited files just to confirm the patch applied.".into()]);
    tool
}
#[cfg(test)]
mod tests {
    use super::*;
    struct Context(std::path::PathBuf);
    impl maho_tools::definition::ToolSessionManager for Context { fn session_id(&self)->&str { "test" } fn session_file(&self)->Option<&std::path::Path> { None } }
    impl maho_tools::definition::ToolContext for Context {
        fn cwd(&self)->&std::path::Path { &self.0 }
        fn model(&self)->Option<&maho_ai::model::Model> { None }
        fn thinking_level(&self)->Option<maho_ai::types::ThinkingLevel> { None }
        fn session_manager(&self)->&dyn maho_tools::definition::ToolSessionManager { self }
        fn goal_store_file(&self)->Option<&std::path::Path> { None }
    }
    #[tokio::test] async fn native_tool_writes_files_and_emits_exact_progress_counts() {
        let directory=tempfile::tempdir().unwrap(); let context=Context(directory.path().into());
        let updates=std::sync::Arc::new(std::sync::Mutex::new(Vec::new())); let captured=updates.clone();
        let tool=create_apply_patch_tool();
        let result=(tool.execute)(maho_tools::definition::ToolCall{id:"one",params:serde_json::json!({"input":"*** Begin Patch\n*** Add File: a\n+hello\n*** End Patch"}),signal:Default::default(),context:Some(&context),on_update:Some(std::sync::Arc::new(move |update|{captured.lock().unwrap().push(update); Ok(())}))}).await.unwrap();
        assert_eq!(tokio::fs::read_to_string(directory.path().join("a")).await.unwrap(),"hello\n"); assert_eq!(result.details.unwrap()["result"]["appliedFiles"],serde_json::json!(["a"]));
        let updates=updates.lock().unwrap(); assert_eq!(updates.len(),2); assert_eq!(updates[0].details.as_ref().unwrap()["progress"]["applied"],0); assert_eq!(updates[1].details.as_ref().unwrap()["progress"]["applied"],1);
    }
    #[test] fn wire_variants_select_grammar_only_for_freeform() { assert!(create_apply_patch_tool().freeform.is_some()); assert!(create_apply_patch_tool_variant(crate::types::ApplyPatchWireMode::Json).freeform.is_none()); }
    #[tokio::test] async fn upstream_raw_and_json_inputs_update_existing_files() {
        let directory=tempfile::tempdir().unwrap();let context=Context(directory.path().into());let tool=create_apply_patch_tool();
        for (name,raw) in [("raw.txt",true),("json.txt",false)] {
            let path=directory.path().join(name);tokio::fs::write(&path,b"before\n").await.unwrap();
            let input=format!("*** Begin Patch\n*** Update File: {name}\n@@\n-before\n+after\n*** End Patch");let params=if raw {serde_json::Value::String(input)} else {serde_json::json!({"input":input})};
            let result=(tool.execute)(maho_tools::definition::ToolCall{id:name,params,signal:Default::default(),context:Some(&context),on_update:None}).await.unwrap();
            assert_eq!(tokio::fs::read(&path).await.unwrap(),b"after\n");let details=result.details.unwrap();assert_eq!(details["result"]["failures"],serde_json::json!([]));assert_eq!(details["result"]["appliedFiles"],serde_json::json!([name]));
        }
    }
    #[test] fn retention_is_bounded_by_utf8_bytes() { assert!(retained_patch(Some(&"a".repeat(16*1024))).is_some()); assert!(retained_patch(Some(&"é".repeat(8193))).is_none()); assert!(retained_patch(None).is_none()); }
    #[tokio::test] async fn upstream_partial_and_complete_failures_keep_structured_recovery() {
        for partial in [false,true] {
            let directory=tempfile::tempdir().unwrap();let context=Context(directory.path().into());let path=directory.path().join("existing.txt");tokio::fs::write(&path,b"actual\n").await.unwrap();
            let add=if partial {"*** Add File: created.txt\n+created\n"} else {""};let input=format!("*** Begin Patch\n{add}*** Update File: existing.txt\n@@\n-expected\n+changed\n*** End Patch");
            let result=(create_apply_patch_tool().execute)(maho_tools::definition::ToolCall{id:"failure",params:serde_json::json!({"input":input}),signal:Default::default(),context:Some(&context),on_update:None}).await.unwrap();
            let details=result.details.unwrap();assert!(crate::extension::has_apply_patch_failures(&details));assert_eq!(details["result"]["failures"].as_array().unwrap().len(),1);assert_eq!(details["result"]["hasPartialSuccess"],partial);
            assert_eq!(details["result"]["recoveryInstructions"]["mustReadFiles"],serde_json::json!(["existing.txt"]));assert_eq!(tokio::fs::read(&path).await.unwrap(),b"actual\n");assert_eq!(directory.path().join("created.txt").exists(),partial);
        }
    }
    #[test] fn empty_application_has_no_preview() { let result=ApplyPatchResult::default(); assert_eq!(applied_preview(&result),None); let (_,details)=execution_result(result); assert!(details.result.is_some()); assert_eq!(details.preview,None); }
}
