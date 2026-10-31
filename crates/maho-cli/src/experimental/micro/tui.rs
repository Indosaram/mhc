//! Port of senpi `experimental/micro/tui.ts` (target 59d58cee): the micro screen host over
//! maho-interactive components.
//!
//! `MicroTui` keeps senpi's component tree (transcript in a following `ScrollView`, then a dock of
//! queue, notices, status, editor and footer) and folds every `MicroView` into it. Pico views are
//! projected to plain JSON once per apply, which is what senpi's components consume. The host loop
//! (`run_micro_tui`) replaces senpi's event-driven `TuiAltScreen`: it pumps the terminal, folds new
//! views and drives the controller, exactly like the mini client's view loop.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::sync::Arc;

use maho_interactive::components::assistant_message::AssistantMessageComponent;
use maho_interactive::components::custom_editor::{CustomEditor, CustomEditorOptions};
use maho_interactive::components::dynamic_border::DynamicBorder;
use maho_interactive::components::extension_editor::editor_theme;
use maho_interactive::components::extension_selector::{
    ExtensionSelectorComponent, ExtensionSelectorOptions,
};
use maho_interactive::components::footer::format_tokens;
use maho_interactive::components::keybinding_hints::key_text;
use maho_interactive::components::login_dialog::{LoginDialogComponent, OAuthDeviceCodeInfo};
use maho_interactive::components::markdown_transform::get_markdown_theme;
use maho_interactive::components::oauth_selector::{
    AuthSelectorAuthType, AuthSelectorMode, AuthSelectorProvider, OAuthSelectorComponent,
};
use maho_interactive::components::status_indicator::StatusIndicator;
use maho_interactive::components::tool_execution::{
    ToolExecutionComponent, ToolExecutionOptions, ToolExecutionPresentation,
};
use maho_interactive::components::tool_execution_types::ToolExecutionResult;
use maho_interactive::components::user_message::UserMessageComponent;
use maho_interactive::theme::{Theme, ThemeColor};
use maho_interactive::tools::renderers::{ToolRenderers, create_all_tool_renderers};
use maho_tui::components::editor::EditorTuiHost;
use maho_tui::components::input::{Input, InputOptions};
use maho_tui::components::scroll_view::{
    Overscroll, ScrollView, ScrollViewFollow, ScrollViewOptions,
};
use maho_tui::components::select_list::{
    SelectItem, SelectList, SelectListLayoutOptions, SelectListTheme,
};
use maho_tui::components::spacer::Spacer;
use maho_tui::components::stack::{StackChild, StackEntryOptions, StackOptions};
use maho_tui::components::text::Text;
use maho_tui::components::truncated_text::TruncatedText;
use maho_tui::components::v_stack::VStack;
use maho_tui::fuzzy::fuzzy_filter;
use maho_tui::keybindings::{KeybindingsManager, get_keybindings};
use maho_tui::layout_node::StackBasis;
use maho_tui::tui::{Component, Container, Focusable};
use serde_json::Value;

use super::api::{
    AuthNotice, AuthPromptRequest, MicroAuthType, MicroAuthView, MicroController, MicroNoticeLevel,
    MicroProviderAccount, MicroView, MicroViewSource,
};

type Shared<T> = Rc<RefCell<T>>;
type Dyn = Rc<RefCell<dyn Component>>;

fn shared<T>(value: T) -> Shared<T> {
    Rc::new(RefCell::new(value))
}

/// senpi `SELECT_THEME`.
fn select_theme(theme: &Theme) -> SelectListTheme {
    let color = |color: ThemeColor| {
        let theme = theme.clone();
        Rc::new(move |text: &str| theme.fg(color, text)) as Rc<dyn Fn(&str) -> String>
    };
    SelectListTheme {
        selected_prefix: color(ThemeColor::Accent),
        selected_text: color(ThemeColor::Accent),
        description: color(ThemeColor::Muted),
        scroll_info: color(ThemeColor::Dim),
        no_match: color(ThemeColor::Warning),
        render_row: None,
    }
}

/// senpi `ListSelector`: a filter input over a fuzzy-filtered `SelectList`.
pub struct ListSelector {
    container: Container,
    input: Shared<Input>,
    list_container: Shared<Container>,
    items: Vec<SelectItem>,
    theme: Theme,
    on_select: Rc<dyn Fn(&str)>,
    on_cancel: Rc<dyn Fn()>,
    list: Shared<SelectList>,
    focused: bool,
}

impl ListSelector {
    pub fn new(
        theme: &Theme,
        title: &str,
        items: Vec<SelectItem>,
        on_select: Rc<dyn Fn(&str)>,
        on_cancel: Rc<dyn Fn()>,
    ) -> Self {
        let input = shared(Input::new(InputOptions::default()));
        let list_container = shared(Container::new());
        let list = Self::build(
            theme,
            &list_container,
            items.clone(),
            &on_select,
            &on_cancel,
        );
        let mut container = Container::new();
        container.add_child(shared(DynamicBorder::new(theme.clone())));
        container.add_child(shared(Spacer::new(1)));
        container.add_child(shared(Text::with_padding(
            theme.fg(ThemeColor::Accent, &theme.bold(title)),
            1,
            0,
        )));
        container.add_child(input.clone());
        container.add_child(shared(Spacer::new(1)));
        container.add_child(list_container.clone());
        container.add_child(shared(DynamicBorder::new(theme.clone())));
        Self {
            container,
            input,
            list_container,
            items,
            theme: theme.clone(),
            on_select,
            on_cancel,
            list,
            focused: false,
        }
    }

    fn build(
        theme: &Theme,
        list_container: &Shared<Container>,
        items: Vec<SelectItem>,
        on_select: &Rc<dyn Fn(&str)>,
        on_cancel: &Rc<dyn Fn()>,
    ) -> Shared<SelectList> {
        let mut list = SelectList::new(
            items,
            10,
            select_theme(theme),
            SelectListLayoutOptions::default(),
        );
        let select = on_select.clone();
        list.on_select = Some(Box::new(move |item: &SelectItem| select(&item.value)));
        let cancel = on_cancel.clone();
        list.on_cancel = Some(Box::new(move || cancel()));
        let list = shared(list);
        let mut container = list_container.borrow_mut();
        container.clear();
        container.add_child(list.clone());
        list
    }
}

impl Component for ListSelector {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.container.render(width)
    }
    fn invalidate(&mut self) {
        self.container.invalidate();
    }
    fn handle_input(&mut self, data: &str) {
        let keybindings = get_keybindings();
        let forwarded = [
            "tui.select.up",
            "tui.select.down",
            "tui.select.confirm",
            "tui.select.cancel",
        ];
        if forwarded
            .iter()
            .any(|action| keybindings.matches(data, action))
        {
            // The list's callbacks may remount the editor; never hold the list across them.
            let list = self.list.clone();
            list.borrow_mut().handle_input(data);
            return;
        }
        self.input.borrow_mut().handle_input(data);
        let query = self.input.borrow().get_value().to_owned();
        let filtered = if query.is_empty() {
            self.items.clone()
        } else {
            fuzzy_filter(&self.items, &query, |item| {
                format!("{} {}", item.label, item.value)
            })
        };
        self.list = Self::build(
            &self.theme,
            &self.list_container,
            filtered,
            &self.on_select,
            &self.on_cancel,
        );
    }
    fn has_input_handler(&self) -> bool {
        true
    }
    fn focusable_get(&self) -> Option<bool> {
        Some(self.focused)
    }
    fn focusable_set(&mut self, focused: bool) {
        Focusable::set_focused(self, focused);
    }
}

impl Focusable for ListSelector {
    fn focused(&self) -> bool {
        self.focused
    }
    fn set_focused(&mut self, value: bool) {
        self.focused = value;
        self.input.borrow_mut().set_focused(value);
    }
}

