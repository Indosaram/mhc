use std::cell::RefCell;
use std::rc::Rc;

use maho_interactive::components::tool_execution_fallback::{
    create_tool_call_fallback, create_tool_result_fallback, format_tool_execution_fallback,
};
use maho_interactive::theme::{ColorMode, Theme};
use maho_tools::definition::{ToolContent, ToolResult};
use maho_tui::tui::Component;
use serde_json::Value;

fn theme() -> Theme {
    Theme::builtin("dark", ColorMode::Color256).expect("builtin theme")
}

fn fixtures() -> Value {
    serde_json::from_str(include_str!("golden/components32-fallback.json")).expect("pinned fixture")
}

fn lines_of(component: &Rc<RefCell<dyn Component>>) -> Vec<String> {
    component.borrow_mut().render(80).into_iter().map(|line| line.trim_end().to_owned()).collect()
}

fn result_from(value: &Value) -> Option<ToolResult> {
    if value.is_null() {
        return None;
    }
    let content = value["content"]
        .as_array()
        .expect("content")
        .iter()
        .map(|part| serde_json::from_value(part.clone()).expect("tool content"))
        .collect();
    Some(ToolResult { content, details: value.get("details").filter(|value| !value.is_null()).cloned() })
}

#[test]
fn tool_call_fallbacks_match_pinned_senpi() {
    let theme = theme();
    for case in fixtures()["calls"].as_array().expect("calls") {
        let tool_name = case["toolName"].as_str().expect("toolName");
        let expected: Vec<String> =
            case["lines"].as_array().expect("lines").iter().map(|line| line.as_str().expect("line").to_owned()).collect();
        assert_eq!(lines_of(&create_tool_call_fallback(tool_name, &theme)), expected, "call fallback {tool_name:?}");
    }
}

#[test]
fn tool_result_fallbacks_match_pinned_senpi_for_text_and_json_bodies() {
    let theme = theme();
    for case in fixtures()["results"].as_array().expect("results") {
        let name = case["name"].as_str().expect("name");
        let show_images = case["showImages"].as_bool().expect("showImages");
        let result = result_from(&case["result"]);
        let actual = create_tool_result_fallback(result.as_ref(), show_images, &theme);
        if case["lines"].is_null() {
            assert!(actual.is_none(), "result fallback {name} should be absent");
            continue;
        }
        let expected: Vec<String> =
            case["lines"].as_array().expect("lines").iter().map(|line| line.as_str().expect("line").to_owned()).collect();
        let component = actual.unwrap_or_else(|| panic!("result fallback {name} should exist"));
        assert_eq!(lines_of(&component), expected, "result fallback {name}");
    }
}

#[test]
fn formatted_tool_execution_fallbacks_match_pinned_senpi() {
    let theme = theme();
    for case in fixtures()["formats"].as_array().expect("formats") {
        let name = case["name"].as_str().expect("name");
        let tool_name = case["toolName"].as_str().expect("toolName");
        let args = &case["args"];
        let result = result_from(&case["result"]);
        assert_eq!(
            format_tool_execution_fallback(tool_name, args, result.as_ref(), false, &theme),
            case["output"].as_str().expect("output"),
            "formatted fallback {name}"
        );
    }
}

#[test]
fn fallback_sanitisation_collapses_controls_and_strips_ansi() {
    let theme = theme();
    let result = ToolResult { content: vec![ToolContent::text("\u{1b}[31mred\u{1b}[0m")], details: None };
    let component = create_tool_result_fallback(Some(&result), false, &theme).expect("component");
    let rendered = lines_of(&component).join("\n");
    assert!(rendered.contains("red"));
    assert!(!rendered.contains("\u{1b}[31m"), "the source's own ANSI must be stripped");
}
