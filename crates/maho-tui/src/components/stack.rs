//! Port of senpi `packages/tui/src/components/stack.ts`.

use std::cell::RefCell;
use std::rc::Rc;

use crate::layout_node::{LayoutViewport, StackAlign, StackBasis, StackLayoutEntry};
use crate::tui::{Component, Container};

#[derive(Debug, Clone, Copy, Default)]
pub struct StackEntryOptions {
    pub basis: Option<StackBasis>,
    pub grow: Option<usize>,
    pub shrink: Option<usize>,
    pub min_size: Option<usize>,
    pub max_size: Option<usize>,
}

pub enum StackChild {
    Component(Rc<RefCell<dyn Component>>),
    Entry {
        component: Rc<RefCell<dyn Component>>,
        options: StackEntryOptions,
    },
}

#[derive(Debug, Clone, Copy, Default)]
pub struct StackOptions {
    pub gap: usize,
    pub align: StackAlign,
}

/// Base shared by [`crate::components::v_stack::VStack`] and
/// [`crate::components::h_stack::HStack`] (senpi's abstract `Stack` class).
pub struct Stack {
    pub container: Container,
    pub entries: Vec<StackLayoutEntry>,
    pub gap: usize,
    pub align: StackAlign,
}

impl Stack {
    pub fn new(children: Vec<StackChild>, options: StackOptions) -> Self {
        let mut stack = Self {
            container: Container::new(),
            entries: Vec::new(),
            gap: options.gap,
            align: options.align,
        };
        for child in children {
            match child {
                StackChild::Component(component) => stack.add_child(component, StackEntryOptions::default()),
                StackChild::Entry { component, options } => stack.add_child(component, options),
            }
        }
        stack
    }

    pub fn add_child(&mut self, component: Rc<RefCell<dyn Component>>, options: StackEntryOptions) {
        self.container.add_child(Rc::clone(&component));
        self.entries.push(StackLayoutEntry {
            component,
            basis: options.basis,
            grow: options.grow.or(Some(0)),
            shrink: options.shrink.or(Some(1)),
            min_size: options.min_size.or(Some(0)),
            max_size: options.max_size.or(Some(usize::MAX)),
            visible: None,
        });
    }

    pub fn remove_child(&mut self, component: &Rc<RefCell<dyn Component>>) {
        self.container.remove_child(component);
        if let Some(index) = self
            .entries
            .iter()
            .position(|entry| Rc::ptr_eq(&entry.component, component))
        {
            self.entries.remove(index);
        }
    }

    pub fn clear(&mut self) {
        self.container.clear();
        self.entries.clear();
    }
}

impl Component for Stack {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.container.render(width)
    }

    fn handle_mouse(&mut self, event: &crate::tui::TuiMouseEvent) -> Option<crate::tui::TuiMouseEventResult> {
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

/// senpi's `visibleStackEntries`.
pub fn visible_stack_entries(entries: &[StackLayoutEntry], viewport: LayoutViewport) -> Vec<StackLayoutEntry> {
    entries
        .iter()
        .filter(|entry| entry.visible.as_ref().is_none_or(|visible| visible(viewport)))
        .cloned()
        .collect()
}

fn clamp_size(size: usize, entry: &StackLayoutEntry) -> usize {
    let min = entry.min_size.unwrap_or(0);
    let max = entry.max_size.unwrap_or(usize::MAX).max(min);
    size.clamp(min, max)
}

fn distribute(sizes: &mut [usize], entries: &[StackLayoutEntry], amount: usize, grow: bool) {
    let mut remaining = amount;
    while remaining > 0 {
        let candidates: Vec<usize> = (0..entries.len())
            .filter(|&index| {
                let entry = &entries[index];
                if grow {
                    entry.grow.unwrap_or(0) > 0 && sizes[index] < entry.max_size.unwrap_or(usize::MAX)
                } else {
                    entry.shrink.unwrap_or(1) > 0 && sizes[index] > entry.min_size.unwrap_or(0)
                }
            })
            .collect();
        if candidates.is_empty() {
            return;
        }

        let total_weight: f64 = candidates
            .iter()
            .map(|&index| {
                let entry = &entries[index];
                if grow {
                    entry.grow.unwrap_or(0) as f64
                } else {
                    entry.shrink.unwrap_or(1) as f64 * (sizes[index].max(1) as f64)
                }
            })
            .sum();
        let mut distributed = 0usize;
        for &index in &candidates {
            if remaining == 0 {
                break;
            }
            let entry = &entries[index];
            let weight = if grow {
                entry.grow.unwrap_or(0) as f64
            } else {
                entry.shrink.unwrap_or(1) as f64 * (sizes[index].max(1) as f64)
            };
            let proposed = ((remaining as f64 * weight) / total_weight).floor().max(1.0) as usize;
            let capacity = if grow {
                entry.max_size.unwrap_or(usize::MAX).saturating_sub(sizes[index])
            } else {
                sizes[index].saturating_sub(entry.min_size.unwrap_or(0))
            };
            let delta = remaining.min(proposed).min(capacity);
            if delta == 0 {
                continue;
            }
            sizes[index] = if grow { sizes[index] + delta } else { sizes[index] - delta };
            remaining -= delta;
            distributed += delta;
        }
        if distributed == 0 {
            return;
        }
    }
}

/// senpi's `allocateStackSizes`.
pub fn allocate_stack_sizes(
    entries: &[StackLayoutEntry],
    intrinsic_sizes: &[usize],
    available_size: Option<usize>,
    gap: usize,
) -> Vec<usize> {
    let mut sizes: Vec<usize> = entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let basis = match entry.basis {
                None | Some(StackBasis::Auto) => intrinsic_sizes.get(index).copied().unwrap_or(0),
                Some(StackBasis::Fixed(value)) => value,
            };
            clamp_size(basis, entry)
        })
        .collect();
    let Some(available_size) = available_size else {
        return sizes;
    };

    let content_size = available_size.saturating_sub(entries.len().saturating_sub(1) * gap);
    let total: usize = sizes.iter().sum();
    if total < content_size {
        distribute(&mut sizes, entries, content_size - total, true);
    } else if total > content_size {
        distribute(&mut sizes, entries, total - content_size, false);
    }
    sizes
}
