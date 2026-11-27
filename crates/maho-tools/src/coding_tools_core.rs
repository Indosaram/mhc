//! Port of `packages/coding-agent/src/core/tools/coding-tools-core.ts`.

use crate::bash_core::{CoreBashToolOptions, create_core_bash_tool_definition};
use crate::definition::ToolDefinition;
use crate::edit_core::{CoreEditToolOptions, create_core_edit_tool_definition};
use crate::read_core::{CoreReadToolOptions, create_core_read_tool_definition};
use crate::tool_definition_wrapper::wrap_tool_definitions;
use crate::tool_machine::ToolMachine;
use crate::write_core::{CoreWriteToolOptions, create_core_write_tool_definition};
use std::path::Path;

pub struct CoreCodingToolsOptions {
    pub machine: ToolMachine,
    pub read: Option<CoreReadToolOptions>,
    pub bash: Option<CoreBashToolOptions>,
    pub edit: Option<CoreEditToolOptions>,
    pub write: Option<CoreWriteToolOptions>,
}

impl CoreCodingToolsOptions {
    pub fn new(machine: ToolMachine) -> Self {
        Self {
            machine,
            read: None,
            bash: None,
            edit: None,
            write: None,
        }
    }
}

/// `read`, `bash`, `edit`, and `write` definitions in `create_coding_tools` order for a worker host.
pub fn create_core_coding_tool_definitions(
    cwd: &Path,
    options: CoreCodingToolsOptions,
) -> Vec<ToolDefinition> {
    let machine = options.machine;
    vec![
        create_core_read_tool_definition(
            cwd.to_path_buf(),
            options
                .read
                .unwrap_or_else(|| CoreReadToolOptions::new(machine.clone())),
        ),
        create_core_bash_tool_definition(
            cwd.to_path_buf(),
            options
                .bash
                .unwrap_or_else(|| CoreBashToolOptions::new(machine.clone())),
        ),
        create_core_edit_tool_definition(
            cwd.to_path_buf(),
            options
                .edit
                .unwrap_or_else(|| CoreEditToolOptions::new(machine.clone())),
        ),
        create_core_write_tool_definition(
            cwd.to_path_buf(),
            options
                .write
                .unwrap_or_else(|| CoreWriteToolOptions::new(machine)),
        ),
    ]
}

pub fn create_core_coding_tools(
    cwd: &Path,
    options: CoreCodingToolsOptions,
) -> Vec<maho_agent::AgentTool> {
    wrap_tool_definitions(
        create_core_coding_tool_definitions(cwd, options),
        None,
        None,
    )
}
