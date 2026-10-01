//! Port of senpi `packages/tui/src/components/loader.ts`.
//!
//! senpi drives the spinner/message animation with `setInterval` timers that call
//! `ui.requestRender()` and a wall clock (`Date.now()`). This crate has no event loop, so the
//! animation is polled instead: [`Loader::tick`] takes the current time, checks the
//! indicator/message intervals against `next_indicator_due_ms`/`next_message_due_ms`, and
//! returns whether a render is needed exactly when senpi's timer would have fired. The host's
//! render loop calls `tick` once per poll iteration and requests a render when it returns
//! `true` (mirroring `terminal.rs`'s `run_due_timers` pattern from todo 6).

use super::text::Text;
use crate::tui::Component;

pub type LoaderMessageFormatter = std::rc::Rc<dyn Fn(&str, u64) -> String>;
pub type LoaderIndicatorFormatter = std::rc::Rc<dyn Fn(&str, u64) -> String>;

#[derive(Clone, Default)]
pub struct LoaderIndicatorOptions {
    /// Animation frames. Empty hides the indicator.
    pub frames: Option<Vec<String>>,
    pub interval_ms: Option<u64>,
    pub indicator_formatter: Option<LoaderIndicatorFormatter>,
    pub message_formatter: Option<LoaderMessageFormatter>,
    pub message_interval_ms: Option<u64>,
}

const DEFAULT_INTERVAL_MS: u64 = 80;

fn default_frames() -> Vec<String> {
    ["\u{280b}", "\u{2819}", "\u{2839}", "\u{2838}", "\u{283c}", "\u{2834}", "\u{2826}", "\u{2827}", "\u{2807}", "\u{280f}"]
        .into_iter()
        .map(str::to_string)
        .collect()
}

/// Loader component that updates with an optional spinning animation (senpi's `Loader` extends
/// `Text`; this port holds a `Text` field instead, since Rust has no class inheritance).
pub struct Loader {
    text: Text,
    frames: Vec<String>,
    interval_ms: u64,
    current_frame: usize,
    indicator_formatter: Option<LoaderIndicatorFormatter>,
    message_formatter: Option<LoaderMessageFormatter>,
    message_interval_ms: u64,
    message_animation_started_at_ms: u64,
    render_indicator_verbatim: bool,
    spinner_color_fn: std::rc::Rc<dyn Fn(&str) -> String>,
    message_color_fn: std::rc::Rc<dyn Fn(&str) -> String>,
    message: String,
    last_displayed_text: Option<String>,
    next_indicator_due_ms: Option<u64>,
    next_message_due_ms: Option<u64>,
    running: bool,
}

impl Loader {
    pub fn new(
        spinner_color_fn: std::rc::Rc<dyn Fn(&str) -> String>,
        message_color_fn: std::rc::Rc<dyn Fn(&str) -> String>,
        message: impl Into<String>,
        indicator: Option<LoaderIndicatorOptions>,
        now_ms: u64,
    ) -> Self {
        let mut loader = Self {
            text: Text::with_padding("", 1, 0),
            frames: default_frames(),
            interval_ms: DEFAULT_INTERVAL_MS,
            current_frame: 0,
            indicator_formatter: None,
            message_formatter: None,
            message_interval_ms: DEFAULT_INTERVAL_MS,
            message_animation_started_at_ms: now_ms,
            render_indicator_verbatim: false,
            spinner_color_fn,
            message_color_fn,
            message: message.into(),
            last_displayed_text: None,
            next_indicator_due_ms: None,
            next_message_due_ms: None,
            running: false,
        };
        loader.set_indicator(indicator, now_ms);
        loader
    }

    pub fn start(&mut self, now_ms: u64) {
        if self.message_formatter.is_some() {
            self.message_animation_started_at_ms = now_ms;
        }
        self.update_display(now_ms);
        self.restart_animation(now_ms);
    }

    pub fn stop(&mut self) {
        self.next_indicator_due_ms = None;
        self.next_message_due_ms = None;
        self.running = false;
    }

