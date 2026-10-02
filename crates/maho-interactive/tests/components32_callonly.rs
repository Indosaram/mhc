use std::cell::RefCell;
use std::rc::Rc;

use maho_interactive::components::tool_execution::{
    ToolExecutionComponent, ToolExecutionOptions, ToolExecutionPresentation,
};
use maho_interactive::components::tool_execution_types::ToolExecutionResult;
use maho_interactive::theme::{ColorMode, Theme};
use maho_interactive::tools::renderers::{
    RenderedComponent, ToolRenderContext, ToolRenderResultOptions, ToolRenderers,
};
use maho_tools::definition::{ToolContent, ToolResult};
use maho_tui::components::text::Text;
use maho_tui::tui::Component;
use serde_json::Value;

fn theme() -> Theme {
    Theme::builtin("dark", ColorMode::Color256).expect("builtin theme")
}

fn fixtures() -> Value {
    serde_json::from_str(include_str!("golden/components32-callonly.json")).expect("pinned fixture")
}

fn trim(lines: Vec<String>) -> Vec<String> {
    lines.into_iter().map(|line| line.trim_end().to_owned()).collect()
}

fn strip_ansi(text: &str) -> String {
    maho_tui::utils::strip_terminal_sequences(text)
}

#[derive(Default)]
struct CallOnlyRenderers;

impl ToolRenderers for CallOnlyRenderers {
    fn has_result_renderer(&self) -> bool {
        false
    }
    fn render_call(&mut self, _theme: &Theme, _context: &ToolRenderContext<'_>) -> Option<RenderedComponent> {
        Some(Rc::new(RefCell::new(Text::with_padding("CALL ONLY", 0, 0))))
    }
    fn render_result(
        &mut self,
        _result: &ToolResult,
        _options: ToolRenderResultOptions,
        _theme: &Theme,
        _context: &ToolRenderContext<'_>,
    ) -> Option<RenderedComponent> {
        None
    }
}

#[test]
fn a_call_only_renderer_collapses_its_fallback_result_like_pinned_senpi() {
    let theme = theme();
    for case in fixtures()["cases"].as_array().expect("cases") {
        let width = case["width"].as_u64().expect("width") as usize;
        let expanded = case["expanded"].as_bool().expect("expanded");
        let name = case["name"].as_str().expect("name");
        let mut component = ToolExecutionComponent::new(
            "custom_tool",
            "call-1",
            serde_json::json!({ "alpha": 1 }),
            ToolExecutionOptions { show_images: Some(false), image_width_cells: None },
            Some(Rc::new(RefCell::new(CallOnlyRenderers)) as Rc<RefCell<dyn ToolRenderers>>),
            "/tmp/project",
            ToolExecutionPresentation::Classic,
            None,
            theme.clone(),
        );
        component.set_args_complete();
        component.update_result(
            ToolExecutionResult {
                content: vec![ToolContent::text(case["output"].as_str().expect("output"))],
                details: None,
                is_error: false,
            },
            false,
        );
        component.set_expanded(expanded);
        component.stop_animation();
        let actual = trim(component.render(width));
        let wanted: Vec<String> =
            case["lines"].as_array().expect("lines").iter().map(|line| line.as_str().expect("line").to_owned()).collect();
        assert_eq!(actual.len(), wanted.len(), "call-only {name} at {width} expanded={expanded} line count");
        for (index, (left, right)) in actual.iter().zip(&wanted).enumerate() {
            assert_eq!(strip_ansi(left), strip_ansi(right), "call-only {name} line {index} at {width}");
        }
    }
}
