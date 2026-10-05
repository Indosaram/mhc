//! Port of senpi `experimental/client-tui-chat.ts` and the snapshot half of
//! `experimental/mini/tui/view.ts`.
//!
//! Everything the view renders comes from a replicated `SessionSnapshot`; rendering is a pure
//! function of that snapshot and everything the view does is a command that answers with data. The
//! alt-screen host loop, the rich editor and the login/model dialogs are the interactive-mode lane's
//! (todo 35), matching the existing CLI38 ledger rows for `tui/view.ts`; the snapshot projection and
//! the mini container tree below are this lane's.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use maho_tui::components::input::{Input, InputOptions};
use maho_tui::components::text::Text;
use maho_tui::components::truncated_text::TruncatedText;
use maho_tui::terminal::{ProcessTerminal, ProcessTerminalOptions, Terminal};
use maho_tui::tui::{Component, Container, Focusable};
use maho_tui::tui_alt_screen::TuiAltScreen;
use serde_json::Value;

#[cfg(unix)]
use super::session::AttachedSession;

pub struct MiniViewState {
    pub transcript: Vec<String>,
    pub queues: Vec<String>,
    pub status: String,
    pub footer: String,
}

pub fn user_message_text(message: &Value) -> String {
    if message["role"] != "user" {
        return String::new();
    }
    match &message["content"] {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|block| block["type"] == "text")
            .filter_map(|block| block["text"].as_str())
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

fn message_text(message: &Value) -> String {
    match &message["content"] {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|block| block["type"] == "text")
            .filter_map(|block| block["text"].as_str())
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

fn entry_lines(entry: &Value) -> Vec<String> {
    match entry["type"].as_str() {
        Some("compaction") => {
            let before = entry["tokensBefore"].as_u64().unwrap_or_default();
            let mut lines = vec![format!("[compaction] compacted from {before} tokens")];
            if let Some(retained) = entry["retainedTail"].as_array() {
                for message in retained {
                    lines.push(message_text(message));
                }
            }
            lines
        }
        Some("branch_summary") => vec!["[branch summary]".to_owned(), entry["summary"].as_str().unwrap_or_default().to_owned()],
        Some("custom") => vec![format!("[{}]", entry["customType"].as_str().unwrap_or_default())],
        _ => {
            let message = &entry["message"];
            match message["role"].as_str() {
                Some("user") => vec![user_message_text(message)],
                Some("assistant") => vec![message_text(message)],
                Some("toolResult") => vec![format!("[tool {}] {}", message["toolName"].as_str().unwrap_or_default(), if message["isError"].as_bool() == Some(true) { "error" } else { "ok" })],
                _ => Vec::new(),
            }
        }
    }
}

pub fn project_lane_snapshot(lane: &Value) -> MiniViewState {
    let mut transcript = Vec::new();
    if let Some(entries) = lane["transcript"].as_array() {
        for entry in entries {
            transcript.extend(entry_lines(entry));
        }
    }
    if let Some(streaming) = lane["operation"]["streamingMessage"].as_object() {
        let text = message_text(&Value::Object(streaming.clone()));
        if !text.is_empty() {
            transcript.push(text);
        }
    }
    if let Some(tools) = lane["operation"]["runningTools"].as_array() {
        for tool in tools {
            transcript.push(format!(
                "[tool {}] {}",
                tool["toolName"].as_str().unwrap_or_default(),
                tool["status"].as_str().unwrap_or_default()
            ));
        }
    }
    let mut queues = Vec::new();
    if let Some(items) = lane["queues"].as_array() {
        for item in items {
            let text = if item["type"] == "message" {
                user_message_text(&item["message"]).split_whitespace().collect::<Vec<_>>().join(" ")
            } else {
                format!("<{}>", item["customType"].as_str().unwrap_or_default())
            };
            queues.push(format!("[{}] {}", item["kind"].as_str().unwrap_or_default(), text));
        }
    }
    let working = !lane["operation"].is_null();
    MiniViewState {
        transcript,
        queues,
        status: if working { "Working... (esc to abort)".to_owned() } else { String::new() },
        footer: footer(lane),
    }
}

fn footer(lane: &Value) -> String {
    let configuration = &lane["configuration"];
    let provider = configuration["model"]["provider"].as_str().unwrap_or_default();
    let model_id = configuration["model"]["modelId"].as_str().unwrap_or_default();
    let thinking = configuration["thinkingLevel"].as_str().unwrap_or_default();
    let messages = lane["stats"]["messageCount"].as_u64().unwrap_or_default();
    format!("{provider}/{model_id} · thinking:{thinking} · {messages} messages · /model · /thinking · /compact · /reload")
}

pub struct MiniView {
    transcript: Rc<RefCell<Container>>,
    queues: Rc<RefCell<Container>>,
    status: Rc<RefCell<Container>>,
    footer: Rc<RefCell<Text>>,
    editor: Rc<RefCell<Input>>,
    submitted: Rc<RefCell<VecDeque<String>>>,
    exit: Rc<RefCell<bool>>,
}

impl MiniView {
    pub fn new() -> Self {
        let transcript = Rc::new(RefCell::new(Container::new()));
        let queues = Rc::new(RefCell::new(Container::new()));
        let status = Rc::new(RefCell::new(Container::new()));
        let footer = Rc::new(RefCell::new(Text::with_padding("", 1, 0)));
        let submitted = Rc::new(RefCell::new(VecDeque::new()));
        let exit = Rc::new(RefCell::new(false));
        let mut editor = Input::new(InputOptions { prompt: Some("› ".to_owned()), placeholder: None, placeholder_style: None });
        {
            let submitted = submitted.clone();
            editor.on_submit = Some(Box::new(move |text| {
                if !text.trim().is_empty() {
                    submitted.borrow_mut().push_back(text.to_owned());
                }
            }));
        }
        {
            let exit = exit.clone();
            editor.on_escape = Some(Box::new(move || *exit.borrow_mut() = true));
        }
        editor.set_focused(true);
        Self { transcript, queues, status, footer, editor: Rc::new(RefCell::new(editor)), submitted, exit }
    }

    pub fn set_state(&mut self, state: MiniViewState) {
        {
            let mut container = self.transcript.borrow_mut();
            container.clear();
            for line in &state.transcript {
                container.add_child(Rc::new(RefCell::new(Text::with_padding(line.clone(), 1, 0))));
            }
        }
        {
            let mut container = self.queues.borrow_mut();
            container.clear();
            for line in &state.queues {
                container.add_child(Rc::new(RefCell::new(TruncatedText::new(line.clone()))));
            }
        }
        {
            let mut container = self.status.borrow_mut();
            container.clear();
            if !state.status.is_empty() {
                container.add_child(Rc::new(RefCell::new(Text::with_padding(state.status.clone(), 1, 0))));
            }
        }
        self.footer.borrow_mut().set_text(state.footer.clone());
    }

    pub fn take_submission(&self) -> Option<String> {
        self.submitted.borrow_mut().pop_front()
    }

    pub fn should_exit(&self) -> bool {
        *self.exit.borrow()
    }

    pub fn dispatch_input(&mut self, data: &str) {
        self.editor.borrow_mut().handle_input(data);
    }
}

impl Component for MiniView {
    fn render(&mut self, width: usize) -> Vec<String> {
        let mut lines = Vec::new();
        lines.extend(self.transcript.borrow_mut().render(width));
        lines.extend(self.queues.borrow_mut().render(width));
        lines.extend(self.status.borrow_mut().render(width));
        lines.push(String::new());
        lines.extend(self.editor.borrow_mut().render(width));
        lines.extend(self.footer.borrow_mut().render(width));
        lines
    }

    fn handle_input(&mut self, data: &str) {
        self.dispatch_input(data);
    }

    fn has_input_handler(&self) -> bool {
        true
    }
}

#[cfg(unix)]
pub async fn run_view(client: &AttachedSession, _cwd: &str) -> Result<(), String> {
    let mut terminal = ProcessTerminal::new(ProcessTerminalOptions::default());
    let mut renderer = TuiAltScreen::new();
    let view = Rc::new(RefCell::new(MiniView::new()));
    let root: Rc<RefCell<dyn Component>> = view.clone();
    renderer.set_layout_root(Some(root.clone()));
    renderer.base.set_focus(Some(root));
    let input: Rc<RefCell<VecDeque<String>>> = Rc::new(RefCell::new(VecDeque::new()));
    let resized = Rc::new(std::cell::Cell::new(false));
    renderer.before_terminal_start(&mut terminal, false, false);
    terminal.start(
        Box::new({
            let input = input.clone();
            move |data: &str| input.borrow_mut().push_back(data.to_owned())
        }),
        Box::new({
            let resized = resized.clone();
            move || resized.set(true)
        }),
    );
    let mut last_lane: Option<Value> = None;
    let result = loop {
        terminal.pump(16).map_err(|error| error.to_string())?;
        if resized.replace(false) {
            renderer.base.invalidate();
        }
        while let Some(data) = input.borrow_mut().pop_front() {
            if renderer.base.has_overlay() {
                renderer.base.handle_terminal_input(&data, false);
                continue;
            }
            view.borrow_mut().dispatch_input(&data);
        }
        let snapshot = client.state();
        if last_lane.as_ref() != Some(&snapshot.lane) {
            last_lane = Some(snapshot.lane.clone());
            view.borrow_mut().set_state(project_lane_snapshot(&snapshot.lane));
        }
        if view.borrow().should_exit() {
            break Ok(());
        }
        if let Some(text) = view.borrow().take_submission() {
            let outcome = if text.starts_with('/') {
                client.compact().await
            } else {
                client.prompt(&text).await
            };
            if let Err(error) = outcome {
                break Err(error);
            }
        }
        renderer.do_render(&mut terminal);
        tokio::task::yield_now().await;
    };
    renderer.before_terminal_stop(&mut terminal, false);
    terminal.drain_input(100, 10);
    terminal.stop().map_err(|error| error.to_string())?;
    renderer.after_terminal_stop(&mut terminal, false);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn projects_transcript_streaming_and_queues() {
        let lane = json!({
            "lane": "main",
            "transcript": [
                {"id": "u", "type": "message", "message": {"role": "user", "content": "hello"}},
                {"id": "a", "type": "message", "message": {"role": "assistant", "content": [{"type": "text", "text": "hi"}]}},
                {"id": "c", "type": "compaction", "tokensBefore": 10, "retainedTail": [{"role": "user", "content": "kept"}]}
            ],
            "operation": {"id": "op", "kind": "run", "streamingMessage": {"role": "assistant", "content": [{"type": "text", "text": "partial"}]}, "runningTools": [{"toolName": "read", "status": "running"}]},
            "queues": [{"kind": "steer", "type": "message", "message": {"role": "user", "content": "later"}}],
            "configuration": {"model": {"provider": "p", "modelId": "m"}, "thinkingLevel": "off", "activeToolNames": []},
            "stats": {"messageCount": 3}
        });
        let state = project_lane_snapshot(&lane);
        assert_eq!(state.transcript, vec!["hello", "hi", "[compaction] compacted from 10 tokens", "kept", "partial", "[tool read] running"]);
        assert_eq!(state.queues, vec!["[steer] later"]);
        assert_eq!(state.status, "Working... (esc to abort)");
        assert!(state.footer.starts_with("p/m · thinking:off · 3 messages"));
    }

    #[test]
    fn idle_snapshot_has_no_status() {
        let lane = json!({"lane": "main", "transcript": [], "operation": null, "queues": [], "configuration": {"model": {"provider": "p", "modelId": "m"}, "thinkingLevel": "off"}, "stats": {"messageCount": 0}});
        let state = project_lane_snapshot(&lane);
        assert!(state.transcript.is_empty());
        assert_eq!(state.status, "");
    }
}