/// senpi `Container.clear()` for the editor slot. A dialog that unmounts itself does so from its
/// own input handler, so it is still borrowed: it is detached without the (no-op) dispose.
fn unmount_all(container: &mut Container) {
    for child in std::mem::take(&mut container.children) {
        if let Ok(mut child) = child.try_borrow_mut() {
            child.dispose();
        }
    }
}

/// senpi `TuiHandlers`.
pub struct TuiHandlers {
    pub submit: Rc<dyn Fn(String)>,
    pub follow_up: Rc<dyn Fn(String)>,
    pub abort: Rc<dyn Fn()>,
    pub exit: Rc<dyn Fn()>,
    pub select_model: Rc<dyn Fn()>,
    pub cycle_thinking: Rc<dyn Fn()>,
}

/// Shared with the editor: render requests, the terminal height, and work that has to wait until
/// the focused component's input handler returns (it is mutably borrowed while it runs).
#[derive(Default)]
pub struct MicroTuiHost {
    pub rows: Cell<usize>,
    pub render: Cell<bool>,
    deferred: RefCell<VecDeque<Box<dyn FnOnce()>>>,
}

impl MicroTuiHost {
    fn defer(&self, run: Box<dyn FnOnce()>) {
        self.deferred.borrow_mut().push_back(run);
    }

    /// Runs the work deferred by input handlers, in order.
    pub fn run_deferred(&self) {
        loop {
            let next = self.deferred.borrow_mut().pop_front();
            let Some(run) = next else { break };
            run();
        }
    }
}
impl EditorTuiHost for MicroTuiHost {
    fn request_render(&self) {
        self.render.set(true);
    }
    fn terminal_rows(&self) -> usize {
        self.rows.get()
    }
}

/// Where the dock's editor slot points: the editor or a mounted dialog.
pub struct Mounted {
    pub component: Dyn,
    pub focus: Dyn,
    dispose: Option<Box<dyn FnOnce()>>,
}

/// senpi `MicroTui`: the component tree and the view fold. The terminal itself is the host loop's.
pub struct MicroTui {
    pub theme: Theme,
    pub host: Rc<MicroTuiHost>,
    pub root: Dyn,
    chat: Shared<Container>,
    queue: Shared<Container>,
    notices: Shared<Container>,
    status: Shared<Container>,
    footer_stats: Shared<Text>,
    footer_hints: Shared<Text>,
    editor_container: Shared<Container>,
    pub editor: Shared<CustomEditor>,
    cwd: String,
    tools: HashMap<String, Shared<ToolExecutionComponent>>,
    renderers: HashMap<&'static str, Rc<RefCell<dyn ToolRenderers>>>,
    rendered_entry_ids: Vec<u64>,
    streaming: Option<Shared<AssistantMessageComponent>>,
    indicator: Option<Shared<StatusIndicator>>,
    status_text: String,
    pub mounted: Option<Mounted>,
    /// The component that should hold keyboard focus; the host loop applies it.
    pub focus: Dyn,
    pub now_ms: Cell<u64>,
}

impl MicroTui {
    pub fn new(
        cwd: &str,
        theme: Theme,
        keybindings: Arc<KeybindingsManager>,
        handlers: TuiHandlers,
    ) -> Self {
        let host = Rc::new(MicroTuiHost::default());
        host.rows.set(24);
        let mut editor = CustomEditor::new(
            host.clone(),
            editor_theme(&theme),
            keybindings,
            CustomEditorOptions {
                editor: maho_tui::components::editor::EditorOptions {
                    padding_x: Some(1),
                    ..Default::default()
                },
                embed_working_status: false,
            },
        );
        let submit = handlers.submit.clone();
        editor.editor.on_submit = Some(Box::new(move |text: &str| submit(text.to_owned())));
        let abort = handlers.abort.clone();
        editor.on_escape = Some(Box::new(move || abort()));
        let exit = handlers.exit.clone();
        editor.on_ctrl_d = Some(Box::new(move || exit()));
        let exit = handlers.exit.clone();
        editor.on_action("app.clear", Box::new(move || exit()));
        let select_model = handlers.select_model.clone();
        editor.on_action("app.model.select", Box::new(move || select_model()));
        let cycle_thinking = handlers.cycle_thinking.clone();
        editor.on_action("app.thinking.cycle", Box::new(move || cycle_thinking()));
        let editor = shared(editor);
        {
            // senpi reads the editor text in the follow-up action; the editor is borrowed while
            // its action runs, so the read is deferred until the input handler returns.
            let weak = Rc::downgrade(&editor);
            let host = host.clone();
            let follow_up = handlers.follow_up.clone();
            editor.borrow_mut().on_action(
                "app.message.followUp",
                Box::new(move || {
                    let (weak, follow_up) = (weak.clone(), follow_up.clone());
                    host.defer(Box::new(move || {
                        let Some(editor) = weak.upgrade() else { return };
                        let text = editor.borrow().editor.get_text().trim().to_owned();
                        if text.is_empty() {
                            return;
                        }
                        editor.borrow_mut().editor.set_text("");
                        follow_up(text);
                    }));
                }),
            );
        }
        let chat = shared(Container::new());
        let queue = shared(Container::new());
        let notices = shared(Container::new());
        let status = shared(Container::new());
        let editor_container = shared(Container::new());
        editor_container.borrow_mut().add_child(editor.clone());
        let footer_stats = shared(Text::with_padding("", 1, 0));
        let footer_hints = shared(Text::with_padding("", 1, 0));
        let footer = shared(Container::new());
        footer.borrow_mut().add_child(footer_stats.clone());
        footer.borrow_mut().add_child(footer_hints.clone());
        let transcript = ScrollView::new(
            chat.clone(),
            ScrollViewOptions {
                follow: ScrollViewFollow::End,
                primary: true,
                overscroll: Overscroll::Chain,
                ..Default::default()
            },
        );
        fn entry(component: Dyn, min_size: usize) -> StackChild {
            StackChild::Entry {
                component,
                options: StackEntryOptions {
                    shrink: Some(1),
                    min_size: Some(min_size),
                    ..Default::default()
                },
            }
        }
        let dock = shared(VStack::new(
            vec![
                entry(queue.clone(), 0),
                entry(notices.clone(), 0),
                entry(status.clone(), 0),
                entry(editor_container.clone(), 3),
                entry(footer.clone(), 0),
            ],
            StackOptions::default(),
        ));
        let root = shared(VStack::new(
            vec![
                StackChild::Entry {
                    component: transcript,
                    options: StackEntryOptions {
                        basis: Some(StackBasis::Fixed(0)),
                        grow: Some(1),
                        shrink: Some(1),
                        min_size: Some(1),
                        max_size: None,
                    },
                },
                StackChild::Entry {
                    component: dock,
                    options: StackEntryOptions {
                        basis: Some(StackBasis::Auto),
                        grow: Some(0),
                        shrink: Some(1),
                        min_size: Some(1),
                        max_size: None,
                    },
                },
            ],
            StackOptions::default(),
        ));
        let focus: Dyn = editor.clone();
        Self {
            theme,
            host,
            root,
            chat,
            queue,
            notices,
            status,
            footer_stats,
            footer_hints,
            editor_container,
            editor,
            cwd: cwd.to_owned(),
            tools: HashMap::new(),
            renderers: create_all_tool_renderers(),
            rendered_entry_ids: Vec::new(),
            streaming: None,
            indicator: None,
            status_text: String::new(),
            mounted: None,
            focus,
            now_ms: Cell::new(0),
        }
    }

