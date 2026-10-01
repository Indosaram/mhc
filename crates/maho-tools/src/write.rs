use std::{path::{Path, PathBuf}, sync::Arc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use crate::{definition::*, filesystem_policy::{FilesystemPolicyChecker, FilesystemOperation, check_filesystem_policy}};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct WriteToolDetails { pub operation: &'static str, pub patch: String }
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WriteBaseline { Missing, Present { content: String }, Unavailable }
pub async fn read_local_write_baseline(path: &Path) -> WriteBaseline {
    match tokio::fs::read(path).await {
        Ok(content) => WriteBaseline::Present { content: String::from_utf8_lossy(&content).into_owned() },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => WriteBaseline::Missing,
        Err(_) => WriteBaseline::Unavailable,
    }
}
pub fn create_write_details(path: &str, content: &str, baseline: WriteBaseline) -> Option<WriteToolDetails> {
    match baseline {
        WriteBaseline::Missing => Some(WriteToolDetails { operation: "add", patch: crate::unified_diff::create_unified_patch("/dev/null", path, "", content, 4) }),
        WriteBaseline::Present { content: old } => if old == content { None } else { Some(WriteToolDetails { operation: "update", patch: crate::unified_diff::create_unified_patch(path, path, &old, content, 4) }) },
        WriteBaseline::Unavailable => None,
    }
}
pub trait WriteOperations: Send + Sync {
    fn write_file<'a>(&'a self, path: &'a Path, content: &'a str) -> ToolFuture<'a, ()>;
    fn mkdir<'a>(&'a self, path: &'a Path) -> ToolFuture<'a, ()>;
}
pub struct LocalWriteOperations;
impl WriteOperations for LocalWriteOperations {
    fn write_file<'a>(&'a self, path: &'a Path, content: &'a str) -> ToolFuture<'a, ()> { Box::pin(async move { Ok(tokio::fs::write(path, content).await?) }) }
    fn mkdir<'a>(&'a self, path: &'a Path) -> ToolFuture<'a, ()> { Box::pin(async move { Ok(tokio::fs::create_dir_all(path).await?) }) }
}
#[derive(Clone, Default)]
pub struct WriteToolOptions { pub operations: Option<Arc<dyn WriteOperations>>, pub filesystem_policy: Option<FilesystemPolicyChecker> }
#[derive(Deserialize)]
pub struct WriteToolInput { pub path: String, pub content: String }
pub fn create_write_tool_definition(cwd: PathBuf, options: WriteToolOptions) -> ToolDefinition {
    let local = options.operations.is_none();
    let ops = options.operations.unwrap_or_else(|| Arc::new(LocalWriteOperations)); let policy = options.filesystem_policy;
    let execute: ToolExecutor = Arc::new(move |call| {
        let cwd = cwd.clone(); let ops = Arc::clone(&ops); let policy = policy.clone();
        Box::pin(async move {
            let input: WriteToolInput = serde_json::from_value(call.params)?;
            let path = crate::path_utils::resolve_to_cwd(&input.path, call.context.map_or(cwd.as_path(), ToolContext::cwd));
            let _guard = crate::file_mutation_queue::lock_file_mutation(&path).await?;
            call.signal.check()?;
            check_filesystem_policy(policy.as_ref(), &path, FilesystemOperation::Write, "write").await?;
            call.signal.check()?;
            let baseline = if local { read_local_write_baseline(&path).await } else { WriteBaseline::Unavailable };
            call.signal.check()?;
            if let Some(parent) = path.parent() { ops.mkdir(parent).await?; }
            call.signal.check()?; ops.write_file(&path, &input.content).await?; call.signal.check()?;
            Ok(ToolResult { content: vec![ToolContent::text(format!("Successfully wrote to {}", input.path))], details: create_write_details(&input.path, &input.content, baseline).map(serde_json::to_value).transpose()? })
        })
    });
    let mut tool = ToolDefinition::new("write", "Write content to a file. Creates the file if it doesn't exist, overwrites if it does. Automatically creates parent directories.", json!({"type":"object","properties":{"path":{"type":"string"},"content":{"type":"string"}},"required":["path","content"]}), execute);
    tool.prompt_snippet = Some("Create or overwrite files".into());
    tool.prompt_guidelines = Some(vec!["Use write only for new files or complete rewrites.".into()]);
    tool.constrained_sampling = Some(maho_ai::types::ConstrainedSampling::Config(maho_ai::types::ConstrainedSamplingConfig::JsonSchema { strict: maho_ai::types::JsonSchemaStrictness::Prefer }));
    tool
}
