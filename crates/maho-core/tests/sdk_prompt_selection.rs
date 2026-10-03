use std::collections::HashMap;
use maho_core::dynamic_prompt::{tool_categorization::categorize_tools, tool_section::build_tool_section, types::ToolCategory};

#[test]
fn original_category_matrix_preserves_machine_consumed_tool_buckets() {
    let names = ["lsp_goto_definition", "ast_grep_search", "grep", "glob", "session_list", "session_read", "skill", "read", "bash", "edit", "write"];
    let tools = categorize_tools(&names.map(String::from));
    assert_eq!(tools.iter().map(|tool| tool.name.as_str()).collect::<Vec<_>>(), names);
    assert_eq!(tools.iter().map(|tool| tool.category).collect::<Vec<_>>(), [
        ToolCategory::Other, ToolCategory::Other, ToolCategory::Search, ToolCategory::Search,
        ToolCategory::Session, ToolCategory::Session, ToolCategory::Command, ToolCategory::Other,
        ToolCategory::Other, ToolCategory::Other, ToolCategory::Other,
    ]);
    assert!(categorize_tools(&[]).is_empty());
}

#[test]
fn withheld_search_tool_is_callable_without_being_advertised() {
    let snippets = ["read", "eval", "grep", "bash"].map(|name| (name.to_owned(), format!("fixture-{name}")));
    let snippets = HashMap::from(snippets);
    let section = build_tool_section(&categorize_tools(&["read".into(), "eval".into()]), &snippets, &[]);
    let advertised = section.lines().filter_map(|line| line.strip_prefix("- ")?.split_once(':').map(|(name, _)| name)).collect::<Vec<_>>();
    assert_eq!(advertised, ["read", "eval"]);
    let parameters = section.split_once("tool.grep({").expect("callable grep").1.split_once('}').expect("call close").0;
    assert_eq!(parameters.split(',').map(str::trim).collect::<Vec<_>>(), ["pattern", "path"]);
}

#[test]
fn directly_selected_search_and_empty_selection_omit_eval_only_call() {
    for selected in [vec!["grep".to_owned()], Vec::new()] {
        let snippets = if selected.is_empty() { HashMap::new() } else { HashMap::from([("grep".into(), "fixture".into())]) };
        let section = build_tool_section(&categorize_tools(&selected), &snippets, &[]);
        assert!(!section.contains("tool.grep("));
    }
}

#[test]
fn selected_search_display_preserves_canonical_order_without_duplicate_names() {
    use maho_core::dynamic_prompt::tool_categorization::get_tools_prompt_display;
    for (names, expected) in [
        (vec!["glob", "grep", "grep", "session_read"], "`grep`, `glob`"),
        (vec!["glob", "lsp_goto_definition"], "`glob`"),
        (vec!["ast_grep_search", "read"], ""),
    ] {
        let tools = categorize_tools(&names.into_iter().map(str::to_owned).collect::<Vec<_>>());
        assert_eq!(get_tools_prompt_display(&tools), expected);
    }
}
