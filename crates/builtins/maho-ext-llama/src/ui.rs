//! Port of `packages/coding-agent/src/extensions/llama/ui.ts` (pin `fe8c564b`).

use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;

use maho_ai::utils::abort::{AbortController, AbortSignal};
use maho_ext_api::{CustomUiDone, CustomUiFactoryOptions, ExtensionContext, ExtensionFailure, ExtensionOverlayOptions, ExtensionTuiHost, ExtensionUi, JsonValue};
use maho_interactive::components::keybinding_hints::key_hint;
use maho_interactive::theme::{Theme, ThemeColor};
use maho_tui::components::input::{Input, InputOptions};
use maho_tui::keybindings::get_keybindings;
use maho_tui::tui::{Component, SizeValue};
use tokio::sync::{mpsc, oneshot};

use crate::client::{LlamaModelInfo, LlamaProgress};
use crate::huggingface::HuggingFaceModel;

pub const DOWNLOAD_VALUE: &str = "\u{0}download";

pub type UiFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

#[derive(Clone)]
pub enum LlamaManagerAction {
    Model(LlamaModelInfo),
    Download,
    Close,
}

#[derive(Clone, Default)]
pub struct ProgressState {
    pub title: String,
    pub model: String,
    pub message: String,
    pub ratio: Option<f64>,
    pub detail: Option<String>,
}

pub type SearchFn = Arc<dyn Fn(String, AbortSignal) -> UiFuture<'static, Result<Vec<HuggingFaceModel>, String>> + Send + Sync>;

pub trait LlamaUi: Send + Sync {
    fn show_models(&self, server_url: String, models: Vec<LlamaModelInfo>) -> UiFuture<'static, LlamaManagerAction>;
    fn select(&self, title: String, options: Vec<String>) -> UiFuture<'static, Option<String>>;
    fn confirm(&self, title: String, message: String) -> UiFuture<'static, bool>;
    fn connection_error(&self, server_url: String, message: String) -> UiFuture<'static, bool>;
    fn search_models(&self, search: SearchFn) -> UiFuture<'static, Option<String>>;
    fn show_status(&self, title: String, message: String);
    fn progress(&self, state: ProgressState) -> UiFuture<'static, ()>;
    fn update_progress(&self, state: ProgressState);
}

pub fn model_is_loaded(model: &LlamaModelInfo) -> bool {
    model.status.value == "loaded" || model.status.value == "sleeping"
}

fn context_label(model: &LlamaModelInfo) -> Option<String> {
    if let Some(context) = model.meta.as_ref().and_then(|meta| meta.n_ctx.or(meta.n_ctx_train)) {
        return Some(if context >= 1000 { format!("{}k", (context as f64 / 1000.0).round() as u64) } else { context.to_string() });
    }
    let args = model.status.args.clone().unwrap_or_default();
    for pair in args.windows(2) {
        if matches!(pair[0].as_str(), "--ctx-size" | "-c" | "-ctx")
            && let Ok(value) = pair[1].parse::<u64>()
            && value > 0
        {
            return Some(if value >= 1000 { format!("{}k", (value as f64 / 1000.0).round() as u64) } else { value.to_string() });
        }
    }
    None
}

pub fn model_description(model: &LlamaModelInfo) -> String {
    let mut details = Vec::new();
    let loaded = model_is_loaded(model);
    if loaded {
        details.push("loaded".to_owned());
    } else if model.status.value != "unloaded" {
        details.push(model.status.value.clone());
    }
    if loaded && let Some(context) = context_label(model) {
        details.push(format!("{context} context"));
    }
    details.join(" · ")
}

pub fn compact_count(value: u64) -> String {
    if value >= 1_000_000 { format!("{:.1}M", value as f64 / 1_000_000.0) }
    else if value >= 1_000 { format!("{:.1}k", value as f64 / 1_000.0) }
    else { value.to_string() }
}

fn exact_query(query: &str) -> bool {
    let query = query.trim();
    let Some((owner, rest)) = query.split_once('/') else { return false; };
    !owner.is_empty() && !rest.is_empty() && !rest.contains('/') && !rest.contains(' ')
}

enum UiRequest {
    ShowModels { server_url: String, models: Vec<LlamaModelInfo>, reply: oneshot::Sender<LlamaManagerAction> },
    Select { title: String, options: Vec<String>, reply: oneshot::Sender<Option<String>> },
    SearchModels { search: SearchFn, reply: oneshot::Sender<Option<String>> },
    Progress { state: ProgressState, reply: oneshot::Sender<()> },
    UpdateProgress { state: ProgressState },
    ShowStatus { title: String, message: String },
    Finish,
}

