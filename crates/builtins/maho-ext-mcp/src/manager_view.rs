//! Port of senpi `builtin/mcp/manager-view.ts` (`McpManagerView`).
//!
//! DIVERGENCE: senpi's view owns the async `menu()` promise directly. Here the component lives on the
//! terminal thread (`!Send`) while the manager flow runs on the runtime, so the flow drives the view
//! through a [`McpManagerHandle`] over channels (the same seam `maho-ext-llama`'s view uses). Event-driven
//! rebuilds, stale-build suppression, selection preservation and dispose semantics match upstream.
use std::{
    cell::RefCell,
    future::Future,
    pin::Pin,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
};

use maho_ext_api::{CustomUiDone, JsonValue, Theme};
use maho_interactive::components::{dynamic_border::DynamicBorder, keybinding_hints::key_text};
use maho_tui::{
    components::{
        select_list::{SelectItem, SelectList, SelectListLayoutOptions, SelectListTheme},
        spacer::Spacer,
        text::Text,
    },
    keybindings::KeybindingsManager,
    terminal_text::sanitize_terminal_label,
    tui::Component,
    utils::truncate_to_width,
};
use tokio::sync::{Notify, mpsc, oneshot};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Upstream `McpManagerMenu`.
#[derive(Clone, Debug, Default)]
pub struct McpManagerMenu {
    pub title: String,
    pub items: Vec<SelectItem>,
    pub details: Option<String>,
    pub empty: Option<String>,
    pub confirm_label: String,
    pub cancel_label: String,
    pub selected: Option<String>,
}

/// Upstream `build: () => McpManagerMenu | Promise<McpManagerMenu>`; an `Err` rejects the menu.
pub type MenuBuild =
    Arc<dyn Fn() -> BoxFuture<'static, Result<McpManagerMenu, String>> + Send + Sync>;
/// Upstream `subscribe(listener)`: each call returns a future that resolves on the next MCP event.
/// Dropping the future is the unsubscribe.
pub type MenuSubscribe = Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>;

/// The menu/status surface the manager flow drives (upstream `McpManagerView`'s public API).
pub trait McpManagerSurface: Send + Sync {
    fn menu(
        &self,
        build: MenuBuild,
        subscribe: Option<MenuSubscribe>,
    ) -> BoxFuture<'_, Result<Option<String>, String>>;
    fn status(&self, title: &str, message: &str);
    fn columns(&self) -> usize;
}

enum ViewRequest {
    Open {
        id: u64,
        reply: oneshot::Sender<Option<String>>,
    },
    Menu {
        id: u64,
        menu: McpManagerMenu,
    },
    Status {
        title: String,
        message: String,
    },
    Finish(JsonValue),
}

/// The runtime-side half: sends menus to the view and awaits the user's choice.
#[derive(Clone)]
pub struct McpManagerHandle {
    tx: mpsc::UnboundedSender<ViewRequest>,
    resize: Arc<Notify>,
    closed: Arc<AtomicBool>,
    columns: Arc<AtomicUsize>,
    request_render: Arc<dyn Fn() + Send + Sync>,
    next_id: Arc<AtomicU64>,
}

/// The terminal-thread half's endpoints, consumed by [`McpManagerView::new`].
pub struct McpManagerViewParts {
    rx: mpsc::UnboundedReceiver<ViewRequest>,
    resize: Arc<Notify>,
    closed: Arc<AtomicBool>,
    columns: Arc<AtomicUsize>,
}

/// Creates a connected handle/view pair; `request_render` schedules a frame after each update.
pub fn mcp_manager_channel(
    request_render: Arc<dyn Fn() + Send + Sync>,
) -> (McpManagerHandle, McpManagerViewParts) {
    let (tx, rx) = mpsc::unbounded_channel();
    let resize = Arc::new(Notify::new());
    let closed = Arc::new(AtomicBool::new(false));
    let columns = Arc::new(AtomicUsize::new(80));
    (
        McpManagerHandle {
            tx,
            resize: resize.clone(),
            closed: closed.clone(),
            columns: columns.clone(),
            request_render,
            next_id: Arc::new(AtomicU64::new(1)),
        },
        McpManagerViewParts {
            rx,
            resize,
            closed,
            columns,
        },
    )
}

