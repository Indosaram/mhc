//! Port of senpi `packages/tui/src/components/box.ts`.

use std::cell::RefCell;
use std::rc::Rc;

use crate::tui::{dispatch_mouse_event, Component, TuiMouseEvent, TuiMouseEventResult};
use crate::utils::{apply_background_to_line, visible_width};

pub type BackgroundFn = Rc<dyn Fn(&str) -> String>;

struct RenderCache {
    child_lines: Vec<String>,
    width: usize,
    bg_sample: Option<String>,
    lines: Vec<String>,
}

struct MouseLayout {
    width: usize,
    children: Vec<(Rc<RefCell<dyn Component>>, usize)>,
}

pub struct Box {
    pub children: Vec<Rc<RefCell<dyn Component>>>,
    padding_x: usize,
    padding_y: usize,
    bg_fn: Option<BackgroundFn>,
    disposed: bool,
    cache: Option<RenderCache>,
    mouse_layout: Option<MouseLayout>,
}

impl Box {
    pub fn new() -> Self {
        Self::with_padding(1, 1)
    }

    pub fn with_padding(padding_x: usize, padding_y: usize) -> Self {
        Self {
            children: Vec::new(),
            padding_x,
            padding_y,
            bg_fn: None,
            disposed: false,
            cache: None,
            mouse_layout: None,
        }
    }

    pub fn add_child(&mut self, component: Rc<RefCell<dyn Component>>) {
        self.children.push(component);
        self.invalidate_cache();
    }

    pub fn remove_child(&mut self, component: &Rc<RefCell<dyn Component>>) {
        if let Some(index) = self.children.iter().position(|c| Rc::ptr_eq(c, component)) {
            let removed = self.children.remove(index);
            self.invalidate_cache();
            removed.borrow_mut().dispose();
        }
    }

    pub fn clear(&mut self) {
        for child in self.children.drain(..) {
            child.borrow_mut().dispose();
        }
        self.invalidate_cache();
    }

    pub fn detach_all(&mut self) {
        self.children.clear();
        self.invalidate_cache();
    }

    pub fn set_bg_fn(&mut self, bg_fn: Option<BackgroundFn>) {
        self.bg_fn = bg_fn;
    }

    fn invalidate_cache(&mut self) {
        self.cache = None;
    }

    fn match_cache(&self, width: usize, child_lines: &[String], bg_sample: &Option<String>) -> bool {
        self.cache.as_ref().is_some_and(|cache| {
            cache.width == width && &cache.bg_sample == bg_sample && cache.child_lines == child_lines
        })
    }

    fn apply_bg(&self, line: &str, width: usize) -> String {
        let visible_len = visible_width(line);
        let pad_needed = width.saturating_sub(visible_len);
        let padded = format!("{line}{}", " ".repeat(pad_needed));
        match &self.bg_fn {
            Some(bg_fn) => apply_background_to_line(&padded, width, bg_fn.as_ref()),
            None => padded,
        }
    }
}

impl Default for Box {
    fn default() -> Self {
        Self::new()
    }
}

impl Component for Box {
    fn render(&mut self, width: usize) -> Vec<String> {
        if self.children.is_empty() {
            return Vec::new();
        }

        let content_width = width.saturating_sub(self.padding_x * 2).max(1);
        let left_pad = " ".repeat(self.padding_x);

        let mut child_lines = Vec::new();
        let mut mouse_children = Vec::with_capacity(self.children.len());
        for child in &self.children {
            let lines = child.borrow_mut().render(content_width);
            mouse_children.push((Rc::clone(child), lines.len()));
            for line in lines {
                child_lines.push(format!("{left_pad}{line}"));
            }
        }
        self.mouse_layout = Some(MouseLayout {
            width: content_width,
            children: mouse_children,
        });

        if child_lines.is_empty() {
            return Vec::new();
        }

        let bg_sample = self.bg_fn.as_ref().map(|bg_fn| bg_fn("test"));

        if let Some(cache) = self.cache.as_ref()
            && self.match_cache(width, &child_lines, &bg_sample)
        {
            return cache.lines.clone();
        }

        let mut result = Vec::new();
        for _ in 0..self.padding_y {
            result.push(self.apply_bg("", width));
        }
        for line in &child_lines {
            result.push(self.apply_bg(line, width));
        }
        for _ in 0..self.padding_y {
            result.push(self.apply_bg("", width));
        }

        self.cache = Some(RenderCache {
            child_lines,
            width,
            bg_sample,
            lines: result.clone(),
        });

        result
    }

    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        let content_width = (event.width.saturating_sub(self.padding_x * 2)).max(1);
        let content_y = event.y - self.padding_y as i64;
        let content_x = event.x - self.padding_x as i64;
        if content_y < 0 || content_x < 0 || content_x as usize >= content_width {
            return None;
        }

        let mouse_children: Vec<(Rc<RefCell<dyn Component>>, usize)> = match &self.mouse_layout {
            Some(layout) if layout.width == content_width => layout.children.clone(),
            _ => self
                .children
                .iter()
                .map(|c| {
                    let height = c.borrow_mut().render(content_width).len();
                    (Rc::clone(c), height)
                })
                .collect(),
        };

        let mut child_y: i64 = 0;
        for (child, child_height) in mouse_children {
            let child_height = child_height as i64;
            if content_y >= child_y && content_y < child_y + child_height {
                let sub_event = TuiMouseEvent {
                    x: content_x,
                    y: content_y - child_y,
                    width: content_width,
                    height: child_height as usize,
                    ..*event
                };
                return dispatch_mouse_event(&child, &sub_event).map(|r| r.result);
            }
            child_y += child_height;
        }
        None
    }

    fn invalidate(&mut self) {
        self.invalidate_cache();
        for child in &self.children {
            child.borrow_mut().invalidate();
        }
    }

    fn dispose(&mut self) {
        if self.disposed {
            return;
        }
        self.disposed = true;
        for child in &self.children {
            child.borrow_mut().dispose();
        }
        self.invalidate_cache();
    }
}
