use std::{cell::RefCell, rc::Rc};
use maho_interactive::chat_viewport::*;
use maho_tui::{components::{scroll_view::ScrollViewScrollbar, text::Text}, layout::render_layout_frame, tui::Component};

fn text(value: &str) -> Rc<RefCell<dyn Component>> { Rc::new(RefCell::new(Text::with_padding(value, 0, 0))) }
fn viewport(scrollbar: Option<ScrollViewScrollbar>) -> ChatViewport {
    create_chat_viewport(ChatViewportOptions {
        document: text("transcript"), pending_messages: text("pending"), status: text("status"), hook_status: Some(text("hook")),
        editor: text("editor"), footer: text("footer"), widgets_above: Some(text("above")), widgets_below: Some(text("below")),
        scrollbar, scrollbar_track_style: None, scrollbar_thumb_style: None,
    })
}

#[test]
fn viewport_scrollbar_defaults_to_auto_and_accepts_override() {
    assert_eq!(viewport(None).transcript.borrow().scrollbar(), ScrollViewScrollbar::Auto);
    assert_eq!(viewport(Some(ScrollViewScrollbar::Hidden)).transcript.borrow().scrollbar(), ScrollViewScrollbar::Hidden);
}

#[test]
fn viewport_keeps_input_dock_in_source_order_at_120_by_36() {
    let viewport = viewport(Some(ScrollViewScrollbar::Hidden));
    let root: Rc<RefCell<dyn Component>> = viewport.root;
    let frame = render_layout_frame(&root, 120, 36);
    assert_eq!(frame.lines.len(), 36);
    let labels: Vec<_> = frame.lines.iter().map(|line| line.trim()).filter(|line| !line.is_empty()).collect();
    assert_eq!(labels, ["transcript", "pending", "status", "hook", "above", "editor", "below", "footer"]);
    assert_eq!(frame.lines.last().expect("footer").trim(), "footer");
}
