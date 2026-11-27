//! Port of `packages/coding-agent/src/core/tools/edit-core.ts`.

use crate::definition::{
    AbortSignal, RenderShell, ToolContent, ToolContext, ToolDefinition, ToolError, ToolExecutor,
    ToolFuture, ToolResult,
};
use crate::edit_diff::{
    Edit, apply_edits_to_normalized_content, detect_line_ending, diff_lines,
    generate_diff_string_from_rows, normalize_to_lf, restore_line_endings,
};
use crate::file_tool_host::{
    ToolPathCanonicalizer, ToolPathResolver, default_posix_path_resolver, lexical_canonical_path,
    run_in_process_file_mutation_queue,
};
use crate::filesystem_policy::{
    FilesystemOperation, FilesystemPolicyChecker, check_filesystem_policy,
};
use crate::fs_error_code::{fs_error_code, io_error_fs_code, tool_error_from_machine};
use crate::tool_machine::ToolMachine;
use maho_machine_ops::{OpName, PathLockMode, ReadRequest, StatAccess, StatRequest, WriteRequest};
use serde::Deserialize;
use serde_json::{Value, json};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

pub trait EditOperations: Send + Sync {
    fn read_file<'a>(&'a self, path: &'a Path) -> ToolFuture<'a, Vec<u8>>;
    fn write_file<'a>(&'a self, path: &'a Path, content: &'a str) -> ToolFuture<'a, ()>;
    fn access<'a>(&'a self, path: &'a Path) -> ToolFuture<'a, ()>;
}

#[derive(Deserialize)]
pub struct EditToolInput {
    pub path: String,
    pub edits: Vec<Edit>,
}

pub type EditMutationQueueFn = Arc<
    dyn Fn(
            PathBuf,
            Box<
                dyn FnOnce() -> Pin<Box<dyn Future<Output = Result<ToolResult, ToolError>> + Send>>
                    + Send,
            >,
        ) -> Pin<Box<dyn Future<Output = Result<ToolResult, ToolError>> + Send>>
        + Send
        + Sync,
>;

#[derive(Clone)]
pub struct CoreEditToolOptions {
    pub machine: ToolMachine,
    pub operations: Option<Arc<dyn EditOperations>>,
    pub filesystem_policy: Option<FilesystemPolicyChecker>,
    pub resolve_path: Option<ToolPathResolver>,
    pub with_mutation_queue: Option<EditMutationQueueFn>,
    pub canonicalize_path: Option<ToolPathCanonicalizer>,
}

impl CoreEditToolOptions {
    pub fn new(machine: ToolMachine) -> Self {
        Self {
            machine,
            operations: None,
            filesystem_policy: None,
            resolve_path: None,
            with_mutation_queue: None,
            canonicalize_path: None,
        }
    }
}

