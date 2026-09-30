//! Port of senpi `packages/tui/src/components/h-stack.ts`.

use std::cell::RefCell;
use std::rc::Rc;

use super::stack::{allocate_stack_sizes, visible_stack_entries, Stack, StackChild, StackEntryOptions, StackOptions};
use crate::layout_node::{LayoutNode, LayoutViewport, StackAlign, StackDirection, StackLayoutNode};
use crate::tui::{composite_tui_line, Component, Container, TuiMouseEvent, TuiMouseEventResult};
use crate::utils::visible_width;

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
    /// senpi's `HStack.render`: intrinsic widths come from each child rendered at the full
    /// width, then children are rendered again at their allocated width and composited into
    /// a shared row buffer at `align` offsets.
    fn render(&mut self, width: usize) -> Vec<String> {
        let safe_width = width.max(1);
        let viewport = LayoutViewport {
            width: safe_width,
            height: usize::MAX,
        };
        let entries = visible_stack_entries(&self.stack.entries, viewport);
        if entries.is_empty() {
            return Vec::new();
        }

        let intrinsic_widths: Vec<usize> = entries
            .iter()
            .map(|entry| {
                entry
                    .component
                    .borrow_mut()
                    .render(safe_width)
                    .iter()
                    .map(|line| visible_width(line))
                    .max()
                    .unwrap_or(0)
            })
            .collect();
        let widths = allocate_stack_sizes(&entries, &intrinsic_widths, Some(safe_width), self.stack.gap);
        let rendered: Vec<Vec<String>> = entries
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                if widths[index] == 0 {
                    Vec::new()
                } else {
                    entry.component.borrow_mut().render(widths[index])
                }
            })
            .collect();
        let height = rendered.iter().map(Vec::len).max().unwrap_or(0);
        let mut result = vec![String::new(); height];
        let mut x = 0usize;
        for (index, lines) in rendered.iter().enumerate() {
            let child_width = widths[index];
            let offset = match self.stack.align {
                StackAlign::Center => (height - lines.len()) / 2,
                StackAlign::End => height - lines.len(),
                _ => 0,
            };
            for (row, line) in lines.iter().enumerate() {
                let target = row + offset;
                if target >= result.len() {
                    continue;
                }
                result[target] = composite_tui_line(&result[target], line, x, child_width, safe_width);
            }
            x += child_width + self.stack.gap;
        }
        result
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
