use std::cell::RefCell;
use std::rc::Rc;

use maho_interactive::components::exploration_call::{ExplorationAction, exploration_call};
use maho_interactive::components::tool_execution::{
    ToolExecutionComponent, ToolExecutionOptions, ToolExecutionPresentation,
};
use maho_interactive::components::tool_execution_types::ToolExecutionResult;
use maho_interactive::theme::{ColorMode, Theme};
use maho_interactive::tools::renderers::{
    RenderedComponent, ToolRenderContext, ToolRenderResultOptions, ToolRenderers, create_all_tool_renderers,
    with_built_in_renderers,
};
use maho_tools::definition::{ToolContent, ToolResult};
use maho_tui::components::text::Text;
use maho_tui::tui::Component;
use serde_json::Value;

fn theme() -> Theme {
    Theme::builtin("dark", ColorMode::Color256).expect("builtin theme")
}

fn fixtures() -> Value {
    serde_json::from_str(include_str!("golden/components32-custom.json")).expect("pinned fixture")
}

fn trim(lines: Vec<String>) -> Vec<String> {
    lines.into_iter().map(|line| line.trim_end().to_owned()).collect()
}

#[derive(Default)]
struct ReplacedRenderers;

impl ToolRenderers for ReplacedRenderers {
    fn render_call(&mut self, _theme: &Theme, _context: &ToolRenderContext<'_>) -> Option<RenderedComponent> {
        Some(Rc::new(RefCell::new(Text::with_padding("CUSTOM CALL", 0, 0))))
    }
    fn render_result(
        &mut self,
        _result: &ToolResult,
        _options: ToolRenderResultOptions,
        _theme: &Theme,
        _context: &ToolRenderContext<'_>,
    ) -> Option<RenderedComponent> {
        Some(Rc::new(RefCell::new(Text::with_padding("CUSTOM RESULT", 0, 0))))
    }
}

fn built_in(name: &str) -> Option<Rc<RefCell<dyn ToolRenderers>>> {
    create_all_tool_renderers().get(name).cloned()
}

fn card(theme: &Theme, tool_name: &str, args: Value, definition: Option<Rc<RefCell<dyn ToolRenderers>>>) -> ToolExecutionComponent {
    ToolExecutionComponent::new(
        tool_name,
        "call-1",
        args,
        ToolExecutionOptions { show_images: Some(false), image_width_cells: None },
        definition,
        "/tmp/project",
        ToolExecutionPresentation::Classic,
        None,
        theme.clone(),
    )
}

#[test]
fn exploration_calls_match_pinned_senpi_including_replaced_renderers() {
    let theme = theme();
    for case in fixtures()["calls"].as_array().expect("calls") {
        let name = case["name"].as_str().expect("name");
        let tool_name = case["toolName"].as_str().expect("toolName");
        let definition = match name {
            "no-definition" | "non-exploration" => None,
            "custom-renderer" | "partial-override" => {
                Some(Rc::new(RefCell::new(ReplacedRenderers)) as Rc<RefCell<dyn ToolRenderers>>)
            }
            "with-builtin-renderers" => with_built_in_renderers(tool_name, None),
            _ => built_in(tool_name),
        };
        let component = card(&theme, tool_name, case["args"].clone(), definition);
        let actual = exploration_call(&component);
        let wanted = &case["call"];
        match (actual, wanted.is_null()) {
            (None, true) => {}
            (Some(call), false) => {
                let action = match call.action {
                    ExplorationAction::Read => "Read",
                    ExplorationAction::Search => "Search",
                    ExplorationAction::List => "List",
                };
                assert_eq!(action, wanted["action"].as_str().expect("action"), "{name} action");
                assert_eq!(call.label, wanted["label"].as_str().expect("label"), "{name} label");
                assert_eq!(call.pending, wanted["pending"].as_bool().expect("pending"), "{name} pending");
                assert_eq!(call.failed, wanted["failed"].as_bool().expect("failed"), "{name} failed");
            }
            (actual, _) => panic!("{name}: expected {wanted}, got {actual:?}"),
        }
    }
}

#[test]
fn a_card_with_replaced_renderers_matches_pinned_senpi() {
    let theme = theme();
    for case in fixtures()["customRender"].as_array().expect("customRender") {
        let width = case["width"].as_u64().expect("width") as usize;
        let mut component = card(
            &theme,
            "read",
            serde_json::json!({ "file_path": "a.rs" }),
            Some(Rc::new(RefCell::new(ReplacedRenderers)) as Rc<RefCell<dyn ToolRenderers>>),
        );
        component.set_args_complete();
        component.update_result(
            ToolExecutionResult { content: vec![ToolContent::text("x")], details: None, is_error: false },
            false,
        );
        component.stop_animation();
        let expected: Vec<String> =
            case["lines"].as_array().expect("lines").iter().map(|line| line.as_str().expect("line").to_owned()).collect();
        assert_eq!(trim(component.render(width)), expected, "custom render at {width}");
    }
}
