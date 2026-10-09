//! Tests matching upstream `test/permission.test.ts`.

use senpi_desktop_tool::permission::computer_permission_parser;
use serde_json::json;

fn patterns_of(input: &serde_json::Value) -> Vec<String> {
    let dummy_path = std::path::Path::new("/work");
    computer_permission_parser("computer", input, dummy_path)
        .into_iter()
        .flat_map(|r| r.patterns)
        .collect()
}

#[test]
fn permission_parser_classifies_window_screenshot_as_read() {
    let input = json!({
        "action": "call",
        "chain": [
            { "method": "window", "args": [{ "app": "Code" }] },
            { "method": "screenshot" }
        ]
    });
    assert_eq!(patterns_of(&input), vec!["read"]);
}

#[test]
fn permission_parser_classifies_window_click_as_exec() {
    let input = json!({
        "action": "call",
        "chain": [
            { "method": "window", "args": [{ "app": "Code" }] },
            { "method": "click", "args": [1, 2] }
        ]
    });
    assert_eq!(patterns_of(&input), vec!["exec"]);
}

#[test]
fn permission_parser_classifies_run_without_read_only_as_exec() {
    let input = json!({
        "action": "run",
        "code": "return 1"
    });
    assert_eq!(patterns_of(&input), vec!["exec"]);
}

#[test]
fn permission_parser_classifies_run_with_read_only_true_as_read() {
    let input = json!({
        "action": "run",
        "code": "return 1",
        "read_only": true
    });
    assert_eq!(patterns_of(&input), vec!["read"]);
}

#[test]
fn permission_parser_classifies_capabilities_as_read() {
    let input = json!({
        "action": "capabilities"
    });
    assert_eq!(patterns_of(&input), vec!["read"]);
}

// 6 it.each parameter expansions
#[test]
fn permission_parser_never_classifies_unknown_root_as_read() {
    let input = json!({
        "action": "call",
        "chain": [{ "method": "userReset" }]
    });
    assert_eq!(patterns_of(&input), vec!["exec"]);
}

#[test]
fn permission_parser_never_classifies_unknown_handle_method_as_read() {
    let input = json!({
        "action": "call",
        "chain": [
            { "method": "window", "args": [{ "app": "Code" }] },
            { "method": "userReset" }
        ]
    });
    assert_eq!(patterns_of(&input), vec!["exec"]);
}

#[test]
fn permission_parser_never_classifies_three_step_chain_as_read() {
    let input = json!({
        "action": "call",
        "chain": [
            { "method": "window", "args": [{ "app": "Code" }] },
            { "method": "ref", "args": ["e1"] },
            { "method": "value" }
        ]
    });
    assert_eq!(patterns_of(&input), vec!["exec"]);
}

#[test]
fn permission_parser_never_classifies_unchainable_root_as_read() {
    let input = json!({
        "action": "call",
        "chain": [
            { "method": "windows" },
            { "method": "screenshot" }
        ]
    });
    assert_eq!(patterns_of(&input), vec!["exec"]);
}

#[test]
fn permission_parser_never_classifies_non_object_step_as_read() {
    let input = json!({
        "action": "call",
        "chain": [serde_json::Value::Null]
    });
    assert_eq!(patterns_of(&input), vec!["exec"]);
}

#[test]
fn permission_parser_never_classifies_missing_chain_as_read() {
    let input = json!({
        "action": "call"
    });
    assert_eq!(patterns_of(&input), vec!["exec"]);
}

#[test]
fn permission_parser_names_computer_permission_and_scopes_always_approval() {
    let input = json!({
        "action": "call",
        "chain": [{ "method": "screenshot" }]
    });
    let requests = computer_permission_parser("computer", &input, std::path::Path::new("/work"));
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].permission, "computer");
    assert_eq!(requests[0].patterns, vec!["read"]);
    assert_eq!(requests[0].always, vec!["read"]);
}
