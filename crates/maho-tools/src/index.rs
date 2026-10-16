use std::{collections::BTreeMap, path::Path};
use crate::{bash::*, definition::ToolDefinition, edit::*, find::*, grep::*, ls::*, powershell::*, read::*, write::*};
pub const ALL_TOOL_NAMES: [&str;9] = ["read","bash","powershell","edit","write","grep","find","glob","ls"];
#[derive(Default)]
pub struct ToolsOptions { pub read: ReadToolOptions, pub bash: BashToolOptions, pub powershell: PowerShellToolOptions, pub write: WriteToolOptions, pub edit: EditToolOptions, pub grep: GrepToolOptions, pub find: FindToolOptions, pub ls: LsToolOptions }
pub fn create_all_tool_definitions(cwd: &Path, options: ToolsOptions) -> BTreeMap<String,ToolDefinition> {
    let find = create_find_tool_definition(cwd.into(), options.find.clone());
    let glob = create_glob_tool_definition(cwd, options.find);
    [create_read_tool_definition(cwd.into(),options.read), create_bash_tool_definition(cwd.into(),options.bash),
        create_powershell_tool_definition(cwd.into(),options.powershell), create_edit_tool_definition(cwd.into(),options.edit),
        create_write_tool_definition(cwd.into(),options.write), create_grep_tool_definition(cwd.into(),options.grep),
        find, glob, create_ls_tool_definition(cwd.into(),options.ls)]
        .into_iter().map(|tool| (tool.name.clone(),tool)).collect()
}
pub fn create_coding_tool_definitions(cwd: &Path, options: ToolsOptions) -> Vec<ToolDefinition> {
    vec![create_read_tool_definition(cwd.into(),options.read),create_bash_tool_definition(cwd.into(),options.bash),create_edit_tool_definition(cwd.into(),options.edit),create_write_tool_definition(cwd.into(),options.write)]
}
pub fn create_glob_tool_definition(cwd: &Path, options: FindToolOptions) -> ToolDefinition {
    let mut glob = create_find_tool_definition(cwd.into(), options);
    glob.name = "glob".into(); glob.label = "glob".into();
    glob.description = "Find files by glob with tool.glob({ pattern, path }) inside eval. Returns matching paths relative to the search directory, respects .gitignore, and applies hooks and permissions.".into();
    glob.prompt_snippet = None; glob.exposure = Some(crate::definition::ToolExposure::Eval); glob
}
pub fn create_read_only_tool_definitions(cwd: &Path, options: ToolsOptions) -> Vec<ToolDefinition> {
    vec![create_read_tool_definition(cwd.into(),options.read),create_grep_tool_definition(cwd.into(),options.grep),create_find_tool_definition(cwd.into(),options.find),create_ls_tool_definition(cwd.into(),options.ls)]
}
pub fn create_coding_tools(cwd: &Path, options: ToolsOptions) -> Vec<maho_agent::AgentTool> {
    crate::tool_definition_wrapper::wrap_tool_definitions(create_coding_tool_definitions(cwd,options),None)
}
pub fn create_read_only_tools(cwd: &Path, options: ToolsOptions) -> Vec<maho_agent::AgentTool> {
    crate::tool_definition_wrapper::wrap_tool_definitions(create_read_only_tool_definitions(cwd,options),None)
}
