//! Port of `packages/coding-agent/src/core/tools/write.ts`.

pub use crate::write_core::{
    WriteBaseline, WriteOperations, WriteToolDetails, WriteToolInput, create_write_details,
};

use crate::definition::{ToolDefinition, ToolFuture};
use crate::file_tool_host::run_local_file_mutation_queue;
use crate::filesystem_policy::{FilesystemPolicyChecker, canonicalize_filesystem_path};
use crate::machine::{ToolMachine, default_tool_machine};
use crate::write_core::{CoreWriteToolOptions, create_core_write_tool_definition};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub async fn read_local_write_baseline(path: &Path) -> WriteBaseline {
    match tokio::fs::read(path).await {
        Ok(content) => WriteBaseline::Present {
            content: String::from_utf8_lossy(&content).into_owned(),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => WriteBaseline::Missing,
        Err(_) => WriteBaseline::Unavailable,
    }
}

pub struct LocalWriteOperations;

impl WriteOperations for LocalWriteOperations {
    fn write_file<'a>(&'a self, path: &'a Path, content: &'a str) -> ToolFuture<'a, ()> {
        Box::pin(async move {
            Ok(tokio::fs::write(path, content)
                .await
                .map_err(|e| crate::definition::node_fs_error(e, "open", path))?)
        })
    }
    fn mkdir<'a>(&'a self, path: &'a Path) -> ToolFuture<'a, ()> {
        Box::pin(async move {
            Ok(tokio::fs::create_dir_all(path)
                .await
                .map_err(|e| crate::definition::node_fs_error(e, "mkdir", path))?)
        })
    }
}

#[derive(Clone, Default)]
pub struct WriteToolOptions {
    pub operations: Option<Arc<dyn WriteOperations>>,
    pub filesystem_policy: Option<FilesystemPolicyChecker>,
    pub machine: Option<ToolMachine>,
}

pub fn create_write_tool_definition(cwd: PathBuf, options: WriteToolOptions) -> ToolDefinition {
    let server_pinned = options.operations.is_some() || options.filesystem_policy.is_some();
    let machine = options.machine.unwrap_or_else(default_tool_machine);
    let mut tool = create_core_write_tool_definition(
        cwd,
        CoreWriteToolOptions {
            machine,
            operations: options.operations,
            filesystem_policy: options.filesystem_policy,
            resolve_path: Some(Arc::new(|path, cwd| {
                crate::path_utils::resolve_to_cwd(path, cwd)
            })),
            dirname: Some(Arc::new(|path| {
                path.parent().unwrap_or(Path::new(".")).to_path_buf()
            })),
            with_mutation_queue: Some(Arc::new(|path, f| {
                Box::pin(async move { run_local_file_mutation_queue(&path, f).await })
            })),
            canonicalize_path: Some(Arc::new(|path| {
                Box::pin(async move { canonicalize_filesystem_path(&path).await })
            })),
            read_baseline: Some(Arc::new(|path| {
                Box::pin(async move { read_local_write_baseline(&path).await })
            })),
        },
    );
    if server_pinned {
        tool.server_pinned = Some(true);
    }
    tool
}