    /// senpi `mount(component, focus, dispose)`.
    pub fn mount(&mut self, component: Dyn, focus: Dyn, dispose: Option<Box<dyn FnOnce()>>) {
        if let Some(dispose) = self.mounted.take().and_then(|mounted| mounted.dispose) {
            dispose();
        }
        let mut container = self.editor_container.borrow_mut();
        unmount_all(&mut container);
        container.add_child(component.clone());
        drop(container);
        self.focus = focus.clone();
        self.mounted = Some(Mounted {
            component,
            focus,
            dispose,
        });
        self.host.render.set(true);
    }

    /// senpi `restoreEditor()`.
    pub fn restore_editor(&mut self) {
        if let Some(dispose) = self.mounted.take().and_then(|mounted| mounted.dispose) {
            dispose();
        }
        let mut container = self.editor_container.borrow_mut();
        unmount_all(&mut container);
        container.add_child(self.editor.clone());
        drop(container);
        self.focus = self.editor.clone();
        self.host.render.set(true);
    }

    /// senpi `apply(view)`.
    pub fn apply(&mut self, view: &MicroView) {
        let conversation = serde_json::to_value(&view.conversation).unwrap_or(Value::Null);
        let entries = conversation["entries"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let turn = &conversation["turn"];
        self.sync_transcript(&entries);
        let message = turn.get("message").filter(|message| !message.is_null());
        if message.is_none() && self.streaming.is_some() {
            self.rebuild_transcript(&entries);
        }
        self.sync_streaming(message);
        for slot in turn["tools"].as_array().into_iter().flatten() {
            let (Some(name), Some(call_id)) = (slot["name"].as_str(), slot["callId"].as_str())
            else {
                continue;
            };
            let component = self.tool(name, call_id, Some(slot["args"].clone()));
            let mut component = component.borrow_mut();
            component.set_args_complete();
            let running = slot["status"] == "running";
            if running {
                component.mark_execution_started();
            }
            if let (true, Some(progress)) = (running, slot["progress"].as_str()) {
                component.update_result(
                    tool_result(
                        Value::Array(vec![serde_json::json!({"type": "text", "text": progress})]),
                        slot.get("details").cloned(),
                        false,
                    ),
                    true,
                );
            }
        }
        self.sync_queue(&conversation);
        self.sync_notices(view);
        self.sync_status(view, &conversation);
        self.sync_footer(view, &conversation);
        self.host.render.set(true);
    }

    fn sync_footer(&mut self, view: &MicroView, conversation: &Value) {
        let theme = &self.theme;
        let usage = &view.usage;
        let mut stats: Vec<String> = Vec::new();
        let tokens = |count: u64| format_tokens(count as f64);
        if usage.input > 0 {
            stats.push(format!("↑{}", tokens(usage.input)));
        }
        if usage.output > 0 {
            stats.push(format!("↓{}", tokens(usage.output)));
        }
        if usage.cache_read > 0 {
            stats.push(format!("R{}", tokens(usage.cache_read)));
        }
        if usage.cache_write > 0 {
            stats.push(format!("W{}", tokens(usage.cache_write)));
        }
        if let Some(rate) = usage.last_cache_hit_rate
            && (usage.cache_read > 0 || usage.cache_write > 0)
        {
            stats.push(format!("CH{rate:.1}%"));
        }
        stats.push(format!(
            "${:.3}{}",
            usage.total_cost,
            if usage.total_cost_unknown { "+?" } else { "" }
        ));
        if usage.context_window > 0 {
            let config = &conversation["config"];
            let automatic = if config["threshold"].as_f64().unwrap_or(0.0) > 0.0 {
                " (auto)"
            } else {
                ""
            };
            let window = tokens(usage.context_window);
            let context = match usage.context_percent {
                None => format!("?/{window}{automatic}"),
                Some(percent) => format!("{percent:.1}%/{window}{automatic}"),
            };
            stats.push(match usage.context_percent {
                Some(percent) if percent > 90.0 => theme.fg(ThemeColor::Error, &context),
                Some(percent) if percent > 70.0 => theme.fg(ThemeColor::Warning, &context),
                _ => context,
            });
        }
        self.footer_stats
            .borrow_mut()
            .set_text(theme.fg(ThemeColor::Dim, &stats.join(" ")));

        let config = &conversation["config"];
        let model = model_ref(&config["model"]);
        let thinking = match &config["thinkingLevel"] {
            Value::String(level) => level.clone(),
            Value::Null => "off".to_owned(),
            other => other.to_string(),
        };
        let model = model.map_or_else(
            || "no model".to_owned(),
            |(provider, model_id)| format!("{provider}/{model_id}"),
        );
        self.footer_hints.borrow_mut().set_text(theme.fg(
            ThemeColor::Dim,
            &format!(
                "{model} · thinking:{thinking} ({}) · {} or /model · /login · /compact · {} follow-up · {} exit",
                key_text("app.thinking.cycle"),
                key_text("app.model.select"),
                key_text("app.message.followUp"),
                key_text("app.clear"),
            ),
        ));
    }

    fn sync_queue(&mut self, conversation: &Value) {
        let mut queue = self.queue.borrow_mut();
        queue.clear();
        for queued in conversation["inbox"].as_array().into_iter().flatten() {
            let mode = queued["mode"].as_str().unwrap_or_default();
            let text = if mode == "write" {
                format!("<{}>", queued["entry"]["kind"].as_str().unwrap_or_default())
            } else {
                user_content(&queued["input"])
            };
            queue.add_child(shared(TruncatedText::with_padding(
                self.theme
                    .fg(ThemeColor::Muted, &format!("[{mode}] {text}")),
                1,
                0,
            )));
        }
    }

    fn sync_notices(&mut self, view: &MicroView) {
        let mut notices = self.notices.borrow_mut();
        notices.clear();
        let start = view.notices.len().saturating_sub(4);
        for item in &view.notices[start..] {
            let color = match item.level {
                MicroNoticeLevel::Error => ThemeColor::Error,
                MicroNoticeLevel::Warning => ThemeColor::Warning,
                MicroNoticeLevel::Info => ThemeColor::Muted,
            };
            notices.add_child(shared(TruncatedText::with_padding(
                self.theme.fg(color, &item.message),
                1,
                0,
            )));
        }
    }

    /// senpi `#syncStatus`: the text that the working indicator shows, if any.
    pub fn status_text(view: &MicroView, conversation: &Value) -> String {
        let compaction = &conversation["compaction"];
        let generation = &conversation["turn"]["generation"];
        let running_tool = conversation["turn"]["tools"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|tool| tool["status"] == "running");
        if let Some(fatal) = &view.fatal {
            return format!("Fatal: {fatal}");
        }
        if compaction.is_object() {
            let reason = match compaction["reason"].as_str().unwrap_or_default() {
                "threshold" => "automatic",
                reason => reason,
            };
            return if compaction["stage"] == "retrying" {
                format!(
                    "Retrying {reason} compaction (attempt {})...",
                    compaction["attempt"]
                )
            } else {
                format!("Running {reason} compaction...")
            };
        }
        if generation.is_object() {
            return match generation["stage"].as_str().unwrap_or_default() {
                "retrying" => format!("Retrying generation (attempt {})...", generation["attempt"]),
                "deferred" => "Waiting for deferred response...".to_owned(),
                "waiting" => "Waiting for compaction...".to_owned(),
                "streaming" => "Working... (esc to abort)".to_owned(),
                _ => "Preparing response...".to_owned(),
            };
        }
        if let Some(tool) = running_tool {
            return format!(
                "Running {}... (esc to abort)",
                tool["name"].as_str().unwrap_or_default()
            );
        }
        String::new()
    }

    fn sync_status(&mut self, view: &MicroView, conversation: &Value) {
        let text = Self::status_text(view, conversation);
        if text == self.status_text {
            return;
        }
        self.status_text = text.clone();
        self.indicator = None;
        let mut status = self.status.borrow_mut();
        status.clear();
        if !text.is_empty() {
            let indicator = shared(StatusIndicator::working(
                &text,
                self.theme.clone(),
                self.now_ms.get(),
            ));
            status.add_child(indicator.clone());
            self.indicator = Some(indicator);
        }
    }

    fn sync_transcript(&mut self, entries: &[Value]) {
        let diverged = self
            .rendered_entry_ids
            .iter()
            .enumerate()
            .any(|(index, id)| {
                entries.get(index).and_then(|entry| entry["id"].as_u64()) != Some(*id)
            });
        if diverged {
            self.rebuild_transcript(entries);
        }
        for entry in entries.iter().skip(self.rendered_entry_ids.len()) {
            self.add_entry(entry);
            self.rendered_entry_ids
                .push(entry["id"].as_u64().unwrap_or_default());
        }
    }

    fn rebuild_transcript(&mut self, entries: &[Value]) {
        self.chat.borrow_mut().clear();
        self.tools.clear();
        self.rendered_entry_ids.clear();
        self.streaming = None;
        for entry in entries {
            self.add_entry(entry);
            self.rendered_entry_ids
                .push(entry["id"].as_u64().unwrap_or_default());
        }
    }

    fn add_entry(&mut self, entry: &Value) {
        let message = &entry["model"][0];
        let role = message["role"].as_str().unwrap_or_default();
        match entry["kind"].as_str().unwrap_or_default() {
            "pi.user" if role == "user" => self.add_user(message),
            "pi.assistant" => {
                let assistant = if role == "assistant" {
                    Some(message)
                } else {
                    entry["data"]
                        .get("display")
                        .filter(|display| display.is_object())
                };
                if let Some(assistant) = assistant {
                    let assistant = assistant.clone();
                    self.add_assistant(&assistant);
                }
            }
            "pi.tool_result" if role == "toolResult" => {
                let name = message["toolName"].as_str().unwrap_or_default();
                let call_id = message["toolCallId"].as_str().unwrap_or_default();
                let result = tool_result(
                    message["content"].clone(),
                    entry["data"].get("details").cloned(),
                    message["isError"].as_bool().unwrap_or(false),
                );
                self.tool(name, call_id, None)
                    .borrow_mut()
                    .update_result(result, false);
            }
            "pi.summary" => {
                self.add_text("[compaction summary]");
                if role == "user" {
                    self.add_text(&user_content(&message["content"]));
                }
            }
            "pi.notice" if role == "user" => self.add_text(&user_content(&message["content"])),
            "pi.handoff" => self.add_text("[handoff]"),
            "pi.reset" => self.add_text("[reset]"),
            _ => {}
        }
    }

    fn add_user(&mut self, message: &Value) {
        let mut chat = self.chat.borrow_mut();
        chat.add_child(shared(Spacer::new(1)));
        chat.add_child(shared(UserMessageComponent::new(
            user_content(&message["content"]),
            self.theme.clone(),
            get_markdown_theme(&self.theme),
            0,
            Vec::new(),
        )));
    }

    fn new_assistant(&self) -> Shared<AssistantMessageComponent> {
        shared(AssistantMessageComponent::new(
            None,
            false,
            get_markdown_theme(&self.theme),
            "Thinking...",
            0,
            Vec::new(),
            self.theme.clone(),
        ))
    }

    fn add_assistant(&mut self, message: &Value) {
        let component = match self.streaming.take() {
            Some(streaming) => streaming,
            None => {
                let component = self.new_assistant();
                self.chat.borrow_mut().add_child(component.clone());
                component
            }
        };
        component.borrow_mut().update_content(message, Some(false));
        for content in message["content"].as_array().into_iter().flatten() {
            if content["type"] == "toolCall"
                && let (Some(name), Some(id)) = (content["name"].as_str(), content["id"].as_str())
            {
                self.tool(name, id, Some(content["arguments"].clone()))
                    .borrow_mut()
                    .set_args_complete();
            }
        }
    }

    fn sync_streaming(&mut self, message: Option<&Value>) {
        let Some(message) = message else { return };
        let streaming = match &self.streaming {
            Some(streaming) => streaming.clone(),
            None => {
                let streaming = self.new_assistant();
                self.chat.borrow_mut().add_child(streaming.clone());
                self.streaming = Some(streaming.clone());
                streaming
            }
        };
        streaming.borrow_mut().update_content(message, Some(true));
        for content in message["content"].as_array().into_iter().flatten() {
            if content["type"] == "toolCall"
                && let (Some(name), Some(id)) = (content["name"].as_str(), content["id"].as_str())
            {
                self.tool(name, id, Some(content["arguments"].clone()));
            }
        }
    }

    fn add_text(&mut self, text: &str) {
        let mut chat = self.chat.borrow_mut();
        chat.add_child(shared(Spacer::new(1)));
        chat.add_child(shared(Text::with_padding(
            self.theme.fg(ThemeColor::Muted, text),
            1,
            0,
        )));
    }

    /// senpi `#tool(toolName, toolCallId, args)`.
    fn tool(
        &mut self,
        tool_name: &str,
        tool_call_id: &str,
        args: Option<Value>,
    ) -> Shared<ToolExecutionComponent> {
        if let Some(existing) = self.tools.get(tool_call_id) {
            if let Some(args) = args {
                existing.borrow_mut().update_args(args);
            }
            return existing.clone();
        }
        let component = shared(ToolExecutionComponent::new(
            tool_name,
            tool_call_id,
            args.unwrap_or_else(|| serde_json::json!({})),
            ToolExecutionOptions::default(),
            self.renderers.get(tool_name).cloned(),
            &self.cwd,
            ToolExecutionPresentation::Classic,
            None,
            self.theme.clone(),
        ));
        self.chat.borrow_mut().add_child(component.clone());
        self.tools
            .insert(tool_call_id.to_owned(), component.clone());
        component
    }

    /// The editor's text (tests and the host loop read it).
    pub fn editor_text(&self) -> String {
        self.editor.borrow().editor.get_text()
    }

    /// Advances the working indicator and tool spinners; true when a frame is due.
    pub fn tick(&mut self, now_ms: u64) -> bool {
        self.now_ms.set(now_ms);
        let mut dirty = false;
        if let Some(indicator) = &self.indicator {
            dirty |= indicator.borrow_mut().tick(now_ms);
        }
        for tool in self.tools.values() {
            dirty |= tool.borrow_mut().tick(now_ms);
        }
        dirty
    }
}

fn tool_result(content: Value, details: Option<Value>, is_error: bool) -> ToolExecutionResult {
    ToolExecutionResult {
        content: serde_json::from_value(content).unwrap_or_default(),
        details: details.filter(|details| !details.is_null()),
        is_error,
    }
}

/// senpi `modelRef(value)`.
fn model_ref(value: &Value) -> Option<(String, String)> {
    Some((
        value["provider"].as_str()?.to_owned(),
        value["modelId"].as_str()?.to_owned(),
    ))
}

/// senpi `userContent(content)`: a string, or the joined text blocks.
pub fn user_content(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|block| block["type"] == "text")
            .filter_map(|block| block["text"].as_str())
            .collect(),
        _ => String::new(),
    }
}

