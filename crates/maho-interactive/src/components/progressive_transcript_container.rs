//! Port of `components/progressive-transcript-container.ts`.
//!
//! senpi warms the deferred head from `setImmediate` macrotasks. A native renderer has no such
//! scheduler and must not widen the frame it is about to paint, so `render` returns exactly the
//! range senpi returns and the host drives hydration by calling [`ProgressiveTranscriptContainer::warm_next_chunk`]
//! on its own timer — the same split the spinner and loader use.
//!
//! Child lines are not cached across paints. Tool cards, bash output, and other children mutate
//! in place without a new child set and without `invalidate`, so a second paint must call `render`
//! again or those updates never appear.
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
    in_render: bool,
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
            in_render: false,
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
            lines.extend(child.borrow_mut().render(width));
        }
        lines
    }

    fn visible_bounds(&mut self, total: usize) -> (usize, usize) {
        if self.hydrated_from == 0 || total == 0 {
            self.hydrated_from = 0;
            return (0, total);
        }
        let first_visible = total.saturating_sub(self.tail_budget);
        if first_visible == 0 {
            self.hydrated_from = 0;
            return (0, total);
        }
        self.hydrated_from = if self.hydrated_from == PENDING_FIRST_PAINT {
            first_visible
        } else {
            self.hydrated_from.min(first_visible)
        };
        (self.hydrated_from, total)
    }

    fn render_frame(&mut self, width: usize) -> Vec<String> {
        self.last_render_width = Some(width);
        let total = self.children.children.len();
        let (from, to) = self.visible_bounds(total);
        // Full range keeps `Container::render` (panic containment and the render-error log).
        // Every paint calls child `render`: in-place tool, bash, and animation updates have no
        // container invalidation, so a line cache keyed on the child set would be stale.
        if from == 0 && to == total {
            self.children.render(width)
        } else {
            self.render_range(from, to, width)
        }
    }
}

impl Component for ProgressiveTranscriptContainer {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.in_render = true;
        let lines = self.render_frame(width);
        self.in_render = false;
        lines
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
    /// One bounded hydration step. Moves the watermark only; the next `render` lays the new rows
    /// out. Rendering the chunk here would repeat that work on the same paint, and a line cache
    /// filled here would hide in-place updates on the following paint.
    pub fn warm_next_chunk(&mut self) {
        self.warm_next_chunk_limited(self.warm_chunk_size);
    }

    /// Advance hydration by at most `limit` children. `render` sets `in_render`, so a warm that
    /// re-enters the paint call does not widen the frame it is already painting.
    pub(crate) fn warm_next_chunk_limited(&mut self, limit: usize) {
        if self.in_render || self.hydration_halted || self.hydrated_from == 0 || limit == 0 {
            return;
        }
        let step = self.warm_chunk_size.min(limit);
        let chunk_start = self.hydrated_from.saturating_sub(step);
        self.hydrated_from = chunk_start;
        if chunk_start == 0 {
            (self.request_render)();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    use maho_tui::tui::Component;

    use super::{ProgressiveTranscriptContainer, ProgressiveTranscriptOptions};

    struct LiveProbe {
        text: Rc<Cell<&'static str>>,
    }

    impl Component for LiveProbe {
        fn render(&mut self, _width: usize) -> Vec<String> {
            vec![self.text.get().to_owned()]
        }

        fn invalidate(&mut self) {}
    }

    fn container(tail_budget: usize, warm_chunk_size: usize) -> ProgressiveTranscriptContainer {
        ProgressiveTranscriptContainer::new(ProgressiveTranscriptOptions {
            tail_budget,
            warm_chunk_size,
            request_render: Rc::new(|| {}),
        })
    }

    /// Tool cards and Other children (bash output, animations) change text without a new child
    /// set and without `invalidate`. A second paint must show that text.
    #[test]
    fn second_render_shows_in_place_child_text_change() {
        let text = Rc::new(Cell::new("before"));
        let mut transcript = container(60, 100);
        transcript.add_child(Rc::new(RefCell::new(LiveProbe { text: Rc::clone(&text) })));
        assert_eq!(transcript.render(48), vec!["before".to_owned()]);
        text.set("after");
        assert_eq!(transcript.render(48), vec!["after".to_owned()]);
    }
}