/// Resolves on the subscription's next event, or never without one.
async fn next_change(change: &mut Option<BoxFuture<'static, ()>>) {
    match change.as_mut() {
        Some(change) => change.await,
        None => std::future::pending().await,
    }
}

async fn next_build(
    pending: &mut Option<BoxFuture<'static, Result<McpManagerMenu, String>>>,
) -> Result<McpManagerMenu, String> {
    match pending.as_mut() {
        Some(build) => build.await,
        None => std::future::pending().await,
    }
}

impl McpManagerHandle {
    fn send(&self, request: ViewRequest) -> bool {
        if self.tx.send(request).is_err() {
            return false;
        }
        (self.request_render)();
        true
    }

    /// Upstream `menu(build, subscribe)`: resolves with the confirmed value, `None` on cancel or
    /// disposal, or the current build's error. A newer rebuild replaces (drops) an older one in
    /// flight, so a superseded snapshot never reaches the view.
    pub async fn menu(
        &self,
        build: MenuBuild,
        subscribe: Option<MenuSubscribe>,
    ) -> Result<Option<String>, String> {
        if self.closed.load(Ordering::SeqCst) {
            return Ok(None);
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (reply_tx, mut reply) = oneshot::channel();
        // Subscribe before the view learns of the menu, so no event between them is lost.
        let mut change = subscribe.as_ref().map(|subscribe| subscribe());
        let mut pending = Some(build());
        if !self.send(ViewRequest::Open {
            id,
            reply: reply_tx,
        }) {
            return Ok(None);
        }
        loop {
            tokio::select! {
                choice = &mut reply => return Ok(choice.unwrap_or(None)),
                built = next_build(&mut pending) => {
                    pending = None;
                    let menu = built?;
                    if !self.send(ViewRequest::Menu { id, menu }) {
                        return Ok(None);
                    }
                }
                () = next_change(&mut change) => {
                    change = subscribe.as_ref().map(|subscribe| subscribe());
                    pending = Some(build());
                }
                () = self.resize.notified() => pending = Some(build()),
            }
        }
    }

    /// Upstream `status(title, message)`.
    pub fn status(&self, title: &str, message: &str) {
        self.send(ViewRequest::Status {
            title: title.into(),
            message: message.into(),
        });
    }

    /// Upstream `get columns()`: the terminal width.
    pub fn columns(&self) -> usize {
        self.columns.load(Ordering::SeqCst)
    }

    /// Completes the custom UI (upstream `done(value)`).
    pub fn finish(&self, value: JsonValue) {
        self.send(ViewRequest::Finish(value));
    }
}

impl McpManagerSurface for McpManagerHandle {
    fn menu(
        &self,
        build: MenuBuild,
        subscribe: Option<MenuSubscribe>,
    ) -> BoxFuture<'_, Result<Option<String>, String>> {
        Box::pin(McpManagerHandle::menu(self, build, subscribe))
    }
    fn status(&self, title: &str, message: &str) {
        McpManagerHandle::status(self, title, message);
    }
    fn columns(&self) -> usize {
        McpManagerHandle::columns(self)
    }
}

struct ActiveMenu {
    id: u64,
    menu: McpManagerMenu,
    list: Option<SelectList>,
    selected: Rc<RefCell<Option<String>>>,
    outcome: Rc<RefCell<Option<Option<String>>>>,
}

enum Screen {
    Status { title: String, message: String },
    Menu(Box<ActiveMenu>),
}

/// A focused menu that rebuilds on MCP events, never on a polling timer.
pub struct McpManagerView {
    theme: Theme,
    keybindings: KeybindingsManager,
    dimensions: Rc<dyn Fn() -> (u16, u16)>,
    render_request: Rc<dyn Fn()>,
    rx: mpsc::UnboundedReceiver<ViewRequest>,
    resize: Arc<Notify>,
    closed: Arc<AtomicBool>,
    columns: Arc<AtomicUsize>,
    done: CustomUiDone,
    finished: bool,
    reply: Option<(u64, oneshot::Sender<Option<String>>)>,
    screen: Screen,
    width: Option<usize>,
    rows: Option<usize>,
}

