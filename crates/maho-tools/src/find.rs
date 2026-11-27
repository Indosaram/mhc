use crate::{
    definition::*,
    filesystem_policy::*,
    fs_error_code::tool_error_from_machine,
    machine::{ToolMachine, default_tool_machine},
    model_only_text::model_only_text,
    truncate::*,
};
use maho_machine_ops::{GlobRequest, OpName};
use serde::Deserialize;
use serde_json::json;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
pub trait FindOperations: Send + Sync {
    fn exists<'a>(&'a self, path: &'a Path) -> ToolFuture<'a, bool>;
    fn glob<'a>(
        &'a self,
        pattern: &'a str,
        cwd: &'a Path,
        ignore: &'a [&'a str],
        limit: usize,
    ) -> ToolFuture<'a, Vec<String>>;
}
#[derive(Clone, Default)]
pub struct FindToolOptions {
    pub operations: Option<Arc<dyn FindOperations>>,
    pub filesystem_policy: Option<FilesystemPolicyChecker>,
    pub machine: Option<ToolMachine>,
}
#[derive(Deserialize)]
pub struct FindToolInput {
    pub pattern: String,
    pub path: Option<String>,
    #[serde(default, deserialize_with = "crate::json_usize::option_usize")]
    pub limit: Option<usize>,
}
pub fn relativize_find_result_path(result: &str, root: &Path) -> String {
    let path = Path::new(result);
    let relative = if path.is_absolute() {
        path.strip_prefix(root).unwrap_or(path)
    } else {
        path
    };
    let mut text = relative
        .to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/");
    if result.ends_with(std::path::MAIN_SEPARATOR) && !text.ends_with('/') {
        text.push('/');
    }
    text
}
pub fn create_find_tool_definition(cwd: PathBuf, options: FindToolOptions) -> ToolDefinition {
    let server_pinned = options.operations.is_some() || options.filesystem_policy.is_some();
    let machine = options.machine.unwrap_or_else(default_tool_machine);
    let custom_ops = options.operations;
    let policy = options.filesystem_policy;
    let execute: ToolExecutor = Arc::new(move |call| {
        let cwd = cwd.clone();
        let custom_ops = custom_ops.clone();
        let machine = machine.clone();
        let policy = policy.clone();
        Box::pin(async move {
            call.signal.check()?;
            let input: FindToolInput = serde_json::from_value(call.params)?;
            let gate_args = json!({
                "pattern": input.pattern,
                "path": input.path,
                "limit": input.limit,
            });
            if let Some(admitted) = machine.begin(OpName::FilesGlob, gate_args, Some(&call.signal))
            {
                admitted.await?;
            }
            let root = crate::path_utils::resolve_to_cwd(
                input
                    .path
                    .as_deref()
                    .filter(|p| !p.is_empty())
                    .unwrap_or("."),
                call.context.map_or(cwd.as_path(), ToolContext::cwd),
            );
            let limit = input.limit.unwrap_or(1000);
            check_filesystem_policy(
                policy.as_ref(),
                &root,
                FilesystemOperation::Enumerate,
                "find",
            )
            .await?;
            call.signal.check()?;
            let custom = custom_ops.is_some();
            let results = if let Some(ops) = &custom_ops {
                if !ops.exists(&root).await? {
                    return Err(ToolError::Message(format!(
                        "Path not found: {}",
                        root.display()
                    )));
                }
                call.signal.check()?;
                let results = ops
                    .glob(
                        &input.pattern,
                        &root,
                        &["**/node_modules/**", "**/.git/**"],
                        limit,
                    )
                    .await?;
                call.signal.check()?;
                results
            } else {
                let ctx = machine.context(Some(&call.signal), None);
                let glob_res = machine
                    .ops()
                    .files()
                    .glob(
                        GlobRequest {
                            pattern: input.pattern,
                            cwd: Some(root.to_string_lossy().into_owned()),
                            limit: Some(limit),
                        },
                        &ctx,
                    )
                    .await
                    .map_err(tool_error_from_machine)?;
                call.signal.check()?;
                glob_res
                    .entries
                    .into_iter()
                    .map(|e| e.path.to_string_lossy().into_owned())
                    .collect()
            };
            if results.is_empty() {
                return Ok(ToolResult::text("No files found matching pattern"));
            }
            let results: Vec<_> = results
                .iter()
                .map(|r| relativize_find_result_path(r, &root))
                .collect();
            let limited = results.len() >= limit;
            let truncated = truncate_head(
                &results.join("\n"),
                TruncationOptions {
                    max_lines: usize::MAX,
                    ..Default::default()
                },
            );
            let mut details = serde_json::Map::new();
            let mut notices = Vec::new();
            if limited {
                details.insert("resultLimitReached".into(), json!(limit));
                notices.push(if custom {
                    format!("{limit} results limit reached")
                } else {
                    format!(
                        "{limit} results limit reached. Use limit={} for more, or refine pattern",
                        limit.saturating_mul(2)
                    )
                });
            }
            if truncated.truncated {
                details.insert("truncation".into(), json!(truncated));
                notices.push(format!("{} limit reached", format_size(DEFAULT_MAX_BYTES)));
            }
            let mut content = vec![ToolContent::text(format!(
                "{}{}",
                truncated.content,
                if notices.is_empty() { "" } else { "\n" }
            ))];
            if !notices.is_empty() {
                content.push(model_only_text(format!("[{}]", notices.join(". "))));
            }
            Ok(ToolResult {
                content,
                details: if details.is_empty() {
                    None
                } else {
                    Some(details.into())
                },
                structured_content: None,
                is_error: None,
            })
        })
    });
    let mut tool = ToolDefinition::new(
        "find",
        "Search for files by glob pattern. Returns matching file paths relative to the search directory. Respects .gitignore. Output is truncated to 1000 results or 50KB (whichever is hit first).",
        json!({"type":"object","properties":{"pattern":{"type":"string","description":"Glob pattern to match files, e.g. '*.ts', '**/*.json', or 'src/**/*.spec.ts'"},"path":{"type":"string","description":"Directory to search in (default: current directory)"},"limit":{"type":"number","description":"Maximum number of results (default: 1000)"}},"required":["pattern"]}),
        execute,
    );
    tool.workspace_effect = Some(crate::definition::ToolWorkspaceEffect::Read);
    tool.prompt_snippet = Some("Find files by glob pattern (respects .gitignore)".into());
    if server_pinned {
        tool.server_pinned = Some(true);
    }
    tool
}

#[cfg(test)]
mod find_input_tests {
    use super::*;
    fn parse(extra: serde_json::Value) -> Result<FindToolInput, serde_json::Error> {
        let mut params = serde_json::json!({"pattern": "*"});
        if let (Some(target), Some(source)) = (params.as_object_mut(), extra.as_object()) {
            target.extend(source.clone());
        }
        serde_json::from_value(params)
    }
    #[test]
    fn limit_accepts_integral_numbers_and_rejects_others() {
        assert_eq!(
            parse(serde_json::json!({"limit": 100.0}))
                .expect("100.0")
                .limit,
            Some(100)
        );
        assert_eq!(
            parse(serde_json::json!({"limit": 7})).expect("7").limit,
            Some(7)
        );
        assert!(parse(serde_json::json!({"limit": 1.5})).is_err());
        assert!(parse(serde_json::json!({"limit": -1})).is_err());
    }
}
