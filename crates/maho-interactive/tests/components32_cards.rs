use std::cell::RefCell;
use std::rc::Rc;

use maho_interactive::components::assistant_message::AssistantMessageComponent;
use maho_interactive::components::custom_message::CustomMessageComponent;
use maho_interactive::components::exploration_call::{ExplorationAction, ExplorationCall};
use maho_interactive::components::exploration_group::ExplorationGroup;
use maho_interactive::components::markdown_transform::get_markdown_theme;
use maho_interactive::components::tool_execution::{
    ToolExecutionComponent, ToolExecutionOptions, ToolExecutionPresentation,
};
use maho_interactive::components::tool_execution_types::ToolExecutionResult;
use maho_interactive::components::user_message::UserMessageComponent;
use maho_interactive::theme::{ColorMode, Theme};
use maho_tools::definition::ToolContent;
use maho_tui::tui::Component;
use serde_json::Value;

fn theme() -> Theme {
    Theme::builtin("dark", ColorMode::Color256).expect("builtin theme")
}

fn fixtures() -> Value {
    serde_json::from_str(include_str!("golden/components32-cards.json")).expect("pinned fixture")
}

fn expected(case: &Value) -> Vec<String> {
    case["lines"].as_array().expect("lines").iter().map(|line| line.as_str().expect("line").to_owned()).collect()
}

fn trim(lines: Vec<String>) -> Vec<String> {
    lines.into_iter().map(|line| line.trim_end().to_owned()).collect()
}

/// The shell command header is syntax-highlighted by the crate's todo 31 `highlight_code`, whose
/// bash grammar classifies builtins like `echo` differently from senpi's highlight.js. Everything
/// the card itself owns is still compared byte-for-byte.
fn strip_ansi(text: &str) -> String {
    maho_tui::utils::strip_terminal_sequences(text)
}

#[test]
fn assistant_messages_match_pinned_senpi_for_every_descriptor_shape() {
    let theme = theme();
    let markdown_theme = get_markdown_theme(&theme);
    for case in fixtures()["assistant"].as_array().expect("assistant") {
        let width = case["width"].as_u64().expect("width") as usize;
        let expanded = case["expanded"].as_bool().expect("expanded");
        let hide_thinking = case["hideThinkingBlock"].as_bool().expect("hideThinkingBlock");
        let mut component = AssistantMessageComponent::new(
            Some(case["message"].clone()),
            hide_thinking,
            markdown_theme.clone(),
            "Thinking...",
            1,
            Vec::new(),
            theme.clone(),
        );
        component.set_expanded(expanded);
        assert_eq!(
            trim(component.render(width)),
            expected(case),
            "assistant {} at {width} expanded={expanded}",
            case["name"].as_str().unwrap_or("")
        );
    }
}

#[test]
fn user_messages_match_pinned_senpi_at_every_width() {
    let theme = theme();
    let markdown_theme = get_markdown_theme(&theme);
    for case in fixtures()["user"].as_array().expect("user") {
        let width = case["width"].as_u64().expect("width") as usize;
        let text = case["text"].as_str().expect("text").to_owned();
        let mut component = UserMessageComponent::new(text, theme.clone(), markdown_theme.clone(), 1, Vec::new());
        assert_eq!(trim(component.render(width)), expected(case), "user at {width}");
    }
}

#[test]
fn custom_messages_match_pinned_senpi_collapsed_and_expanded() {
    let theme = theme();
    let markdown_theme = get_markdown_theme(&theme);
    for case in fixtures()["custom"].as_array().expect("custom") {
        let width = case["width"].as_u64().expect("width") as usize;
        let mut component = CustomMessageComponent::new(
            case.get("message").cloned().unwrap_or_else(|| serde_json::json!({"customType": "note", "content": "custom **body**"})),
            None,
            theme.clone(),
            markdown_theme.clone(),
            1,
        );
        component.set_expanded(case.get("expanded").and_then(Value::as_bool).unwrap_or(false));
        assert_eq!(trim(component.render(width)), expected(case), "custom at {width}");
    }
}