/// senpi `authProviders(accounts)`.
fn auth_providers(accounts: &[MicroProviderAccount]) -> Vec<AuthSelectorProvider> {
    accounts
        .iter()
        .map(|account| {
            let auth_type = match account.auth_type {
                MicroAuthType::Oauth => maho_ai::auth::types::AuthType::OAuth,
                MicroAuthType::ApiKey => maho_ai::auth::types::AuthType::ApiKey,
            };
            AuthSelectorProvider {
                id: account.id.clone(),
                name: account.name.clone(),
                auth_type: match account.auth_type {
                    MicroAuthType::Oauth => AuthSelectorAuthType::OAuth,
                    MicroAuthType::ApiKey => AuthSelectorAuthType::ApiKey,
                },
                method_name: None,
                login_label: None,
                method_has_login: false,
                status: account.configured.then(|| maho_ai::auth::types::AuthCheck {
                    ambient: false,
                    source: Some(
                        account
                            .source
                            .clone()
                            .unwrap_or_else(|| "configured".to_owned()),
                    ),
                    auth_type,
                }),
            }
        })
        .collect()
}

/// senpi `showAuthNotice(dialog, notice)`.
fn show_auth_notice(dialog: &mut LoginDialogComponent, notice: &AuthNotice) {
    match notice {
        AuthNotice::AuthUrl { url, instructions } => dialog.show_auth(url, instructions.as_deref()),
        AuthNotice::DeviceCode {
            user_code,
            verification_uri,
            ..
        } => {
            dialog.show_device_code(&OAuthDeviceCodeInfo {
                user_code: user_code.clone(),
                verification_uri: verification_uri.clone(),
            });
            dialog.show_waiting("Waiting for authentication...");
        }
        AuthNotice::Info { message, links } => {
            let links: Vec<_> = links
                .iter()
                .flatten()
                .map(|link| maho_ai::auth::types::AuthInfoLink {
                    url: link.url.clone(),
                    label: link.label.clone(),
                })
                .collect();
            dialog.show_info(message, &links, false);
        }
        AuthNotice::Progress { message } => dialog.show_progress(message),
    }
}

