//! Port of `components/tool-renderer-boundary.ts`.
use std::cell::RefCell;
use std::rc::Rc;

use maho_tui::tui::Component;

pub struct ToolRendererBoundary {
    component: Rc<RefCell<dyn Component>>,
    component_disposed: bool,
    fallback: Option<Rc<RefCell<dyn Component>>>,
    failed: bool,
    on_failure: Box<dyn FnMut()>,
}

impl ToolRendererBoundary {
    pub fn new(
        component: Rc<RefCell<dyn Component>>,
        fallback: Option<Rc<RefCell<dyn Component>>>,
        on_failure: Box<dyn FnMut()>,
    ) -> Self {
        Self { component, component_disposed: false, fallback, failed: false, on_failure }
    }

    fn fail(&mut self) {
        if self.failed {
            return;
        }
        self.failed = true;
        (self.on_failure)();
        self.dispose_component();
    }

    fn dispose_component(&mut self) {
        if self.component_disposed {
            return;
        }
        self.component_disposed = true;
        self.component.borrow_mut().dispose();
    }

    fn render_fallback(&mut self, width: usize) -> Vec<String> {
        match &self.fallback {
            Some(fallback) => fallback.borrow_mut().render(width),
            None => Vec::new(),
        }
    }
}

impl Component for ToolRendererBoundary {
    fn render(&mut self, width: usize) -> Vec<String> {
        if !self.failed {
            let component = Rc::clone(&self.component);
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                component.borrow_mut().render(width)
            })) {
                Ok(lines) => return lines,
                Err(_) => self.fail(),
            }
        }
        self.render_fallback(width)
    }

    fn invalidate(&mut self) {
        if !self.failed {
            let component = Rc::clone(&self.component);
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                component.borrow_mut().invalidate();
            }))
            .is_err()
            {
                self.fail();
            }
        }
        if let Some(fallback) = &self.fallback {
            fallback.borrow_mut().invalidate();
        }
    }

    fn dispose(&mut self) {
        self.dispose_component();
        if let Some(fallback) = &self.fallback {
            fallback.borrow_mut().dispose();
        }
    }
}