enum SearchOutcome {
    Results { query: String, results: Vec<HuggingFaceModel> },
}

struct ChannelUi {
    tx: mpsc::UnboundedSender<UiRequest>,
    ui: Arc<dyn ExtensionUi>,
    closed: Arc<tokio::sync::Notify>,
}

impl ChannelUi {
    fn send(&self, request: UiRequest) -> bool {
        if self.tx.send(request).is_err() || self.ui.request_render().is_err() {
            self.closed.notify_waiters();
            return false;
        }
        true
    }

    async fn request<T>(&self, make: impl FnOnce(oneshot::Sender<T>) -> UiRequest) -> Option<T> {
        let (tx, rx) = oneshot::channel();
        if !self.send(make(tx)) { return None; }
        rx.await.ok()
    }
}

impl LlamaUi for ChannelUi {
    fn show_models(&self, server_url: String, models: Vec<LlamaModelInfo>) -> UiFuture<'static, LlamaManagerAction> {
        let reply = self.request(|reply| UiRequest::ShowModels { server_url, models, reply });
        Box::pin(async move { reply.await.unwrap_or(LlamaManagerAction::Close) })
    }

    fn select(&self, title: String, options: Vec<String>) -> UiFuture<'static, Option<String>> {
        let reply = self.request(|reply| UiRequest::Select { title, options, reply });
        Box::pin(async move { reply.await.flatten() })
    }

    fn confirm(&self, title: String, message: String) -> UiFuture<'static, bool> {
        let selected = self.select(format!("{title}\n{message}"), vec!["Yes".to_owned(), "No".to_owned()]);
        Box::pin(async move { selected.await.as_deref() == Some("Yes") })
    }

    fn connection_error(&self, server_url: String, message: String) -> UiFuture<'static, bool> {
        let selected = self.select(format!("llama.cpp unavailable\n{server_url}\n\n{message}"), vec!["Retry".to_owned(), "Close".to_owned()]);
        Box::pin(async move { selected.await.as_deref() == Some("Retry") })
    }

    fn search_models(&self, search: SearchFn) -> UiFuture<'static, Option<String>> {
        let reply = self.request(|reply| UiRequest::SearchModels { search, reply });
        Box::pin(async move { reply.await.flatten() })
    }

    fn show_status(&self, title: String, message: String) {
        self.send(UiRequest::ShowStatus { title, message });
    }

    fn progress(&self, state: ProgressState) -> UiFuture<'static, ()> {
        let reply = self.request(|reply| UiRequest::Progress { state, reply });
        Box::pin(async move { let _ = reply.await; })
    }

    fn update_progress(&self, state: ProgressState) {
        self.send(UiRequest::UpdateProgress { state });
    }
}

enum Screen {
    Empty,
    List { server_url: String, models: Vec<LlamaModelInfo>, selected: usize, reply: Option<oneshot::Sender<LlamaManagerAction>> },
    Select { title: String, options: Vec<String>, selected: usize, reply: Option<oneshot::Sender<Option<String>>> },
    Search { input: Input, query: String, results: Vec<HuggingFaceModel>, status: String, cache: BTreeMap<String, Vec<HuggingFaceModel>>, search: Option<SearchFn>, reply: Option<oneshot::Sender<Option<String>>> },
    Progress { state: ProgressState, reply: Option<oneshot::Sender<()>> },
}

pub struct LlamaView {
    theme: Theme,
    render_request: Rc<dyn Fn()>,
    ui: Arc<dyn ExtensionUi>,
    closed: Arc<tokio::sync::Notify>,
    rx: mpsc::UnboundedReceiver<UiRequest>,
    search_tx: mpsc::UnboundedSender<SearchOutcome>,
    search_rx: mpsc::UnboundedReceiver<SearchOutcome>,
    done: CustomUiDone,
    screen: Screen,
    finished: bool,
}

impl LlamaView {
    fn new(theme: Theme, render_request: Rc<dyn Fn()>, ui: Arc<dyn ExtensionUi>, closed: Arc<tokio::sync::Notify>, rx: mpsc::UnboundedReceiver<UiRequest>, search_tx: mpsc::UnboundedSender<SearchOutcome>, search_rx: mpsc::UnboundedReceiver<SearchOutcome>, done: CustomUiDone) -> Self {
        Self { theme, render_request, ui, closed, rx, search_tx, search_rx, done, screen: Screen::Empty, finished: false }
    }

