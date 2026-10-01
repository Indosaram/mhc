use std::cell::RefCell;
use std::rc::Rc;

use maho_interactive::components::assistant_message::AssistantMessageComponent;
use maho_interactive::components::custom_entry::CustomEntryComponent;
use maho_interactive::components::exploration_transcript_container::{
    ExplorationTranscriptContainer, TranscriptChild,
};
use maho_interactive::components::markdown_transform::get_markdown_theme;
use maho_interactive::components::progressive_transcript_container::ProgressiveTranscriptOptions;
use maho_interactive::components::tool_execution::{
    ToolExecutionComponent, ToolExecutionOptions, ToolExecutionPresentation,
};
use maho_interactive::components::tool_execution_types::ToolExecutionResult;
use maho_interactive::theme::{ColorMode, Theme};
use maho_tools::definition::ToolContent;
use maho_tui::components::text::Text;
use maho_tui::tui::Component;
use serde_json::Value;

fn theme() -> Theme {
    Theme::builtin("dark", ColorMode::Color256).expect("builtin theme")
}

fn fixtures() -> Value {
    serde_json::from_str(include_str!("golden/components32-transcript.json")).expect("pinned fixture")
}

fn expected(case: &Value) -> Vec<String> {
    case["lines"].as_array().expect("lines").iter().map(|line| line.as_str().expect("line").to_owned()).collect()
}

fn trim(lines: Vec<String>) -> Vec<String> {
    lines.into_iter().map(|line| line.trim_end().to_owned()).collect()
}

fn tool_card(theme: &Theme, name: &str, id: &str, args: Value, output: &str) -> Rc<RefCell<ToolExecutionComponent>> {
    let component = Rc::new(RefCell::new(ToolExecutionComponent::new(
        name,
        id,
        args,
        ToolExecutionOptions { show_images: Some(false), image_width_cells: None },
        None,
        "/tmp/project",
        ToolExecutionPresentation::Classic,
        None,
        theme.clone(),
    )));
    component.borrow_mut().update_result(
        ToolExecutionResult { content: vec![ToolContent::text(output)], details: None, is_error: false },
        false,
    );
    component.borrow_mut().stop_animation();
    component
}

fn assistant(theme: &Theme, message: Value) -> Rc<RefCell<AssistantMessageComponent>> {
    Rc::new(RefCell::new(AssistantMessageComponent::new(
        Some(message),
        false,
        get_markdown_theme(theme),
        "Thinking...",
        1,
        Vec::new(),
        theme.clone(),
    )))
}

#[test]
fn exploration_transcript_projection_matches_pinned_senpi() {
    let theme = theme();
    for case in fixtures()["transcript"].as_array().expect("transcript") {
        let width = case["width"].as_u64().expect("width") as usize;
        let mut container = ExplorationTranscriptContainer::new(
            ProgressiveTranscriptOptions {
                tail_budget: 60,
                warm_chunk_size: 100,
                request_render: Rc::new(|| {}),
            },
            theme.clone(),
        );
        container.add_child(TranscriptChild::Tool(tool_card(
            &theme,
            "read",
            "call-1",
            serde_json::json!({ "file_path": "a.rs" }),
            "body\n",
        )));
        container.add_child(TranscriptChild::Tool(tool_card(
            &theme,
            "read",
            "call-1",
            serde_json::json!({ "file_path": "b.rs" }),
            "body\n",
        )));
        container.add_child(TranscriptChild::Assistant(assistant(
            &theme,
            serde_json::json!({ "content": [{ "type": "thinking", "thinking": "reasoning" }], "stopReason": "stop" }),
        )));
        let renderer = Rc::new(|_entry: &Value, _expanded: bool, _theme: &Theme| {
            Ok(Some(Rc::new(RefCell::new(Text::with_padding("rules", 0, 0))) as Rc<RefCell<dyn Component>>))
        });
        container.add_child(TranscriptChild::Entry(Rc::new(RefCell::new(CustomEntryComponent::new(
            serde_json::json!({
                "customType": "rule-activation",
                "data": { "kind": "project-rules", "targetPath": "src", "rules": ["r1", "r2"], "toolCallId": "call-1" }
            }),
            renderer,
            theme.clone(),
        )))));
        container.add_child(TranscriptChild::Tool(tool_card(
            &theme,
            "grep",
            "call-2",
            serde_json::json!({ "pattern": "needle", "path": "src" }),
            "no matches",
        )));
        container.add_child(TranscriptChild::Assistant(assistant(
            &theme,
            serde_json::json!({ "content": [{ "type": "text", "text": "done" }], "stopReason": "stop" }),
        )));
        assert_eq!(trim(container.render(width)), expected(case), "transcript at {width}");
    }
}

#[test]
fn tool_execution_fallback_matches_pinned_senpi_without_a_renderer() {
    let theme = theme();
    for case in fixtures()["fallback"].as_array().expect("fallback") {
        let width = case["width"].as_u64().expect("width") as usize;
        let with_result = case["name"].as_str() == Some("with-result");
        let args = if with_result { serde_json::json!({ "alpha": 1 }) } else { serde_json::json!({ "alpha": 1, "beta": "two" }) };
        let mut component = ToolExecutionComponent::new(
            "custom_tool",
            "call-9",
            args,
            ToolExecutionOptions { show_images: Some(false), image_width_cells: None },
            None,
            "/tmp/project",
            ToolExecutionPresentation::Classic,
            None,
            theme.clone(),
        );
        component.mark_execution_started();
        component.set_args_complete();
        if with_result {
            component.update_result(
                ToolExecutionResult {
                    content: vec![ToolContent::text("custom output")],
                    details: None,
                    is_error: false,
                },
                false,
            );
        }
        component.stop_animation();
        assert_eq!(
            trim(component.render(width)),
            expected(case),
            "fallback {} at {width}",
            case["name"].as_str().unwrap_or("")
        );
    }
}
