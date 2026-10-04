//! Port of `interactive-mode.ts`'s registered tool definition rendering, over the erased factory
//! (`maho_ext_api::ErasedToolRenderers`).
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use maho_ext_api::{AgentToolResult, ErasedToolRenderers};
use maho_tools::definition::{ToolContent, ToolResult};
use maho_tui::tui::Component;
use serde_json::Value;

use super::{RenderedComponent, ToolRenderContext, ToolRenderResultOptions, ToolRenderers};
use crate::theme::Theme;

struct RegisteredCall {
    renderers: Arc<dyn ErasedToolRenderers>,
    args: Value,
    theme: maho_ext_api::Theme,
}

impl Component for RegisteredCall {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.renderers.render_call(&self.args, &self.theme, width).unwrap_or_default()
    }
}

struct RegisteredResult {
    renderers: Arc<dyn ErasedToolRenderers>,
    args: Value,
    result: AgentToolResult,
    theme: maho_ext_api::Theme,
}

impl Component for RegisteredResult {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.renderers.render_result(&self.args, &self.result, &self.theme, width).unwrap_or_default()
    }
}

pub struct RegisteredToolRenderers {
    renderers: Arc<dyn ErasedToolRenderers>,
}

impl RegisteredToolRenderers {
    pub fn new(renderers: Arc<dyn ErasedToolRenderers>) -> Self {
        Self { renderers }
    }
}

pub(super) fn to_agent_result(result: &ToolResult) -> AgentToolResult {
    serde_json::from_value(serde_json::to_value(result).unwrap_or(Value::Null)).unwrap_or_else(|_| {
        AgentToolResult::text(
            result
                .content
                .iter()
                .filter_map(|content| match content {
                    ToolContent::Text { text, .. } => Some(text.clone()),
                    ToolContent::Image { .. } => None,
                })
                .collect::<Vec<_>>()
                .join(""),
        )
    })
}

impl ToolRenderers for RegisteredToolRenderers {
    fn render_call(&mut self, theme: &Theme, context: &ToolRenderContext<'_>) -> Option<RenderedComponent> {
        Some(Rc::new(RefCell::new(RegisteredCall {
            renderers: self.renderers.clone(),
            args: context.args.clone(),
            theme: crate::interactive_extension_ui::extension_theme(theme),
        })))
    }

    fn render_result(&mut self, result: &ToolResult, _options: ToolRenderResultOptions, theme: &Theme, context: &ToolRenderContext<'_>) -> Option<RenderedComponent> {
        Some(Rc::new(RefCell::new(RegisteredResult {
            renderers: self.renderers.clone(),
            args: context.args.clone(),
            result: to_agent_result(result),
            theme: crate::interactive_extension_ui::extension_theme(theme),
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeErased;

    impl ErasedToolRenderers for FakeErased {
        fn render_call(&self, args: &Value, _theme: &maho_ext_api::Theme, width: usize) -> Option<Vec<String>> {
            Some(vec![format!("call {} @{width}", args["x"])])
        }
        fn render_result(&self, _args: &Value, result: &AgentToolResult, _theme: &maho_ext_api::Theme, width: usize) -> Option<Vec<String>> {
            Some(vec![format!("result {} @{width}", result.details["kind"].as_str().unwrap_or_default())])
        }
    }

    #[test]
    fn registered_renderers_defer_to_the_erased_factory_at_the_live_width() {
        let mut renderers = RegisteredToolRenderers::new(Arc::new(FakeErased));
        let theme = crate::theme::Theme::builtin("dark", crate::theme::ColorMode::Truecolor).expect("theme");
        let args = serde_json::json!({"x": 1});
        let context = ToolRenderContext {
            args: &args,
            tool_call_id: "id",
            cwd: "/tmp",
            execution_started: false,
            args_complete: true,
            is_partial: false,
            expanded: false,
            show_images: true,
            is_error: false,
            has_result: false,
            spinner_frame: None,
            now_ms: 0.,
            invalidate: Rc::new(|| {}),
        };
        let call = renderers.render_call(&theme, &context).expect("call component");
        assert_eq!(call.borrow_mut().render(40), vec!["call 1 @40".to_owned()]);
        let result = ToolResult { content: vec![ToolContent::text("ok")], details: Some(serde_json::json!({"kind": "image"})) };
        let component = renderers.render_result(&result, ToolRenderResultOptions::default(), &theme, &context).expect("result component");
        assert_eq!(component.borrow_mut().render(40), vec!["result image @40".to_owned()]);
    }
}
