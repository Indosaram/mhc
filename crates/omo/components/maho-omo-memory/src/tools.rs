use std::sync::Arc;
use maho_ext_api::{ExtensionApi, ToolDefinition, ToolError, ToolExecutionMode, ToolResult};
use memory_core::tools::{memory::{MemoryToolParams, run_memory_tool}, memory_apply_patch::{MemoryApplyPatchParams, run_memory_apply_patch}};
use crate::{context::MemoryIdentityContext, engine_session::{MemoryEngineSessionOptions, prepare_memory_engine_session}, tool_metadata::*};

pub type MemoryContextResolver = Arc<dyn Fn() -> Option<MemoryIdentityContext> + Send + Sync>;
pub type MemoryCommitNotice = Arc<dyn Fn(memory_core::tools::memory::MemoryToolCommit) + Send + Sync>;
#[derive(Clone, Default)]
pub struct MemoryToolsOptions {
    pub lock_wait_timeout_ms: Option<u64>,
    pub lock_retry_delay_ms: Option<u64>,
    pub on_commit: Option<MemoryCommitNotice>,
}

fn result(message: String) -> ToolResult {
    let mut result = ToolResult::text(&message);
    result.details = Some(serde_json::json!({"message":message}));
    result
}

pub fn create_memory_tools(resolve: MemoryContextResolver) -> [ToolDefinition; 2] {
    create_memory_tools_with_options(resolve, MemoryToolsOptions::default())
}

pub fn create_memory_tools_with_options(resolve: MemoryContextResolver, options: MemoryToolsOptions) -> [ToolDefinition; 2] {
    let memory_resolve = Arc::clone(&resolve);
    let memory_options = options.clone();
    let mut memory = ToolDefinition::new(MEMORY_TOOL_NAME, MEMORY_TOOL_DESCRIPTION, serde_json::json!({"type":"object","required":["command","reason"],"properties":{"command":{"enum":["create","str_replace","insert","delete","rename","update_description"]},"reason":{"type":"string"},"file_path":{"type":"string"},"old_path":{"type":"string"},"new_path":{"type":"string"},"old_string":{"type":"string"},"new_string":{"type":"string"},"insert_line":{"type":"number"},"insert_text":{"type":"string"},"description":{"type":"string"},"file_text":{"type":"string"}}}), Arc::new(move |call| {
        let resolve = Arc::clone(&memory_resolve);
        let options = memory_options.clone();
        Box::pin(async move {
            let context = resolve().ok_or_else(|| ToolError::Message("memory: no memory identity bound to this session; enable omo memory and restart the session so the memory tools can initialize".into()))?;
            let engine = prepare_memory_engine_session(&context.identity, &context.identity_paths, MemoryEngineSessionOptions { lock_wait_timeout_ms: options.lock_wait_timeout_ms, lock_retry_delay_ms: options.lock_retry_delay_ms }).map_err(|e| ToolError::Message(e.message))?;
            let params: MemoryToolParams = serde_json::from_value(call.params)?;
            engine.lock.run("memory-write", || run_memory_tool(&engine.repo, &engine.author, &params, None)).map(|value| { if let (Some(commit), Some(notice)) = (value.commit, options.on_commit) { notice(commit); } result(value.result) }).map_err(|e| ToolError::Message(e.message))
        })
    }));
    memory.label = "Memory".into();
    memory.execution_mode = Some(ToolExecutionMode::Sequential);
    memory.prompt_snippet = Some("memory - edit omo memory blocks (create/str_replace/insert/delete/rename/update_description); auto-commits each change".into());
    memory.prompt_guidelines = Some(vec!["Record durable facts, preferences, and decisions with the memory tool as you learn them; every change is committed with the reason you provide.".into(), "Memory files are markdown with YAML frontmatter; keep each block's description accurate because the memory index surfaces it.".into(), "When creating, renaming, or deleting memory files, update [[path]] references in other memory files so they stay discoverable.".into()]);
    let mut patch = ToolDefinition::new(MEMORY_APPLY_PATCH_TOOL_NAME, MEMORY_APPLY_PATCH_DESCRIPTION, serde_json::json!({"type":"object","required":["reason","input"],"properties":{"reason":{"type":"string"},"input":{"type":"string"}}}), Arc::new(move |call| {
        let resolve = Arc::clone(&resolve);
        let options = options.clone();
        Box::pin(async move {
            let context = resolve().ok_or_else(|| ToolError::Message("memory_apply_patch: no memory identity bound to this session; enable omo memory and restart the session so the memory tools can initialize".into()))?;
            let engine = prepare_memory_engine_session(&context.identity, &context.identity_paths, MemoryEngineSessionOptions { lock_wait_timeout_ms: options.lock_wait_timeout_ms, lock_retry_delay_ms: options.lock_retry_delay_ms }).map_err(|e| ToolError::Message(e.message))?;
            let reason = call.params.get("reason").and_then(serde_json::Value::as_str).ok_or_else(|| ToolError::Message("memory_apply_patch: reason must be a string".into()))?;
            let input = call.params.get("input").and_then(serde_json::Value::as_str).ok_or_else(|| ToolError::Message("memory_apply_patch: input must be a string".into()))?;
            let params = MemoryApplyPatchParams { reason: reason.into(), input: input.into(), author: engine.author.clone(), provenance: None };
            engine.lock.run("memory-write", || run_memory_apply_patch(&engine.repo, &params, None)).map(|value| { if let (Some(commit), Some(notice)) = (value.commit, options.on_commit) { notice(commit); } result(value.message) }).map_err(|e| ToolError::Message(e.message))
        })
    }));
    patch.label = "Memory Apply Patch".into();
    patch.execution_mode = Some(ToolExecutionMode::Sequential);
    patch.prompt_snippet = Some("memory_apply_patch - apply a codex-style patch to omo memory files; auto-commits the change".into());
    patch.prompt_guidelines = Some(vec!["Use memory_apply_patch for multi-file or multi-hunk memory edits; prefer the memory tool for single-block changes.".into(), "Patches may only target paths inside the memory repo, and read_only memory files cannot be modified.".into()]);
    [memory, patch]
}

