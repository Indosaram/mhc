use std::cell::RefCell;
use std::rc::Rc;

use maho_interactive::components::assistant_message::AssistantMessageComponent;
use maho_interactive::components::exploration_call::{ExplorationAction, ExplorationCall};
use maho_interactive::components::exploration_group::ExplorationGroup;
use maho_interactive::components::markdown_transform::get_markdown_theme;
use maho_interactive::components::tool_execution::{
    ToolExecutionComponent, ToolExecutionOptions, ToolExecutionPresentation,
};
use maho_interactive::components::user_message_selector::{UserMessageItem, UserMessageSelectorComponent};
use maho_interactive::theme::{ColorMode, Theme};
use maho_tui::tui::Component;
use serde_json::Value;

fn theme() -> Theme {
    Theme::builtin("dark", ColorMode::Color256).expect("builtin theme")
}

fn fixtures() -> Value {
    serde_json::from_str(include_str!("golden/components32-overflow.json")).expect("pinned fixture")
}

fn expected(case: &Value) -> Vec<String> {
    case["lines"].as_array().expect("lines").iter().map(|line| line.as_str().expect("line").to_owned()).collect()
}

fn trim(lines: Vec<String>) -> Vec<String> {
    lines.into_iter().map(|line| line.trim_end().to_owned()).collect()
}

fn call_from(value: &Value) -> ExplorationCall {
    ExplorationCall {
        action: match value["action"].as_str().expect("action") {
            "Read" => ExplorationAction::Read,
            "Search" => ExplorationAction::Search,
            _ => ExplorationAction::List,
        },
        label: value["label"].as_str().expect("label").to_owned(),
        pending: value["pending"].as_bool().expect("pending"),
        failed: value["failed"].as_bool().expect("failed"),
    }
}

#[test]
fn exploration_groups_overflow_and_dedupe_rules_like_pinned_senpi() {
    let theme = theme();
    for case in fixtures()["groups"].as_array().expect("groups") {
        let width = case["width"].as_u64().expect("width") as usize;
        let name = case["name"].as_str().expect("name");
        let mut members: Vec<Rc<RefCell<ToolExecutionComponent>>> = Vec::new();
        let mut group_calls: Vec<(Rc<RefCell<ToolExecutionComponent>>, ExplorationCall)> = Vec::new();
        for (index, call) in case["calls"].as_array().expect("calls").iter().enumerate() {
            let component = Rc::new(RefCell::new(ToolExecutionComponent::new(
                "grep",
                &format!("call-{index}"),
                serde_json::json!({ "pattern": "p" }),
                ToolExecutionOptions { show_images: Some(false), image_width_cells: None },
                None,
                "/tmp/project",
                ToolExecutionPresentation::Classic,
                None,
                theme.clone(),
            )));
            members.push(Rc::clone(&component));
            group_calls.push((component, call_from(call)));
        }
        let rules: Vec<String> =
            case["rules"].as_array().expect("rules").iter().map(|rule| rule.as_str().expect("rule").to_owned()).collect();
        let mut group = ExplorationGroup::new(theme.clone());
        group.set_members(
            members.into_iter().map(|component| component as Rc<RefCell<dyn Component>>).collect(),
            group_calls,
            rules,
        );
        assert_eq!(trim(group.render(width)), expected(case), "group {name} at {width}");
    }
}

#[test]
fn assistant_message_setters_match_pinned_senpi() {
    let theme = theme();
    let markdown_theme = get_markdown_theme(&theme);
    for case in fixtures()["assistant"].as_array().expect("assistant") {
        let width = case["width"].as_u64().expect("width") as usize;
        let name = case["name"].as_str().expect("name");
        let hide = case["hideThinkingBlock"].as_bool().expect("hideThinkingBlock");
        let mut component = AssistantMessageComponent::new(
            Some(case["message"].clone()),
            hide,
            markdown_theme.clone(),
            "Thinking...",
            1,
            Vec::new(),
            theme.clone(),
        );
        match name {
            "hide-thinking" => component.set_hide_thinking_block(true),
            "hidden-label" => component.set_hidden_thinking_label("Hmm..."),
            "output-pad" => component.set_output_pad(3),
            "expanded" => component.set_expanded(true),
            _ => {}
        }
        assert_eq!(
            component.is_exploration_detail(),
            case["isExplorationDetail"].as_bool().expect("isExplorationDetail"),
            "assistant {name} at {width} is_exploration_detail"
        );
        assert_eq!(trim(component.render(width)), expected(case), "assistant {name} at {width}");
    }
}

#[test]
fn user_message_selector_scrolls_and_reports_an_empty_list_like_pinned_senpi() {
    let theme = theme();
    for case in fixtures()["selectors"].as_array().expect("selectors") {
        let width = case["width"].as_u64().expect("width") as usize;
        let name = case["name"].as_str().expect("name");
        let messages: Vec<UserMessageItem> = case["messages"]
            .as_array()
            .expect("messages")
            .iter()
            .map(|message| UserMessageItem {
                id: message["id"].as_str().expect("id").to_owned(),
                text: message["text"].as_str().expect("text").to_owned(),
                timestamp: None,
            })
            .collect();
        let mut component = UserMessageSelectorComponent::new(
            messages,
            Box::new(|_| {}),
            Box::new(|| {}),
            case["initialSelectedId"].as_str(),
            theme.clone(),
        );
        assert_eq!(trim(component.render(width)), expected(case), "selector {name} at {width}");
    }
}