fn sanitize_lines(text: &str) -> String {
    text.split('\n')
        .map(sanitize_terminal_label)
        .collect::<Vec<_>>()
        .join("\n")
}

fn key_hint(theme: &Theme, keybinding: &str, description: &str) -> String {
    theme.fg("dim", &key_text(keybinding)) + &theme.fg("muted", &format!(" {description}"))
}

/// senpi `getSelectListTheme()` over the extension-facing theme.
fn select_list_theme(theme: &Theme) -> SelectListTheme {
    let color = |name: &'static str| -> Rc<dyn Fn(&str) -> String> {
        let theme = theme.clone();
        Rc::new(move |text: &str| theme.fg(name, text))
    };
    SelectListTheme {
        selected_prefix: color("accent"),
        selected_text: color("accent"),
        description: color("muted"),
        scroll_info: color("muted"),
        no_match: color("muted"),
        render_row: None,
    }
}

impl McpManagerView {
    pub fn new(
        parts: McpManagerViewParts,
        theme: Theme,
        keybindings: KeybindingsManager,
        dimensions: Rc<dyn Fn() -> (u16, u16)>,
        render_request: Rc<dyn Fn()>,
        done: CustomUiDone,
    ) -> Self {
        let McpManagerViewParts {
            rx,
            resize,
            closed,
            columns,
        } = parts;
        columns.store(usize::from(dimensions().0), Ordering::SeqCst);
        Self {
            theme,
            keybindings,
            dimensions,
            render_request,
            rx,
            resize,
            closed,
            columns,
            done,
            finished: false,
            reply: None,
            screen: Screen::Status {
                title: "MCP servers".into(),
                message: "Loading...".into(),
            },
            width: None,
            rows: None,
        }
    }

    fn terminal_rows(&self) -> usize {
        usize::from((self.dimensions)().1)
    }

    fn build_menu(&self, id: u64, menu: McpManagerMenu, wanted: Option<String>) -> ActiveMenu {
        let selected = Rc::new(RefCell::new(None));
        let outcome = Rc::new(RefCell::new(None));
        let list = (!menu.items.is_empty()).then(|| {
            let items = menu
                .items
                .iter()
                .map(|item| {
                    let label = sanitize_terminal_label(if item.label.is_empty() {
                        &item.value
                    } else {
                        &item.label
                    });
                    SelectItem {
                        value: item.value.clone(),
                        label: if label.is_empty() {
                            "(empty)".into()
                        } else {
                            label
                        },
                        description: item.description.as_deref().map(sanitize_terminal_label),
                    }
                })
                .collect();
            let visible = (self.terminal_rows() as i64 - 10).clamp(1, 12) as usize;
            let mut list = SelectList::new(
                items,
                visible,
                select_list_theme(&self.theme),
                SelectListLayoutOptions::default(),
            );
            let wanted = wanted.or_else(|| menu.selected.clone());
            if let Some(index) = menu
                .items
                .iter()
                .position(|item| Some(&item.value) == wanted.as_ref())
            {
                list.set_selected_index(index);
            }
            *selected.borrow_mut() = list.get_selected_item().map(|item| item.value.clone());
            let on_change = selected.clone();
            list.on_selection_change = Some(Box::new(move |item: &SelectItem| {
                *on_change.borrow_mut() = Some(item.value.clone());
            }));
            let on_select = outcome.clone();
            list.on_select = Some(Box::new(move |item: &SelectItem| {
                *on_select.borrow_mut() = Some(Some(item.value.clone()));
            }));
            let on_cancel = outcome.clone();
            list.on_cancel = Some(Box::new(move || {
                *on_cancel.borrow_mut() = Some(None);
            }));
            list
        });
        ActiveMenu {
            id,
            menu,
            list,
            selected,
            outcome,
        }
    }