pub fn register_memory_tools(api: &mut ExtensionApi, resolve: MemoryContextResolver) {
    for tool in create_memory_tools(resolve) { api.register_tool(tool); }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn call(params: serde_json::Value) -> maho_ext_api::ToolCall<'static> { maho_ext_api::ToolCall { id: "call", params, signal: Default::default(), on_update: None, context: None } }
    #[test] fn definitions_are_sequential_with_schemas() { let tools = create_memory_tools(Arc::new(|| None)); assert_eq!(tools[0].name, "memory"); assert_eq!(tools[1].name, "memory_apply_patch"); for tool in tools { assert_eq!(tool.execution_mode, Some(ToolExecutionMode::Sequential)); assert!(tool.parameters.is_object()); assert!(tool.prompt_snippet.is_some()); assert!(!tool.prompt_guidelines.unwrap().is_empty()); } }
    #[tokio::test] async fn unbound_tools_fail_before_storage() { for tool in create_memory_tools(Arc::new(|| None)) { assert!((tool.execute)(call(serde_json::json!({}))).await.is_err()); } }
    #[tokio::test] async fn real_create_and_patch_commit_clean_repo() {
        let root = tempfile::tempdir().unwrap();
        let paths = memory_core::identity::layout::build_identity_paths(root.path(), "agent");
        let context = MemoryIdentityContext::new("agent".into(), paths.clone(), crate::binding::MemorySessionBinding { identity: "agent".into(), repo_path_hash: "hash".into(), bound_at: 0.0 });
        let commits = Arc::new(std::sync::Mutex::new(Vec::new()));
        let notices = Arc::clone(&commits);
        let [memory, patch] = create_memory_tools_with_options(Arc::new(move || Some(context.clone())), MemoryToolsOptions { on_commit: Some(Arc::new(move |commit| notices.lock().unwrap().push(commit))), ..Default::default() });
        let created = (memory.execute)(call(serde_json::json!({"command":"create","reason":"Track fact","file_path":"reference/fact.md","description":"Fact","file_text":"before"}))).await.unwrap();
        assert_eq!(created.details.as_ref().unwrap()["message"], match &created.content[0] { maho_ext_api::ToolContent::Text { text, .. } => text.as_str(), _ => panic!("expected text") });
        (patch.execute)(call(serde_json::json!({"reason":"Update fact","input":"*** Begin Patch\n*** Update File: reference/fact.md\n@@\n-before\n+after\n*** End Patch"}))).await.unwrap();
        assert!(std::fs::read_to_string(paths.repo.join("reference/fact.md")).unwrap().contains("after"));
        let repo = memory_core::git::GitMemoryRepo::open(&paths.repo, "agent").unwrap();
        repo.clean_check().unwrap();
        assert!(!memory_core::locks::memory_writer_lock_path(&paths.locks).exists());
        assert_eq!(commits.lock().unwrap().len(), 2);
        assert_eq!(repo.head().unwrap().unwrap(), commits.lock().unwrap()[1].sha);
        assert!((memory.execute)(call(serde_json::json!({"command":"delete","reason":"missing","file_path":"missing.md"}))).await.is_err());
        assert_eq!(commits.lock().unwrap().len(), 2);
    }
}