/// A controller call senpi fires with `void`; the host loop runs them in order.
pub type Command =
    Box<dyn FnOnce(Rc<dyn MicroController>) -> futures::future::LocalBoxFuture<'static, ()>>;

/// The state `runMicroTui` closes over: the view, the controller queue and the auth dialog.
pub struct MicroTuiSession {
    pub view: Rc<RefCell<MicroTui>>,
    source: Rc<dyn MicroViewSource>,
    pub commands: Rc<RefCell<VecDeque<Command>>>,
    pub exited: Rc<Cell<bool>>,
    auth_dialog: Option<Shared<LoginDialogComponent>>,
    auth_key: String,
    auth_notice_count: usize,
    auth_prompt_id: Option<String>,
    keybindings: Arc<KeybindingsManager>,
}

fn push(commands: &Rc<RefCell<VecDeque<Command>>>, command: Command) {
    commands.borrow_mut().push_back(command);
}

/// A controller call as a [`Command`].
fn call<F, Fut>(run: F) -> Command
where
    F: FnOnce(Rc<dyn MicroController>) -> Fut + 'static,
    Fut: std::future::Future<Output = ()> + 'static,
{
    Box::new(
        move |controller: Rc<dyn MicroController>| -> futures::future::LocalBoxFuture<'static, ()> {
            Box::pin(run(controller))
        },
    )
}

/// A view change run after the current input handler returns (the editor is borrowed during it).
fn later(run: impl FnOnce() + 'static) -> Command {
    Box::new(
        move |_: Rc<dyn MicroController>| -> futures::future::LocalBoxFuture<'static, ()> {
            run();
            Box::pin(async {})
        },
    )
}

impl MicroTuiSession {
    /// senpi `runMicroTui`'s setup: the handlers, `selectModel` and `login`.
    pub fn new(
        source: Rc<dyn MicroViewSource>,
        theme: Theme,
        keybindings: Arc<KeybindingsManager>,
    ) -> Self {
        let commands: Rc<RefCell<VecDeque<Command>>> = Rc::default();
        let exited = Rc::new(Cell::new(false));
        let view_slot: Rc<RefCell<std::rc::Weak<RefCell<MicroTui>>>> = Rc::default();

        let select_model: Rc<dyn Fn()> = {
            let (source, commands, view_slot, theme) = (
                source.clone(),
                commands.clone(),
                view_slot.clone(),
                theme.clone(),
            );
            Rc::new(move || {
                let Some(view) = view_slot.borrow().upgrade() else {
                    return;
                };
                let snapshot = source.current();
                let conversation =
                    serde_json::to_value(&snapshot.conversation).unwrap_or(Value::Null);
                let current = model_ref(&conversation["config"]["model"]);
                let mut models = snapshot.models.models.clone();
                let is_current = |provider: &str, model_id: &str| {
                    current
                        .as_ref()
                        .is_some_and(|(p, m)| p == provider && m == model_id)
                };
                models.sort_by_key(|model| !is_current(&model.provider, &model.model_id));
                let items = models
                    .iter()
                    .map(|model| SelectItem {
                        value: format!("{}/{}", model.provider, model.model_id),
                        label: model.model_id.clone(),
                        description: Some(model.provider.clone()),
                    })
                    .collect();
                let on_select: Rc<dyn Fn(&str)> = {
                    let (view_slot, commands) = (view_slot.clone(), commands.clone());
                    Rc::new(move |value: &str| {
                        if let Some(view) = view_slot.borrow().upgrade() {
                            view.borrow_mut().restore_editor();
                        }
                        let Some((provider, model_id)) = value.split_once('/') else {
                            return;
                        };
                        let model = maho_agent::harness::pico3::types::ModelRef {
                            provider: provider.to_owned(),
                            model_id: model_id.to_owned(),
                        };
                        push(
                            &commands,
                            call(
                                move |controller| async move { controller.set_model(model).await },
                            ),
                        );
                    })
                };
                let on_cancel: Rc<dyn Fn()> = {
                    let view_slot = view_slot.clone();
                    Rc::new(move || {
                        if let Some(view) = view_slot.borrow().upgrade() {
                            view.borrow_mut().restore_editor();
                        }
                    })
                };
                let selector = shared(ListSelector::new(
                    &theme,
                    "Select model:",
                    items,
                    on_select,
                    on_cancel,
                ));
                // Mounted after the editor's action returns: the editor is borrowed while it runs.
                let mount = view.clone();
                push(
                    &commands,
                    later(move || mount.borrow_mut().mount(selector.clone(), selector, None)),
                );
            })
        };
        let login: Rc<dyn Fn()> = {
            let (source, commands, view_slot, theme) = (
                source.clone(),
                commands.clone(),
                view_slot.clone(),
                theme.clone(),
            );
            Rc::new(move || {
                let Some(view) = view_slot.borrow().upgrade() else {
                    return;
                };
                let accounts = source.current().models.accounts;
                let on_select = {
                    let (view_slot, commands) = (view_slot.clone(), commands.clone());
                    Box::new(move |provider_id: &str, auth_type: AuthSelectorAuthType| {
                        if let Some(view) = view_slot.borrow().upgrade()
                            && let Ok(mut view) = view.try_borrow_mut()
                        {
                            view.restore_editor();
                        }
                        let provider_id = provider_id.to_owned();
                        let auth_type = match auth_type {
                            AuthSelectorAuthType::OAuth => MicroAuthType::Oauth,
                            AuthSelectorAuthType::ApiKey => MicroAuthType::ApiKey,
                        };
                        push(
                            &commands,
                            call(move |controller| async move {
                                controller.login(provider_id, auth_type).await
                            }),
                        );
                    })
                };
                let on_cancel = {
                    let view_slot = view_slot.clone();
                    Box::new(move || {
                        if let Some(view) = view_slot.borrow().upgrade()
                            && let Ok(mut view) = view.try_borrow_mut()
                        {
                            view.restore_editor();
                        }
                    })
                };
                let selector = shared(OAuthSelectorComponent::new(
                    &theme,
                    AuthSelectorMode::Login,
                    auth_providers(&accounts),
                    on_select,
                    on_cancel,
                    None,
                ));
                let mount = view.clone();
                push(
                    &commands,
                    later(move || mount.borrow_mut().mount(selector.clone(), selector, None)),
                );
            })
        };

        let handlers = TuiHandlers {
            submit: {
                let (source, commands, select_model, login) = (
                    source.clone(),
                    commands.clone(),
                    select_model.clone(),
                    login.clone(),
                );
                Rc::new(move |text: String| {
                    let trimmed = text.trim().to_owned();
                    if trimmed.is_empty() {
                        return;
                    }
                    match trimmed.as_str() {
                        "/model" => return select_model(),
                        "/login" => return login(),
                        "/compact" => {
                            return push(
                                &commands,
                                call(
                                    move |controller| async move { controller.compact(None).await },
                                ),
                            );
                        }
                        _ => {}
                    }
                    let busy = source.current().conversation.turn.is_some();
                    push(
                        &commands,
                        call(move |controller| async move {
                            if busy {
                                controller.steer(trimmed).await
                            } else {
                                controller.prompt(trimmed).await
                            }
                        }),
                    );
                })
            },
            follow_up: {
                let commands = commands.clone();
                Rc::new(move |text: String| {
                    push(
                        &commands,
                        call(move |controller| async move { controller.follow_up(text).await }),
                    )
                })
            },
            abort: {
                let commands = commands.clone();
                Rc::new(move || {
                    push(
                        &commands,
                        call(move |controller| async move { controller.abort().await }),
                    )
                })
            },
            exit: {
                let exited = exited.clone();
                Rc::new(move || exited.set(true))
            },
            select_model,
            cycle_thinking: {
                let commands = commands.clone();
                Rc::new(move || {
                    push(
                        &commands,
                        call(move |controller| async move { controller.cycle_thinking().await }),
                    )
                })
            },
        };
        let cwd = source.current().session.cwd;
        let view = Rc::new(RefCell::new(MicroTui::new(
            &cwd,
            theme,
            keybindings.clone(),
            handlers,
        )));
        *view_slot.borrow_mut() = Rc::downgrade(&view);
        Self {
            view,
            source,
            commands,
            exited,
            auth_dialog: None,
            auth_key: String::new(),
            auth_notice_count: 0,
            auth_prompt_id: None,
            keybindings,
        }
    }

