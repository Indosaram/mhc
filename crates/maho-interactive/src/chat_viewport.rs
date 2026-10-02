use std::{cell::RefCell, rc::Rc};
use maho_tui::{components::{scroll_view::{Overscroll, ScrollView, ScrollViewFollow, ScrollViewOptions, ScrollViewScrollbar}, stack::{StackChild, StackEntryOptions, StackOptions}, v_stack::VStack}, layout_node::StackBasis, tui::Component};

pub type ScrollbarStyle = Rc<dyn Fn(&str) -> String>;

pub struct ChatViewportOptions {
    pub document: Rc<RefCell<dyn Component>>,
    pub pending_messages: Rc<RefCell<dyn Component>>,
    pub status: Rc<RefCell<dyn Component>>,
    pub hook_status: Option<Rc<RefCell<dyn Component>>>,
    pub editor: Rc<RefCell<dyn Component>>,
    pub footer: Rc<RefCell<dyn Component>>,
    pub widgets_above: Option<Rc<RefCell<dyn Component>>>,
    pub widgets_below: Option<Rc<RefCell<dyn Component>>>,
    pub scrollbar: Option<ScrollViewScrollbar>,
    pub scrollbar_track_style: Option<ScrollbarStyle>,
    pub scrollbar_thumb_style: Option<ScrollbarStyle>,
}

pub struct ChatViewport {
    pub root: Rc<RefCell<VStack>>,
    pub transcript: Rc<RefCell<ScrollView>>,
}

pub fn create_chat_viewport(options: ChatViewportOptions) -> ChatViewport {
    let mut scroll_options = ScrollViewOptions {
        follow: ScrollViewFollow::End,
        primary: true,
        overscroll: Overscroll::Chain,
        scrollbar: options.scrollbar.unwrap_or(ScrollViewScrollbar::Auto),
        ..Default::default()
    };
    if let Some(style) = options.scrollbar_track_style { scroll_options.scrollbar_track_style = style; }
    if let Some(style) = options.scrollbar_thumb_style { scroll_options.scrollbar_thumb_style = style; }
    let transcript = ScrollView::new(options.document, scroll_options);
    let mut dock_children = Vec::new();
    for (component, min_size) in [Some(options.pending_messages), Some(options.status), options.hook_status, options.widgets_above, Some(options.editor), options.widgets_below, Some(options.footer)].into_iter().zip([0, 0, 0, 0, 3, 0, 0]) {
        if let Some(component) = component {
            dock_children.push(StackChild::Entry { component, options: StackEntryOptions { shrink: Some(1), min_size: Some(min_size), ..Default::default() } });
        }
    }
    let dock = Rc::new(RefCell::new(VStack::new(dock_children, StackOptions::default())));
    let root = Rc::new(RefCell::new(VStack::new(vec![
        StackChild::Entry { component: transcript.clone(), options: StackEntryOptions { basis: Some(StackBasis::Fixed(0)), grow: Some(1), shrink: Some(1), min_size: Some(1), ..Default::default() } },
        StackChild::Entry { component: dock, options: StackEntryOptions { basis: Some(StackBasis::Auto), grow: Some(0), shrink: Some(1), min_size: Some(1), ..Default::default() } },
    ], StackOptions::default())));
    ChatViewport { root, transcript }
}
