use maho_codemode::tool::{json_tree::format_scalar,tool_widgets::{format_duration,code_point_prefix}};
use serde_json::json;

#[test]
fn scalar_format_preserves_code_points_and_bounds_escaped_text() {
    assert_eq!(format_scalar(&json!("😀😀😀"),2),"\"😀…\"");
    assert_eq!(format_scalar(&json!("a\nb\t"),20),"\"a\\nb\\t\"");
    assert_eq!(format_scalar(&json!([1,2]),0),"[2 items]");
    assert_eq!(format_scalar(&json!({"a":1}),0),"{1 keys}");
    assert_eq!(format_scalar(&json!(null),0),"null");
}

#[test]
fn widget_prefix_and_duration_boundaries() {
    assert_eq!(code_point_prefix("😀abc",2),"😀a");
    for (ms,expected) in [(-1.0,"<1s"),(999.0,"<1s"),(1000.0,"1s"),(60000.0,"1m"),(61000.0,"1m 1s"),(3600000.0,"1h"),(3660000.0,"1h 1m")] {
        assert_eq!(format_duration(ms),expected);
    }
}

#[test]
fn json_tree_bounds_depth_lines_and_filters_only_root_internal_keys() {
    use maho_codemode::tool::json_tree::render_json_tree_lines;
    let result=render_json_tree_lines(&json!({"i":1,"__partialJson":true,"nested":{"i":2,"array":[1,2]}}),6,200,60);
    assert!(result.lines.iter().any(|line|line.contains("i: 2")));
    assert!(!result.lines.iter().any(|line|line.contains("__partialJson")));
    assert!(!result.truncated);
    let limited=render_json_tree_lines(&json!([1,2,3]),6,2,60);
    assert_eq!(limited.lines.len(),2);
    assert!(limited.truncated);
    let collapsed=render_json_tree_lines(&json!({"nested":{"array":[1,2]}}),1,200,60);
    assert_eq!(collapsed.lines.len(),2);
    assert!(collapsed.lines[1].ends_with('…'));
}
