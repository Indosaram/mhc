//! Port of senpi `packages/tui/src/components/v-stack.ts`.

use std::cell::RefCell;
use std::rc::Rc;

use super::stack::{allocate_stack_sizes, visible_stack_entries, Stack, StackChild, StackEntryOptions, StackOptions};
use crate::layout_node::{LayoutNode, LayoutViewport, StackDirection, StackLayoutNode};
use crate::tui::{Component, Container, TuiMouseEvent, TuiMouseEventResult};

pub struct VStack {
    stack: Stack,
}

impl VStack {
    pub fn new(children: Vec<StackChild>, options: StackOptions) -> Self {
        Self {
            stack: Stack::new(children, options),
        }
    }

    pub fn add_child(&mut self, component: Rc<RefCell<dyn Component>>, options: StackEntryOptions) {
        self.stack.add_child(component, options);
    }

    pub fn remove_child(&mut self, component: &Rc<RefCell<dyn Component>>) {
        self.stack.remove_child(component);
    }

    pub fn clear(&mut self) {
        self.stack.clear();
    }

    pub fn layout_node(&self) -> LayoutNode {
        LayoutNode::Stack(StackLayoutNode {
            direction: StackDirection::VStack,
            entries: self.stack.entries.clone(),
            gap: self.stack.gap,
            align: self.stack.align,
        })
    }
}

impl Component for VStack {
    /// senpi's `VStack.render`: sizes come from `allocateStackSizes` with no available size,
    /// so a `minSize` larger than the child's own line count pads it and a smaller one
    /// truncates it. Layout measurement reads this height (senpi's `measureHeight` calls
    /// `component.render`).
    fn render(&mut self, width: usize) -> Vec<String> {
        let safe_width = width.max(1);
        let viewport = LayoutViewport {
            width: safe_width,
            height: usize::MAX,
        };
        let entries = visible_stack_entries(&self.stack.entries, viewport);
        let rendered: Vec<Vec<String>> = entries
            .iter()
            .map(|entry| entry.component.borrow_mut().render(viewport.width))
            .collect();
        let intrinsic_sizes: Vec<usize> = rendered.iter().map(Vec::len).collect();
        let sizes = allocate_stack_sizes(&entries, &intrinsic_sizes, None, self.stack.gap);
        let mut lines = Vec::new();
        for (index, child_lines) in rendered.iter().enumerate() {
            if index > 0 {
                for _ in 0..self.stack.gap {
                    lines.push(String::new());
                }
            }
            let size = sizes[index];
            lines.extend(child_lines.iter().take(size).cloned());
            for _ in child_lines.len()..size {
                lines.push(String::new());
            }
        }
        lines
    }

    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.stack.handle_mouse(event)
    }

    fn invalidate(&mut self) {
        self.stack.invalidate();
    }

    fn dispose(&mut self) {
        self.stack.dispose();
    }

    fn as_container(&self) -> Option<&Container> {
        self.stack.as_container()
    }

    fn as_container_mut(&mut self) -> Option<&mut Container> {
        self.stack.as_container_mut()
    }

    fn as_layout_component(&self) -> Option<&dyn crate::layout_node::LayoutComponent> {
        Some(self)
    }
}

impl crate::layout_node::LayoutComponent for VStack {
    fn layout_node(&self) -> LayoutNode {
        VStack::layout_node(self)
    }
}
