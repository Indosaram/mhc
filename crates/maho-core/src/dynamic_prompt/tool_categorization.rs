//! Port of senpi `packages/coding-agent/src/core/dynamic-prompt/tool-categorization.ts`.

use super::types::{AvailableTool, ToolCategory};

/// `getToolCategory`.
pub fn get_tool_category(name: &str) -> ToolCategory {
    if name == "grep" || name == "glob" {
        return ToolCategory::Search;
    }
    if name.starts_with("session_") {
        return ToolCategory::Session;
    }
    if name == "skill" {
        return ToolCategory::Command;
    }
    ToolCategory::Other
}

/// `categorizeTools`.
pub fn categorize_tools(tool_names: &[String]) -> Vec<AvailableTool> {
    tool_names
        .iter()
        .map(|name| AvailableTool {
            name: name.clone(),
            category: get_tool_category(name),
        })
        .collect()
}

/// `getToolsPromptDisplay`.
pub fn get_tools_prompt_display(tools: &[AvailableTool]) -> String {
    let mut display_names: Vec<&str> = Vec::new();

    if tools
        .iter()
        .any(|tool| tool.category == ToolCategory::Search && tool.name == "grep")
    {
        display_names.push("`grep`");
    }
    if tools
        .iter()
        .any(|tool| tool.category == ToolCategory::Search && tool.name == "glob")
    {
        display_names.push("`glob`");
    }

    display_names.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn categories_follow_the_four_fork_buckets() {
        assert_eq!(get_tool_category("grep"), ToolCategory::Search);
        assert_eq!(get_tool_category("glob"), ToolCategory::Search);
        assert_eq!(get_tool_category("session_list"), ToolCategory::Session);
        assert_eq!(get_tool_category("skill"), ToolCategory::Command);
        assert_eq!(get_tool_category("read"), ToolCategory::Other);
    }

    #[test]
    fn categorization_keeps_the_input_order() {
        let tools =
            categorize_tools(&["grep".to_string(), "read".to_string(), "skill".to_string()]);
        let names: Vec<&str> = tools.iter().map(|tool| tool.name.as_str()).collect();
        assert_eq!(names, vec!["grep", "read", "skill"]);
        assert_eq!(tools[0].category, ToolCategory::Search);
    }

    #[test]
    fn the_display_names_only_search_tools() {
        let tools = categorize_tools(&["grep".to_string(), "glob".to_string(), "read".to_string()]);
        assert_eq!(get_tools_prompt_display(&tools), "`grep`, `glob`");
        assert_eq!(
            get_tools_prompt_display(&categorize_tools(&["read".to_string()])),
            ""
        );
        assert_eq!(get_tools_prompt_display(&[]), "");
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| (*name).to_string()).collect()
    }

    #[test]
    fn categorizes_lsp_and_ast_grep_prefixed_tools_as_other() {
        for tool in categorize_tools(&names(&["lsp_goto_definition", "ast_grep_search"])) {
            assert_eq!(tool.category, ToolCategory::Other);
        }
    }

    #[test]
    fn categorizes_grep_and_glob_as_search() {
        let tools = categorize_tools(&names(&["grep", "glob"]));
        assert_eq!(
            tools,
            vec![
                AvailableTool {
                    name: "grep".to_string(),
                    category: ToolCategory::Search
                },
                AvailableTool {
                    name: "glob".to_string(),
                    category: ToolCategory::Search
                },
            ]
        );
    }

    #[test]
    fn categorizes_session_prefixed_tools_as_session() {
        for tool in categorize_tools(&names(&["session_list", "session_read"])) {
            assert_eq!(tool.category, ToolCategory::Session);
        }
    }

    #[test]
    fn categorizes_skill_as_command() {
        assert_eq!(
            categorize_tools(&names(&["skill"]))[0].category,
            ToolCategory::Command
        );
    }

    #[test]
    fn categorizes_remaining_tools_as_other() {
        for tool in categorize_tools(&names(&["read", "bash", "edit", "write"])) {
            assert_eq!(tool.category, ToolCategory::Other);
        }
    }

    #[test]
    fn handles_mixed_tools_correctly() {
        let tools = categorize_tools(&names(&[
            "read",
            "lsp_diagnostics",
            "grep",
            "ast_grep_search",
            "bash",
        ]));
        assert_eq!(tools.len(), 5);
        let category = |name: &str| {
            tools
                .iter()
                .find(|tool| tool.name == name)
                .map(|tool| tool.category)
        };
        assert_eq!(category("lsp_diagnostics"), Some(ToolCategory::Other));
        assert_eq!(category("grep"), Some(ToolCategory::Search));
        assert_eq!(category("ast_grep_search"), Some(ToolCategory::Other));
        assert_eq!(category("read"), Some(ToolCategory::Other));
        assert_eq!(category("bash"), Some(ToolCategory::Other));
    }

    #[test]
    fn returns_empty_array_for_empty_input() {
        assert!(categorize_tools(&[]).is_empty());
    }

    #[test]
    fn does_not_display_lsp_tools() {
        let tools = categorize_tools(&names(&["lsp_goto_definition", "lsp_find_references"]));
        assert_eq!(get_tools_prompt_display(&tools), "");
    }

    #[test]
    fn only_displays_search_tools_not_lsp_or_ast() {
        let display = get_tools_prompt_display(&categorize_tools(&names(&[
            "grep",
            "lsp_diagnostics",
            "ast_grep_search",
        ])));
        assert!(display.contains("`grep`"));
        assert!(!display.contains("lsp"));
        assert!(!display.contains("ast"));
    }

    #[test]
    fn returns_empty_string_when_only_other_category_tools_exist() {
        assert_eq!(
            get_tools_prompt_display(&categorize_tools(&names(&[
                "read", "bash", "edit", "write"
            ]))),
            ""
        );
    }
}
