use crate::{
    body::create_panel_body,
    constants::{MIN_TRANSCRIPT_COLUMNS, SIDE_PANEL_ANCHOR_WIDGET_KEY, SIDE_PANEL_WIDGET_KEY},
    links::parse_panel_action_url,
    types::*,
};
use std::sync::{Arc, Mutex};
pub type Defer = Arc<dyn Fn(Box<dyn FnOnce() + Send>) + Send + Sync>;
pub struct PanelHostSurfaceDeps {
    pub context: PanelHostContext,
    pub source: RowSource,
    pub width: Arc<dyn Fn(usize) -> usize + Send + Sync>,
    pub min_columns: usize,
    pub clickable: bool,
    pub on_action: Option<ActionHook>,
    pub defer: Defer,
    pub on_regular_mode_fallback: Option<RegularModeFallback>,
}
struct InstalledColumn {
    tui: Arc<dyn PanelHostTui>,
    original: Component,
    root: Component,
}
#[derive(Default)]
struct SurfaceState {
    kind: PanelSurfaceKind,
    installed: Option<InstalledColumn>,
    widget_mounted: bool,
    fallback_notified: bool,
    renderer: Option<Arc<dyn PanelHostTui>>,
    generation: u64,
    url_hook: Option<(Arc<dyn PanelHostTui>, UrlHook)>,
    widget_host: Option<Arc<dyn PanelHostTui>>,
}
pub struct PanelHostSurface {
    deps: PanelHostSurfaceDeps,
    state: Mutex<SurfaceState>,
    theme: Arc<Mutex<Option<Theme>>>,
    body: Component,
}
pub fn create_panel_host_surface(deps: PanelHostSurfaceDeps) -> Arc<PanelHostSurface> {
    let theme: Arc<Mutex<Option<Theme>>> = Arc::default();
    let read_theme = theme.clone();
    let clickable = deps.clickable && deps.on_action.is_some();
    let body = Arc::new(create_panel_body(
        deps.source.clone(),
        Arc::new(move || {
            read_theme
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
        }),
        Arc::new(move || clickable),
    ));
    Arc::new(PanelHostSurface {
        deps,
        state: Mutex::default(),
        theme,
        body,
    })
}
impl PanelHostSurface {
    pub fn mount(self: &Arc<Self>) -> PanelSurfaceKind {
        if !self.deps.context.has_ui || self.deps.context.mode.as_deref() != Some("tui") {
            return PanelSurfaceKind::Dark;
        }
        if self.deps.context.ui.mount_native_column(
            self.body.clone(),
            self.deps.width.clone(),
            self.deps.min_columns,
            self.deps.on_action.clone().filter(|_| self.deps.clickable),
        ) {
            *self
                .theme
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                self.deps.context.ui.panel_theme();
            self.state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .kind = PanelSurfaceKind::Column;
            return PanelSurfaceKind::Column;
        }
        self.mount_anchor();
        self.kind()
    }
    pub fn kind(&self) -> PanelSurfaceKind {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .kind
    }
    fn mount_anchor(self: &Arc<Self>) {
        let generation = {
            let mut s = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            s.generation = s.generation.wrapping_add(1);
            s.generation
        };
        let weak = Arc::downgrade(self);
        self.deps.context.ui.set_widget(
            SIDE_PANEL_ANCHOR_WIDGET_KEY,
            Some(Arc::new(move |tui, theme| {
                if let Some(surface) = weak.upgrade() {
                    *surface
                        .theme
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = theme;
                    let deferred = Arc::downgrade(&surface);
                    (surface.deps.defer)(Box::new(move || {
                        if let Some(surface) = deferred.upgrade() {
                            surface.attach(tui, generation);
                        }
                    }));
                }
                Arc::new(Anchor)
            })),
            false,
        );
    }
    fn attach(&self, tui: Arc<dyn PanelHostTui>, generation: u64) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if generation != state.generation || state.installed.is_some() {
            return;
        }
        if self.deps.clickable
            && state.url_hook.is_none()
            && let (Some(action), Some(current)) = (&self.deps.on_action, tui.open_url())
        {
            let previous = match tui.parked_url_hook() {
                Some(parked) => parked,
                None => {
                    tui.park_url_hook(current.clone());
                    current
                }
            };
            state.url_hook = Some((tui.clone(), previous.clone()));
            let action = action.clone();
            tui.set_open_url(Arc::new(move |url| match parse_panel_action_url(url) {
                Some(parsed) => action(parsed),
                None => previous(url),
            }));
        }
        let original = if tui.viewport() {
            state.renderer = Some(tui.clone());
            tui.layout_root()
        } else {
            None
        };
        if let Some(original) = original {
            let root: Component = Arc::new(ColumnRoot {
                original: original.clone(),
                body: self.body.clone(),
                tui: tui.clone(),
                width: self.deps.width.clone(),
                min_columns: self.deps.min_columns,
            });
            tui.set_layout_root(Some(root.clone()));
            state.installed = Some(InstalledColumn {
                tui: tui.clone(),
                original,
                root,
            });
            state.kind = PanelSurfaceKind::Column;
            drop(state);
            tui.request_render(true);
        } else {
            let notify = !state.fallback_notified;
            state.fallback_notified = true;
            state.widget_mounted = true;
            state.widget_host = Some(tui);
            state.kind = PanelSurfaceKind::Widget;
            drop(state);
            if notify && let Some(callback) = &self.deps.on_regular_mode_fallback {
                callback();
            }
            let body = self.body.clone();
            self.deps.context.ui.set_widget(
                SIDE_PANEL_WIDGET_KEY,
                Some(Arc::new(move |_, _| body.clone())),
                true,
            );
        }
    }
    pub fn request_render(self: &Arc<Self>) {
        if let Some(theme) = self.deps.context.ui.panel_theme() {
            *self
                .theme
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(theme);
        }
        self.deps.context.ui.repaint_native_column();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let rebuilt = state.installed.as_ref().is_some_and(|installed| {
            !installed
                .tui
                .layout_root()
                .is_some_and(|root| Arc::ptr_eq(&root, &installed.root))
        });
        let upgrade = state.kind == PanelSurfaceKind::Widget
            && state
                .widget_host
                .as_ref()
                .is_some_and(|tui| tui.viewport() && tui.layout_root().is_some());
        if rebuilt || upgrade {
            state.installed = None;
            state.url_hook = None;
            state.renderer = None;
            state.kind = PanelSurfaceKind::Dark;
            if upgrade {
                state.widget_mounted = false;
                state.widget_host = None;
            }
            drop(state);
            if upgrade {
                self.deps
                    .context
                    .ui
                    .set_widget(SIDE_PANEL_WIDGET_KEY, None, false);
            }
            self.mount_anchor();
            return;
        }
        let renderer = state.renderer.clone();
        drop(state);
        if let Some(renderer) = renderer {
            renderer.request_render(false);
        }
    }
    pub fn dispose(&self) {
        self.deps.context.ui.clear_native_column();
        let (hook, installed, widget) = {
            let mut s = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            s.generation = s.generation.wrapping_add(1);
            let hook = s.url_hook.take();
            let installed = s.installed.take();
            let widget = s.widget_mounted;
            s.widget_mounted = false;
            s.widget_host = None;
            s.renderer = None;
            s.kind = PanelSurfaceKind::Dark;
            (hook, installed, widget)
        };
        if let Some((host, previous)) = hook {
            host.set_open_url(previous);
        }
        if let Some(installed) = installed
            && installed
                .tui
                .layout_root()
                .is_some_and(|root| Arc::ptr_eq(&root, &installed.root))
        {
            installed.tui.set_layout_root(Some(installed.original));
            installed.tui.request_render(true);
        }
        if widget {
            self.deps
                .context
                .ui
                .set_widget(SIDE_PANEL_WIDGET_KEY, None, false);
        }
        self.deps
            .context
            .ui
            .set_widget(SIDE_PANEL_ANCHOR_WIDGET_KEY, None, false);
    }
}
struct Anchor;
impl PanelComponent for Anchor {
    fn render(&self, _: usize) -> Vec<String> {
        Vec::new()
    }
}
pub struct ColumnRoot {
    original: Component,
    body: Component,
    tui: Arc<dyn PanelHostTui>,
    width: Arc<dyn Fn(usize) -> usize + Send + Sync>,
    min_columns: usize,
}
impl ColumnRoot {
    pub fn children(&self) -> [Component; 2] {
        [self.original.clone(), self.body.clone()]
    }
    pub fn entries(&self, viewport_width: usize) -> Vec<PanelStackEntry> {
        vec![
            PanelStackEntry {
                component: self.original.clone(),
                basis: 0,
                grow: 1,
                shrink: 1,
                min_size: MIN_TRANSCRIPT_COLUMNS,
                visible: true,
            },
            PanelStackEntry {
                component: self.body.clone(),
                basis: (self.width)(self.tui.columns()),
                grow: 0,
                shrink: 0,
                min_size: 0,
                visible: viewport_width >= self.min_columns,
            },
        ]
    }
}
impl PanelComponent for ColumnRoot {
    fn render(&self, width: usize) -> Vec<String> {
        self.original.render(width)
    }
    fn invalidate(&self) {
        self.original.invalidate();
        self.body.invalidate();
    }
    fn layout_entries(&self, width: usize) -> Option<Vec<PanelStackEntry>> {
        Some(self.entries(width))
    }
}
#[cfg(test)]
mod tests;
