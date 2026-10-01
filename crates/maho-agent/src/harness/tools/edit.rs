use super::{
    edit_diff::{
        Edit, apply_edits_to_normalized_content, detect_line_ending, generate_diff_string,
        generate_unified_patch, normalize_to_lf, restore_line_endings, strip_bom,
    },
    file_mutation_queue::with_file_mutation_queue,
    path_utils::resolve_tool_path,
    post_mutate::{append_post_mutate_note, run_post_mutate},
    tool_context::{HasExecutionToolContext, MutationTool, PostMutateContext},
};
use crate::{
    harness::types::{AgentHarnessTool, FileKind},
    types::AgentToolResult,
};
use maho_ai::types::Tool;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditToolInput {
    pub path: String,
    pub edits: Vec<Edit>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditToolDetails {
    pub diff: String,
    pub patch: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_changed_line: Option<usize>,
}
fn is_single_edit(value: &Value) -> bool {
    value.is_object()
        && value.get("oldText").is_some_and(Value::is_string)
        && value.get("newText").is_some_and(Value::is_string)
}
pub fn prepare_edit_arguments(mut input: Value) -> Value {
    let Some(args) = input.as_object_mut() else {
        return input;
    };
    if let Some(edits) = args.get_mut("edits") {
        if let Some(text) = edits.as_str() {
            if let Ok(parsed) = serde_json::from_str::<Value>(text) {
                if parsed.is_array() {
                    *edits = parsed;
                } else if is_single_edit(&parsed) {
                    *edits = json!([parsed]);
                }
            }
        } else if is_single_edit(edits) {
            *edits = json!([edits.clone()]);
        }
    }
    if args.get("oldText").is_some_and(Value::is_string)
        && args.get("newText").is_some_and(Value::is_string)
    {
        let old = args.remove("oldText");
        let new = args.remove("newText");
        let mut edits = args
            .remove("edits")
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default();
        edits.push(json!({"oldText": old, "newText": new}));
        args.insert("edits".into(), json!(edits));
    }
    input
}
pub fn create_edit_tool<T: HasExecutionToolContext>() -> AgentHarnessTool<T> {
    AgentHarnessTool {
        label: "edit".into(), prepare_arguments: Some(Arc::new(prepare_edit_arguments)),
        tool: Tool { name: "edit".into(), description: "Edit a single file using exact text replacement. Every edits[].oldText must match a unique, non-overlapping region of the original file. If two changes affect the same block or nearby lines, merge them into one edit instead of emitting overlapping edits. Do not include large unchanged regions just to connect distant changes.".into(), parameters: json!({"type":"object","properties":{"path":{"type":"string","description":"Path to the file to edit (relative or absolute)"},"edits":{"type":"array","description":"One or more targeted replacements. Each edit is matched against the original file, not incrementally. Do not include overlapping or nested edits. If two changes touch the same block or nearby lines, merge them into one edit instead.","items":{"type":"object","properties":{"oldText":{"type":"string","description":"Exact text for one targeted replacement. It must be unique in the original file and must not overlap with any other edits[].oldText in the same call."},"newText":{"type":"string","description":"Replacement text for this targeted edit."}},"required":["oldText","newText"]}}},"required":["path","edits"]}), freeform: None, constrained_sampling: None },
        execute: Arc::new(|_, input, _, turn: T, _, context| Box::pin(async move {
            if input.get("edits").and_then(Value::as_array).is_none_or(|edits| edits.is_empty()) { return Err("Edit tool input is invalid. edits must contain at least one replacement.".into()); }
            let input: EditToolInput = serde_json::from_value(input).map_err(|e| e.to_string())?;
            let tool_context = turn.execution_tool_context(); let env = &tool_context.env;
            let absolute = resolve_tool_path(env.as_ref(), &input.path, &context).await?;
            with_file_mutation_queue(env, &absolute, || async {
                if context.is_aborted() { return Err("Operation aborted".into()); }
                let access_error = |e: crate::harness::types::FileError| format!("Could not edit file: {}. Error code: {}.", input.path, e.code.as_str());
                let info = env.file_info(&absolute, &context).await.map_err(access_error)?;
                if !matches!(info.kind, FileKind::File | FileKind::Symlink) { return Err(format!("Could not edit file: {}. Path is not a file.", input.path)); }
                let content = env.read_text_file(&absolute, &context).await.map_err(access_error)?;
                if context.is_aborted() { return Err("Operation aborted".into()); }
                let (bom, content) = strip_bom(&content); let ending = detect_line_ending(content);
                let applied = apply_edits_to_normalized_content(&normalize_to_lf(content), &input.edits, &input.path)?;
                if context.is_aborted() { return Err("Operation aborted".into()); }
                let final_content = format!("{bom}{}", restore_line_endings(&applied.new_content, ending));
                env.write_file(&absolute, final_content.as_bytes(), &context).await.map_err(access_error)?;
                if context.is_aborted() { return Err("Operation aborted".into()); }
                let outcome = run_post_mutate(tool_context.post_mutate.as_ref(), PostMutateContext { tool: MutationTool::Edit, path: absolute.clone(), signal: context.abort_signal() }).await;
                if context.is_aborted() { return Err("Operation aborted".into()); }
                let mut committed = applied.new_content; let mut reread_note = None;
                if outcome.file_may_have_changed {
                    match env.read_text_file(&absolute, &context).await {
                        Ok(content) => committed = normalize_to_lf(strip_bom(&content).1),
                        Err(error) => reread_note = Some(format!("postMutate left the file unreadable: {}. Reported diff describes the edit before the hook ran.", error.code.as_str())),
                    }
                }
                let diff = generate_diff_string(&applied.base_content, &committed, 4);
                let mut result = AgentToolResult::text(append_post_mutate_note(&format!("Successfully replaced {} block(s) in {}.", input.edits.len(), input.path), &[outcome.note, reread_note]));
                result.details = serde_json::to_value(EditToolDetails { diff: diff.diff, patch: generate_unified_patch(&input.path, &applied.base_content, &committed, 4), first_changed_line: diff.first_changed_line }).map_err(|e| e.to_string())?;
                Ok(result)
            }, &context).await
        })),
    }
}