    fn drain(&mut self) {
        while let Ok(request) = self.rx.try_recv() {
            match request {
                ViewRequest::Open { id, reply } => self.reply = Some((id, reply)),
                ViewRequest::Menu { id, menu } => {
                    // Only the open menu accepts snapshots; a settled menu ignores late rebuilds.
                    if self.reply.as_ref().is_none_or(|(open, _)| *open != id) {
                        continue;
                    }
                    let wanted = match &self.screen {
                        Screen::Menu(active) if active.id == id => active.selected.borrow().clone(),
                        _ => None,
                    };
                    self.screen = Screen::Menu(Box::new(self.build_menu(id, menu, wanted)));
                }
                ViewRequest::Status { title, message } => {
                    self.screen = Screen::Status { title, message }
                }
                ViewRequest::Finish(value) => {
                    if !self.finished {
                        self.finished = true;
                        (self.done)(value);
                    }
                }
            }
        }
    }

    fn settle(&mut self, value: Option<String>) {
        if let Some((_, reply)) = self.reply.take()
            && reply.send(value).is_err()
        {
            // The flow stopped waiting (disposed or superseded); nothing to resolve.
        }
    }

    fn content(&mut self, width: usize) -> Vec<String> {
        let theme = self.theme.clone();
        let border_theme = theme.clone();
        let mut border =
            DynamicBorder::with_color(Rc::new(move |text: &str| border_theme.fg("accent", text)));
        let mut lines = border.render(width);
        let title = match &self.screen {
            Screen::Status { title, .. } => title.clone(),
            Screen::Menu(active) => active.menu.title.clone(),
        };
        lines.extend(
            Text::with_padding(
                theme.fg("accent", &theme.bold(&sanitize_terminal_label(&title))),
                1,
                0,
            )
            .render(width),
        );
        match &mut self.screen {
            Screen::Status { message, .. } => {
                lines.extend(Spacer::new(1).render(width));
                lines.extend(
                    Text::with_padding(theme.fg("muted", &sanitize_lines(message)), 1, 0)
                        .render(width),
                );
            }
            Screen::Menu(active) => {
                let menu = &active.menu;
                if let Some(details) = menu.details.as_deref().filter(|text| !text.is_empty()) {
                    lines.extend(
                        Text::with_padding(theme.fg("muted", &sanitize_lines(details)), 1, 0)
                            .render(width),
                    );
                }
                lines.extend(Spacer::new(1).render(width));
                match active.list.as_mut() {
                    Some(list) => lines.extend(list.render(width)),
                    None => lines.extend(
                        Text::with_padding(
                            sanitize_terminal_label(
                                menu.empty.as_deref().unwrap_or("Nothing to show."),
                            ),
                            1,
                            0,
                        )
                        .render(width),
                    ),
                }
                let footer = if menu.items.is_empty() {
                    key_hint(&theme, "tui.select.cancel", &menu.cancel_label)
                } else {
                    format!(
                        "{} | {}",
                        key_hint(&theme, "tui.select.confirm", &menu.confirm_label),
                        key_hint(&theme, "tui.select.cancel", &menu.cancel_label)
                    )
                };
                lines.extend(Spacer::new(1).render(width));
                lines.extend(Text::with_padding(footer, 1, 0).render(width));
            }
        }
        lines.extend(border.render(width));
        lines
    }
}

impl Component for McpManagerView {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.drain();
        let (columns, rows) = (self.dimensions)();
        self.columns.store(usize::from(columns), Ordering::SeqCst);
        let rows = usize::from(rows);
        if self.width != Some(width) || self.rows != Some(rows) {
            self.width = Some(width);
            self.rows = Some(rows);
            self.resize.notify_one();
        }
        self.content(width)
            .into_iter()
            .map(|line| truncate_to_width(&line, width, "", false))
            .collect()
    }

    fn handle_input(&mut self, data: &str) {
        self.drain();
        let open = self.reply.as_ref().map(|(id, _)| *id);
        let mut outcome = None;
        match &mut self.screen {
            Screen::Menu(active) if Some(active.id) == open => match active.list.as_mut() {
                Some(list) => {
                    list.handle_input(data);
                    outcome = active.outcome.borrow_mut().take();
                }
                None => {
                    if self.keybindings.matches(data, "tui.select.cancel") {
                        outcome = Some(None);
                    }
                }
            },
            _ => {
                // Still loading: only cancellation is accepted.
                if open.is_some() && self.keybindings.matches(data, "tui.select.cancel") {
                    outcome = Some(None);
                }
            }
        }
        if let Some(value) = outcome {
            self.settle(value);
        }
        (self.render_request)();
    }

    fn has_input_handler(&self) -> bool {
        true
    }

    fn invalidate(&mut self) {
        self.resize.notify_one();
    }

    fn dispose(&mut self) {
        self.closed.store(true, Ordering::SeqCst);
        self.settle(None);
    }
}

