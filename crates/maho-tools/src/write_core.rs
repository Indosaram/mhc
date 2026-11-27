//! Port of `packages/coding-agent/src/core/tools/write-core.ts`.

use crate::definition::{
    AbortSignal, ToolContent, ToolContext, ToolDefinition, ToolError, ToolExecutor, ToolFuture,
    ToolResult,
};
use crate::file_tool_host::{
    ToolPathCanonicalizer, ToolPathResolver, default_posix_path_resolver, lexical_canonical_path,
    posix_dirname, run_in_process_file_mutation_queue,
};
use crate::filesystem_policy::{
    FilesystemOperation, FilesystemPolicyChecker, check_filesystem_policy,
};
use crate::fs_error_code::{machine_error_fs_code, tool_error_from_machine};
use crate::tool_machine::ToolMachine;
use maho_machine_ops::{OpName, PathLockMode, ReadRequest, WriteRequest};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct WriteToolDetails {
    pub operation: &'static str,
    pub patch: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WriteBaseline {
    Missing,
    Present { content: String },
    Unavailable,
}

pub fn create_write_details(
    path: &str,
    content: &str,
    baseline: WriteBaseline,
) -> Option<WriteToolDetails> {
    match baseline {
        WriteBaseline::Missing => Some(WriteToolDetails {
            operation: "add",
            patch: crate::unified_diff::create_unified_patch("/dev/null", path, "", content, 4),
        }),
        WriteBaseline::Present { content: old } => {
            if old == content {
                None
            } else {
                Some(WriteToolDetails {
                    operation: "update",
                    patch: crate::unified_diff::create_unified_patch(path, path, &old, content, 4),
                })
            }
        }
        WriteBaseline::Unavailable => None,
    }
}

pub trait WriteOperations: Send + Sync {
    fn write_file<'a>(&'a self, path: &'a Path, content: &'a str) -> ToolFuture<'a, ()>;
    fn mkdir<'a>(&'a self, path: &'a Path) -> ToolFuture<'a, ()>;
}

#[derive(Deserialize)]
pub struct WriteToolInput {
    pub path: String,
    pub content: String,
}

pub type WriteBaselineReader =
    Arc<dyn Fn(PathBuf) -> Pin<Box<dyn Future<Output = WriteBaseline> + Send>> + Send + Sync>;

pub type DirnameFn = Arc<dyn Fn(&Path) -> PathBuf + Send + Sync>;

pub type WriteMutationQueueFn = Arc<
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
pub struct CoreWriteToolOptions {
    pub machine: ToolMachine,
    pub operations: Option<Arc<dyn WriteOperations>>,
    pub filesystem_policy: Option<FilesystemPolicyChecker>,
    pub resolve_path: Option<ToolPathResolver>,
    pub dirname: Option<DirnameFn>,
    pub with_mutation_queue: Option<WriteMutationQueueFn>,
    pub canonicalize_path: Option<ToolPathCanonicalizer>,
    pub read_baseline: Option<WriteBaselineReader>,
}

impl CoreWriteToolOptions {
    pub fn new(machine: ToolMachine) -> Self {
        Self {
            machine,
            operations: None,
            filesystem_policy: None,
            resolve_path: None,
            dirname: None,
            with_mutation_queue: None,
            canonicalize_path: None,
            read_baseline: None,
        }
    }
}

struct MachineWriteOperations {
    machine: ToolMachine,
    signal: AbortSignal,
}

impl WriteOperations for MachineWriteOperations {
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
                        create_parent_dirs: true,
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

    fn mkdir<'a>(&'a self, _path: &'a Path) -> ToolFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
}

