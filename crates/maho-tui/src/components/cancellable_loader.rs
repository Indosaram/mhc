//! Port of senpi `packages/tui/src/components/cancellable-loader.ts`.
//!
//! senpi's `AbortController`/`AbortSignal` becomes a plain `aborted` flag plus an `on_abort`
//! callback: nothing in this port consumes the JS `AbortSignal` object identity or its
//! `addEventListener` API, only whether it fired and the abort callback.

use super::loader::{Loader, LoaderIndicatorOptions};
use crate::keybindings::get_keybindings;
use crate::tui::Component;

pub struct CancellableLoader {
    loader: Loader,
    aborted: bool,
    pub on_abort: Option<Box<dyn FnMut()>>,
}

impl CancellableLoader {
    pub fn new(
        spinner_color_fn: std::rc::Rc<dyn Fn(&str) -> String>,
        message_color_fn: std::rc::Rc<dyn Fn(&str) -> String>,
        message: impl Into<String>,
        indicator: Option<LoaderIndicatorOptions>,
        now_ms: u64,
    ) -> Self {
        Self {
            loader: Loader::new(spinner_color_fn, message_color_fn, message, indicator, now_ms),
            aborted: false,
            on_abort: None,
        }
    }

    pub fn aborted(&self) -> bool {
        self.aborted
    }

    pub fn start(&mut self, now_ms: u64) {
        self.loader.start(now_ms);
    }

    pub fn stop(&mut self) {
        self.loader.stop();
    }

    pub fn set_message(&mut self, message: impl Into<String>, now_ms: u64) {
        self.loader.set_message(message, now_ms);
    }

    pub fn tick(&mut self, now_ms: u64) -> bool {
        self.loader.tick(now_ms)
    }
}

impl Component for CancellableLoader {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.loader.render(width)
    }

    fn handle_input(&mut self, data: &str) {
        if get_keybindings().matches(data, "tui.select.cancel") {
            self.aborted = true;
            if let Some(on_abort) = &mut self.on_abort {
                on_abort();
            }
        }
    }

    fn has_input_handler(&self) -> bool {
        true
    }

    fn invalidate(&mut self) {
        self.loader.invalidate();
    }

    fn dispose(&mut self) {
        self.stop();
    }
}
