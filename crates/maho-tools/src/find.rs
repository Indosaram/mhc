use std::{path::{Path, PathBuf}, sync::Arc};
use serde::Deserialize;
use serde_json::json;
use crate::{definition::*, filesystem_policy::*, model_only_text::model_only_text, truncate::*};
pub trait FindOperations: Send + Sync {
    fn exists<'a>(&'a self, path: &'a Path) -> ToolFuture<'a, bool>;
    fn glob<'a>(&'a self, pattern: &'a str, cwd: &'a Path, ignore: &'a [&'a str], limit: usize) -> ToolFuture<'a, Vec<String>>;
}
#[derive(Clone, Default)]
pub struct FindToolOptions { pub operations: Option<Arc<dyn FindOperations>>, pub filesystem_policy: Option<FilesystemPolicyChecker> }
#[derive(Deserialize)]
pub struct FindToolInput { pub pattern: String, pub path: Option<String>, pub limit: Option<usize> }
pub fn relativize_find_result_path(result: &str, root: &Path) -> String {
    let path = Path::new(result);
    let relative = if path.is_absolute() { path.strip_prefix(root).unwrap_or(path) } else { path };
    let mut text = relative.to_string_lossy().replace(std::path::MAIN_SEPARATOR, "/");
    if result.ends_with(std::path::MAIN_SEPARATOR) && !text.ends_with('/') { text.push('/'); }
    text
}
pub fn create_find_tool_definition(cwd: PathBuf, options: FindToolOptions) -> ToolDefinition {
    let execute: ToolExecutor = Arc::new(move |call| {
        let cwd = cwd.clone(); let options = options.clone();
        Box::pin(async move {
            call.signal.check()?;
            let input: FindToolInput = serde_json::from_value(call.params)?;
            let root = crate::path_utils::resolve_to_cwd(input.path.as_deref().filter(|p| !p.is_empty()).unwrap_or("."), call.context.map_or(cwd.as_path(), ToolContext::cwd));
            let limit = input.limit.unwrap_or(1000);
            check_filesystem_policy(options.filesystem_policy.as_ref(), &root, FilesystemOperation::Enumerate, "find").await?;
            call.signal.check()?;
            let custom = options.operations.is_some();
            let results = if let Some(ops) = &options.operations {
                if !ops.exists(&root).await? { return Err(ToolError::Message(format!("Path not found: {}", root.display()))); }
                call.signal.check()?;
                let results = ops.glob(&input.pattern, &root, &["**/node_modules/**", "**/.git/**"], limit).await?;
                call.signal.check()?; results
            } else {
                let mut command = tokio::process::Command::new("fd");
                command.kill_on_drop(true).args(["--glob", "--color=never", "--hidden"]);
                let mut inside_git = false;
                for parent in root.ancestors() { if crate::path_utils::path_exists(&parent.join(".git")).await { inside_git = true; break; } }
                if !inside_git { command.arg("--no-require-git"); }
                command.args(["--max-results", &limit.to_string()]);
                let mut pattern = input.pattern;
                if pattern.contains('/') {
                    command.arg("--full-path");
                    if !pattern.starts_with('/') && !pattern.starts_with("**/") && pattern != "**" { pattern = format!("**/{pattern}"); }
                    #[cfg(windows)]
                    { pattern = pattern.replace('/', "[/\\]"); }
                }
                command.arg("--").arg(pattern).arg(&root);
                let output = tokio::select! {
                    result = command.output() => result.map_err(|e| ToolError::Message(format!("Failed to run fd: {e}")))?,
                    () = call.signal.cancelled() => return Err(ToolError::Aborted),
                };
                let text = String::from_utf8_lossy(&output.stdout);
                if !output.status.success() && text.is_empty() {
                    let error = String::from_utf8_lossy(&output.stderr).trim().to_string();
                    return Err(ToolError::Message(if error.is_empty() { format!("fd exited with code {}", output.status.code().map_or("null".into(), |n| n.to_string())) } else { error }));
                }
                text.lines().map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned).collect()
            };
            if results.is_empty() { return Ok(ToolResult::text("No files found matching pattern")); }
            let results: Vec<_> = results.iter().map(|r| relativize_find_result_path(r, &root)).collect();
            let limited = results.len() >= limit;
            let truncated = truncate_head(&results.join("\n"), TruncationOptions { max_lines: usize::MAX, ..Default::default() });
            let mut details = serde_json::Map::new(); let mut notices = Vec::new();
            if limited {
                details.insert("resultLimitReached".into(), json!(limit));
                notices.push(if custom { format!("{limit} results limit reached") } else { format!("{limit} results limit reached. Use limit={} for more, or refine pattern", limit.saturating_mul(2)) });
            }
            if truncated.truncated { details.insert("truncation".into(), json!(truncated)); notices.push(format!("{} limit reached", format_size(DEFAULT_MAX_BYTES))); }
            let mut content = vec![ToolContent::text(format!("{}{}", truncated.content, if notices.is_empty() { "" } else { "\n" }))];
            if !notices.is_empty() { content.push(model_only_text(format!("[{}]", notices.join(". ")))); }
            Ok(ToolResult { content, details: if details.is_empty() { None } else { Some(details.into()) } })
        })
    });
    let mut tool = ToolDefinition::new("find", "Search for files by glob pattern. Returns matching file paths relative to the search directory. Respects .gitignore. Output is truncated to 1000 results or 50KB (whichever is hit first).", json!({"type":"object","properties":{"pattern":{"type":"string"},"path":{"type":"string"},"limit":{"type":"number"}},"required":["pattern"]}), execute);
    tool.prompt_snippet = Some("Find files by glob pattern (respects .gitignore)".into()); tool
}
