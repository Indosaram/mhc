//! Port of senpi `packages/tui/src/components/scroll-view.ts`.
//!
//! senpi's transient-scrollbar hide delay is an unref'd `setTimeout`. This crate has no event
//! loop, so [`ScrollView::hide_due_at_ms`] exposes the deadline for the host's poll loop
//! (mirroring `terminal.rs`'s `run_due_timers` pattern from todo 6): call
//! [`ScrollView::fire_hide_timer_if_due`] once per tick.

use std::rc::Rc;

use crate::layout_node::{LayoutNode, LayoutViewport, ScrollLayoutNode, ScrollLayoutState};
use crate::tui::{Component, Container, TuiMouseEvent, TuiMouseEventResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollViewScrollbar {
    Hidden,
    Auto,
    Always,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollViewFollow {
    None,
    End,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overscroll {
    Chain,
    Contain,
}

pub struct ScrollViewOptions {
    pub follow: ScrollViewFollow,
    pub primary: bool,
    pub overscroll: Overscroll,
    pub scrollbar: ScrollViewScrollbar,
    pub scrollbar_track_style: Rc<dyn Fn(&str) -> String>,
    pub scrollbar_thumb_style: Rc<dyn Fn(&str) -> String>,
    pub scrollbar_hide_delay_ms: u64,
}

fn default_track_style(text: &str) -> String {
    format!("\x1b[90m{text}\x1b[39m")
}

fn default_thumb_style(text: &str) -> String {
    format!("\x1b[37m{text}\x1b[39m")
}

impl Default for ScrollViewOptions {
    fn default() -> Self {
        Self {
            follow: ScrollViewFollow::None,
            primary: false,
            overscroll: Overscroll::Chain,
            scrollbar: ScrollViewScrollbar::Hidden,
            scrollbar_track_style: Rc::new(default_track_style),
            scrollbar_thumb_style: Rc::new(default_thumb_style),
            scrollbar_hide_delay_ms: 1000,
        }
    }
}

#[derive(Default, Clone, Copy)]
pub struct ScrollToOptions {
    /// Keep follow-end disabled even when the target is the current content end.
    pub disable_follow: bool,
}

pub struct ScrollView {
    container: Container,
    child: Rc<std::cell::RefCell<dyn Component>>,
    follow_end: bool,
    primary: bool,
    overscroll: Overscroll,
    scrollbar_track_style: Rc<dyn Fn(&str) -> String>,
    scrollbar_thumb_style: Rc<dyn Fn(&str) -> String>,
    current_scrollbar: ScrollViewScrollbar,
    scrollbar_hide_delay_ms: u64,
    current_scroll_top: usize,
    content_height: usize,
    current_viewport_height: usize,
    following_end: bool,
    follow_suppressed_at_end: bool,
    transient_scrollbar_visible: bool,
    scrollbar_active: bool,
    hide_due_at_ms: Option<u64>,
    render_requested: bool,
}

impl ScrollView {
    /// Panics if `options.axis` would be non-vertical in senpi (this port has no `axis` field
    /// because vertical is the only supported value; senpi throws for anything else).
    pub fn new(component: Rc<std::cell::RefCell<dyn Component>>, options: ScrollViewOptions) -> Self {
        let mut container = Container::new();
        container.add_child(Rc::clone(&component));
        let follow_end = options.follow == ScrollViewFollow::End;
        Self {
            container,
            child: component,
            follow_end,
            primary: options.primary,
            overscroll: options.overscroll,
            scrollbar_track_style: options.scrollbar_track_style,
            scrollbar_thumb_style: options.scrollbar_thumb_style,
            current_scrollbar: options.scrollbar,
            scrollbar_hide_delay_ms: options.scrollbar_hide_delay_ms,
            current_scroll_top: 0,
            content_height: 0,
            current_viewport_height: 0,
            following_end: follow_end,
            follow_suppressed_at_end: false,
            transient_scrollbar_visible: false,
            scrollbar_active: false,
            hide_due_at_ms: None,
            render_requested: false,
        }
    }

    pub fn scroll_top(&self) -> usize {
        self.current_scroll_top
    }

    pub fn is_following_end(&self) -> bool {
        self.following_end
    }

    /// Whether follow-end is enabled at all (senpi's `ScrollView.followEnd`).
    pub fn follows_end(&self) -> bool {
        self.follow_end
    }

    pub fn viewport_height(&self) -> usize {
        self.current_viewport_height
    }

    pub fn primary(&self) -> bool {
        self.primary
    }

    pub fn overscroll(&self) -> Overscroll {
        self.overscroll
    }

    pub fn scrollbar_track_style(&self, text: &str) -> String {
        (self.scrollbar_track_style)(text)
    }

    pub fn scrollbar_thumb_style(&self, text: &str) -> String {
        (self.scrollbar_thumb_style)(text)
    }

    pub fn scrollbar(&self) -> ScrollViewScrollbar {
        self.current_scrollbar
    }

    pub fn is_scrollbar_visible(&self) -> bool {
        match self.current_scrollbar {
            ScrollViewScrollbar::Always => self.current_viewport_height > 0,
            ScrollViewScrollbar::Auto => {
                self.content_height > self.current_viewport_height && self.transient_scrollbar_visible
            }
            ScrollViewScrollbar::Hidden => false,
        }
    }

    pub fn is_scrollbar_active(&self) -> bool {
        self.scrollbar_active
    }

    /// Deadline for the transient-scrollbar auto-hide timer, polled by the host loop.
    pub fn hide_due_at_ms(&self) -> Option<u64> {
        self.hide_due_at_ms
    }

    /// Fire the auto-hide timer if `now_ms` has passed its deadline. Returns `true` when a
    /// render should be requested (mirrors senpi's `requestRenderCallback?.()`).
    pub fn fire_hide_timer_if_due(&mut self, now_ms: u64) -> bool {
        if self.hide_due_at_ms.is_some_and(|due| due <= now_ms) {
            self.hide_due_at_ms = None;
            self.transient_scrollbar_visible = false;
            self.render_requested = true;
            return true;
        }
        false
    }

    /// Drains and clears the render-request flag set by scroll/scrollbar mutations, mirroring
    /// senpi's `requestRenderCallback?.()` calls (there is no injected callback in this port;
    /// callers poll this after each mutating method instead).
    pub fn take_render_requested(&mut self) -> bool {
        std::mem::take(&mut self.render_requested)
    }

    pub fn set_scrollbar(&mut self, scrollbar: ScrollViewScrollbar) {
        if scrollbar == self.current_scrollbar {
            return;
        }
        self.current_scrollbar = scrollbar;
        if scrollbar != ScrollViewScrollbar::Auto {
            self.hide_transient_scrollbar();
        } else if self.scrollbar_active {
            self.mark_scrollbar_activity(0);
        }
        self.render_requested = true;
    }

    pub fn get_content_width(&self, width: usize) -> usize {
        if self.current_scrollbar == ScrollViewScrollbar::Always && width > 1 {
            width - 1
        } else {
            width
        }
    }

    fn mark_scrollbar_activity(&mut self, now_ms: u64) {
        if self.current_scrollbar != ScrollViewScrollbar::Auto || self.content_height <= self.current_viewport_height
        {
            return;
        }
        self.transient_scrollbar_visible = true;
        if self.scrollbar_active {
            self.hide_due_at_ms = None;
            return;
        }
        self.hide_due_at_ms = Some(now_ms + self.scrollbar_hide_delay_ms);
    }

    fn hide_transient_scrollbar(&mut self) {
        self.transient_scrollbar_visible = false;
        self.hide_due_at_ms = None;
    }

    pub fn set_scrollbar_active(&mut self, active: bool, now_ms: u64) {
        if active == self.scrollbar_active {
            return;
        }
        self.scrollbar_active = active;
        self.mark_scrollbar_activity(now_ms);
        self.render_requested = true;
    }

    pub fn scroll_to(&mut self, scroll_top: i64, options: ScrollToOptions, now_ms: u64) {
        let requested = scroll_top.max(0) as usize;
        let max_scroll_top = self.content_height.saturating_sub(self.current_viewport_height);
        let next = requested.min(max_scroll_top);
        let next_follow_suppressed_at_end = options.disable_follow && next == max_scroll_top;
        let next_following_end = !next_follow_suppressed_at_end && self.follow_end && next == max_scroll_top;
        if next == self.current_scroll_top
            && next_following_end == self.following_end
            && next_follow_suppressed_at_end == self.follow_suppressed_at_end
        {
            return;
        }
        let moved = next != self.current_scroll_top;
        self.current_scroll_top = next;
        self.following_end = next_following_end;
        self.follow_suppressed_at_end = next_follow_suppressed_at_end;
        if moved {
            self.mark_scrollbar_activity(now_ms);
        }
        self.render_requested = true;
    }

    /// Returns the unconsumed remainder, mirroring senpi's `scrollBy` return value.
    pub fn scroll_by(&mut self, lines: i64, now_ms: u64) -> i64 {
        if lines == 0 {
            return 0;
        }
        let max_scroll_top = self.content_height.saturating_sub(self.current_viewport_height) as i64;
        let start = if self.following_end {
            max_scroll_top
        } else {
            self.current_scroll_top as i64
        };
        let next = (start + lines).clamp(0, max_scroll_top);
        let moved = next - start;
        let was_following_end = self.following_end;
        self.current_scroll_top = next as usize;
        self.following_end = self.follow_end && next == max_scroll_top;
        self.follow_suppressed_at_end = false;
        if moved != 0 {
            self.mark_scrollbar_activity(now_ms);
        }
        if moved != 0 || self.following_end != was_following_end {
            self.render_requested = true;
        }
        lines - moved
    }

    pub fn scroll_to_start(&mut self, now_ms: u64) {
        let next_following_end = self.follow_end && self.content_height <= self.current_viewport_height;
        let changed = self.current_scroll_top != 0 || self.following_end != next_following_end;
        self.current_scroll_top = 0;
        self.following_end = next_following_end;
        self.follow_suppressed_at_end = false;
        if changed {
            self.mark_scrollbar_activity(now_ms);
            self.render_requested = true;
        }
    }

    pub fn scroll_to_end(&mut self, now_ms: u64) {
        let next = self.content_height.saturating_sub(self.current_viewport_height);
        let changed = self.current_scroll_top != next || self.following_end != self.follow_end;
        self.current_scroll_top = next;
        self.following_end = self.follow_end;
        self.follow_suppressed_at_end = false;
        if changed {
            self.mark_scrollbar_activity(now_ms);
            self.render_requested = true;
        }
    }

    pub fn update_layout(&mut self, content_height: usize, viewport_height: usize) {
        self.content_height = content_height;
        self.current_viewport_height = viewport_height;
        let max_scroll_top = self.content_height.saturating_sub(self.current_viewport_height);
        if self.following_end {
            self.current_scroll_top = max_scroll_top;
        } else {
            self.current_scroll_top = self.current_scroll_top.min(max_scroll_top);
        }
        if self.current_scroll_top < max_scroll_top {
            self.follow_suppressed_at_end = false;
        }
        if self.follow_end && self.current_scroll_top == max_scroll_top && !self.follow_suppressed_at_end {
            self.following_end = true;
        }
        if self.content_height <= self.current_viewport_height {
            self.hide_transient_scrollbar();
        }
    }
}

impl Component for ScrollView {
    fn render(&mut self, width: usize) -> Vec<String> {
        let content_width = self.get_content_width(width);
        let lines = self.child.borrow_mut().render(content_width);
        if content_width == width {
            lines
        } else {
            lines.into_iter().map(|line| format!("{line} ")).collect()
        }
    }

    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.container.handle_mouse(event)
    }

    fn invalidate(&mut self) {
        self.container.invalidate();
    }

    fn dispose(&mut self) {
        self.container.dispose();
    }

    fn as_container(&self) -> Option<&Container> {
        Some(&self.container)
    }

    fn as_container_mut(&mut self) -> Option<&mut Container> {
        Some(&mut self.container)
    }
}

impl ScrollLayoutState for ScrollView {
    fn scroll_top(&self) -> usize {
        self.current_scroll_top
    }

    fn primary(&self) -> bool {
        self.primary
    }

    fn overscroll(&self) -> crate::layout_node::ScrollOverscroll {
        match self.overscroll {
            Overscroll::Chain => crate::layout_node::ScrollOverscroll::Chain,
            Overscroll::Contain => crate::layout_node::ScrollOverscroll::Contain,
        }
    }

    fn viewport_height(&self) -> usize {
        self.current_viewport_height
    }

    fn get_content_width(&self, width: usize) -> usize {
        ScrollView::get_content_width(self, width)
    }

    fn update_layout(&mut self, content_height: usize, viewport_height: usize) {
        ScrollView::update_layout(self, content_height, viewport_height);
    }
}

/// senpi's `[LAYOUT_NODE]()`.
pub fn scroll_layout_node(
    view: &Rc<std::cell::RefCell<ScrollView>>,
) -> LayoutNode {
    let child = Rc::clone(&view.borrow().child);
    LayoutNode::Scroll(ScrollLayoutNode {
        component: child,
        state: Rc::clone(view),
    })
}

#[allow(unused)]
fn ensure_layout_viewport_used(_: LayoutViewport) {}
