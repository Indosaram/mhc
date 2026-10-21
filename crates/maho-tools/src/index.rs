use crate::{
    bash::*, definition::ToolDefinition, edit::*, find::*, grep::*, ls::*, powershell::*, read::*,
    write::*,
};
use std::{collections::BTreeMap, path::Path};
pub const ALL_TOOL_NAMES: [&str; 9] = [
    "read",
    "bash",
    "powershell",
    "edit",
    "write",
    "grep",
    "find",
    "glob",
    "ls",
];
#[derive(Default)]
pub struct ToolsOptions {
    pub read: ReadToolOptions,
    pub bash: BashToolOptions,
    pub powershell: PowerShellToolOptions,
    pub write: WriteToolOptions,
    pub edit: EditToolOptions,
    pub grep: GrepToolOptions,
    pub find: FindToolOptions,
    pub glob: FindToolOptions,
    pub ls: LsToolOptions,
}
pub fn create_all_tool_definitions(
    cwd: &Path,
    options: ToolsOptions,
) -> BTreeMap<String, ToolDefinition> {
    let find = create_find_tool_definition(cwd.into(), options.find);
    let glob = create_glob_tool_definition(cwd, options.glob);
    [
        create_read_tool_definition(cwd.into(), options.read),
        create_bash_tool_definition(cwd.into(), options.bash),
        create_powershell_tool_definition(cwd.into(), options.powershell),
        create_edit_tool_definition(cwd.into(), options.edit),
        create_write_tool_definition(cwd.into(), options.write),
        create_grep_tool_definition(cwd.into(), options.grep),
        find,
        glob,
        create_ls_tool_definition(cwd.into(), options.ls),
    ]
    .into_iter()
    .map(|tool| (tool.name.clone(), tool))
    .collect()
}
pub fn create_coding_tool_definitions(cwd: &Path, options: ToolsOptions) -> Vec<ToolDefinition> {
    vec![
        create_read_tool_definition(cwd.into(), options.read),
        create_bash_tool_definition(cwd.into(), options.bash),
        create_edit_tool_definition(cwd.into(), options.edit),
        create_write_tool_definition(cwd.into(), options.write),
    ]
}
pub fn create_glob_tool_definition(cwd: &Path, options: FindToolOptions) -> ToolDefinition {
    let mut glob = create_find_tool_definition(cwd.into(), options);
    glob.name = "glob".into();
    glob.label = "glob".into();
    glob.description = "Find files by glob with tool.glob({ pattern, path }) inside eval. Returns matching paths relative to the search directory, respects .gitignore, and applies hooks and permissions.".into();
    glob.prompt_snippet = None;
    glob.exposure = Some(crate::definition::ToolExposure::Eval);
    glob
}
pub fn create_read_only_tool_definitions(cwd: &Path, options: ToolsOptions) -> Vec<ToolDefinition> {
    vec![
        create_read_tool_definition(cwd.into(), options.read),
        create_grep_tool_definition(cwd.into(), options.grep),
        create_find_tool_definition(cwd.into(), options.find),
        create_ls_tool_definition(cwd.into(), options.ls),
    ]
}
pub fn create_coding_tools(cwd: &Path, options: ToolsOptions) -> Vec<maho_agent::AgentTool> {
    crate::tool_definition_wrapper::wrap_tool_definitions(
        create_coding_tool_definitions(cwd, options),
        None,
    )
}
pub fn create_read_only_tools(cwd: &Path, options: ToolsOptions) -> Vec<maho_agent::AgentTool> {
    crate::tool_definition_wrapper::wrap_tool_definitions(
        create_read_only_tool_definitions(cwd, options),
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::definition::{AbortSignal, ToolCall, ToolContent, ToolFuture};
    use std::sync::Arc;

    struct NamedFiles(&'static str);
    impl FindOperations for NamedFiles {
        fn exists<'a>(&'a self, _path: &'a Path) -> ToolFuture<'a, bool> {
            Box::pin(async { Ok(true) })
        }
        fn glob<'a>(
            &'a self,
            _pattern: &'a str,
            _cwd: &'a Path,
            _ignore: &'a [&'a str],
            _limit: usize,
        ) -> ToolFuture<'a, Vec<String>> {
            Box::pin(async { Ok(vec![self.0.into()]) })
        }
    }

    #[tokio::test]
    async fn find_and_glob_use_independent_operations() {
        let tools = create_all_tool_definitions(
            Path::new("."),
            ToolsOptions {
                find: FindToolOptions {
                    operations: Some(Arc::new(NamedFiles("find.rs"))),
                    ..Default::default()
                },
                glob: FindToolOptions {
                    operations: Some(Arc::new(NamedFiles("glob.rs"))),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        for (name, expected) in [("find", "find.rs"), ("glob", "glob.rs")] {
            let result = (tools[name].execute)(ToolCall {
                id: name,
                params: serde_json::json!({"pattern":"*.rs"}),
                signal: AbortSignal::default(),
                on_update: None,
                context: None,
            })
            .await
            .expect("execute tool");
            assert_eq!(result.content, vec![ToolContent::text(expected)]);
        }
    }
}