    fn drain(&mut self) {
        while let Ok(request) = self.rx.try_recv() {
            match request {
                UiRequest::ShowModels { server_url, models, reply } => self.screen = Screen::List { server_url, models, selected: 0, reply: Some(reply) },
                UiRequest::Select { title, options, reply } => self.screen = Screen::Select { title, options, selected: 0, reply: Some(reply) },
                UiRequest::SearchModels { search, reply } => self.screen = Screen::Search { input: Input::new(InputOptions::default()), query: String::new(), results: Vec::new(), status: "Type at least 2 characters".to_owned(), cache: BTreeMap::new(), search: Some(search), reply: Some(reply) },
                UiRequest::Progress { state, reply } => self.screen = Screen::Progress { state, reply: Some(reply) },
                UiRequest::UpdateProgress { state } => if let Screen::Progress { state: current, .. } = &mut self.screen { *current = state; },
                UiRequest::ShowStatus { title, message } => self.screen = Screen::Select { title: format!("{title}\n\n{message}"), options: Vec::new(), selected: 0, reply: None },
                UiRequest::Finish => self.finish_and_wake(),
            }
        }
        while let Ok(SearchOutcome::Results { query, results }) = self.search_rx.try_recv() {
            if let Screen::Search { query: current, results: slot, status, cache, .. } = &mut self.screen
                && current.eq_ignore_ascii_case(&query)
            {
                cache.insert(query.to_lowercase(), results.clone());
                *slot = results;
                *status = if slot.is_empty() { "No GGUF models found".to_owned() } else { String::new() };
            }
        }
        (self.render_request)();
    }

    fn finish(&mut self) {
        if !self.finished {
            self.finished = true;
            (self.done)(JsonValue::Null);
        }
    }

    fn finish_and_wake(&mut self) {
        if !self.finished {
            self.finished = true;
            (self.done)(JsonValue::Null);
            let _ = self.ui.request_render();
        }
    }

    fn body_lines(&self, width: usize) -> Vec<String> {
        let theme = &self.theme;
        let border = theme.fg(ThemeColor::Accent, &"─".repeat(width.max(1)));
        let mut lines = vec![border.clone()];
        match &self.screen {
            Screen::Empty => {}
            Screen::List { server_url, models, selected, .. } => {
                lines.push(theme.fg(ThemeColor::Accent, &theme.bold("llama.cpp models")));
                lines.push(theme.fg(ThemeColor::Dim, server_url));
                let mut sorted = models.clone();
                sorted.sort_by(|left, right| model_is_loaded(right).cmp(&model_is_loaded(left)).then_with(|| left.id.cmp(&right.id)));
                for (index, model) in sorted.iter().enumerate() {
                    let line = format!("{}{}  {}", if index == *selected { "→ " } else { "  " }, model.id, model_description(model));
                    lines.push(if index == *selected { theme.fg(ThemeColor::Accent, &line) } else { line });
                }
                lines.push(format!("{}Download model…  Hugging Face owner/repository[:quant]", if *selected == sorted.len() { "→ " } else { "  " }));
                lines.push(theme.fg(ThemeColor::Dim, &format!("{} • {}", key_hint("tui.select.confirm", "load/unload/download", theme), key_hint("tui.select.cancel", "close", theme))));
            }
            Screen::Select { title, options, selected, .. } => {
                lines.push(theme.fg(ThemeColor::Accent, &theme.bold(title)));
                for (index, option) in options.iter().enumerate() {
                    let line = format!("{}{option}", if index == *selected { "→ " } else { "  " });
                    lines.push(if index == *selected { theme.fg(ThemeColor::Accent, &line) } else { line });
                }
                lines.push(theme.fg(ThemeColor::Dim, &format!("{} • {}", key_hint("tui.select.confirm", "select", theme), key_hint("tui.select.cancel", "cancel", theme))));
            }
            Screen::Search { results, status, input, .. } => {
                lines.push(theme.fg(ThemeColor::Accent, &theme.bold("Download model")));
                lines.push(theme.fg(ThemeColor::Dim, "Model name or owner/repository[:quant]"));
                let mut input = input.clone();
                lines.extend(input.render(width));
                for model in results.iter().take(10) { lines.push(format!("  {}  {} downloads", model.id, compact_count(model.downloads))); }
                if !status.is_empty() { lines.push(theme.fg(ThemeColor::Dim, &format!("  {status}"))); }
                lines.push(theme.fg(ThemeColor::Dim, &format!("{} • {}", key_hint("tui.select.confirm", "select", theme), key_hint("tui.select.cancel", "back", theme))));
            }
            Screen::Progress { state, .. } => {
                lines.push(theme.fg(ThemeColor::Accent, &theme.bold(&state.title)));
                lines.push(theme.fg(ThemeColor::Text, &state.model));
                lines.push(theme.fg(ThemeColor::Muted, &state.message));
                if let Some(ratio) = state.ratio {
                    let available = 40usize;
                    let filled = ((ratio.clamp(0.0, 1.0)) * available as f64).round() as usize;
                    lines.push(theme.fg(ThemeColor::Accent, &format!("{}{} {}%", "█".repeat(filled), "─".repeat(available - filled), (ratio * 100.0).round() as u64)));
                }
                if let Some(detail) = &state.detail { lines.push(theme.fg(ThemeColor::Dim, detail)); }
                lines.push(theme.fg(ThemeColor::Dim, &key_hint("tui.select.cancel", "stop", theme)));
            }
        }
        lines.push(border);
        lines
    }