    /// senpi `render()`: fold the current view, then the auth dialog.
    pub fn render(&mut self) {
        let snapshot = self.source.current();
        self.view.borrow_mut().apply(&snapshot);
        self.sync_auth(snapshot.auth.as_ref());
    }

    /// senpi `syncAuth(auth)`.
    fn sync_auth(&mut self, auth: Option<&MicroAuthView>) {
        let Some(auth) = auth else {
            if self.auth_dialog.take().is_some() {
                self.view.borrow_mut().restore_editor();
            }
            self.auth_key.clear();
            self.auth_notice_count = 0;
            self.auth_prompt_id = None;
            return;
        };
        let auth_type = match auth.auth_type {
            MicroAuthType::Oauth => "oauth",
            MicroAuthType::ApiKey => "api_key",
        };
        let key = format!("{}/{auth_type}", auth.provider_id);
        if self.auth_dialog.is_none() || self.auth_key != key {
            self.auth_key = key;
            self.auth_notice_count = 0;
            self.auth_prompt_id = None;
            let commands = self.commands.clone();
            let theme = self.view.borrow().theme.clone();
            let dialog = shared(LoginDialogComponent::new(
                &theme,
                &auth.provider_id,
                Box::new(move |_success: bool, _message: Option<&str>| {
                    push(
                        &commands,
                        call(move |controller| async move { controller.cancel_login().await }),
                    );
                }),
                Some(&auth.provider_name),
                None,
            ));
            self.view
                .borrow_mut()
                .mount(dialog.clone(), dialog.clone(), None);
            self.auth_dialog = Some(dialog);
        }
        let Some(dialog) = self.auth_dialog.clone() else {
            return;
        };
        for notice in auth.notices.iter().skip(self.auth_notice_count) {
            show_auth_notice(&mut dialog.borrow_mut(), notice);
        }
        self.auth_notice_count = auth.notices.len();
        let Some(prompt) = &auth.prompt else { return };
        if self.auth_prompt_id.as_deref() == Some(prompt.id.as_str()) {
            return;
        }
        self.auth_prompt_id = Some(prompt.id.clone());
        let id = prompt.id.clone();
        match &prompt.request {
            AuthPromptRequest::Select { message, options } => {
                let labels = options.iter().map(|option| option.label.clone()).collect();
                let reply = |answer: Option<String>| {
                    let (view, dialog, commands, id) = (
                        self.view.clone(),
                        dialog.clone(),
                        self.commands.clone(),
                        id.clone(),
                    );
                    move || {
                        if let Ok(mut view) = view.try_borrow_mut() {
                            view.mount(dialog.clone(), dialog.clone(), None);
                        }
                        let (id, answer) = (id.clone(), answer.clone());
                        push(
                            &commands,
                            call(move |controller| async move {
                                controller.reply_auth(id, answer).await
                            }),
                        );
                    }
                };
                let select_options = options.clone();
                let on_select = {
                    let (view, dialog, commands, id) = (
                        self.view.clone(),
                        dialog.clone(),
                        self.commands.clone(),
                        id.clone(),
                    );
                    Box::new(move |label: &str| {
                        if let Ok(mut view) = view.try_borrow_mut() {
                            view.mount(dialog.clone(), dialog.clone(), None);
                        }
                        let answer = select_options
                            .iter()
                            .find(|option| option.label == label)
                            .map(|option| option.id.clone());
                        let id = id.clone();
                        push(
                            &commands,
                            call(move |controller| async move {
                                controller.reply_auth(id, answer).await
                            }),
                        );
                    })
                };
                let on_cancel = Box::new(reply(None));
                let rows = self.view.borrow().host.rows.get();
                let theme = self.view.borrow().theme.clone();
                let selector = shared(ExtensionSelectorComponent::new(
                    &theme,
                    self.keybindings.clone(),
                    message,
                    labels,
                    on_select,
                    on_cancel,
                    ExtensionSelectorOptions {
                        terminal_rows: Some(rows),
                        ..Default::default()
                    },
                ));
                self.view
                    .borrow_mut()
                    .mount(selector.clone(), selector, None);
            }
            AuthPromptRequest::ManualCode { message, .. } => {
                let answer = dialog.borrow_mut().show_manual_input(message);
                self.reply_when(id, answer);
            }
            AuthPromptRequest::Text {
                message,
                placeholder,
            }
            | AuthPromptRequest::Secret {
                message,
                placeholder,
            } => {
                let answer = dialog
                    .borrow_mut()
                    .show_prompt(message, placeholder.as_deref());
                self.reply_when(id, answer);
            }
        }
    }

    /// senpi `answer.then(value => replyAuth(id, value), () => replyAuth(id, null))`.
    fn reply_when(&self, id: String, answer: tokio::sync::oneshot::Receiver<String>) {
        push(
            &self.commands,
            call(move |controller| async move {
                let value = answer.await.ok();
                controller.reply_auth(id, value).await;
            }),
        );
    }
}