pub fn prepare_edit_arguments(mut input: Value) -> Result<Value, ToolError> {
    let Some(args) = input.as_object_mut() else {
        return Ok(input);
    };
    let single = |v: &Value| {
        v.get("oldText").is_some_and(Value::is_string)
            && v.get("newText").is_some_and(Value::is_string)
    };
    if let Some(value) = args.get("edits").cloned() {
        let parsed = if let Some(text) = value.as_str() {
            serde_json::from_str(text).unwrap_or(value)
        } else {
            value
        };
        if parsed.is_array() {
            args.insert("edits".into(), parsed);
        } else if single(&parsed) {
            args.insert("edits".into(), json!([parsed]));
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
        edits.push(json!({"oldText":old,"newText":new}));
        args.insert("edits".into(), json!(edits));
    }
    Ok(input)
}

struct MachineEditOperations {
    machine: ToolMachine,
    signal: AbortSignal,
}

impl EditOperations for MachineEditOperations {
    fn read_file<'a>(&'a self, path: &'a Path) -> ToolFuture<'a, Vec<u8>> {
        Box::pin(async move {
            let ctx = self
                .machine
                .context(Some(&self.signal), Some(PathLockMode::Caller));
            let res = self
                .machine
                .ops()
                .files()
                .read(
                    ReadRequest {
                        path: path.to_string_lossy().into_owned(),
                        offset: None,
                        limit_bytes: None,
                    },
                    &ctx,
                )
                .await
                .map_err(tool_error_from_machine)?;
            Ok(res.bytes)
        })
    }

    fn write_file<'a>(&'a self, path: &'a Path, content: &'a str) -> ToolFuture<'a, ()> {
        Box::pin(async move {
            let ctx = self
                .machine
                .context(Some(&self.signal), Some(PathLockMode::Caller));
            self.machine
                .ops()
                .files()
                .write(
                    WriteRequest {
                        path: path.to_string_lossy().into_owned(),
                        bytes: content.as_bytes().to_vec(),
                        create_parent_dirs: false,
                        offset: None,
                        total: None,
                    },
                    &ctx,
                )
                .await
                .map(|_| ())
                .map_err(tool_error_from_machine)
        })
    }

    fn access<'a>(&'a self, path: &'a Path) -> ToolFuture<'a, ()> {
        Box::pin(async move {
            let ctx = self
                .machine
                .context(Some(&self.signal), Some(PathLockMode::Caller));
            self.machine
                .ops()
                .files()
                .stat(
                    StatRequest {
                        path: path.to_string_lossy().into_owned(),
                        access: Some(StatAccess::ReadWrite),
                    },
                    &ctx,
                )
                .await
                .map(|_| ())
                .map_err(|op_err| {
                    ToolError::Message(format!("Error code: {}", fs_error_code(&op_err)))
                })
        })
    }
}

fn default_mutation_queue() -> EditMutationQueueFn {
    Arc::new(|path, f| Box::pin(async move { run_in_process_file_mutation_queue(&path, f).await }))
}

