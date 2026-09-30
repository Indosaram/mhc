//! Port of senpi `packages/coding-agent/src/core/dynamic-prompt/tool-section.ts`.

use std::collections::HashMap;

use super::types::{AvailableTool, ToolCategory};
use crate::system_prompt::get_eval_only_grep_guideline;

/// `CATEGORY_ORDER`.
pub const CATEGORY_ORDER: [ToolCategory; 4] =
    [ToolCategory::Search, ToolCategory::Other, ToolCategory::Session, ToolCategory::Command];

/// `CATEGORY_LABELS`.
pub fn category_label(category: ToolCategory) -> &'static str {
    match category {
        ToolCategory::Search => "Search",
        ToolCategory::Other => "Core Tools",
        ToolCategory::Session => "Session",
        ToolCategory::Command => "Commands",
    }
}

/// `buildToolSection`.
pub fn build_tool_section(
    tools: &[AvailableTool],
    tool_snippets: &HashMap<String, String>,
    prompt_guidelines: &[String],
) -> String {
    let mut grouped_tools: Vec<(ToolCategory, Vec<(String, String)>)> = Vec::new();

    for tool in tools {
        let Some(snippet) = tool_snippets.get(&tool.name).map(|snippet| snippet.trim()).filter(|snippet| !snippet.is_empty())
        else {
            continue;
        };

        match grouped_tools.iter_mut().find(|(category, _)| *category == tool.category) {
            Some((_, existing)) => existing.push((tool.name.clone(), snippet.to_string())),
            None => grouped_tools.push((tool.category, vec![(tool.name.clone(), snippet.to_string())])),
        }
    }

    let mut lines: Vec<String> = vec!["## Available Tools".to_string(), String::new()];
    let mut has_visible_tools = false;

    for category in CATEGORY_ORDER {
        let Some((_, category_tools)) = grouped_tools.iter().find(|(candidate, _)| *candidate == category) else {
            continue;
        };
        if category_tools.is_empty() {
            continue;
        }

        has_visible_tools = true;
        lines.push(format!("### {}", category_label(category)));
        for (name, snippet) in category_tools {
            lines.push(format!("- {name}: {snippet}"));
        }
        lines.push(String::new());
    }

    if !has_visible_tools {
        lines.push("(none)".to_string());
        lines.push(String::new());
    }

    let mut guidelines: Vec<String> = prompt_guidelines
        .iter()
        .map(|guideline| guideline.trim())
        .filter(|guideline| !guideline.is_empty())
        .map(str::to_string)
        .collect();
    let names: Vec<String> = tools.iter().map(|tool| tool.name.clone()).collect();
    if let Some(eval_only_grep_guideline) = get_eval_only_grep_guideline(&names, Some(tool_snippets)) {
        guidelines.insert(0, eval_only_grep_guideline);
    }
    if !guidelines.is_empty() {
        lines.push("## Tool Guidelines".to_string());
        lines.push(String::new());
        for guideline in &guidelines {
            lines.push(format!("- {guideline}"));
        }
        lines.push(String::new());
    }

    lines.join("\n").trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dynamic_prompt::tool_categorization::categorize_tools;

    fn snippets(entries: &[(&str, &str)]) -> HashMap<String, String> {
        entries.iter().map(|(name, snippet)| ((*name).to_string(), (*snippet).to_string())).collect()
    }

    #[test]
    fn categories_render_in_the_fixed_order_with_their_labels() {
        let tools = categorize_tools(&[
            "skill".to_string(),
            "read".to_string(),
            "session_list".to_string(),
            "grep".to_string(),
        ]);
        let section = build_tool_section(
            &tools,
            &snippets(&[
                ("skill", "Invoke a skill"),
                ("read", "Read file contents"),
                ("session_list", "List sessions"),
                ("grep", "Search file contents"),
            ]),
            &[],
        );
        let search = section.find("### Search").expect("search");
        let core = section.find("### Core Tools").expect("core");
        let session = section.find("### Session").expect("session");
        let commands = section.find("### Commands").expect("commands");
        assert!(search < core && core < session && session < commands);
        assert!(section.contains("- grep: Search file contents"));
    }

    #[test]
    fn tools_without_a_snippet_are_not_rendered() {
        let tools = categorize_tools(&["read".to_string(), "write".to_string()]);
        let section = build_tool_section(&tools, &snippets(&[("read", "Read file contents")]), &[]);
        assert!(section.contains("- read: Read file contents"));
        assert!(!section.contains("write"));
    }

    #[test]
    fn an_empty_selection_renders_none() {
        let section = build_tool_section(&[], &snippets(&[]), &[]);
        assert_eq!(section, "## Available Tools\n\n(none)");
    }

    #[test]
    fn guidelines_are_trimmed_deduplicated_in_order_and_prefixed_by_the_eval_grep_rule() {
        let tools = categorize_tools(&["bash".to_string()]);
        let section = build_tool_section(
            &tools,
            &snippets(&[("bash", "Run commands"), ("grep", "Search file contents")]),
            &["  first rule  ".to_string(), String::new()],
        );
        let guidelines_start = section.find("## Tool Guidelines").expect("guidelines");
        let guidelines = &section[guidelines_start..];
        assert!(guidelines.contains("- Search file contents with tool.grep({ pattern, path }) inside eval; prefer it over rg/grep in shell"));
        assert!(guidelines.contains("- first rule"));
        let eval_rule = guidelines.find("tool.grep").expect("eval rule");
        let first_rule = guidelines.find("- first rule").expect("first rule");
        assert!(eval_rule < first_rule);
    }

    #[test]
    fn a_snippet_is_trimmed_before_it_is_rendered() {
        let tools = categorize_tools(&["read".to_string()]);
        let section = build_tool_section(&tools, &snippets(&[("read", "  Read file contents  ")]), &[]);
        assert!(section.ends_with("- read: Read file contents"));
    }
}
