use maho_codemode::tool::json_tree::{format_scalar,render_json_tree_lines};
use serde_json::json;

#[test]
fn scalar_budget_matches_source_code_point_width() {
    assert_eq!(format_scalar(&json!("한글문자"),5),"\"한글문자\"");
    assert_eq!(format_scalar(&json!("e\u{301}x"),2),"\"e…\"");
}

#[test]
fn pinned_json_tree_depth_lines_and_nested_array_contract() {
    let nested=render_json_tree_lines(&json!({"user":{"name":"Ada"},"items":[1,2]}),6,20,40);
    assert!(!nested.truncated);
    assert!(nested.lines.iter().any(|line|line.contains("name")));
    assert!(nested.lines.iter().any(|line|line.contains("[0]")&&line.contains("├─")));
    let depth=render_json_tree_lines(&json!({"a":{"b":{"c":{"d":1}}}}),2,20,40);
    assert!(depth.lines.iter().any(|line|line.contains('…')));
    let limited=render_json_tree_lines(&json!({"first":1,"second":2,"third":3}),6,2,40);
    assert_eq!(limited.lines.len(),2);
    assert!(limited.truncated);
}
