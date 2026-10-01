//! Port of components/bordered-loader.ts.
use std::rc::Rc;
use maho_tui::{components::{cancellable_loader::CancellableLoader, loader::Loader, text::Text}, tui::Component};
use crate::theme::{Theme, ThemeColor};
use super::dynamic_border::DynamicBorder;

enum LoaderKind { Cancellable(CancellableLoader), Plain(Loader) }
pub struct BorderedLoader {
    loader: LoaderKind,
    border: DynamicBorder,
    hint: Text,
}
impl BorderedLoader {
    pub fn new(theme: Theme, message: &str, cancellable: bool, cancel_hint: &str, now_ms: u64) -> Self {
        let spinner_theme = theme.clone();
        let message_theme = theme.clone();
        let spinner = Rc::new(move |text: &str| spinner_theme.fg(ThemeColor::Accent, text));
        let color = Rc::new(move |text: &str| message_theme.fg(ThemeColor::Muted, text));
        let loader = if cancellable {
            LoaderKind::Cancellable(CancellableLoader::new(spinner, color, message, None, now_ms))
        } else {
            LoaderKind::Plain(Loader::new(spinner, color, message, None, now_ms))
        };
        Self { loader, border: DynamicBorder::new(theme), hint: Text::with_padding(cancel_hint, 1, 0) }
    }
    pub fn aborted(&self) -> bool {
        match &self.loader {
            LoaderKind::Cancellable(loader) => loader.aborted(),
            LoaderKind::Plain(_) => false,
        }
    }
    pub fn set_on_abort(&mut self, callback: Option<Box<dyn FnMut()>>) {
        if let LoaderKind::Cancellable(loader) = &mut self.loader { loader.on_abort = callback; }
    }
    pub fn tick(&mut self, now_ms: u64) -> bool {
        match &mut self.loader {
            LoaderKind::Cancellable(loader) => loader.tick(now_ms),
            LoaderKind::Plain(loader) => loader.tick(now_ms),
        }
    }
}
impl Component for BorderedLoader {
    fn render(&mut self, width: usize) -> Vec<String> {
        let mut lines = self.border.render(width);
        match &mut self.loader {
            LoaderKind::Cancellable(loader) => {
                lines.extend(loader.render(width));
                lines.push(String::new());
                lines.extend(self.hint.render(width));
            }
            LoaderKind::Plain(loader) => lines.extend(loader.render(width)),
        }
        lines.push(String::new());
        lines.extend(self.border.render(width));
        lines
    }
    fn handle_input(&mut self, data: &str) {
        if let LoaderKind::Cancellable(loader) = &mut self.loader { loader.handle_input(data); }
    }
    fn has_input_handler(&self) -> bool { true }
    fn invalidate(&mut self) {
        match &mut self.loader {
            LoaderKind::Cancellable(loader) => loader.invalidate(),
            LoaderKind::Plain(loader) => loader.invalidate(),
        }
        self.hint.invalidate();
    }
    fn dispose(&mut self) {
        match &mut self.loader {
            LoaderKind::Cancellable(loader) => loader.dispose(),
            LoaderKind::Plain(loader) => loader.dispose(),
        }
    }
}
