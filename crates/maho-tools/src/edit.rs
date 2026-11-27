//! Port of `packages/coding-agent/src/core/tools/edit.ts`.

pub use crate::edit_core::{EditOperations, EditToolInput, prepare_edit_arguments};

use crate::definition::{ToolDefinition, ToolFuture};
use crate::edit_core::{CoreEditToolOptions, create_core_edit_tool_definition};
use crate::file_tool_host::run_local_file_mutation_queue;
use crate::filesystem_policy::{FilesystemPolicyChecker, canonicalize_filesystem_path};
use crate::machine::{ToolMachine, default_tool_machine};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub struct LocalEditOperations;

impl EditOperations for LocalEditOperations {
    fn read_file<'a>(&'a self, path: &'a Path) -> ToolFuture<'a, Vec<u8>> {
        Box::pin(async move { Ok(tokio::fs::read(path).await?) })
    }
    fn write_file<'a>(&'a self, path: &'a Path, content: &'a str) -> ToolFuture<'a, ()> {
        Box::pin(async move { Ok(tokio::fs::write(path, content).await?) })
    }
    fn access<'a>(&'a self, path: &'a Path) -> ToolFuture<'a, ()> {
        Box::pin(async move {
            tokio::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(path)
                .await?;
            Ok(())
        })
    }
}

#[derive(Clone, Default)]
pub struct EditToolOptions {
    pub operations: Option<Arc<dyn EditOperations>>,
    pub filesystem_policy: Option<FilesystemPolicyChecker>,
    pub machine: Option<ToolMachine>,
}

pub fn create_edit_tool_definition(cwd: PathBuf, options: EditToolOptions) -> ToolDefinition {
    let server_pinned = options.operations.is_some() || options.filesystem_policy.is_some();
    let machine = options.machine.unwrap_or_else(default_tool_machine);
    let mut tool = create_core_edit_tool_definition(
        cwd,
        CoreEditToolOptions {
            machine,
            operations: options.operations,
            filesystem_policy: options.filesystem_policy,
            resolve_path: Some(Arc::new(|path, cwd| {
                crate::path_utils::resolve_to_cwd(path, cwd)
            })),
            with_mutation_queue: Some(Arc::new(|path, f| {
                Box::pin(async move { run_local_file_mutation_queue(&path, f).await })
            })),
            canonicalize_path: Some(Arc::new(|path| {
                Box::pin(async move { canonicalize_filesystem_path(&path).await })
            })),
        },
    );
    if server_pinned {
        tool.server_pinned = Some(true);
    }
    tool
}
