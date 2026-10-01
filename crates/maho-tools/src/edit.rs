use std::{path::{Path, PathBuf}, sync::Arc};
use serde::Deserialize;
use serde_json::{Value, json};
use crate::{definition::*, edit_diff::*, filesystem_policy::{FilesystemPolicyChecker, FilesystemOperation, check_filesystem_policy}};

pub trait EditOperations: Send + Sync {
    fn read_file<'a>(&'a self, path: &'a Path) -> ToolFuture<'a, Vec<u8>>;
    fn write_file<'a>(&'a self, path: &'a Path, content: &'a str) -> ToolFuture<'a, ()>;
    fn access<'a>(&'a self, path: &'a Path) -> ToolFuture<'a, ()>;
}
pub struct LocalEditOperations;
impl EditOperations for LocalEditOperations {
    fn read_file<'a>(&'a self, path: &'a Path) -> ToolFuture<'a, Vec<u8>> { Box::pin(async move { Ok(tokio::fs::read(path).await?) }) }
    fn write_file<'a>(&'a self, path: &'a Path, content: &'a str) -> ToolFuture<'a, ()> { Box::pin(async move { Ok(tokio::fs::write(path, content).await?) }) }
    fn access<'a>(&'a self, path: &'a Path) -> ToolFuture<'a, ()> { Box::pin(async move {
        tokio::fs::OpenOptions::new().read(true).write(true).open(path).await?; Ok(())
    }) }
}
#[derive(Clone, Default)]
pub struct EditToolOptions { pub operations: Option<Arc<dyn EditOperations>>, pub filesystem_policy: Option<FilesystemPolicyChecker> }
#[derive(Deserialize)]
pub struct EditToolInput { pub path: String, pub edits: Vec<Edit> }
pub fn prepare_edit_arguments(mut input: Value) -> Result<Value, ToolError> {
    let Some(args) = input.as_object_mut() else { return Ok(input); };
    let single = |v: &Value| v.get("oldText").is_some_and(Value::is_string) && v.get("newText").is_some_and(Value::is_string);
    if let Some(value) = args.get("edits").cloned() {
        let parsed = if let Some(text) = value.as_str() { serde_json::from_str(text).unwrap_or(value) } else { value };
        if parsed.is_array() { args.insert("edits".into(), parsed); }
        else if single(&parsed) { args.insert("edits".into(), json!([parsed])); }
    }
    if args.get("oldText").is_some_and(Value::is_string) && args.get("newText").is_some_and(Value::is_string) {
        let old = args.remove("oldText"); let new = args.remove("newText");
        let mut edits = args.remove("edits").and_then(|v| v.as_array().cloned()).unwrap_or_default();
        edits.push(json!({"oldText":old,"newText":new})); args.insert("edits".into(), json!(edits));
    }
    Ok(input)
}
pub fn create_edit_tool_definition(cwd: PathBuf, options: EditToolOptions) -> ToolDefinition {
    let ops = options.operations.unwrap_or_else(|| Arc::new(LocalEditOperations));
    let policy = options.filesystem_policy;
    let execute: ToolExecutor = Arc::new(move |call| {
        let cwd = cwd.clone(); let ops = Arc::clone(&ops); let policy = policy.clone();
        Box::pin(async move {
            let input: EditToolInput = serde_json::from_value(call.params)?;
            if input.edits.is_empty() { return Err(ToolError::Message("Edit tool input is invalid. edits must contain at least one replacement.".into())); }
            let path = crate::path_utils::resolve_to_cwd(&input.path, call.context.map_or(cwd.as_path(), ToolContext::cwd));
            let _guard = crate::file_mutation_queue::lock_file_mutation(&path).await?;
            call.signal.check()?;
            check_filesystem_policy(policy.as_ref(), &path, FilesystemOperation::Write, "edit").await?;
            call.signal.check()?;
            if let Err(error) = ops.access(&path).await {
                call.signal.check()?;
                let message = match &error {
                    ToolError::Io(error) => format!("Error code: {}", match error.kind() {
                        std::io::ErrorKind::NotFound => "ENOENT", std::io::ErrorKind::PermissionDenied => "EACCES",
                        std::io::ErrorKind::IsADirectory => "EISDIR", _ => "UNKNOWN",
                    }),
                    _ => error.to_string(),
                };
                return Err(ToolError::Message(format!("Could not edit file: {}. {message}.", input.path)));
            }
            call.signal.check()?;
            let buffer = ops.read_file(&path).await?; call.signal.check()?;
            let raw = String::from_utf8_lossy(&buffer);
            let (bom, content) = match raw.strip_prefix('\u{feff}') { Some(text) => ("\u{feff}", text), None => ("", raw.as_ref()) };
            let ending = detect_line_ending(content);
            let applied = apply_edits_to_normalized_content(&normalize_to_lf(content), &input.edits, &input.path)?;
            call.signal.check()?;
            let final_content = format!("{bom}{}", restore_line_endings(&applied.new_content, ending));
            ops.write_file(&path, &final_content).await?; call.signal.check()?;
            let diff = generate_diff_string(&applied.base_content, &applied.new_content, 4);
            let patch = generate_unified_patch(&input.path, &applied.base_content, &applied.new_content, 4);
            Ok(ToolResult { content: vec![ToolContent::text(format!("Successfully replaced {} block(s) in {}.", input.edits.len(), input.path))],
                details: Some(json!({"diff":diff.diff,"patch":patch,"firstChangedLine":diff.first_changed_line})) })
        })
    });
    let mut tool = ToolDefinition::new("edit", "Edit a single file using exact text replacement. Every edits[].oldText must match a unique, non-overlapping region of the original file. If two changes affect the same block or nearby lines, merge them into one edit instead of emitting overlapping edits. Do not include large unchanged regions just to connect distant changes.",
        json!({"type":"object","properties":{"path":{"type":"string"},"edits":{"type":"array","items":{"type":"object","properties":{"oldText":{"type":"string"},"newText":{"type":"string"}},"required":["oldText","newText"]}}},"required":["path","edits"]}), execute);
    tool.prepare_arguments = Some(Arc::new(prepare_edit_arguments));
    tool.render_shell = Some(RenderShell::Own);
    tool.prompt_snippet = Some("Make precise file edits with exact text replacement, including multiple disjoint edits in one call".into());
    tool.prompt_guidelines = Some(vec![
        "Use edit for precise changes (edits[].oldText must match exactly)".into(),
        "When changing multiple separate locations in one file, use one edit call with multiple entries in edits[] instead of multiple edit calls".into(),
        "Each edits[].oldText is matched against the original file, not after earlier edits are applied. Do not emit overlapping or nested edits. Merge nearby changes into one edit.".into(),
        "Keep edits[].oldText as small as possible while still being unique in the file. Do not pad with large unchanged regions.".into(),
    ]);
    tool.constrained_sampling = Some(maho_ai::types::ConstrainedSampling::Config(maho_ai::types::ConstrainedSamplingConfig::JsonSchema { strict: maho_ai::types::JsonSchemaStrictness::Prefer }));
    tool
}
