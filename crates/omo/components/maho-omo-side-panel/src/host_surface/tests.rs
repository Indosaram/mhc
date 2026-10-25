use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
#[derive(Default)]
struct Ui {
    widgets: Mutex<std::collections::BTreeMap<String, WidgetFactory>>,
}
impl PanelUi for Ui {
    fn set_widget(&self, key: &str, factory: Option<WidgetFactory>, _: bool) {
        let mut widgets = self.widgets.lock().expect("widgets");
        if let Some(factory) = factory {
            widgets.insert(key.into(), factory);
        } else {
            widgets.remove(key);
        }
    }
    fn notify(&self, _: &str, _: &str) {}
    fn select<'a>(&'a self, _: &'a str, _: Vec<String>) -> PanelFuture<'a, Option<String>> {
        Box::pin(async { None })
    }
    fn viewer<'a>(&'a self, _: &'a str, _: Vec<PanelRow>) -> PanelFuture<'a, ()> {
        Box::pin(async {})
    }
    fn on_terminal_input(&self, _: TerminalHandler) -> Option<Stop> {
        None
    }
}
struct Transcript;
impl PanelComponent for Transcript {
    fn render(&self, width: usize) -> Vec<String> {
        vec![format!("transcript:{width}")]
    }
}
struct Tui {
    root: Mutex<Option<Component>>,
    columns: AtomicUsize,
    viewport: bool,
    hook: Mutex<Option<UrlHook>>,
    parked: Mutex<Option<UrlHook>>,
}
impl Tui {
    fn new(viewport: bool, root: bool) -> Arc<Self> {
        Arc::new(Self {
            root: Mutex::new(root.then(|| Arc::new(Transcript) as Component)),
            columns: AtomicUsize::new(200),
            viewport,
            hook: Mutex::new(Some(Arc::new(|_| {}))),
            parked: Mutex::default(),
        })
    }
}
impl PanelHostTui for Tui {
    fn viewport(&self) -> bool {
        self.viewport
    }
    fn layout_root(&self) -> Option<Component> {
        self.root.lock().expect("root").clone()
    }
    fn set_layout_root(&self, root: Option<Component>) {
        *self.root.lock().expect("root") = root;
    }
    fn request_render(&self, _: bool) {}
    fn columns(&self) -> usize {
        self.columns.load(Ordering::SeqCst)
    }
    fn open_url(&self) -> Option<UrlHook> {
        self.hook.lock().expect("hook").clone()
    }
    fn set_open_url(&self, hook: UrlHook) {
        *self.hook.lock().expect("hook") = Some(hook);
    }
    fn parked_url_hook(&self) -> Option<UrlHook> {
        self.parked.lock().expect("parked").clone()
    }
    fn park_url_hook(&self, hook: UrlHook) {
        *self.parked.lock().expect("parked") = Some(hook);
    }
}
fn surface(
    ui: Arc<Ui>,
    has_ui: bool,
    mode: &str,
    clickable: bool,
    action: Option<ActionHook>,
) -> Arc<PanelHostSurface> {
    create_panel_host_surface(PanelHostSurfaceDeps {
        context: PanelHostContext {
            ui,
            has_ui,
            mode: Some(mode.into()),
        },
        source: Arc::new(|_| vec![PanelRow::new("row")]),
        width: Arc::new(|w| w / 4),
        min_columns: 120,
        clickable,
        on_action: action,
        on_regular_mode_fallback: None,
        defer: Arc::new(|f| f()),
    })
}
fn attach(ui: &Ui, tui: Arc<Tui>) {
    let factory = ui
        .widgets
        .lock()
        .expect("widgets")
        .get(SIDE_PANEL_ANCHOR_WIDGET_KEY)
        .expect("anchor")
        .clone();
    factory(tui, None);
}
#[test]
fn noninteractive_mount_is_dark() {
    for (has_ui, mode) in [(false, "tui"), (true, "print")] {
        let ui = Arc::new(Ui::default());
        let s = surface(ui.clone(), has_ui, mode, false, None);
        assert_eq!(s.mount(), PanelSurfaceKind::Dark);
        assert!(ui.widgets.lock().expect("widgets").is_empty());
    }
}
#[test]
fn column_preserves_transcript_and_resolves_resize() {
    let ui = Arc::new(Ui::default());
    let tui = Tui::new(true, true);
    let original = tui.layout_root().expect("root");
    let s = surface(ui.clone(), true, "tui", false, None);
    s.mount();
    attach(&ui, tui.clone());
    assert_eq!(s.kind(), PanelSurfaceKind::Column);
    let root = tui.layout_root().expect("root");
    assert_eq!(root.render(120), ["transcript:120"]);
    let entries = root.layout_entries(119).expect("stack");
    assert!(Arc::ptr_eq(&entries[0].component, &original));
    assert_eq!(entries[0].min_size, 60);
    assert_eq!(entries[1].basis, 50);
    assert!(!entries[1].visible);
    tui.columns.store(160, Ordering::SeqCst);
    let entries = root.layout_entries(120).expect("stack");
    assert_eq!(entries[1].basis, 40);
    assert!(entries[1].visible);
    s.dispose();
    assert!(Arc::ptr_eq(&tui.layout_root().expect("root"), &original));
    assert!(ui.widgets.lock().expect("widgets").is_empty());
    s.dispose();
}
#[test]
fn fallback_and_foreign_root_ownership() {
    for (viewport, root) in [(false, true), (true, false)] {
        let ui = Arc::new(Ui::default());
        let tui = Tui::new(viewport, root);
        let s = surface(ui.clone(), true, "tui", false, None);
        s.mount();
        attach(&ui, tui);
        assert_eq!(s.kind(), PanelSurfaceKind::Widget);
        assert!(
            ui.widgets
                .lock()
                .expect("widgets")
                .contains_key(SIDE_PANEL_WIDGET_KEY)
        );
        s.dispose();
    }
    let ui = Arc::new(Ui::default());
    let tui = Tui::new(true, true);
    let s = surface(ui.clone(), true, "tui", false, None);
    s.mount();
    attach(&ui, tui.clone());
    let foreign: Component = Arc::new(Transcript);
    tui.set_layout_root(Some(foreign.clone()));
    s.dispose();
    assert!(Arc::ptr_eq(&tui.layout_root().expect("root"), &foreign));
}
#[test]
fn urls_dispatch_and_restore_without_wrapping() {
    let ui = Arc::new(Ui::default());
    let tui = Tui::new(true, true);
    let host_urls = Arc::new(Mutex::new(Vec::new()));
    let urls = host_urls.clone();
    let original: UrlHook = Arc::new(move |url| urls.lock().expect("urls").push(url.to_owned()));
    tui.set_open_url(original.clone());
    let actions = Arc::new(Mutex::new(Vec::new()));
    for _ in 0..2 {
        let sink = actions.clone();
        let s = surface(
            ui.clone(),
            true,
            "tui",
            true,
            Some(Arc::new(move |a| sink.lock().expect("actions").push(a))),
        );
        s.mount();
        attach(&ui, tui.clone());
        let hook = tui.open_url().expect("hook");
        hook("omo-panel:goal/current");
        hook("https://example.com/docs");
        s.dispose();
        assert!(Arc::ptr_eq(&tui.open_url().expect("hook"), &original));
    }
    assert_eq!(actions.lock().expect("actions").len(), 2);
    assert_eq!(host_urls.lock().expect("urls").len(), 2);
}
#[test]
fn stale_anchor_and_mode_rebuild_are_handled() {
    let ui = Arc::new(Ui::default());
    let tui = Tui::new(true, true);
    let s = surface(ui.clone(), true, "tui", false, None);
    s.mount();
    let stale = ui
        .widgets
        .lock()
        .expect("widgets")
        .get(SIDE_PANEL_ANCHOR_WIDGET_KEY)
        .expect("anchor")
        .clone();
    s.dispose();
    stale(tui.clone(), None);
    assert_eq!(s.kind(), PanelSurfaceKind::Dark);
    s.mount();
    attach(&ui, tui.clone());
    tui.set_layout_root(Some(Arc::new(Transcript)));
    s.request_render();
    assert_eq!(s.kind(), PanelSurfaceKind::Dark);
    attach(&ui, tui);
    assert_eq!(s.kind(), PanelSurfaceKind::Column);
}

#[test]
fn regular_fallback_notifies_once_per_surface_even_after_remount() {
    let ui = Arc::new(Ui::default());
    let calls = Arc::new(AtomicUsize::new(0));
    let sink = calls.clone();
    let surface = create_panel_host_surface(PanelHostSurfaceDeps {
        context: PanelHostContext {
            ui: ui.clone(),
            has_ui: true,
            mode: Some("tui".into()),
        },
        source: Arc::new(|_| vec![]),
        width: Arc::new(|_| 40),
        min_columns: 120,
        clickable: false,
        on_action: None,
        defer: Arc::new(|f| f()),
        on_regular_mode_fallback: Some(Arc::new(move || {
            sink.fetch_add(1, Ordering::SeqCst);
        })),
    });
    surface.mount();
    attach(&ui, Tui::new(false, true));
    attach(&ui, Tui::new(false, true));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    surface.dispose();
    surface.mount();
    attach(&ui, Tui::new(false, true));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
