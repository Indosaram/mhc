use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use maho_ext_api::{AgentToolResult, ErasedToolRenderers};
use maho_interactive::components::tool_execution::{ToolExecutionComponent, ToolExecutionOptions, ToolExecutionPresentation};
use maho_interactive::components::tool_execution_types::ToolExecutionResult;
use maho_interactive::replay_assistant_tools::{ReplayToolHost, replay_assistant_tools};
use maho_interactive::theme::{ColorMode, Theme};
use maho_interactive::tools::renderers::{ToolRenderers, registered::RegisteredToolRenderers};
use maho_tools::definition::ToolContent;
use maho_tui::tui::Component;
use serde_json::Value;

struct FakeErased;

impl ErasedToolRenderers for FakeErased {
    fn render_call(&self, _args: &Value, _theme: &maho_ext_api::Theme, _width: usize) -> Option<Vec<String>> {
        Some(vec!["REGISTERED CALL".to_owned()])
    }
    fn render_result(&self, _args: &Value, _result: &AgentToolResult, _theme: &maho_ext_api::Theme, _width: usize) -> Option<Vec<String>> {
        Some(vec!["REGISTERED RESULT".to_owned()])
    }
}

fn theme() -> Theme {
    Theme::builtin("dark", ColorMode::Truecolor).expect("theme")
}

fn registered() -> Rc<RefCell<dyn ToolRenderers>> {
    Rc::new(RefCell::new(RegisteredToolRenderers::new(Arc::new(FakeErased))))
}

fn card(name: &str, definition: Option<Rc<RefCell<dyn ToolRenderers>>>) -> ToolExecutionComponent {
    ToolExecutionComponent::new(
        name,
        "call-1",
        serde_json::json!({}),
        ToolExecutionOptions { show_images: Some(false), image_width_cells: None },
        definition,
        "/tmp/project",
        ToolExecutionPresentation::Classic,
        None,
        theme(),
    )
}

#[test]
fn a_live_card_renders_registered_call_and_result_renderers() {
    let mut component = card("registered_tool", Some(registered()));
    assert!(component.render(80).join("\n").contains("REGISTERED CALL"), "the call half renders through the erased factory");
    component.update_result(ToolExecutionResult { content: vec![ToolContent::text("done")], details: None, is_error: false }, false);
    assert!(component.render(80).join("\n").contains("REGISTERED RESULT"), "the result half renders through the erased factory");
}

#[test]
fn a_card_without_a_registered_definition_uses_the_built_in_renderer() {
    let mut component = card("read", None);
    let rendered = component.render(80).join("\n");
    assert!(!rendered.contains("REGISTERED CALL"), "a tool with no registered definition must not take the registered path");
}

struct ReplayHost {
    definition: Rc<RefCell<dyn ToolRenderers>>,
    cards: Vec<Rc<RefCell<ToolExecutionComponent>>>,
}

impl ReplayToolHost for ReplayHost {
    fn expanded(&self) -> bool {
        false
    }
    fn add_message(&mut self, _message: maho_ai::types::AssistantMessage) {}
    fn add_child(&mut self, component: Rc<RefCell<ToolExecutionComponent>>) {
        self.cards.push(component);
    }
    fn create_tool(&mut self, name: &str, id: &str, args: &serde_json::Map<String, Value>) -> ToolExecutionComponent {
        ToolExecutionComponent::new(
            name,
            id,
            Value::Object(args.clone()),
            ToolExecutionOptions { show_images: Some(false), image_width_cells: None },
            Some(self.definition.clone()),
            "/tmp/project",
            ToolExecutionPresentation::Classic,
            None,
            theme(),
        )
    }
    fn add_pending(&mut self, _id: &str, _component: Rc<RefCell<ToolExecutionComponent>>) {}
}

#[test]
fn a_replay_card_renders_registered_renderers() {
    let mut message = maho_ai::providers::faux::faux_assistant_message("", Default::default());
    message.content = vec![maho_ai::types::ContentBlock::ToolCall(maho_ai::types::ToolCall {
        id: "call-3".into(),
        name: "registered_tool".into(),
        arguments: serde_json::Map::new(),
        ..Default::default()
    })];
    let mut host = ReplayHost { definition: registered(), cards: Vec::new() };
    replay_assistant_tools(&message, &mut host);
    assert_eq!(host.cards.len(), 1, "the replayed tool call produced one card");
    assert!(host.cards[0].borrow_mut().render(80).join("\n").contains("REGISTERED CALL"), "the replay card renders through the erased factory");
}