    fn handle_list(&mut self, data: &str) {
        let keys = get_keybindings();
        let Screen::List { models, selected, reply, .. } = &mut self.screen else { return; };
        let count = models.len() + 1;
        if keys.matches(data, "tui.select.up") { *selected = if *selected == 0 { count - 1 } else { *selected - 1 }; }
        else if keys.matches(data, "tui.select.down") { *selected = if *selected == count - 1 { 0 } else { *selected + 1 }; }
        else if keys.matches(data, "tui.select.confirm") {
            let value = if *selected == models.len() { LlamaManagerAction::Download } else { LlamaManagerAction::Model(models[*selected].clone()) };
            if let Some(reply) = reply.take() { let _ = reply.send(value); }
        } else if keys.matches(data, "tui.select.cancel") && let Some(reply) = reply.take() { let _ = reply.send(LlamaManagerAction::Close); }
    }

    fn handle_select(&mut self, data: &str) {
        let keys = get_keybindings();
        let Screen::Select { options, selected, reply, .. } = &mut self.screen else { return; };
        if keys.matches(data, "tui.select.up") { *selected = if *selected == 0 { options.len().saturating_sub(1) } else { *selected - 1 }; }
        else if keys.matches(data, "tui.select.down") { *selected = if *selected + 1 >= options.len() { 0 } else { *selected + 1 }; }
        else if keys.matches(data, "tui.select.confirm") { let value = options.get(*selected).cloned(); if let Some(reply) = reply.take() { let _ = reply.send(value); } }
        else if keys.matches(data, "tui.select.cancel") && let Some(reply) = reply.take() { let _ = reply.send(None); }
    }

    fn handle_search(&mut self, data: &str) {
        let keys = get_keybindings();
        if keys.matches(data, "tui.select.cancel") { self.reply_search(None); return; }
        if keys.matches(data, "tui.select.confirm") {
            if let Screen::Search { query, results, reply, .. } = &mut self.screen {
                let value = if exact_query(query) { Some(query.clone()) } else { results.first().map(|model| model.id.clone()) };
                if let Some(reply) = reply.take() { let _ = reply.send(value); }
            }
            return;
        }
        let mut spawn: Option<(SearchFn, String)> = None;
        if let Screen::Search { input, query, results, status, cache, search, .. } = &mut self.screen {
            input.handle_input(data);
            let next = input.get_value().trim().to_owned();
            if next == *query { return; }
            *query = next.clone();
            if next.len() < 2 { *status = "Type at least 2 characters".to_owned(); results.clear(); return; }
            if let Some(cached) = cache.get(&next.to_lowercase()) { *results = cached.clone(); *status = if results.is_empty() { "No GGUF models found".to_owned() } else { String::new() }; return; }
            *status = "Searching Hugging Face…".to_owned();
            if let Some(search) = search.clone() { spawn = Some((search, next)); }
        }
        if let Some((search, query)) = spawn {
            let tx = self.search_tx.clone();
            let ui = self.ui.clone();
            let closed = self.closed.clone();
            tokio::spawn(async move {
                let results = search(query.clone(), AbortController::new().signal()).await.unwrap_or_default();
                if tx.send(SearchOutcome::Results { query, results }).is_err() || ui.request_render().is_err() {
                    closed.notify_waiters();
                }
            });
        }
    }

    fn reply_search(&mut self, value: Option<String>) {
        if let Screen::Search { reply, .. } = &mut self.screen
            && let Some(reply) = reply.take() { let _ = reply.send(value); }
    }
}