/// senpi `runMicroTui(source, controller)`: the alt-screen host loop until the user exits.
#[cfg(unix)]
pub async fn run_micro_tui(
    source: Rc<dyn MicroViewSource>,
    controller: Rc<dyn MicroController>,
) -> Result<(), String> {
    use futures::StreamExt;
    use maho_tui::terminal::{ProcessTerminal, ProcessTerminalOptions, Terminal};
    use maho_tui::tui_alt_screen::TuiAltScreen;

    // senpi `initTheme()` (no name: detect from the terminal) + `KeybindingsManager.create()`.
    let theme = crate::cli::startup_ui::resolve_startup_theme(
        None,
        std::env::var("COLORFGBG").ok().as_deref(),
    )?;
    let agent_dir = maho_core::config::get_agent_dir();
    let keybindings = maho_core::keybindings::KeybindingsManager::create(Some(&agent_dir))
        .inner()
        .clone();
    maho_tui::keybindings::set_keybindings(keybindings.clone());
    let keybindings = Arc::new(keybindings);

    let mut session = MicroTuiSession::new(source.clone(), theme, keybindings);
    let changed = Rc::new(Cell::new(true));
    let unsubscribe = {
        let changed = changed.clone();
        source.subscribe(Box::new(move || changed.set(true)))
    };

    let mut terminal = ProcessTerminal::new(ProcessTerminalOptions::default());
    let mut renderer = TuiAltScreen::new();
    renderer.set_layout_root(Some(session.view.borrow().root.clone()));
    let mut focused: Dyn = session.view.borrow().focus.clone();
    renderer.base.set_focus(Some(focused.clone()));
    let input: Rc<RefCell<VecDeque<String>>> = Rc::default();
    let resized = Rc::new(Cell::new(false));
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
    let mut running = futures::stream::FuturesUnordered::new();
    let clock = std::time::Instant::now();
    let result = loop {
        if let Err(error) = terminal.pump(16) {
            break Err(error.to_string());
        }
        let now = u64::try_from(clock.elapsed().as_millis()).unwrap_or(u64::MAX);
        let mut dirty = false;
        session
            .view
            .borrow()
            .host
            .rows
            .set(usize::from(terminal.rows()));
        if resized.replace(false) {
            renderer.base.invalidate();
            dirty = true;
        }
        loop {
            let data = input.borrow_mut().pop_front();
            let Some(data) = data else { break };
            dirty = true;
            renderer
                .base
                .handle_terminal_input(&data, maho_tui::keys::is_key_release(&data));
        }
        let host = session.view.borrow().host.clone();
        host.run_deferred();
        if changed.replace(false) {
            session.render();
            dirty = true;
        }
        // senpi fires controller calls with `void`: they run alongside input and rendering.
        loop {
            let command = session.commands.borrow_mut().pop_front();
            let Some(command) = command else { break };
            running.push(command(controller.clone()));
            dirty = true;
        }
        while let std::task::Poll::Ready(Some(())) = running.poll_next_unpin(
            &mut std::task::Context::from_waker(futures::task::noop_waker_ref()),
        ) {
            dirty = true;
        }
        let focus = session.view.borrow().focus.clone();
        if !Rc::ptr_eq(&focus, &focused) {
            renderer.base.set_focus(Some(focus.clone()));
            focused = focus;
            dirty = true;
        }
        dirty |= session.view.borrow_mut().tick(now);
        dirty |= session.view.borrow().host.render.replace(false);
        if session.exited.get() {
            break Ok(());
        }
        if dirty {
            renderer.do_render(&mut terminal);
        }
        tokio::task::yield_now().await;
    };
    unsubscribe();
    renderer.before_terminal_stop(&mut terminal, false);
    terminal.drain_input(100, 10);
    let stopped = terminal.stop().map_err(|error| error.to_string());
    renderer.after_terminal_stop(&mut terminal, false);
    result.and(stopped)
}