impl Drop for McpManagerView {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use maho_tui::keybindings::{
        Keys, KeybindingsConfig, get_keybindings, set_keybindings, tui_keybindings,
    };
    use maho_tui::utils::{strip_terminal_sequences, visible_width};
    use std::cell::Cell;
    use std::time::Duration;

    /// Serializes tests that read or replace the process-wide keybindings.
    static KEYS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    const CONTROLS: &str = "\x1b]2;MCP_INJECTION\x07\x1b[2J";

    fn theme() -> Theme {
        Theme {
            colors: [
                ("accent", "\x1b[38;5;1m"),
                ("muted", "\x1b[38;5;2m"),
                ("dim", "\x1b[38;5;3m"),
            ]
            .map(|(key, value)| (key.into(), value.into()))
            .into(),
            ..Default::default()
        }
    }

    fn item(value: &str, label: &str) -> SelectItem {
        SelectItem {
            value: value.into(),
            label: label.into(),
            description: None,
        }
    }

    fn menu(items: Vec<SelectItem>) -> McpManagerMenu {
        McpManagerMenu {
            title: "MCP servers".into(),
            items,
            confirm_label: "manage".into(),
            cancel_label: "close".into(),
            ..Default::default()
        }
    }

    fn fixed(menu: McpManagerMenu) -> MenuBuild {
        Arc::new(move || {
            let menu = menu.clone();
            Box::pin(async move { Ok(menu) })
        })
    }

    struct Surface {
        view: McpManagerView,
        handle: McpManagerHandle,
        renders: mpsc::UnboundedReceiver<()>,
        dimensions: Rc<Cell<(u16, u16)>>,
    }

    fn surface(keys: KeybindingsManager) -> Surface {
        let (render_tx, renders) = mpsc::unbounded_channel();
        let (handle, parts) = mcp_manager_channel(Arc::new(move || {
            render_tx.send(()).ok();
        }));
        let dimensions = Rc::new(Cell::new((120u16, 36u16)));
        let reader = dimensions.clone();
        let view = McpManagerView::new(
            parts,
            theme(),
            keys,
            Rc::new(move || reader.get()),
            Rc::new(|| {}),
            Rc::new(|_| {}),
        );
        Surface {
            view,
            handle,
            renders,
            dimensions,
        }
    }

    impl Surface {
        fn text(&mut self, width: usize) -> String {
            self.view
                .render(width)
                .iter()
                .map(|line| strip_terminal_sequences(line))
                .collect::<Vec<_>>()
                .join("\n")
        }
        /// Upstream `waitFor`: re-check on every render request the flow schedules.
        async fn wait_for(&mut self, width: usize, predicate: impl Fn(&str) -> bool) -> String {
            loop {
                let text = self.text(width);
                if predicate(&text) {
                    return text;
                }
                tokio::time::timeout(Duration::from_secs(5), self.renders.recv())
                    .await
                    .unwrap_or_else(|_| panic!("manager render not observed:\n{text}"))
                    .expect("render channel open");
            }
        }
        fn open(
            &self,
            build: MenuBuild,
            subscribe: Option<MenuSubscribe>,
        ) -> tokio::task::JoinHandle<Result<Option<String>, String>> {
            let handle = self.handle.clone();
            tokio::spawn(async move { handle.menu(build, subscribe).await })
        }
    }

    fn default_keys() -> KeybindingsManager {
        KeybindingsManager::new(tui_keybindings(), KeybindingsConfig::default())
    }