fn machine_write_baseline(machine: ToolMachine) -> WriteBaselineReader {
    Arc::new(move |absolute_path: PathBuf| {
        let machine = machine.clone();
        Box::pin(async move {
            let ctx = machine.context(None, Some(PathLockMode::Caller));
            match machine
                .ops()
                .files()
                .read(
                    ReadRequest {
                        path: absolute_path.to_string_lossy().into_owned(),
                        offset: None,
                        limit_bytes: None,
                    },
                    &ctx,
                )
                .await
            {
                Ok(res) => WriteBaseline::Present {
                    content: String::from_utf8_lossy(&res.bytes).into_owned(),
                },
                Err(op_err) if machine_error_fs_code(&op_err) == "ENOENT" => WriteBaseline::Missing,
                Err(_) => WriteBaseline::Unavailable,
            }
        })
    })
}

fn default_mutation_queue() -> WriteMutationQueueFn {
    Arc::new(|path, f| Box::pin(async move { run_in_process_file_mutation_queue(&path, f).await }))
}

pub fn create_core_write_tool_definition(
    cwd: PathBuf,
    options: CoreWriteToolOptions,
) -> ToolDefinition {
    let custom_ops = options.operations;
    let machine = options.machine;
    let read_baseline = if custom_ops.is_some() {
        None
    } else {
        Some(
            options
                .read_baseline
                .unwrap_or_else(|| machine_write_baseline(machine.clone())),
        )
    };
    let policy = options.filesystem_policy;
    let resolve_path = options
        .resolve_path
        .unwrap_or_else(default_posix_path_resolver);
    let dirname: DirnameFn = options.dirname.unwrap_or_else(|| Arc::new(posix_dirname));
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
        let read_baseline = read_baseline.clone();
        let policy = policy.clone();
        let resolve_path = Arc::clone(&resolve_path);
        let dirname = Arc::clone(&dirname);
        let with_mutation_queue = Arc::clone(&with_mutation_queue);
        let canonicalize_path = Arc::clone(&canonicalize_path);

        Box::pin(async move {
            let input: WriteToolInput = serde_json::from_value(call.params)?;
            let gate_args = json!({ "path": input.path, "content": input.content });
            if let Some(admitted) = machine.begin(OpName::FilesWrite, gate_args, Some(&call.signal))
            {
                admitted.await?;
            }
            let ops: Arc<dyn WriteOperations> = custom_ops.unwrap_or_else(|| {
                Arc::new(MachineWriteOperations {
                    machine: machine.clone(),
                    signal: call.signal.clone(),
                })
            });
            let base_cwd = call.context.map_or(cwd.as_path(), ToolContext::cwd);
            let path = resolve_path(&input.path, base_cwd);
            let dir = dirname(&path);

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
                                "write",
                            )
                            .await?;
                            call.signal.check()?;
                        }
                        let baseline = if let Some(reader) = &read_baseline {
                            reader(fut_path.clone()).await
                        } else {
                            WriteBaseline::Unavailable
                        };
                        call.signal.check()?;
                        ops.mkdir(&dir).await?;
                        call.signal.check()?;
                        ops.write_file(&fut_path, &input.content).await?;
                        call.signal.check()?;
                        Ok(ToolResult {
                            content: vec![ToolContent::text(format!(
                                "Successfully wrote to {}",
                                input.path
                            ))],
                            details: create_write_details(&input.path, &input.content, baseline)
                                .map(serde_json::to_value)
                                .transpose()?,
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
        "write",
        "Write content to a file. Creates the file if it doesn't exist, overwrites if it does. Automatically creates parent directories.",
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to the file to write (relative or absolute)" },
                "content": { "type": "string", "description": "Content to write to the file" }
            },
            "required": ["path", "content"]
        }),
        execute,
    );
    tool.prompt_snippet = Some("Create or overwrite files".into());
    tool.prompt_guidelines = Some(vec![
        "Use write only for new files or complete rewrites.".into(),
    ]);
    tool.constrained_sampling = Some(maho_ai::types::ConstrainedSampling::Config(
        maho_ai::types::ConstrainedSamplingConfig::JsonSchema {
            strict: maho_ai::types::JsonSchemaStrictness::Prefer,
        },
    ));
    tool
}