    pub fn set_message(&mut self, message: impl Into<String>, now_ms: u64) {
        self.message = message.into();
        self.update_display(now_ms);
    }

    pub fn set_indicator(&mut self, indicator: Option<LoaderIndicatorOptions>, now_ms: u64) {
        self.render_indicator_verbatim = indicator.is_some();
        let indicator = indicator.unwrap_or_default();
        self.frames = indicator.frames.unwrap_or_else(default_frames);
        self.interval_ms = indicator.interval_ms.filter(|&ms| ms > 0).unwrap_or(DEFAULT_INTERVAL_MS);
        self.indicator_formatter = indicator.indicator_formatter;
        self.message_formatter = indicator.message_formatter;
        self.message_interval_ms = indicator
            .message_interval_ms
            .filter(|&ms| ms > 0)
            .unwrap_or(self.interval_ms);
        self.current_frame = 0;
        self.message_animation_started_at_ms = now_ms;
        self.start(now_ms);
    }

    fn restart_animation(&mut self, now_ms: u64) {
        self.stop();
        self.running = true;
        if self.frames.len() > 1 {
            self.next_indicator_due_ms = Some(now_ms + self.interval_ms);
        }
        if self.indicator_formatter.is_some() || self.message_formatter.is_some() {
            self.message_animation_started_at_ms = now_ms;
            self.next_message_due_ms = Some(now_ms + self.message_interval_ms);
        }
    }

    fn get_rendered_indicator(&self, animation_elapsed_ms: u64) -> String {
        let Some(frame) = self.frames.get(self.current_frame) else {
            return String::new();
        };
        if frame.is_empty() {
            return String::new();
        }
        if let Some(formatter) = &self.indicator_formatter {
            return formatter(frame, animation_elapsed_ms);
        }
        if self.render_indicator_verbatim {
            frame.clone()
        } else {
            (self.spinner_color_fn)(frame)
        }
    }

    fn animation_elapsed_ms(&self, now_ms: u64) -> u64 {
        now_ms.saturating_sub(self.message_animation_started_at_ms)
    }

    fn update_display(&mut self, now_ms: u64) {
        let animation_elapsed_ms = self.animation_elapsed_ms(now_ms);
        let rendered_frame = self.get_rendered_indicator(animation_elapsed_ms);
        let indicator = if rendered_frame.is_empty() {
            String::new()
        } else {
            format!("{rendered_frame} ")
        };
        let rendered_message = match &self.message_formatter {
            Some(formatter) => formatter(&self.message, animation_elapsed_ms),
            None => (self.message_color_fn)(&self.message),
        };
        let displayed_text = format!("{indicator}{rendered_message}");
        if self.last_displayed_text.as_deref() == Some(displayed_text.as_str()) {
            return;
        }
        self.last_displayed_text = Some(displayed_text.clone());
        self.text.set_text(displayed_text);
    }

    /// Fire any due indicator/message timers and return whether a render was requested
    /// (senpi's `ui.requestRender()` calls inside the `setInterval` callbacks).
    pub fn tick(&mut self, now_ms: u64) -> bool {
        if !self.running {
            return false;
        }
        let mut requested = false;
        if self.next_indicator_due_ms.is_some_and(|due| due <= now_ms) {
            self.current_frame = (self.current_frame + 1) % self.frames.len().max(1);
            self.update_display(now_ms);
            self.next_indicator_due_ms = Some(now_ms + self.interval_ms);
            requested = true;
        }
        if self.next_message_due_ms.is_some_and(|due| due <= now_ms) {
            self.update_display(now_ms);
            self.next_message_due_ms = Some(now_ms + self.message_interval_ms);
            requested = true;
        }
        requested
    }
}

impl Component for Loader {
    fn invalidate(&mut self) {
        self.text.invalidate();
    }

    fn dispose(&mut self) {
        self.stop();
    }

    fn render(&mut self, width: usize) -> Vec<String> {
        let mut lines = vec![String::new()];
        lines.extend(self.text.render(width));
        lines
    }
}