impl Component for LlamaView {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.drain();
        self.body_lines(width)
    }

    fn handle_input(&mut self, data: &str) {
        self.drain();
        if matches!(self.screen, Screen::List { .. }) {
            self.handle_list(data);
        } else if matches!(self.screen, Screen::Select { .. }) {
            self.handle_select(data);
        } else if matches!(self.screen, Screen::Search { .. }) {
            self.handle_search(data);
        } else if get_keybindings().matches(data, "tui.select.cancel")
            && let Screen::Progress { reply, .. } = &mut self.screen
            && let Some(reply) = reply.take()
        {
            let _ = reply.send(());
        }
        (self.render_request)();
    }

    fn has_input_handler(&self) -> bool { true }

    fn invalidate(&mut self) {}
}

pub struct ShowLlamaOptions {
    /// The UI-thread repaint handle. The llama flow builds it from `ctx.ui`'s `request_render`
    /// repaint seam, so the binding carries no per-session host state and the extension stays the
    /// pinned static inline factory.
    pub render: RenderBinding,
    pub flow: Box<dyn FnOnce(Arc<dyn LlamaUi>) -> UiFuture<'static, ()> + Send>,
}

/// `RenderBinding`: given the extension TUI host, returns a `'static` request-render closure
/// (mirrors `maho-ext-builtin-loose::files::RenderBinding`).
pub type RenderBinding = Arc<dyn Fn(&dyn ExtensionTuiHost) -> Rc<dyn Fn()> + Send + Sync>;

/// Pinned `showLlamaUi`: the Send flow runs on the runtime, driving the UI-thread component over channels.
pub async fn show_llama_ui(ctx: ExtensionContext, options: ShowLlamaOptions) -> Result<(), ExtensionFailure> {
    let (tx, rx) = mpsc::unbounded_channel();
    let (search_tx, search_rx) = mpsc::unbounded_channel();
    let ui_host = ctx.ui.clone();
    let render = options.render;
    let flow = Arc::new(std::sync::Mutex::new(Some(options.flow)));
    let rx_slot = Arc::new(std::sync::Mutex::new(Some(rx)));
    let search_rx_slot = Arc::new(std::sync::Mutex::new(Some(search_rx)));
    let closed = Arc::new(tokio::sync::Notify::new());
    let factory_ui = ui_host.clone();
    ctx.ui.custom_factory(Arc::new(move |host, theme, _keybindings, done| {
        let render_request = (render)(host);
        let rx = rx_slot.lock().unwrap_or_else(|error| error.into_inner()).take();
        let search_rx = search_rx_slot.lock().unwrap_or_else(|error| error.into_inner()).take();
        let (Some(rx), Some(search_rx)) = (rx, search_rx) else {
            return Box::pin(async { Ok(Box::new(EmptyView) as Box<dyn Component>) });
        };
        let view = LlamaView::new(theme.clone(), render_request, factory_ui.clone(), closed.clone(), rx, search_tx.clone(), search_rx, done);
        if let Some(flow) = flow.lock().unwrap_or_else(|error| error.into_inner()).take() {
            let ui: Arc<dyn LlamaUi> = Arc::new(ChannelUi { tx: tx.clone(), ui: ui_host.clone(), closed: closed.clone() });
            let tx = tx.clone();
            tokio::spawn(async move {
                let flow = flow(ui);
                tokio::pin!(flow);
                tokio::select! {
                    _ = &mut flow => {}
                    _ = closed.notified() => {}
                }
                let _ = tx.send(UiRequest::Finish);
            });
        }
        Box::pin(async move { Ok(Box::new(view) as Box<dyn Component>) })
    }), CustomUiFactoryOptions {
        overlay: true,
        overlay_options: Some(ExtensionOverlayOptions::Static(Arc::new(|| maho_tui::tui::OverlayOptions {
            width: Some(SizeValue::Percent(80.0)),
            min_width: Some(60),
            max_height: Some(SizeValue::Percent(80.0)),
            ..Default::default()
        }))),
        ..Default::default()
    }).await?;
    Ok(())
}

struct EmptyView;

impl Component for EmptyView {
    fn render(&mut self, _width: usize) -> Vec<String> { Vec::new() }
}

pub struct RunProgressOptions<T> {
    pub title: String,
    pub model: String,
    pub initial_message: String,
    pub cancel_title: String,
    pub cancel_message: String,
    pub run: Box<dyn FnOnce(AbortSignal, Box<dyn FnMut(LlamaProgress) + Send>) -> UiFuture<'static, Result<T, String>> + Send>,
    pub cancel: Box<dyn FnOnce() -> UiFuture<'static, ()> + Send>,
}

