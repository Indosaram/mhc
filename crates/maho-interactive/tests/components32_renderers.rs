use std::cell::RefCell;
use std::rc::Rc;

use maho_interactive::theme::{ColorMode, Theme};
use maho_interactive::tools::renderers::{ToolRenderContext, ToolRenderResultOptions, create_all_tool_renderers};
use maho_tools::definition::ToolResult;
use maho_tui::tui::Component;
use serde_json::Value;

fn theme() -> Theme {
    Theme::builtin("dark", ColorMode::Color256).expect("builtin theme")
}

fn fixtures() -> Vec<Value> {
    serde_json::from_str(include_str!("golden/components32-renderers.json")).expect("pinned fixture")
}

fn result_from(value: &Value) -> ToolResult {
    serde_json::from_value(value.clone()).expect("tool result")
}

fn render_lines(component: &Rc<RefCell<dyn Component>>) -> Vec<String> {
    component.borrow_mut().render(80).into_iter().map(|line| line.trim_end().to_owned()).collect()
}

fn strip_ansi(text: &str) -> String {
    maho_tui::utils::strip_terminal_sequences(text)
}

/// The shell call body is syntax-highlighted by the crate's todo 31 `highlight_code`, whose bash
/// grammar classifies `echo` differently from senpi's highlight.js; the layout this renderer owns
/// is what is compared here.
fn normalize(name: &str, lines: Vec<String>) -> Vec<String> {
    if name == "bash" { lines.into_iter().map(|line| strip_ansi(&line)).collect() } else { lines }
}

#[test]
fn built_in_renderers_match_pinned_senpi_calls_and_results() {
    let theme = theme();
    let renderers = create_all_tool_renderers();
    for case in fixtures() {
        let name = case["name"].as_str().expect("name");
        let args = &case["args"];
        let expanded = case["expanded"].as_bool().expect("expanded");
        let expected: Vec<String> = case["lines"]
            .as_array()
            .expect("lines")
            .iter()
            .map(|line| {
                let line = line.as_str().expect("line").to_owned();
                if name == "bash" { strip_ansi(&line) } else { line }
            })
            .collect();
        let renderer = renderers.get(name).unwrap_or_else(|| panic!("renderer {name}")).clone();
        let invalidate: Rc<dyn Fn()> = Rc::new(|| {});
        let mut context = ToolRenderContext {
            args,
            tool_call_id: "call-1",
            cwd: "/tmp/project",
            execution_started: true,
            args_complete: true,
            is_partial: false,
            expanded,
            show_images: false,
            is_error: case["isError"].as_bool().unwrap_or(false),
            has_result: case["kind"] == "result",
            spinner_frame: None,
            now_ms: 0.,
            invalidate,
        };
        let call = renderer.borrow_mut().render_call(&theme, &context);
        if case["kind"] == "call" {
            let component = call.unwrap_or_else(|| panic!("{name} produced no call component"));
            assert_eq!(normalize(name, render_lines(&component)), expected, "{name} call");
            continue;
        }
        context.args = args;
        let result = result_from(&case["result"]);
        let options = ToolRenderResultOptions { expanded, is_partial: false };
        let component = renderer
            .borrow_mut()
            .render_result(&result, options, &theme, &context)
            .unwrap_or_else(|| panic!("{name} produced no result component"));
        assert_eq!(normalize(name, render_lines(&component)), expected, "{name} result");
    }
}
