use std::{path::{Path, PathBuf}, sync::Arc};
use serde::Deserialize;
use serde_json::json;
use crate::{definition::*, filesystem_policy::*, model_only_text::model_only_text, truncate::*};
pub trait LsOperations: Send + Sync {
    fn exists<'a>(&'a self, path: &'a Path) -> ToolFuture<'a, bool>;
    fn is_directory<'a>(&'a self, path: &'a Path) -> ToolFuture<'a, bool>;
    fn readdir<'a>(&'a self, path: &'a Path) -> ToolFuture<'a, Vec<String>>;
}
pub struct LocalLsOperations;
impl LsOperations for LocalLsOperations {
    fn exists<'a>(&'a self, path: &'a Path) -> ToolFuture<'a, bool> { Box::pin(async move { Ok(tokio::fs::metadata(path).await.is_ok()) }) }
    fn is_directory<'a>(&'a self, path: &'a Path) -> ToolFuture<'a, bool> { Box::pin(async move { Ok(tokio::fs::metadata(path).await?.is_dir()) }) }
    fn readdir<'a>(&'a self, path: &'a Path) -> ToolFuture<'a, Vec<String>> { Box::pin(async move {
        let mut entries = tokio::fs::read_dir(path).await?; let mut names = Vec::new();
        while let Some(entry) = entries.next_entry().await? { names.push(entry.file_name().to_string_lossy().into_owned()); } Ok(names)
    }) }
}
#[derive(Default, Clone)]
pub struct LsToolOptions { pub operations: Option<Arc<dyn LsOperations>>, pub filesystem_policy: Option<FilesystemPolicyChecker> }
#[derive(Deserialize)]
pub struct LsToolInput { pub path: Option<String>, pub limit: Option<usize> }
pub fn create_ls_tool_definition(cwd: PathBuf, options: LsToolOptions) -> ToolDefinition {
    let ops = options.operations.unwrap_or_else(|| Arc::new(LocalLsOperations)); let policy = options.filesystem_policy;
    let execute: ToolExecutor = Arc::new(move |call| {
        let cwd = cwd.clone(); let ops = Arc::clone(&ops); let policy = policy.clone();
        Box::pin(async move {
            call.signal.check()?;
            let input: LsToolInput = serde_json::from_value(call.params)?;
            let path = crate::path_utils::resolve_to_cwd(input.path.as_deref().filter(|p| !p.is_empty()).unwrap_or("."), call.context.map_or(cwd.as_path(), ToolContext::cwd));
            let limit = input.limit.unwrap_or(500);
            let work = async {
                check_filesystem_policy(policy.as_ref(), &path, FilesystemOperation::Enumerate, "ls").await?;
                if !ops.exists(&path).await? { return Err(ToolError::Message(format!("Path not found: {}", path.display()))); }
                if !ops.is_directory(&path).await? { return Err(ToolError::Message(format!("Not a directory: {}", path.display()))); }
                let mut entries = ops.readdir(&path).await.map_err(|e| ToolError::Message(format!("Cannot read directory: {e}")))?;
                entries.sort_by_key(|a| a.to_lowercase());
                let mut results = Vec::new(); let mut limited = false;
                for entry in entries {
                    if results.len() >= limit { limited = true; break; }
                    if let Ok(directory) = ops.is_directory(&path.join(&entry)).await { results.push(format!("{entry}{}", if directory { "/" } else { "" })); }
                }
                if results.is_empty() { return Ok(ToolResult::text("(empty directory)")); }
                let truncated = truncate_head(&results.join("\n"), TruncationOptions { max_lines: usize::MAX, ..Default::default() });
                let mut details = serde_json::Map::new(); let mut notices = Vec::new();
                if limited { details.insert("entryLimitReached".into(), json!(limit)); notices.push(format!("{limit} entries limit reached. Use limit={} for more", limit.saturating_mul(2))); }
                if truncated.truncated { details.insert("truncation".into(), json!(truncated)); notices.push(format!("{} limit reached", format_size(DEFAULT_MAX_BYTES))); }
                let mut content = vec![ToolContent::text(format!("{}{}", truncated.content, if notices.is_empty() { "" } else { "\n" }))];
                if !notices.is_empty() { content.push(model_only_text(format!("[{}]", notices.join(". ")))); }
                Ok(ToolResult { content, details: if details.is_empty() { None } else { Some(details.into()) } })
            };
            tokio::select! { result = work => result, () = call.signal.cancelled() => Err(ToolError::Aborted) }
        })
    });
    let mut tool = ToolDefinition::new("ls", "List directory contents. Returns entries sorted alphabetically, with '/' suffix for directories. Includes dotfiles. Output is truncated to 500 entries or 50KB (whichever is hit first).", json!({"type":"object","properties":{"path":{"type":"string"},"limit":{"type":"number"}}}), execute);
    tool.prompt_snippet = Some("List directory contents".into()); tool
}