pub async fn run_with_progress<T: Send + 'static>(ui: Arc<dyn LlamaUi>, options: RunProgressOptions<T>) -> Result<Option<T>, String> {
    let signal = AbortController::new().signal();
    let progress_slot: Arc<std::sync::Mutex<ProgressState>> = Arc::new(std::sync::Mutex::new(ProgressState { title: options.title.clone(), model: options.model.clone(), message: options.initial_message.clone(), ratio: None, detail: None }));
    let update_slot = progress_slot.clone();
    let ui_update = ui.clone();
    let run = (options.run)(signal.clone(), Box::new(move |progress| {
        if let Ok(mut state) = update_slot.lock() {
            state.message = progress.message;
            state.ratio = progress.ratio;
            state.detail = progress.detail;
            ui_update.update_progress(state.clone());
        }
    }));
    tokio::pin!(run);
    let progress_future = ui.progress(progress_slot.lock().map(|state| state.clone()).unwrap_or_default());
    tokio::pin!(progress_future);
    loop {
        tokio::select! {
            result = &mut run => return Ok(Some(result?)),
            () = &mut progress_future => {
                let stop = ui.confirm(options.cancel_title.clone(), options.cancel_message.clone()).await;
                if !stop {
                    progress_future.set(ui.progress(progress_slot.lock().map(|state| state.clone()).unwrap_or_default()));
                    continue;
                }
                (options.cancel)().await;
                signal.abort(None);
                return Ok(None);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::{LlamaModelMeta, LlamaModelState};
    use maho_ext_api::{
        ComponentFactory, CustomUiOptions, ExtensionFuture, ExtensionUiDialogOptions,
        ExtensionWidgetOptions, NotificationType, WidgetContent,
    };
    use maho_interactive::theme::ColorMode;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    fn model(id: &str, status: &str) -> LlamaModelInfo {
        LlamaModelInfo { id: id.to_owned(), status: LlamaModelState { value: status.to_owned(), ..Default::default() }, ..Default::default() }
    }

    #[test]
    fn list_sort_puts_loaded_first_then_id() {
        let mut models = vec![model("b", "unloaded"), model("a", "loaded"), model("c", "sleeping")];
        models.sort_by(|left, right| model_is_loaded(right).cmp(&model_is_loaded(left)).then_with(|| left.id.cmp(&right.id)));
        assert_eq!(models.iter().map(|model| model.id.as_str()).collect::<Vec<_>>(), vec!["a", "c", "b"]);
    }

    #[test]
    fn description_reports_loaded_context_and_status() {
        let mut loaded = model("m", "loaded");
        loaded.meta = Some(LlamaModelMeta { n_ctx: Some(8192), ..Default::default() });
        assert_eq!(model_description(&loaded), "loaded · 8k context");
        assert_eq!(model_description(&model("m", "unloaded")), "");
        assert_eq!(model_description(&model("m", "loading")), "loading");
    }

    #[test]
    fn exact_query_requires_owner_and_repo() {
        assert!(exact_query("owner/repo"));
        assert!(exact_query("owner/repo:Q4_K_M"));
        assert!(!exact_query("repo"));
        assert!(!exact_query("owner/"));
        assert!(!exact_query("a/b/c"));
    }

    struct FauxUi {
        updates: Mutex<Vec<ProgressState>>,
        confirm_answers: Mutex<VecDeque<bool>>,
        confirms: AtomicUsize,
        confirm_permits: Arc<tokio::sync::Semaphore>,
        progress_gates: Mutex<VecDeque<tokio::sync::oneshot::Receiver<()>>>,
    }

    impl FauxUi {
        fn new() -> Self {
            Self {
                updates: Mutex::new(Vec::new()),
                confirm_answers: Mutex::new(VecDeque::new()),
                confirms: AtomicUsize::new(0),
                confirm_permits: Arc::new(tokio::sync::Semaphore::new(0)),
                progress_gates: Mutex::new(VecDeque::new()),
            }
        }
        fn gate(&self) -> tokio::sync::oneshot::Sender<()> {
            let (tx, rx) = tokio::sync::oneshot::channel();
            self.progress_gates.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push_back(rx);
            tx
        }
        fn answer(&self, value: bool) {
            self.confirm_answers.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push_back(value);
        }
    }

    impl LlamaUi for FauxUi {
        fn show_models(&self, _: String, _: Vec<LlamaModelInfo>) -> UiFuture<'static, LlamaManagerAction> {
            panic!("show_models is not expected in a run_with_progress test")
        }
        fn select(&self, _: String, _: Vec<String>) -> UiFuture<'static, Option<String>> {
            panic!("select is not expected in a run_with_progress test")
        }
        fn confirm(&self, _: String, _: String) -> UiFuture<'static, bool> {
            self.confirms.fetch_add(1, Ordering::SeqCst);
            self.confirm_permits.add_permits(1);
            let answer = self.confirm_answers.lock().unwrap_or_else(std::sync::PoisonError::into_inner).pop_front().unwrap_or(false);
            Box::pin(async move { answer })
        }
        fn connection_error(&self, _: String, _: String) -> UiFuture<'static, bool> {
            panic!("connection_error is not expected in a run_with_progress test")
        }
        fn search_models(&self, _: SearchFn) -> UiFuture<'static, Option<String>> {
            panic!("search_models is not expected in a run_with_progress test")
        }
        fn show_status(&self, _: String, _: String) {
            panic!("show_status is not expected in a run_with_progress test")
        }
        fn progress(&self, _: ProgressState) -> UiFuture<'static, ()> {
            match self.progress_gates.lock().unwrap_or_else(std::sync::PoisonError::into_inner).pop_front() {
                Some(receiver) => Box::pin(async move { let _ = receiver.await; }),
                None => Box::pin(std::future::pending::<()>()),
            }
        }
        fn update_progress(&self, state: ProgressState) {
            self.updates.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(state);
        }
    }

    #[tokio::test]
    async fn run_with_progress_streams_updates_and_returns_the_completed_value() {
        let faux = Arc::new(FauxUi::new());
        let ui: Arc<dyn LlamaUi> = faux.clone();
        let outcome = tokio::time::timeout(Duration::from_secs(3), run_with_progress::<u32>(ui, RunProgressOptions {
            title: "Loading model".to_owned(), model: "m".to_owned(), initial_message: "Starting".to_owned(),
            cancel_title: "Stop loading?".to_owned(), cancel_message: "m".to_owned(),
            run: Box::new(|_signal, mut update| Box::pin(async move {
                update(LlamaProgress { message: "half".to_owned(), ratio: Some(0.5), detail: None });
                update(LlamaProgress { message: "done".to_owned(), ratio: Some(1.0), detail: Some("x".to_owned()) });
                Ok(42)
            })),
            cancel: Box::new(|| -> UiFuture<'static, ()> { Box::pin(async { panic!("cancel must not run when the run completes"); }) }),
        })).await.expect("bounded");
        assert_eq!(outcome.expect("completed"), Some(42));
        let updates = faux.updates.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(updates.len(), 2, "every progress update must reach update_progress");
        assert_eq!(updates[0].message, "half");
        assert_eq!(updates[1].ratio, Some(1.0));
        assert_eq!(faux.confirms.load(Ordering::SeqCst), 0, "a completed run must not prompt for cancellation");
    }

    #[tokio::test]
    async fn declining_cancellation_keeps_the_same_pending_operation() {
        let faux = Arc::new(FauxUi::new());
        faux.answer(false);
        let gate = faux.gate();
        let ui: Arc<dyn LlamaUi> = faux.clone();
        let permits = faux.confirm_permits.clone();
        let started = Arc::new(AtomicUsize::new(0));
        let cancels = Arc::new(AtomicUsize::new(0));
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        let run_started = started.clone();
        let cancel_count = cancels.clone();
        let handle = tokio::spawn(async move {
            run_with_progress::<u32>(ui, RunProgressOptions {
                title: "Loading model".to_owned(), model: "m".to_owned(), initial_message: "Starting".to_owned(),
                cancel_title: "Stop loading?".to_owned(), cancel_message: "m".to_owned(),
                run: Box::new(move |_signal, _update| Box::pin(async move {
                    run_started.fetch_add(1, Ordering::SeqCst);
                    let _ = release_rx.await;
                    Ok(7)
                })),
                cancel: Box::new(move || {
                    let cancel_count = cancel_count.clone();
                    Box::pin(async move { cancel_count.fetch_add(1, Ordering::SeqCst); })
                }),
            }).await
        });
        gate.send(()).expect("progress gate");
        tokio::time::timeout(Duration::from_secs(3), permits.acquire()).await.expect("the cancel prompt must appear").expect("permit");
        release_tx.send(()).expect("release");
        let outcome = tokio::time::timeout(Duration::from_secs(3), handle).await.expect("bounded").expect("join");
        assert_eq!(outcome.expect("completed"), Some(7));
        assert_eq!(started.load(Ordering::SeqCst), 1, "the same operation must continue, not restart");
        assert_eq!(cancels.load(Ordering::SeqCst), 0, "a declined cancellation must not run the cancel closure");
        assert_eq!(faux.confirms.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn confirming_cancellation_runs_cancel_and_drops_the_operation() {
        let faux = Arc::new(FauxUi::new());
        faux.answer(true);
        let gate = faux.gate();
        let ui: Arc<dyn LlamaUi> = faux.clone();
        let cancels = Arc::new(AtomicUsize::new(0));
        let cancel_count = cancels.clone();
        let handle = tokio::spawn(async move {
            run_with_progress::<u32>(ui, RunProgressOptions {
                title: "Loading model".to_owned(), model: "m".to_owned(), initial_message: "Starting".to_owned(),
                cancel_title: "Stop loading?".to_owned(), cancel_message: "m".to_owned(),
                run: Box::new(|_signal, _update| Box::pin(async { std::future::pending::<Result<u32, String>>().await })),
                cancel: Box::new(move || {
                    let cancel_count = cancel_count.clone();
                    Box::pin(async move { cancel_count.fetch_add(1, Ordering::SeqCst); })
                }),
            }).await
        });
        gate.send(()).expect("progress gate");
        let outcome = tokio::time::timeout(Duration::from_secs(3), handle).await.expect("bounded").expect("join");
        assert_eq!(outcome.expect("cancelled"), None, "a confirmed cancellation returns the cancelled outcome");
        assert_eq!(cancels.load(Ordering::SeqCst), 1, "the cancel closure must run exactly once");
    }

    struct NullUi {
        renders: AtomicUsize,
    }

    impl ExtensionUi for NullUi {
        fn select<'a>(&'a self, _: &'a str, _: &'a [String], _: ExtensionUiDialogOptions) -> UiFuture<'a, Option<String>> { Box::pin(async { None }) }
        fn confirm<'a>(&'a self, _: &'a str, _: &'a str, _: ExtensionUiDialogOptions) -> UiFuture<'a, bool> { Box::pin(async { false }) }
        fn input<'a>(&'a self, _: &'a str, _: Option<&'a str>, _: ExtensionUiDialogOptions) -> UiFuture<'a, Option<String>> { Box::pin(async { None }) }
        fn notify(&self, _: &str, _: NotificationType) {}
        fn set_status(&self, _: &str, _: Option<&str>) {}
        fn set_widget(&self, _: &str, _: Option<WidgetContent>, _: ExtensionWidgetOptions) {}
        fn set_header(&self, _: Option<ComponentFactory>) {}
        fn set_footer(&self, _: Option<ComponentFactory>) {}
        fn set_title(&self, _: &str) {}
        fn paste_to_editor(&self, _: &str) {}
        fn set_editor_text(&self, _: &str) {}
        fn get_editor_text(&self) -> String { String::new() }
        fn custom(&self, _: ComponentFactory, _: CustomUiOptions) -> ExtensionFuture<'_, JsonValue> { Box::pin(async { Err(ExtensionFailure::new("unavailable")) }) }
        fn theme(&self) -> Theme { Theme::builtin("dark", ColorMode::Truecolor).expect("theme") }
        fn request_render(&self) -> Result<(), ExtensionFailure> { self.renders.fetch_add(1, Ordering::SeqCst); Ok(()) }
    }

    #[tokio::test]
    async fn finish_marks_the_view_done_and_requests_a_redraw() {
        let (tx, rx) = mpsc::unbounded_channel();
        let (search_tx, search_rx) = mpsc::unbounded_channel();
        let renders = Arc::new(AtomicUsize::new(0));
        let render_request = {
            let renders = renders.clone();
            Rc::new(move || { renders.fetch_add(1, Ordering::SeqCst); }) as Rc<dyn Fn()>
        };
        let null = Arc::new(NullUi { renders: AtomicUsize::new(0) });
        let ui: Arc<dyn ExtensionUi> = null.clone();
        let done_count = Arc::new(AtomicUsize::new(0));
        let done: CustomUiDone = {
            let done_count = done_count.clone();
            Rc::new(move |_: JsonValue| { done_count.fetch_add(1, Ordering::SeqCst); })
        };
        let mut view = LlamaView::new(
            Theme::builtin("dark", ColorMode::Truecolor).expect("theme"),
            render_request,
            ui,
            Arc::new(tokio::sync::Notify::new()),
            rx,
            search_tx,
            search_rx,
            done,
        );
        tx.send(UiRequest::Finish).expect("finish");
        let _ = view.render(80);
        assert_eq!(done_count.load(Ordering::SeqCst), 1, "the finished flow must complete the custom UI exactly once");
        assert!(renders.load(Ordering::SeqCst) >= 1, "the finished flow must request a repaint");
        assert!(null.renders.load(Ordering::SeqCst) >= 1, "the repaint must reach the UI seam");
    }
}
