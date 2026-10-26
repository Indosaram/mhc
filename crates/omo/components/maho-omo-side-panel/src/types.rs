use std::{future::Future, pin::Pin, sync::Arc};
use crate::links::PanelAction;
pub type PanelFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PanelRow { pub text: String, pub color: Option<PanelColor>, pub action: Option<PanelAction> }
impl PanelRow { pub fn new(text: impl Into<String>) -> Self { Self { text: text.into(), ..Self::default() } } pub fn colored(text: impl Into<String>, color: PanelColor) -> Self { Self { text: text.into(), color: Some(color), action: None } } }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelColor { Text, Muted, Dim, Accent, Warning, Error, Success }
impl PanelColor { pub fn as_str(self) -> &'static str { match self { Self::Text => "text", Self::Muted => "muted", Self::Dim => "dim", Self::Accent => "accent", Self::Warning => "warning", Self::Error => "error", Self::Success => "success" } } }
pub trait PanelComponent: Send + Sync { fn render(&self, width: usize) -> Vec<String>; fn invalidate(&self) {} fn layout_entries(&self, _width: usize) -> Option<Vec<PanelStackEntry>> { None } }
pub type Component = Arc<dyn PanelComponent>;
pub type Theme = Arc<dyn Fn(PanelColor, &str) -> String + Send + Sync>;
pub type RowSource = Arc<dyn Fn(usize) -> Vec<PanelRow> + Send + Sync>;
pub type UrlHook = Arc<dyn Fn(&str) + Send + Sync>;
pub type ActionHook = Arc<dyn Fn(PanelAction) + Send + Sync>;
pub type RegularModeFallback = Arc<dyn Fn() + Send + Sync>;
pub type Stop = Box<dyn FnOnce() + Send>;
pub type TerminalHandler = Arc<dyn Fn(&str) -> bool + Send + Sync>;
pub type WidgetFactory = Arc<dyn Fn(Arc<dyn PanelHostTui>, Option<Theme>) -> Component + Send + Sync>;
#[derive(Clone)] pub struct PanelStackEntry { pub component: Component, pub basis: usize, pub grow: usize, pub shrink: usize, pub min_size: usize, pub visible: bool }
pub trait PanelHostTui: Send + Sync {
    fn viewport(&self) -> bool;
    fn layout_root(&self) -> Option<Component>;
    fn set_layout_root(&self, root: Option<Component>);
    fn request_render(&self, force: bool);
    fn columns(&self) -> usize;
    fn open_url(&self) -> Option<UrlHook>;
    fn set_open_url(&self, hook: UrlHook);
    fn parked_url_hook(&self) -> Option<UrlHook>;
    fn park_url_hook(&self, hook: UrlHook);
}
pub trait PanelUi: Send + Sync {
    fn panel_theme(&self) -> Option<Theme> { None }
    fn mount_native_column(&self, _body: Component, _width: Arc<dyn Fn(usize)->usize + Send + Sync>, _min_columns: usize, _action: Option<ActionHook>) -> bool { false }
    fn clear_native_column(&self) {}
    fn repaint_native_column(&self) {}
    fn set_widget(&self, key: &str, factory: Option<WidgetFactory>, above_editor: bool);
    fn notify(&self, message: &str, kind: &str);
    fn select<'a>(&'a self, title: &'a str, options: Vec<String>) -> PanelFuture<'a, Option<String>>;
    fn viewer<'a>(&'a self, _title: &'a str, rows: Vec<PanelRow>) -> PanelFuture<'a, ()> {
        Box::pin(async move { self.notify(&rows.iter().map(|r| crate::format::sanitize_terminal_text(&r.text)).collect::<Vec<_>>().join("\n"), "info"); })
    }
    fn on_terminal_input(&self, handler: TerminalHandler) -> Option<Stop>;
}
#[derive(Clone)] pub struct PanelHostContext { pub ui: Arc<dyn PanelUi>, pub mode: Option<String>, pub has_ui: bool }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)] pub enum PanelSurfaceKind { Column, Widget, #[default] Dark }
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PanelGoal { pub objective: String, pub status: PanelGoalStatus, pub tokens_used: f64, pub time_used_seconds: f64, #[serde(default)] pub consecutive_continuations: u64, #[serde(default)] pub unattended_continuations: u64, pub token_budget: Option<f64> }
#[derive(Clone, Copy, Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")] pub enum PanelGoalStatus { Active, Paused, Blocked, BudgetLimited, Complete }
#[derive(Clone, Debug, Default)] pub struct PanelMemory { pub identity: String, pub rows: Vec<PanelRow>, pub detail: Vec<PanelRow> }
#[derive(Clone, Debug, Default)] pub struct PanelGitEntry { pub xy: String, pub path: String, pub from: Option<String>, pub added: Option<u64>, pub removed: Option<u64> }
#[derive(Clone, Debug, Default)] pub struct PanelGitStatus { pub root: String, pub files: Vec<PanelGitEntry> }