pub fn create_core_edit_tool_definition(
    cwd: PathBuf,
    options: CoreEditToolOptions,
) -> ToolDefinition {
    let custom_ops = options.operations;
    let machine = options.machine;
    let policy = options.filesystem_policy;
    let resolve_path = options
        .resolve_path
        .unwrap_or_else(default_posix_path_resolver);
    let with_mutation_queue = options
        .with_mutation_queue
        .unwrap_or_else(default_mutation_queue);
    let canonicalize_path = options
        .canonicalize_path
        .unwrap_or_else(lexical_canonical_path);

    let execute: ToolExecutor = Arc::new(move |call| {
        let cwd = cwd.clone();
        let custom_ops = custom_ops.clone();
        let machine = machine.clone();
        let policy = policy.clone();
        let resolve_path = Arc::clone(&resolve_path);
        let with_mutation_queue = Arc::clone(&with_mutation_queue);
        let canonicalize_path = Arc::clone(&canonicalize_path);

        Box::pin(async move {
            let input: EditToolInput = serde_json::from_value(call.params)?;
            if input.edits.is_empty() {
                return Err(ToolError::Message(
                    "Edit tool input is invalid. edits must contain at least one replacement."
                        .into(),
                ));
            }
            let gate_args = json!({ "path": input.path, "edits": input.edits });
            if let Some(admitted) = machine.begin(OpName::FilesEdit, gate_args, Some(&call.signal))
            {
                admitted.await?;
            }

            let ops: Arc<dyn EditOperations> = custom_ops.unwrap_or_else(|| {
                Arc::new(MachineEditOperations {
                    machine: machine.clone(),
                    signal: call.signal.clone(),
                })
            });
            let base_cwd = call.context.map_or(cwd.as_path(), ToolContext::cwd);
            let path = resolve_path(&input.path, base_cwd);

            let fut_path = path.clone();
            with_mutation_queue(
                path,
                Box::new(move || {
                    Box::pin(async move {
                        call.signal.check()?;
                        if let Some(checker) = policy.as_ref() {
                            let canonical = canonicalize_path(fut_path.clone()).await?;
                            check_filesystem_policy(
                                Some(checker),
                                &canonical,
                                FilesystemOperation::Write,
                                "edit",
                            )
                            .await?;
                            call.signal.check()?;
                        }
                        if let Err(error) = ops.access(&fut_path).await {
                            call.signal.check()?;
                            let message = match &error {
                                ToolError::Io(io_err) => {
                                    format!("Error code: {}", io_error_fs_code(io_err))
                                }
                                other => other.to_string(),
                            };
                            return Err(ToolError::Message(format!(
                                "Could not edit file: {}. {message}.",
                                input.path
                            )));
                        }
                        call.signal.check()?;
                        let buffer = ops.read_file(&fut_path).await?;
                        call.signal.check()?;
                        let raw = String::from_utf8_lossy(&buffer);
                        let (bom, content) = match raw.strip_prefix('\u{feff}') {
                            Some(text) => ("\u{feff}", text),
                            None => ("", raw.as_ref()),
                        };
                        let ending = detect_line_ending(content);
                        let applied = apply_edits_to_normalized_content(
                            &normalize_to_lf(content),
                            &input.edits,
                            &input.path,
                        )?;
                        call.signal.check()?;
                        let final_content = format!(
                            "{bom}{}",
                            restore_line_endings(&applied.new_content, ending)
                        );
                        ops.write_file(&fut_path, &final_content).await?;
                        call.signal.check()?;
                        let rows = diff_lines(&applied.base_content, &applied.new_content);
                        let diff = generate_diff_string_from_rows(
                            &rows,
                            &applied.base_content,
                            &applied.new_content,
                            4,
                        );
                        let patch = crate::unified_diff::create_unified_patch_from_rows(
                            &input.path,
                            &input.path,
                            &rows,
                            4,
                        );
                        Ok(ToolResult {
                            content: vec![ToolContent::text(format!(
                                "Successfully replaced {} block(s) in {}.",
                                input.edits.len(),
                                input.path
                            ))],
                            details: Some(json!({
                                "diff": diff.diff,
                                "patch": patch,
                                "firstChangedLine": diff.first_changed_line,
                            })),
                            structured_content: None,
                            is_error: None,
                        })
                    })
                }),
            )
            .await
        })
    });

    let mut tool = ToolDefinition::new(
        "edit",
        "Edit a single file using exact text replacement. Every edits[].oldText must match a unique, non-overlapping region of the original file. If two changes affect the same block or nearby lines, merge them into one edit instead of emitting overlapping edits. Do not include large unchanged regions just to connect distant changes.",
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to the file to edit (relative or absolute)" },
                "edits": {
                    "type": "array",
                    "description": "One or more targeted replacements. Each edit is matched against the original file, not incrementally. Do not include overlapping or nested edits. If two changes touch the same block or nearby lines, merge them into one edit instead.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "oldText": { "type": "string", "description": "Exact text for one targeted replacement. It must be unique in the original file and must not overlap with any other edits[].oldText in the same call." },
                            "newText": { "type": "string", "description": "Replacement text for this targeted edit." }
                        },
                        "required": ["oldText", "newText"]
                    }
                }
            },
            "required": ["path", "edits"]
        }),
        execute,
    );
    tool.prepare_arguments = Some(Arc::new(prepare_edit_arguments));
    tool.render_shell = Some(RenderShell::Own);
    tool.prompt_snippet = Some("Make precise file edits with exact text replacement, including multiple disjoint edits in one call".into());
    tool.prompt_guidelines = Some(vec![
        "Use edit for precise changes (edits[].oldText must match exactly)".into(),
        "When changing multiple separate locations in one file, use one edit call with multiple entries in edits[] instead of multiple edit calls".into(),
        "Each edits[].oldText is matched against the original file, not after earlier edits are applied. Do not emit overlapping or nested edits. Merge nearby changes into one edit.".into(),
        "Keep edits[].oldText as small as possible while still being unique in the file. Do not pad with large unchanged regions.".into(),
    ]);
    tool.constrained_sampling = Some(maho_ai::types::ConstrainedSampling::Config(
        maho_ai::types::ConstrainedSamplingConfig::JsonSchema {
            strict: maho_ai::types::JsonSchemaStrictness::Prefer,
        },
    ));
    tool
}
