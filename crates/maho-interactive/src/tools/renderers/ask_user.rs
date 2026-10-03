//! Port of `core/extensions/builtin/ask-user/render.ts`.

use std::cell::RefCell;
use std::rc::Rc;

use maho_tools::definition::ToolResult;
use maho_tui::components::text::Text;
use serde_json::json;

use super::{RenderedComponent, ToolRenderContext, ToolRenderResultOptions, ToolRenderers};
use crate::theme::{Theme, ThemeColor};

pub struct AskUserRenderers;

impl ToolRenderers for AskUserRenderers {
    fn render_call(&mut self, theme: &Theme, context: &ToolRenderContext<'_>) -> Option<RenderedComponent> {
        let headers = maho_ext_ask_user::render::call_headers(context.args);
        let wait = if maho_ext_ask_user::render::wait_for_answer(context.args) { "wait for answer" } else { "answer later" };
        Some(Rc::new(RefCell::new(Text::with_padding(format!("{} {wait}", theme.fg(ThemeColor::ToolTitle, &headers)), 0, 0))))
    }

    fn render_result(&mut self, result: &ToolResult, _options: ToolRenderResultOptions, _theme: &Theme, _context: &ToolRenderContext<'_>) -> Option<RenderedComponent> {
        let value = json!({ "content": result.content, "details": result.details });
        Some(Rc::new(RefCell::new(Text::with_padding(maho_ext_ask_user::render::result_text(&value), 0, 0))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context<'a>(args: &'a serde_json::Value) -> ToolRenderContext<'a> {
        ToolRenderContext {
            args,
            tool_call_id: "call",
            cwd: "/tmp",
            execution_started: true,
            args_complete: true,
            is_partial: false,
            expanded: false,
            show_images: false,
            is_error: false,
            has_result: false,
            spinner_frame: None,
            now_ms: 0.,
            invalidate: Rc::new(|| {}),
        }
    }

    fn render(component: RenderedComponent) -> String {
        use maho_tui::tui::Component;
        component.borrow_mut().render(80).join("\n")
    }

    #[test]
    fn call_line_lists_headers_and_the_wait_mode() {
        let theme = Theme::builtin("dark", crate::theme::ColorMode::Truecolor).expect("theme");
        let args = json!({"questions": [{"header": "Pick one"}, {"header": "Then this"}], "waitForAnswer": true});
        let line = render(AskUserRenderers.render_call(&theme, &context(&args)).expect("call renderer"));
        assert!(line.contains("[Pick one] [Then this]"), "headers: {line}");
        assert!(line.contains("wait for answer"), "wait mode: {line}");
        let args = json!({"questions": [{"header": "Pick one"}], "wait_for_answer": false});
        let line = render(AskUserRenderers.render_call(&theme, &context(&args)).expect("call renderer"));
        assert!(line.contains("answer later"), "later mode: {line}");
        let args = json!({});
        let line = render(AskUserRenderers.render_call(&theme, &context(&args)).expect("call renderer"));
        assert!(line.contains("Question answer later"), "no-questions label: {line}");
    }

    #[test]
    fn result_line_summarises_status_answers_and_unanswered() {
        let theme = Theme::builtin("dark", crate::theme::ColorMode::Truecolor).expect("theme");
        let result = ToolResult {
            content: vec![maho_tools::definition::ToolContent::text("The user responded")],
            details: Some(json!({"status": "answered", "answers": {"a": "x"}, "unanswered": ["b"]})),
        };
        let line = render(AskUserRenderers.render_result(&result, ToolRenderResultOptions::default(), &theme, &context(&json!({}))).expect("result renderer"));
        assert!(line.contains("answered; 1 answered; 1 unanswered"), "summary: {line}");
        assert!(line.contains("The user responded"), "content text: {line}");
    }
}