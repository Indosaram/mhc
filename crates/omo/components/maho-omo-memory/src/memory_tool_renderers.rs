//! The `memory` tool's `renderCall`/`renderResult` (pin `tools.ts` memory tool definition,
//! `renderShell: "self"`): a pending `Box(1, 1)` while the write runs, then the write notice on
//! `customMessageBg`, or the plain tool card when the write notice is disabled.

use std::{cell::RefCell, rc::Rc, sync::Arc};

use maho_ext_api::{AgentToolResult, Component, ContentBlock, Theme, ToolRenderers};
use maho_tui::components::{box_::Box as TuiBox, text::Text};
use serde_json::Value;

use crate::memory_notice_spec::{
    MemoryNoticeArgs, MemoryWriteNotice, memory_degraded_notice_spec, memory_failure_notice_spec, memory_pending_line,
    memory_write_notice_spec,
};
use crate::tool_metadata::MEMORY_TOOL_NAME;
use crate::tools::WriteNoticeGate;
use crate::worker::entry_renderers::{EntryRenderTheme, NoticeSpec, normalize_renderer_text, notice_box};

struct ApiTheme(Theme);
impl EntryRenderTheme for ApiTheme {
    fn fg(&self, tone: &str, text: &str) -> String { self.0.fg(tone, text) }
    fn italic(&self, text: &str) -> String { format!("\x1b[3m{text}\x1b[23m") }
    fn bg(&self, name: &str, text: &str) -> String { self.0.bg(name, text) }
}

struct Empty;
impl Component for Empty {
    fn render(&mut self, _width: usize) -> Vec<String> { Vec::new() }
}

fn notice_args(args: &Value) -> MemoryNoticeArgs {
    let field = |key: &str| args.get(key).and_then(Value::as_str).map(str::to_owned);
    MemoryNoticeArgs { command: field("command"), file_path: field("file_path"), old_path: field("old_path"), new_path: field("new_path") }
}

/// omo `resultMessage`: `details.message`, else the first non-empty text block.
fn result_message(result: &AgentToolResult) -> String {
    if let Some(message) = result.details.get("message").and_then(Value::as_str).filter(|message| !message.is_empty()) {
        return message.to_owned();
    }
    result.content.iter().find_map(|block| match block {
        ContentBlock::Text(text) if !text.text.is_empty() => Some(text.text.clone()),
        _ => None,
    }).unwrap_or_default()
}

fn now_ms() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |elapsed| elapsed.as_millis() as f64)
}

fn notice_spec(result: &AgentToolResult, is_error: bool, args: &MemoryNoticeArgs) -> NoticeSpec {
    if is_error {
        return memory_failure_notice_spec(&result_message(result), args);
    }
    match result.details.get("writeNotice").cloned().map(serde_json::from_value::<MemoryWriteNotice>) {
        Some(Ok(notice)) => memory_write_notice_spec(&notice, now_ms(), args),
        _ => memory_degraded_notice_spec(args),
    }
}

fn text(content: String) -> Rc<RefCell<dyn Component>> { Rc::new(RefCell::new(Text::with_padding(content, 0, 0))) }

fn plain_card(result: &AgentToolResult, args: &Value, is_error: bool, theme: &Theme) -> TuiBox {
    let target = ["file_path", "old_path", "new_path", "reason"].iter().find_map(|key| args.get(*key).and_then(Value::as_str));
    let summary = [args.get("command").and_then(Value::as_str), target].into_iter().flatten().collect::<Vec<_>>().join(" ");
    let mut card = TuiBox::with_padding(1, 1);
    let background = theme.clone();
    let name = if is_error { "toolErrorBg" } else { "toolSuccessBg" };
    card.set_bg_fn(Some(Rc::new(move |line: &str| background.bg(name, line))));
    card.add_child(text(format!("{} {}", theme.bold(MEMORY_TOOL_NAME), normalize_renderer_text(&summary)).trim_end().to_owned()));
    card.add_child(text(result_message(result).split('\n').map(normalize_renderer_text).collect::<Vec<_>>().join("\n")));
    card
}