#[test]
fn native_slots_preserve_each_half_and_isolate_interleaved_tool_calls() {
    use maho_interactive::tools::renderers::{native::NativeToolRenderers, ToolRenderContext, ToolRenderResultOptions};
    use maho_tools::definition::ToolResult;
    struct NativeChild { text: String }
    impl Component for NativeChild {
        fn render(&mut self, width: usize) -> Vec<String> { vec![format!("{}:{width}", self.text)] }
    }
    let native = maho_ext_api::ToolRenderers {
        render_call: Some(Arc::new(|_: &Value, _: &maho_ext_api::Theme, context: &mut maho_ext_api::ToolRenderContext<usize, Value>| {
            let previous = context.last_component.as_mut().map(|component| component.render(40).join(""));
            context.state += 1;
            Box::new(NativeChild { text: format!("call:{}:{}:{previous:?}", context.tool_call_id, context.state) })
        })),
        render_result: Some(Arc::new(|_: &AgentToolResult, options, _: &maho_ext_api::Theme, context: &mut maho_ext_api::ToolRenderContext<usize, Value>| {
            let previous = context.last_component.as_mut().map(|component| component.render(40).join(""));
            Box::new(NativeChild { text: format!("result:{}:{}:{}:{}:{previous:?}", context.tool_call_id, context.state, options.expanded, options.is_partial) })
        })),
    };
    let mut renderers = NativeToolRenderers::new(Arc::new(native));
    let args = serde_json::json!({});
    let mut context = ToolRenderContext { args: &args, tool_call_id: "first", cwd: "/tmp", execution_started: true,
        args_complete: true, is_partial: false, expanded: false, show_images: false, is_error: false,
        has_result: false, spinner_frame: None, now_ms: 0.0, invalidate: Rc::new(|| {}) };
    let first = renderers.render_call(&theme(), &context).expect("first");
    assert_eq!(first.borrow_mut().render(40), ["call:first:1:None:40"]);
    context.tool_call_id = "second";
    let second = renderers.render_call(&theme(), &context).expect("second");
    assert_eq!(second.borrow_mut().render(80), ["call:second:1:None:80"]);
    context.tool_call_id = "first";
    let result = ToolResult { content: vec![ToolContent::text("done")], details: Some(serde_json::json!({"key":1})) };
    let result_child = renderers.render_result(&result, ToolRenderResultOptions { expanded: true, is_partial: true }, &theme(), &context).expect("result");
    assert_eq!(result_child.borrow_mut().render(80), ["result:first:1:true:true:None:80"]);
    let next_call = renderers.render_call(&theme(), &context).expect("next call");
    let lines = next_call.borrow_mut().render(80).join("");
    assert!(lines.contains("call:first:2:Some(\"call:first:1:None:40\")"));
    assert!(!lines.contains("result:first"));
}

#[test]
fn native_missing_result_half_retains_plain_card_fallback() {
    let native = maho_ext_api::ToolRenderers::<(), Value> {
        render_call: Some(Arc::new(|_, _, _| Box::new(maho_tui::components::text::Text::new("native call")))),
        render_result: None,
    };
    let definition = Rc::new(RefCell::new(maho_interactive::tools::renderers::native::NativeToolRenderers::new(Arc::new(native))));
    let mut component = card("native", Some(definition));
    component.update_result(ToolExecutionResult { content: vec![ToolContent::text("fallback result")], details: None, is_error: false }, false);
    let lines = component.render(80).join("\n");
    assert!(lines.contains("native call")); assert!(lines.contains("fallback result"));
}

#[test]
fn ask_card_fallback_reuses_the_ask_user_renderers() {
    use maho_interactive::tools::renderers::{ask_user_renderers, ToolRenderContext, ToolRenderResultOptions};
    use maho_tools::definition::ToolResult;
    let renderers = ask_user_renderers("ask_user_question").expect("the ask-user pair is covered");
    assert!(ask_user_renderers("read").is_none(), "a non ask-user tool takes no ask-user fallback");
    let args = serde_json::json!({"questions": [{"header": "Pick one"}, {"header": "Then this"}], "waitForAnswer": true});
    let context = ToolRenderContext { args: &args, tool_call_id: "ask", cwd: "/tmp", execution_started: true,
        args_complete: true, is_partial: false, expanded: false, show_images: false, is_error: false,
        has_result: false, spinner_frame: None, now_ms: 0.0, invalidate: Rc::new(|| {}) };
    let call = renderers.borrow_mut().render_call(&theme(), &context).expect("call half");
    let line = call.borrow_mut().render(80).join("\n");
    assert!(line.contains("[Pick one] [Then this]"), "headers: {line}");
    assert!(line.contains("wait for answer"), "wait mode: {line}");
    let result = ToolResult { content: vec![ToolContent::text("The user responded")], details: Some(serde_json::json!({"status": "answered", "answers": {"a": "x"}, "unanswered": ["b"]})) };
    let result_child = renderers.borrow_mut().render_result(&result, ToolRenderResultOptions::default(), &theme(), &context).expect("result half");
    let line = result_child.borrow_mut().render(80).join("\n");
    assert!(line.contains("answered; 1 answered; 1 unanswered"), "summary: {line}");
    assert!(line.contains("The user responded"), "content: {line}");
}
