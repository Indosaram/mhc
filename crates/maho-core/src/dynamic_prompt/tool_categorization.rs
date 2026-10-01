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
        .map(|name| AvailableTool { name: name.clone(), category: get_tool_category(name) })
        .collect()
}

/// `getToolsPromptDisplay`.
pub fn get_tools_prompt_display(tools: &[AvailableTool]) -> String {
    let mut display_names: Vec<&str> = Vec::new();

    if tools.iter().any(|tool| tool.category == ToolCategory::Search && tool.name == "grep") {
        display_names.push("`grep`");
    }
    if tools.iter().any(|tool| tool.category == ToolCategory::Search && tool.name == "glob") {
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
        let tools = categorize_tools(&["grep".to_string(), "read".to_string(), "skill".to_string()]);
        let names: Vec<&str> = tools.iter().map(|tool| tool.name.as_str()).collect();
        assert_eq!(names, vec!["grep", "read", "skill"]);
        assert_eq!(tools[0].category, ToolCategory::Search);
    }

    #[test]
    fn the_display_names_only_search_tools() {
        let tools = categorize_tools(&["grep".to_string(), "glob".to_string(), "read".to_string()]);
        assert_eq!(get_tools_prompt_display(&tools), "`grep`, `glob`");
        assert_eq!(get_tools_prompt_display(&categorize_tools(&["read".to_string()])), "");
        assert_eq!(get_tools_prompt_display(&[]), "");
    }
}
