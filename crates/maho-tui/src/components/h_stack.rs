//! Port of senpi `packages/tui/src/components/h-stack.ts`.

use std::cell::RefCell;
use std::rc::Rc;

use super::stack::{Stack, StackChild, StackEntryOptions, StackOptions};
use crate::layout_node::{LayoutNode, StackDirection, StackLayoutNode};
use crate::tui::{Component, Container, TuiMouseEvent, TuiMouseEventResult};

pub struct HStack {
    stack: Stack,
}

impl HStack {
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
            direction: StackDirection::HStack,
            entries: self.stack.entries.clone(),
            gap: self.stack.gap,
            align: self.stack.align,
        })
    }
}

impl Component for HStack {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.stack.render(width)
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

impl crate::layout_node::LayoutComponent for HStack {
    fn layout_node(&self) -> LayoutNode {
        HStack::layout_node(self)
    }
}
