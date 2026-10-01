//! Port of senpi `packages/tui/src/components/mouse-region.ts`.

use std::cell::RefCell;
use std::rc::Rc;

use crate::tui::{dispatch_mouse_event, Component, TuiMouseEvent, TuiMouseEventResult};

pub type MouseRegionHandler = Box<dyn FnMut(&TuiMouseEvent) -> Option<TuiMouseEventResult>>;

/// Adds mouse handling to an existing component without changing its rendering.
pub struct MouseRegion {
    child: Rc<RefCell<dyn Component>>,
    on_mouse: MouseRegionHandler,
}

impl MouseRegion {
    pub fn new(child: Rc<RefCell<dyn Component>>, on_mouse: MouseRegionHandler) -> Self {
        Self { child, on_mouse }
    }
}

impl Component for MouseRegion {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.child.borrow_mut().render(width)
    }

    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        if let Some(dispatch) = dispatch_mouse_event(&self.child, event) {
            return Some(dispatch.result);
        }
        (self.on_mouse)(event)
    }

    fn invalidate(&mut self) {
        self.child.borrow_mut().invalidate();
    }
}