    #[tokio::test]
    async fn sanitizes_menu_text_and_preserves_raw_selected_values() {
        let _keys = KEYS.lock().await;
        for label in ["tool", ""] {
            let mut s = surface(default_keys());
            let name = format!("{label}{CONTROLS}");
            let pending = s.open(
                fixed(McpManagerMenu {
                    title: format!("MCP servers{CONTROLS}"),
                    details: Some(format!("first{CONTROLS}\nsecond")),
                    ..menu(vec![SelectItem {
                        value: name.clone(),
                        label: name.clone(),
                        description: Some(format!("safe description{CONTROLS}")),
                    }])
                }),
                None,
            );
            s.wait_for(120, |text| text.contains("safe description"))
                .await;
            let output = s.view.render(120).join("\n");
            s.view.handle_input("\r");
            assert!(!output.contains("MCP_INJECTION"));
            assert!(!output.contains("\x1b[2J"));
            let plain = strip_terminal_sequences(&output);
            let lines = plain.lines().collect::<Vec<_>>();
            let first = lines
                .iter()
                .position(|line| line.contains("first"))
                .expect("first details line");
            assert!(lines[first + 1].contains("second"));
            assert_eq!(pending.await.expect("join"), Ok(Some(name)));
        }
    }

    #[tokio::test]
    async fn removes_terminal_controls_from_status_and_empty_menu_text() {
        let _keys = KEYS.lock().await;
        let mut s = surface(default_keys());
        s.handle
            .status(&format!("status{CONTROLS}"), &format!("message{CONTROLS}"));
        let status = s.view.render(120).join("\n");
        let pending = s.open(
            fixed(McpManagerMenu {
                empty: Some(format!("empty{CONTROLS}")),
                ..menu(Vec::new())
            }),
            None,
        );
        s.wait_for(120, |text| text.contains("empty")).await;
        let empty = s.view.render(120).join("\n");
        s.view.handle_input("\x1b");
        assert_eq!(pending.await.expect("join"), Ok(None));
        for output in [status, empty] {
            assert!(!output.contains("MCP_INJECTION"));
            assert!(!output.contains("\x1b[2J"));
        }
    }