#[test]
fn exploration_groups_match_pinned_senpi() {
    let theme = theme();
    for case in fixtures()["exploration"].as_array().expect("exploration") {
        let width = case["width"].as_u64().expect("width") as usize;
        let calls = [
            ("read", serde_json::json!({"file_path": "a.rs"}), ExplorationCall { action: ExplorationAction::Read, label: String::from("a.rs"), pending: false, failed: false }, false),
            ("read", serde_json::json!({"file_path": "b.rs"}), ExplorationCall { action: ExplorationAction::Read, label: String::from("b.rs"), pending: false, failed: false }, false),
            ("grep", serde_json::json!({"pattern": "needle", "path": "src"}), ExplorationCall { action: ExplorationAction::Search, label: String::from("needle in src"), pending: false, failed: false }, false),
            ("ls", serde_json::json!({}), ExplorationCall { action: ExplorationAction::List, label: String::from("."), pending: false, failed: true }, true),
        ];
        let mut members: Vec<Rc<RefCell<ToolExecutionComponent>>> = Vec::new();
        let mut group_calls: Vec<(Rc<RefCell<ToolExecutionComponent>>, ExplorationCall)> = Vec::new();
        for (index, (tool, args, call, failed)) in calls.into_iter().enumerate() {
            let component = Rc::new(RefCell::new(ToolExecutionComponent::new(
                tool,
                &format!("call-{index}"),
                args,
                ToolExecutionOptions { show_images: Some(false), image_width_cells: None },
                None,
                "/tmp/project",
                ToolExecutionPresentation::Classic,
                None,
                theme.clone(),
            )));
            if failed {
                component.borrow_mut().update_result(
                    ToolExecutionResult { content: vec![ToolContent::text("boom")], details: None, is_error: true },
                    false,
                );
            }
            members.push(Rc::clone(&component));
            group_calls.push((component, call));
        }
        let mut group = ExplorationGroup::new(theme.clone());
        group.set_members(
            members.into_iter().map(|component| component as Rc<RefCell<dyn Component>>).collect(),
            group_calls,
            vec![String::from("rule-a"), String::from("rule-b"), String::from("rule-a")],
        );
        assert_eq!(trim(group.render(width)), expected(case), "exploration at {width}");
    }
}

#[test]
fn tool_cards_match_pinned_senpi_for_every_presentation_state() {
    let theme = theme();
    for case in fixtures()["tool"].as_array().expect("tool") {
        let width = case["width"].as_u64().expect("width") as usize;
        let expanded = case["expanded"].as_bool().expect("expanded");
        let tool_name = case["toolName"].as_str().expect("toolName");
        let mut component = ToolExecutionComponent::new(
            tool_name,
            "call-1",
            case["args"].clone(),
            ToolExecutionOptions { show_images: Some(false), image_width_cells: None },
            None,
            "/tmp/project",
            ToolExecutionPresentation::Classic,
            None,
            theme.clone(),
        );
        component.set_expanded(expanded);
        let result = &case["result"];
        component.update_result(
            ToolExecutionResult {
                content: result["content"]
                    .as_array()
                    .expect("content")
                    .iter()
                    .map(|part| {
                        serde_json::from_value(part.clone()).expect("tool content")
                    })
                    .collect(),
                details: result.get("details").filter(|value| !value.is_null()).cloned(),
                is_error: result["isError"].as_bool().expect("isError"),
            },
            false,
        );
        component.stop_animation();
        let actual = trim(component.render(width));
        let wanted = expected(case);
        assert_eq!(actual.len(), wanted.len(), "tool {tool_name} line count at {width}");
        for (index, (left, right)) in actual.iter().zip(&wanted).enumerate() {
            let is_command_header = strip_ansi(left).contains("echo hi");
            let (left, right) = if is_command_header {
                (strip_ansi(left), strip_ansi(right))
            } else {
                (left.clone(), right.clone())
            };
            assert_eq!(left, right, "tool {tool_name} line {index} at {width} expanded={expanded}");
        }
    }
}

#[test]
fn tool_card_progress_line_matches_pinned_senpi() {
    let theme = theme();
    for case in fixtures()["progress"].as_array().expect("progress") {
        let width = case["width"].as_u64().expect("width") as usize;
        let mut component = ToolExecutionComponent::new(
            "read",
            "call-1",
            serde_json::json!({ "file_path": "src/main.rs" }),
            ToolExecutionOptions { show_images: Some(false), image_width_cells: None },
            None,
            "/tmp/project",
            ToolExecutionPresentation::Classic,
            None,
            theme.clone(),
        );
        component.set_now_ms(case["nowMs"].as_f64().expect("nowMs"));
        component.mark_execution_started();
        component.set_args_complete();
        component.update_result(
            ToolExecutionResult {
                content: vec![ToolContent::text("partial")],
                details: Some(serde_json::json!({
                    "progress": { "startedAt": 995_000., "activity": "reading", "maxWaitMs": 60_000. }
                })),
                is_error: false,
            },
            true,
        );
        assert_eq!(trim(component.render(width)), expected(case), "progress at {width}");
    }
}