/// senpi `runMicroTui` needs a terminal; there is no Windows `ProcessTerminal` host loop here.
#[cfg(not(unix))]
pub async fn run_micro_tui(
    _source: Rc<dyn MicroViewSource>,
    _controller: Rc<dyn MicroController>,
) -> Result<(), String> {
    Err("the micro TUI requires a Unix terminal".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::experimental::micro::api::{
        MicroAuthPrompt, MicroModelSummary, MicroModelsView, MicroNotice, MicroSessionView,
        MicroUsageView,
    };
    use maho_agent::harness::pico3::types::{ConversationView, LocalBoxFuture, ModelRef};
    use maho_interactive::theme::ColorMode;

    fn conversation(value: Value) -> ConversationView {
        let mut base = serde_json::json!({
            "conversation": {"id": 1},
            "entries": [],
            "config": {"model": {"provider": "p", "modelId": "m"}, "thinkingLevel": "medium"},
            "inbox": [],
            "tasks": {},
            "plugins": {},
        });
        if let (Some(base), Some(patch)) = (base.as_object_mut(), value.as_object()) {
            for (key, value) in patch {
                base.insert(key.clone(), value.clone());
            }
        }
        serde_json::from_str(&base.to_string()).expect("conversation view")
    }

    fn view(conversation_patch: Value) -> MicroView {
        MicroView {
            session: MicroSessionView {
                id: "s".into(),
                path: "/tmp/s".into(),
                cwd: "/tmp".into(),
            },
            conversation: conversation(conversation_patch),
            models: MicroModelsView {
                models: vec![
                    MicroModelSummary {
                        provider: "q".into(),
                        model_id: "other".into(),
                        name: "Other".into(),
                        context_window: 1000,
                        max_tokens: 100,
                    },
                    MicroModelSummary {
                        provider: "p".into(),
                        model_id: "m".into(),
                        name: "M".into(),
                        context_window: 1000,
                        max_tokens: 100,
                    },
                ],
                accounts: Vec::new(),
                refreshing: false,
            },
            usage: MicroUsageView::default(),
            auth: None,
            notices: Vec::new(),
            fatal: None,
        }
    }

    struct Source(RefCell<MicroView>);
    impl MicroViewSource for Source {
        fn current(&self) -> MicroView {
            self.0.borrow().clone()
        }
        fn subscribe(&self, _listener: Box<dyn Fn()>) -> Box<dyn FnOnce()> {
            Box::new(|| {})
        }
    }

    #[derive(Default)]
    struct Recorder(RefCell<Vec<String>>);
    impl Recorder {
        fn record(&self, call: String) -> LocalBoxFuture<'_, ()> {
            self.0.borrow_mut().push(call);
            Box::pin(async {})
        }
    }
    impl MicroController for Recorder {
        fn prompt(&self, text: String) -> LocalBoxFuture<'_, ()> {
            self.record(format!("prompt {text}"))
        }
        fn steer(&self, text: String) -> LocalBoxFuture<'_, ()> {
            self.record(format!("steer {text}"))
        }
        fn follow_up(&self, text: String) -> LocalBoxFuture<'_, ()> {
            self.record(format!("followUp {text}"))
        }
        fn compact(&self, _: Option<String>) -> LocalBoxFuture<'_, ()> {
            self.record("compact".into())
        }
        fn abort(&self) -> LocalBoxFuture<'_, ()> {
            self.record("abort".into())
        }
        fn cycle_thinking(&self) -> LocalBoxFuture<'_, ()> {
            self.record("cycleThinking".into())
        }
        fn set_model(&self, model: ModelRef) -> LocalBoxFuture<'_, ()> {
            self.record(format!("setModel {}/{}", model.provider, model.model_id))
        }
        fn refresh_models(&self) -> LocalBoxFuture<'_, ()> {
            self.record("refreshModels".into())
        }
        fn login(&self, provider_id: String, _: MicroAuthType) -> LocalBoxFuture<'_, ()> {
            self.record(format!("login {provider_id}"))
        }
        fn reply_auth(&self, id: String, answer: Option<String>) -> LocalBoxFuture<'_, ()> {
            self.record(format!("replyAuth {id} {answer:?}"))
        }
        fn cancel_login(&self) -> LocalBoxFuture<'_, ()> {
            self.record("cancelLogin".into())
        }
    }

    fn session(view: MicroView) -> (MicroTuiSession, Rc<Source>) {
        let source = Rc::new(Source(RefCell::new(view)));
        let theme = Theme::builtin("dark", ColorMode::Truecolor).expect("builtin theme");
        let keybindings = Arc::new(maho_tui::keybindings::KeybindingsManager::new(
            maho_core::keybindings::keybindings().clone(),
            Default::default(),
        ));
        (
            MicroTuiSession::new(source.clone(), theme, keybindings),
            source,
        )
    }

    async fn drain(session: &MicroTuiSession, controller: &Rc<Recorder>) -> Vec<String> {
        loop {
            let command = session.commands.borrow_mut().pop_front();
            let Some(command) = command else { break };
            command(controller.clone() as Rc<dyn MicroController>).await;
        }
        controller.0.borrow_mut().drain(..).collect()
    }

    fn rendered(session: &MicroTuiSession) -> String {
        // The transcript and the dock, in senpi's order (the root stack needs a viewport).
        let view = session.view.borrow();
        let parts: [Dyn; 7] = [
            view.chat.clone(),
            view.queue.clone(),
            view.notices.clone(),
            view.status.clone(),
            view.editor_container.clone(),
            view.footer_stats.clone(),
            view.footer_hints.clone(),
        ];
        let text = parts
            .iter()
            .flat_map(|part| part.borrow_mut().render(100))
            .collect::<Vec<_>>()
            .join("\n");
        String::from_utf8(strip_ansi(text.as_bytes())).unwrap_or_default()
    }

    fn strip_ansi(bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut index = 0;
        while index < bytes.len() {
            if bytes[index] == 0x1b {
                index += 1;
                if bytes.get(index) == Some(&b'[') {
                    index += 1;
                    while index < bytes.len() && !(0x40..=0x7e).contains(&bytes[index]) {
                        index += 1;
                    }
                    index += 1;
                } else if bytes.get(index) == Some(&b']') {
                    while index < bytes.len() && bytes[index] != 0x07 && bytes[index] != b'\\' {
                        index += 1;
                    }
                    index += 1;
                }
                continue;
            }
            out.push(bytes[index]);
            index += 1;
        }
        out
    }

    #[test]
    fn status_text_follows_fatal_compaction_generation_then_running_tools() {
        let mut fatal = view(serde_json::json!({}));
        fatal.fatal = Some("gone".into());
        assert_eq!(MicroTui::status_text(&fatal, &Value::Null), "Fatal: gone");
        let idle = view(serde_json::json!({}));
        let status = |conversation: Value| MicroTui::status_text(&idle, &conversation);
        assert_eq!(
            status(
                serde_json::json!({"compaction": {"reason": "threshold", "stage": "summarizing", "attempt": 1}})
            ),
            "Running automatic compaction..."
        );
        assert_eq!(
            status(
                serde_json::json!({"compaction": {"reason": "manual", "stage": "retrying", "attempt": 2}})
            ),
            "Retrying manual compaction (attempt 2)..."
        );
        assert_eq!(
            status(
                serde_json::json!({"turn": {"generation": {"stage": "streaming", "attempt": 1}, "tools": []}})
            ),
            "Working... (esc to abort)"
        );
        assert_eq!(
            status(
                serde_json::json!({"turn": {"generation": {"stage": "preparing"}, "tools": []}})
            ),
            "Preparing response..."
        );
        assert_eq!(
            status(serde_json::json!({"turn": {"tools": [{"name": "read", "status": "running"}]}})),
            "Running read... (esc to abort)"
        );
        assert_eq!(status(serde_json::json!({})), "");
    }

    #[test]
    fn user_content_joins_text_blocks() {
        assert_eq!(user_content(&serde_json::json!("plain")), "plain");
        assert_eq!(
            user_content(
                &serde_json::json!([{"type": "text", "text": "a"}, {"type": "image"}, {"type": "text", "text": "b"}])
            ),
            "ab"
        );
    }

    #[test]
    fn renders_entries_queue_notices_and_footer() {
        let mut current = view(serde_json::json!({
            "entries": [
                {"id": 1, "conversationId": 1, "kind": "pi.user", "model": [{"role": "user", "content": "hello there"}]},
                {"id": 2, "conversationId": 1, "kind": "pi.reset"},
            ],
            "inbox": [{"mode": "followUp", "id": 3, "input": [{"type": "text", "text": "later"}]}],
        }));
        current.notices.push(MicroNotice {
            id: 1,
            level: MicroNoticeLevel::Warning,
            message: "careful".into(),
        });
        current.usage = MicroUsageView {
            input: 1200,
            output: 30,
            total_cost: 0.5,
            total_cost_unknown: true,
            ..Default::default()
        };
        let (mut session, _) = session(current);
        session.render();
        let screen = rendered(&session);
        for expected in [
            "hello there",
            "[reset]",
            "[followUp] later",
            "careful",
            "↑1.2K",
            "$0.500+?",
            "p/m · thinking:medium",
        ] {
            assert!(
                screen.contains(expected),
                "{expected:?} missing from\n{screen}"
            );
        }
    }

    #[tokio::test]
    async fn submit_routes_prompts_steers_and_slash_commands() {
        let controller = Rc::new(Recorder::default());
        let (session, source) = session(view(serde_json::json!({})));
        let submit = |text: &str| {
            let editor = session.view.borrow().editor.clone();
            editor.borrow_mut().editor.set_text(text);
            let on_submit = editor.borrow_mut().editor.on_submit.take();
            if let Some(mut on_submit) = on_submit {
                on_submit(text);
                editor.borrow_mut().editor.on_submit = Some(on_submit);
            }
        };
        submit("  ");
        submit("hi");
        submit("/compact");
        assert_eq!(drain(&session, &controller).await, ["prompt hi", "compact"]);

        *source.0.borrow_mut() = view(serde_json::json!({"turn": {"inputs": [], "tools": []}}));
        submit("again");
        assert_eq!(drain(&session, &controller).await, ["steer again"]);

        submit("/model");
        drain(&session, &controller).await;
        assert!(
            session.view.borrow().mounted.is_some(),
            "/model mounts the selector"
        );
        let screen = rendered(&session);
        assert!(screen.contains("Select model:"), "{screen}");
        // Confirming the first row picks the current model, which sorts first, then the editor
        // comes back.
        let focus = session.view.borrow().focus.clone();
        focus.borrow_mut().handle_input("\r");
        assert_eq!(drain(&session, &controller).await, ["setModel p/m"]);
        assert!(session.view.borrow().mounted.is_none());
    }

    #[tokio::test]
    async fn an_auth_prompt_mounts_the_login_dialog_and_answers_selects() {
        let controller = Rc::new(Recorder::default());
        let mut current = view(serde_json::json!({}));
        current.auth = Some(MicroAuthView {
            provider_id: "radius".into(),
            provider_name: "Radius".into(),
            auth_type: MicroAuthType::Oauth,
            notices: vec![AuthNotice::Progress {
                message: "Contacting Radius".into(),
            }],
            prompt: Some(MicroAuthPrompt {
                id: "r1".into(),
                request: AuthPromptRequest::Select {
                    message: "Pick an org".into(),
                    options: vec![
                        crate::experimental::mini::shared::protocol::AuthPromptOption {
                            id: "o1".into(),
                            label: "Org one".into(),
                            description: None,
                        },
                    ],
                },
            }),
        });
        let (mut session, source) = session(current);
        session.render();
        let screen = rendered(&session);
        assert!(screen.contains("Pick an org"), "{screen}");

        // Confirm the only option: the dialog is remounted and the answer is the option id.
        let focus = session.view.borrow().focus.clone();
        focus.borrow_mut().handle_input("\r");
        assert_eq!(
            drain(&session, &controller).await,
            ["replyAuth r1 Some(\"o1\")"]
        );
        assert!(rendered(&session).contains("Login to Radius"));

        // The login ends: the editor comes back.
        source.0.borrow_mut().auth = None;
        session.render();
        assert!(session.view.borrow().mounted.is_none());
    }
}
