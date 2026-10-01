use std::cell::RefCell;
use std::rc::Rc;

use maho_interactive::components::exploration_call::{ExplorationAction, ExplorationCall};
use maho_interactive::components::exploration_group::ExplorationGroup;
use maho_interactive::components::tool_execution::{
    ToolExecutionComponent, ToolExecutionOptions, ToolExecutionPresentation,
};
use maho_interactive::components::tool_execution_types::ToolExecutionResult;
use maho_interactive::theme::{ColorMode, Theme};
use maho_tools::definition::ToolContent;
use maho_tui::tui::{Component, TuiMouseButton, TuiMouseEvent, TuiMouseEventType};
use serde_json::Value;

fn theme() -> Theme {
    Theme::builtin("dark", ColorMode::Color256).expect("builtin theme")
}

fn fixtures() -> Value {
    serde_json::from_str(include_str!("golden/components32-mouse.json")).expect("pinned fixture")
}

fn trim(lines: Vec<String>) -> Vec<String> {
    lines.into_iter().map(|line| line.trim_end().to_owned()).collect()
}

fn mouse_event(step: &Value) -> TuiMouseEvent {
    let event = &step["event"];
    let y = event.get("y").and_then(Value::as_i64).unwrap_or(1);
    let event_type = match event.get("type").and_then(Value::as_str).unwrap_or("click") {
        "press" => TuiMouseEventType::Press,
        "release" => TuiMouseEventType::Release,
        "move" => TuiMouseEventType::Move,
        "wheel" => TuiMouseEventType::Wheel,
        _ => TuiMouseEventType::Click,
    };
    let button = match event.get("button").and_then(Value::as_str).unwrap_or("left") {
        "right" => TuiMouseButton::Right,
        "middle" => TuiMouseButton::Middle,
        "none" => TuiMouseButton::None,
        _ => TuiMouseButton::Left,
    };
    TuiMouseEvent {
        event_type,
        button,
        x: event.get("x").and_then(Value::as_i64).unwrap_or(0),
        y,
        screen_x: 0,
        screen_y: y,
        width: 80,
        height: 10,
        shift: false,
        alt: false,
        ctrl: false,
        wheel_delta: None,
        click_count: None,
    }
}

fn card(theme: &Theme, file_path: &str, id: &str) -> Rc<RefCell<ToolExecutionComponent>> {
    let component = Rc::new(RefCell::new(ToolExecutionComponent::new(
        "read",
        id,
        serde_json::json!({ "file_path": file_path }),
        ToolExecutionOptions { show_images: Some(false), image_width_cells: None },
        None,
        "/tmp/project",
        ToolExecutionPresentation::Classic,
        None,
        theme.clone(),
    )));
    component.borrow_mut().update_result(
        ToolExecutionResult { content: vec![ToolContent::text("body\n")], details: None, is_error: false },
        false,
    );
    component.borrow_mut().stop_animation();
    component
}

#[test]
fn exploration_group_mouse_handling_matches_pinned_senpi() {
    let theme = theme();
    for case in fixtures()["cases"].as_array().expect("cases") {
        let name = case["name"].as_str().expect("name");
        let cards = [card(&theme, "a.rs", "call-1"), card(&theme, "b.rs", "call-2")];
        let calls: Vec<(Rc<RefCell<ToolExecutionComponent>>, ExplorationCall)> = cards
            .iter()
            .map(|component| {
                (
                    Rc::clone(component),
                    ExplorationCall {
                        action: ExplorationAction::Read,
                        label: String::from("x"),
                        pending: false,
                        failed: false,
                    },
                )
            })
            .collect();
        let mut group = ExplorationGroup::new(theme.clone());
        group.set_members(
            cards.iter().map(|component| Rc::clone(component) as Rc<RefCell<dyn Component>>).collect(),
            calls,
            Vec::new(),
        );

        for (index, step) in case["steps"].as_array().expect("steps").iter().enumerate() {
            let event = mouse_event(step);
            let handled = group.handle_mouse(&event).is_some();
            assert_eq!(
                handled,
                step["handled"].as_bool().expect("handled"),
                "{name} step {index}: handled"
            );
            let wanted: Vec<String> =
                step["lines"].as_array().expect("lines").iter().map(|line| line.as_str().expect("line").to_owned()).collect();
            assert_eq!(trim(group.render(80)), wanted, "{name} step {index}: render");
        }
    }
}
