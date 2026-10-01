//! Port of senpi `packages/tui/src/layout-node.ts`.

use std::cell::RefCell;
use std::rc::Rc;

/// Layout viewport dimensions passed to a stack entry's `visible` predicate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayoutViewport {
    pub width: usize,
    pub height: usize,
}

/// Basis size for a stack entry: an explicit column/row count, or intrinsic ("auto") sizing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StackBasis {
    Auto,
    Fixed(usize),
}

/// A predicate deciding whether a stack entry is visible for a given viewport.
pub type VisiblePredicate = Rc<dyn Fn(LayoutViewport) -> bool>;

/// One child of a [`StackLayoutNode`], mirroring senpi's `StackLayoutEntry`.
#[derive(Clone)]
pub struct StackLayoutEntry {
    pub component: Rc<RefCell<dyn crate::tui::Component>>,
    pub basis: Option<StackBasis>,
    pub grow: Option<usize>,
    pub shrink: Option<usize>,
    pub min_size: Option<usize>,
    pub max_size: Option<usize>,
    pub visible: Option<VisiblePredicate>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StackAlign {
    #[default]
    Stretch,
    Start,
    Center,
    End,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StackDirection {
    VStack,
    HStack,
}

/// A vertical or horizontal stack layout node, mirroring senpi's `StackLayoutNode`.
pub struct StackLayoutNode {
    pub direction: StackDirection,
    pub entries: Vec<StackLayoutEntry>,
    pub gap: usize,
    pub align: StackAlign,
}

/// A scroll layout node, mirroring senpi's `ScrollLayoutNode`. senpi's `ScrollLayoutState` is
/// an interface any component could implement; this crate ports exactly one implementor
/// ([`crate::components::scroll_view::ScrollView`]), so `state` holds it directly rather than
/// through a trait object - avoiding an `unsafe` downcast in `layout.rs` for zero behavioral
/// difference, since no second implementor exists upstream to require erasure.
pub struct ScrollLayoutNode {
    pub component: Rc<RefCell<dyn crate::tui::Component>>,
    pub state: Rc<RefCell<crate::components::scroll_view::ScrollView>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollOverscroll {
    Chain,
    Contain,
}

/// Mirrors senpi's `ScrollLayoutState` interface, implemented directly by
/// [`crate::components::scroll_view::ScrollView`].
pub trait ScrollLayoutState {
    fn scroll_top(&self) -> usize;
    fn primary(&self) -> bool;
    fn overscroll(&self) -> ScrollOverscroll;
    fn viewport_height(&self) -> usize;
    fn get_content_width(&self, width: usize) -> usize;
    fn update_layout(&mut self, content_height: usize, viewport_height: usize);
}

/// Layout node attached to a component, mirroring senpi's `LayoutNode` union.
pub enum LayoutNode {
    Stack(StackLayoutNode),
    Scroll(ScrollLayoutNode),
}

/// Components that expose a [`LayoutNode`] implement this to participate in the layout engine
/// (senpi's `component[LAYOUT_NODE]()` via the `LAYOUT_NODE` symbol).
pub trait LayoutComponent {
    fn layout_node(&self) -> LayoutNode;
}