/// Renderers for the `memory` tool; `write_notice` gates the notice card per render.
pub fn memory_tool_renderers(write_notice: Option<WriteNoticeGate>) -> ToolRenderers<(), Value> {
    ToolRenderers {
        render_call: Some(Arc::new(|args, theme, context| {
            if context.has_result.unwrap_or(false) {
                return Box::new(Empty) as Box<dyn Component>;
            }
            let mut pending = TuiBox::with_padding(1, 1);
            let background = theme.clone();
            pending.set_bg_fn(Some(Rc::new(move |line: &str| background.bg("toolPendingBg", line))));
            pending.add_child(text(theme.fg("dim", &memory_pending_line(&notice_args(args)))));
            Box::new(pending)
        })),
        render_result: Some(Arc::new(move |result, options, theme, context| {
            let enabled = write_notice.as_ref().is_some_and(|gate| gate());
            if !enabled {
                return Box::new(plain_card(result, &context.args, context.is_error, theme)) as Box<dyn Component>;
            }
            let spec = notice_spec(result, context.is_error, &notice_args(&context.args));
            let theme: Arc<dyn EntryRenderTheme> = Arc::new(ApiTheme(theme.clone()));
            Box::new(notice_box(&spec, options.expanded, &theme))
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use maho_ext_api::{ToolRenderContext, ToolRenderResultOptions};

    fn context(args: Value, has_result: bool, is_error: bool) -> ToolRenderContext<(), Value> {
        ToolRenderContext { args, tool_call_id: "call".into(), invalidate: Rc::new(|| {}), last_component: None, state: (), cwd: Default::default(),
            execution_started: true, args_complete: true, is_partial: !has_result, expanded: false, show_images: false, image_protocol: None,
            is_error, has_result: Some(has_result), spinner_frame: None }
    }

    fn theme() -> Theme {
        Theme { backgrounds: [("toolPendingBg", "<P>"), ("toolSuccessBg", "<S>"), ("toolErrorBg", "<E>"), ("customMessageBg", "<C>")]
            .into_iter().map(|(name, value)| (name.to_owned(), value.to_owned())).collect(), ..Default::default() }
    }

    fn joined(mut component: Box<dyn Component>) -> String { component.render(60).join("\n") }

    #[test]
    fn pending_call_draws_a_pending_box_and_settles_to_nothing() {
        let renderers = memory_tool_renderers(None);
        let call = renderers.render_call.as_ref().unwrap();
        let args = serde_json::json!({"command":"create","reason":"r","file_path":"notes/a.md"});
        let pending = joined(call(&args, &theme(), &mut context(args.clone(), false, false)));
        assert!(pending.contains("<P>") && pending.contains("Remembering") && pending.contains("notes/a.md"), "{pending}");
        assert!(call(&args, &theme(), &mut context(args.clone(), true, false)).render(60).is_empty());
    }

    #[test]
    fn result_draws_the_notice_on_custom_message_bg_when_enabled() {
        let renderers = memory_tool_renderers(Some(Arc::new(|| true)));
        let render = renderers.render_result.as_ref().unwrap();
        let args = serde_json::json!({"command":"create","reason":"r","file_path":"notes/a.md"});
        let ok = AgentToolResult::text("created");
        let notice = joined(render(&ok, ToolRenderResultOptions::default(), &theme(), &mut context(args.clone(), true, false)));
        assert!(notice.contains("<C>") && notice.contains("Remembered"), "{notice}");
        let failed = AgentToolResult::text("memory: old_string was not found");
        let failure = joined(render(&failed, ToolRenderResultOptions::default(), &theme(), &mut context(args.clone(), true, true)));
        assert!(failure.contains("<C>") && failure.contains("Not remembered"), "{failure}");
    }

    #[test]
    fn result_falls_back_to_the_plain_card_when_the_notice_is_disabled() {
        let renderers = memory_tool_renderers(Some(Arc::new(|| false)));
        let render = renderers.render_result.as_ref().unwrap();
        let args = serde_json::json!({"command":"delete","reason":"r","file_path":"notes/a.md"});
        let card = joined(render(&AgentToolResult::text("deleted"), ToolRenderResultOptions::default(), &theme(), &mut context(args.clone(), true, true)));
        assert!(card.contains("<E>") && card.contains("delete notes/a.md") && card.contains("deleted"), "{card}");
    }
}