    struct Live(Arc<AtomicUsize>);
    impl Drop for Live {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn preserves_selected_identity_across_event_refreshes_and_releases_the_subscription() {
        let _keys = KEYS.lock().await;
        let mut s = surface(default_keys());
        let items = Arc::new(std::sync::Mutex::new(vec![item("a", "a"), item("b", "b")]));
        let source = items.clone();
        let build: MenuBuild = Arc::new(move || {
            let items = source
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            Box::pin(async move { Ok(menu(items)) })
        });
        let (refresh_tx, refresh_rx) = tokio::sync::broadcast::channel::<()>(4);
        let live = Arc::new(AtomicUsize::new(0));
        let counter = live.clone();
        let subscribe: MenuSubscribe = Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            let guard = Live(counter.clone());
            let mut events = refresh_rx.resubscribe();
            Box::pin(async move {
                let _guard = guard;
                if events.recv().await.is_err() {
                    std::future::pending::<()>().await;
                }
            })
        });
        let pending = s.open(build, Some(subscribe));
        s.wait_for(120, |text| text.contains("→ a")).await;
        s.view.handle_input("\x1b[B");
        *items
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            vec![item("z", "z"), item("a", "a"), item("b", "b connected")];
        refresh_tx.send(()).expect("refresh");
        s.wait_for(120, |text| text.contains("→ b connected")).await;
        s.view.handle_input("\r");
        assert_eq!(pending.await.expect("join"), Ok(Some("b".into())));
        assert_eq!(live.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn ignores_an_old_async_menu_after_a_newer_refresh_or_disposal() {
        let _keys = KEYS.lock().await;
        for outcome in ["resolve", "reject"] {
            let mut s = surface(default_keys());
            let (old_tx, old_rx) = oneshot::channel::<Result<McpManagerMenu, String>>();
            let old = Arc::new(std::sync::Mutex::new(Some(old_rx)));
            let calls = Arc::new(AtomicUsize::new(0));
            let build: MenuBuild = {
                let calls = calls.clone();
                Arc::new(move || {
                    let first = calls.fetch_add(1, Ordering::SeqCst) == 0;
                    let old = old
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .take();
                    Box::pin(async move {
                        match (first, old) {
                            (true, Some(old)) => {
                                old.await.unwrap_or_else(|_| Err("dropped".into()))
                            }
                            _ => Ok(menu(vec![item("new", "new")])),
                        }
                    })
                })
            };
            let (refresh_tx, refresh_rx) = tokio::sync::broadcast::channel::<()>(4);
            let subscribe: MenuSubscribe = Arc::new(move || {
                let mut events = refresh_rx.resubscribe();
                Box::pin(async move {
                    if events.recv().await.is_err() {
                        std::future::pending::<()>().await;
                    }
                })
            });
            let pending = s.open(build, Some(subscribe));
            while s.view.reply.is_none() {
                tokio::time::timeout(Duration::from_secs(5), s.renders.recv())
                    .await
                    .expect("menu open not observed")
                    .expect("render channel open");
                s.text(120);
            }
            refresh_tx.send(()).expect("refresh");
            s.wait_for(120, |text| text.contains("→ new")).await;
            // The superseded build was dropped, so its late result has nowhere to land.
            let late = if outcome == "resolve" {
                Ok(menu(vec![item("old", "old")]))
            } else {
                Err("superseded catalog failure".to_owned())
            };
            assert!(old_tx.send(late).is_err());
            s.view.handle_input("\r");
            assert_eq!(pending.await.expect("join"), Ok(Some("new".into())));
            let closing = s.open(
                Arc::new(|| Box::pin(std::future::pending::<Result<McpManagerMenu, String>>())),
                None,
            );
            while s.view.reply.is_none() {
                tokio::time::timeout(Duration::from_secs(5), s.renders.recv())
                    .await
                    .expect("menu open not observed")
                    .expect("render channel open");
                s.text(120);
            }
            s.view.dispose();
            assert_eq!(closing.await.expect("join"), Ok(None));
            let reopened = s.handle.menu(fixed(menu(vec![item("reopened", "reopened")])), None);
            assert_eq!(reopened.await, Ok(None));
        }
    }

    #[tokio::test]
    async fn scrolls_long_catalogs_fits_narrow_terminals_and_honors_configured_bindings() {
        let _keys = KEYS.lock().await;
        let previous = get_keybindings();
        let mut keys = default_keys();
        let mut config = KeybindingsConfig::default();
        config.insert(
            "tui.select.confirm".into(),
            Some(Keys::One("ctrl+y".into())),
        );
        config.insert("tui.select.cancel".into(), Some(Keys::One("ctrl+x".into())));
        keys.set_user_bindings(config);
        set_keybindings(keys.clone());
        let mut s = surface(keys);
        let items = (0..20)
            .map(|i| SelectItem {
                value: i.to_string(),
                label: format!("server-{i}-{}", "長".repeat(20)),
                description: Some("connected · 44 tools · search · global".into()),
            })
            .collect();
        let pending = s.open(fixed(menu(items)), None);
        s.wait_for(120, |text| text.contains("server-0")).await;
        for _ in 0..15 {
            s.view.handle_input("\x1b[B");
        }
        assert!(s.text(120).contains("→ server-15"));
        s.dimensions.set((28, 18));
        let narrow = s.wait_for(28, |text| text.contains("→ server-15")).await;
        assert!(narrow.lines().all(|line| visible_width(line) <= 28));
        // Default Enter must not confirm and default Esc must not cancel; navigation continues.
        s.view.handle_input("\r");
        s.view.handle_input("\x1b[B");
        assert!(s.text(28).contains("→ server-16"));
        s.view.handle_input("\x1b");
        s.view.handle_input("\x1b[B");
        assert!(s.text(28).contains("→ server-17"));
        s.view.handle_input("\x19");
        assert_eq!(pending.await.expect("join"), Ok(Some("17".into())));
        let live = Arc::new(AtomicUsize::new(0));
        let counter = live.clone();
        let subscribe: MenuSubscribe = Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            let guard = Live(counter.clone());
            Box::pin(async move {
                let _guard = guard;
                std::future::pending::<()>().await;
            })
        });
        let empty = s.open(fixed(menu(Vec::new())), Some(subscribe));
        s.wait_for(28, |text| text.contains("Nothing to show.")).await;
        s.view.handle_input("\x1b");
        assert_eq!(live.load(Ordering::SeqCst), 1);
        s.view.handle_input("\x18");
        assert_eq!(empty.await.expect("join"), Ok(None));
        assert_eq!(live.load(Ordering::SeqCst), 0);
        set_keybindings((*previous).clone());
    }
}
