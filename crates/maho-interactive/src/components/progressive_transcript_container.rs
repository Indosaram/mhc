//! Port of `components/progressive-transcript-container.ts`.
//!
//! senpi warms the deferred head from `setImmediate` macrotasks. A native renderer has no such
//! scheduler and must not widen the frame it is about to paint, so `render` returns exactly the
//! range senpi returns and the host drives hydration by calling [`ProgressiveTranscriptContainer::warm_next_chunk`]
//! on its own timer — the same split the spinner and loader use.
use std::cell::RefCell;
use std::rc::Rc;

use maho_tui::tui::{Component, Container};

pub const DEFAULT_TAIL_BUDGET: usize = 60;
pub const DEFAULT_WARM_CHUNK_SIZE: usize = 100;

const PENDING_FIRST_PAINT: usize = usize::MAX;

pub struct ProgressiveTranscriptOptions {
    pub tail_budget: usize,
    pub warm_chunk_size: usize,
    pub request_render: Rc<dyn Fn()>,
}

pub struct ProgressiveTranscriptContainer {
    children: Container,
    tail_budget: usize,
    warm_chunk_size: usize,
    request_render: Rc<dyn Fn()>,
    hydrated_from: usize,
    hydration_halted: bool,
    last_render_width: Option<usize>,
}

impl ProgressiveTranscriptContainer {
    pub fn new(options: ProgressiveTranscriptOptions) -> Self {
        Self {
            children: Container::new(),
            tail_budget: options.tail_budget,
            warm_chunk_size: options.warm_chunk_size,
            request_render: options.request_render,
            hydrated_from: PENDING_FIRST_PAINT,
            hydration_halted: false,
            last_render_width: None,
        }
    }

    pub fn children_mut(&mut self) -> &mut Vec<Rc<RefCell<dyn Component>>> {
        &mut self.children.children
    }

    pub fn add_child(&mut self, component: Rc<RefCell<dyn Component>>) {
        self.children.add_child(component);
    }

    pub fn is_fully_hydrated(&self) -> bool {
        self.hydrated_from == 0
    }

    pub fn rearm(&mut self) {
        self.hydrated_from = PENDING_FIRST_PAINT;
        self.hydration_halted = false;
    }

    fn render_range(&mut self, from: usize, to: usize, width: usize) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        for index in from..to {
            let Some(child) = self.children.children.get(index) else {
                continue;
            };
            let child_lines = child.borrow_mut().render(width);
            lines.extend(child_lines);
        }
        lines
    }
}

impl Component for ProgressiveTranscriptContainer {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.last_render_width = Some(width);
        let total = self.children.children.len();
        if self.hydrated_from == 0 || total == 0 {
            self.hydrated_from = 0;
            return self.children.render(width);
        }

        let first_visible = total.saturating_sub(self.tail_budget);
        if first_visible == 0 {
            self.hydrated_from = 0;
            return self.children.render(width);
        }

        self.hydrated_from = if self.hydrated_from == PENDING_FIRST_PAINT {
            first_visible
        } else {
            self.hydrated_from.min(first_visible)
        };
        self.render_range(self.hydrated_from, total, width)
    }

    fn invalidate(&mut self) {
        self.children.invalidate();
    }

    fn dispose(&mut self) {
        self.hydration_halted = true;
        self.children.dispose();
    }
}

impl ProgressiveTranscriptContainer {
    /// One bounded hydration step: render the next chunk of the deferred head so its line caches
    /// are warm, then move the watermark down. The host calls this off the paint path; `render`
    /// never widens its own frame.
    pub fn warm_next_chunk(&mut self) {
        if self.hydration_halted || self.hydrated_from == 0 {
            return;
        }
        let chunk_end = self.hydrated_from;
        let chunk_start = chunk_end.saturating_sub(self.warm_chunk_size);
        if let Some(width) = self.last_render_width {
            let _ = self.render_range(chunk_start, chunk_end, width);
        }
        self.hydrated_from = chunk_start;
        if chunk_start == 0 {
            (self.request_render)();
        }
    }
}
