//! Port of senpi `packages/tui/test/layout.test.ts`.
//!
//! Two senpi cases are not ported here: "crops Kitty images at a scroll view's lower boundary"
//! needs the Kitty metadata registry owned by todo 9, and "renders a proportional glyph
//! scrollbar with an expanded active thumb" is a multi-stage timer scenario whose scrollbar
//! geometry the `tui-screen-*` golden cases already pin byte-for-byte. "omits gaps around
//! invisible entries" needs a `visible` predicate on stack entries, which `StackChild` does not
//! expose; it ports with the interactive layer that owns entry visibility.

use std::cell::RefCell;
use std::rc::Rc;

use maho_tui::components::h_stack::HStack;
use maho_tui::components::scroll_view::{ScrollView, ScrollViewFollow, ScrollViewOptions, ScrollViewScrollbar};
use maho_tui::components::stack::{StackChild, StackEntryOptions, StackOptions};
use maho_tui::components::text::Text;
use maho_tui::components::v_stack::VStack;
use maho_tui::layout::{render_layout_frame, LayoutBox};
use maho_tui::layout_node::StackBasis;
use maho_tui::tui::Component;
use maho_tui::utils::strip_terminal_sequences;

fn text(value: &str) -> Rc<RefCell<dyn Component>> {
    Rc::new(RefCell::new(Text::with_padding(value, 0, 0)))
}

fn entry(component: Rc<RefCell<dyn Component>>, options: StackEntryOptions) -> StackChild {
    StackChild::Entry { component, options }
}

fn basis(value: usize) -> StackEntryOptions {
    StackEntryOptions {
        basis: Some(StackBasis::Fixed(value)),
        ..StackEntryOptions::default()
    }
}

fn visible_lines(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .map(|line| strip_terminal_sequences(line).trim_end().to_string())
        .collect()
}

fn child(frame: &maho_tui::layout::LayoutFrame, index: usize) -> &LayoutBox {
    &frame.root.children[index]
}

fn vstack(children: Vec<StackChild>, options: StackOptions) -> Rc<RefCell<dyn Component>> {
    Rc::new(RefCell::new(VStack::new(children, options)))
}

fn hstack(children: Vec<StackChild>, options: StackOptions) -> Rc<RefCell<dyn Component>> {
    Rc::new(RefCell::new(HStack::new(children, options)))
}

#[test]
fn allocates_vertical_grow_space_deterministically() {
    let frame = render_layout_frame(
        &vstack(
            vec![
                entry(text("top"), basis(1)),
                entry(
                    text("body"),
                    StackEntryOptions {
                        basis: Some(StackBasis::Fixed(0)),
                        grow: Some(1),
                        shrink: Some(1),
                        ..StackEntryOptions::default()
                    },
                ),
            ],
            StackOptions::default(),
        ),
        10,
        4,
    );

    let heights: Vec<usize> = frame.root.children.iter().map(|child| child.rect.height).collect();
    assert_eq!(heights, vec![1, 3]);
    assert_eq!(visible_lines(&frame.lines), vec!["top", "body", "", ""]);
}

#[test]
fn does_not_render_fixed_basis_scroll_content_during_stack_measurement() {
    let render_count = Rc::new(RefCell::new(0usize));
    let counter = Rc::clone(&render_count);
    let transcript: Rc<RefCell<dyn Component>> = Rc::new(RefCell::new(CountingComponent {
        lines: vec!["one".to_string(), "two".to_string(), "three".to_string()],
        count: counter,
    }));
    let root = vstack(
        vec![
            entry(
                Rc::new(RefCell::new(ScrollView::new(
                    transcript,
                    ScrollViewOptions {
                        follow: ScrollViewFollow::End,
                        ..ScrollViewOptions::default()
                    },
                ))),
                StackEntryOptions {
                    basis: Some(StackBasis::Fixed(0)),
                    grow: Some(1),
                    ..StackEntryOptions::default()
                },
            ),
            entry(text("dock"), StackEntryOptions { basis: Some(StackBasis::Auto), ..StackEntryOptions::default() }),
        ],
        StackOptions::default(),
    );
    render_layout_frame(&root, 10, 3);
    assert_eq!(*render_count.borrow(), 1);
}

#[test]
fn paints_only_clipped_rows_from_very_large_scroll_content() {
    let transcript: Rc<RefCell<dyn Component>> = Rc::new(RefCell::new(VirtualLinesComponent {
        line_count: 1_000_000_000,
        overrides: vec![
            (999_999_996, "before".to_string()),
            (999_999_997, "visible 1".to_string()),
            (999_999_998, "visible 2".to_string()),
            (999_999_999, "visible 3".to_string()),
        ],
    }));
    let scroll_view: Rc<RefCell<dyn Component>> = Rc::new(RefCell::new(ScrollView::new(
        transcript,
        ScrollViewOptions {
            follow: ScrollViewFollow::End,
            ..ScrollViewOptions::default()
        },
    )));
    let frame = render_layout_frame(&scroll_view, 10, 3);
    assert_eq!(visible_lines(&frame.lines), vec!["visible 1", "visible 2", "visible 3"]);
}

#[test]
fn shrinks_entries_to_their_minimum_sizes() {
    let frame = render_layout_frame(
        &vstack(
            vec![
                entry(
                    text("a1\na2\na3"),
                    StackEntryOptions {
                        shrink: Some(1),
                        min_size: Some(1),
                        ..StackEntryOptions::default()
                    },
                ),
                entry(text("b1\nb2\nb3"), StackEntryOptions { shrink: Some(0), ..StackEntryOptions::default() }),
            ],
            StackOptions::default(),
        ),
        10,
        4,
    );
    let heights: Vec<usize> = frame.root.children.iter().map(|child| child.rect.height).collect();
    assert_eq!(heights, vec![1, 3]);
    assert_eq!(visible_lines(&frame.lines), vec!["a1", "b1", "b2", "b3"]);
}

#[test]
fn includes_nested_minimum_sizes_in_intrinsic_stack_measurement() {
    let dock = vstack(
        vec![
            StackChild::Component(text("top1\ntop2\ntop3")),
            entry(text("selector"), StackEntryOptions { min_size: Some(3), ..StackEntryOptions::default() }),
            StackChild::Component(text("below")),
            entry(text("footer"), StackEntryOptions { min_size: Some(1), ..StackEntryOptions::default() }),
        ],
        StackOptions::default(),
    );
    let frame = render_layout_frame(
        &vstack(
            vec![
                entry(
                    text("body"),
                    StackEntryOptions {
                        basis: Some(StackBasis::Fixed(0)),
                        grow: Some(1),
                        min_size: Some(1),
                        ..StackEntryOptions::default()
                    },
                ),
                entry(
                    dock,
                    StackEntryOptions {
                        basis: Some(StackBasis::Auto),
                        min_size: Some(1),
                        ..StackEntryOptions::default()
                    },
                ),
            ],
            StackOptions::default(),
        ),
        10,
        9,
    );
    assert_eq!(
        visible_lines(&frame.lines),
        vec!["body", "top1", "top2", "top3", "selector", "", "", "below", "footer"]
    );
}

#[test]
fn composes_horizontal_children_at_allocated_widths() {
    let frame = render_layout_frame(
        &hstack(
            vec![
                entry(text("left"), StackEntryOptions { basis: Some(StackBasis::Fixed(6)), shrink: Some(0), ..StackEntryOptions::default() }),
                entry(text("right"), StackEntryOptions { basis: Some(StackBasis::Fixed(6)), shrink: Some(0), ..StackEntryOptions::default() }),
            ],
            StackOptions::default(),
        ),
        12,
        1,
    );
    assert_eq!(visible_lines(&frame.lines), vec!["left  right"]);
}

#[test]
fn does_not_paint_zero_width_horizontal_children() {
    let frame = render_layout_frame(
        &hstack(
            vec![
                entry(text("hidden"), StackEntryOptions { basis: Some(StackBasis::Fixed(0)), shrink: Some(0), ..StackEntryOptions::default() }),
                entry(
                    text("shown"),
                    StackEntryOptions {
                        basis: Some(StackBasis::Fixed(0)),
                        grow: Some(1),
                        ..StackEntryOptions::default()
                    },
                ),
            ],
            StackOptions::default(),
        ),
        5,
        1,
    );
    assert_eq!(visible_lines(&frame.lines), vec!["shown"]);
}

#[test]
fn tracks_follow_end_state_and_returns_unused_scroll_delta() {
    let scroll_view = ScrollView::new(
        text("1\n2\n3\n4\n5\n6"),
        ScrollViewOptions {
            follow: ScrollViewFollow::End,
            primary: true,
            ..ScrollViewOptions::default()
        },
    );
    let scroll_view = Rc::new(RefCell::new(scroll_view));
    render_layout_frame(&(Rc::clone(&scroll_view) as Rc<RefCell<dyn Component>>), 10, 3);

    assert_eq!(scroll_view.borrow().scroll_top(), 3);
    assert!(scroll_view.borrow().is_following_end());

    assert_eq!(scroll_view.borrow_mut().scroll_by(-2, 0), 0);
    assert_eq!(scroll_view.borrow().scroll_top(), 1);
    assert!(!scroll_view.borrow().is_following_end());
    assert_eq!(scroll_view.borrow_mut().scroll_by(-3, 0), -2);
    assert_eq!(scroll_view.borrow().scroll_top(), 0);
    assert_eq!(scroll_view.borrow_mut().scroll_by(10, 0), 7);
    assert_eq!(scroll_view.borrow().scroll_top(), 3);
    assert!(scroll_view.borrow().is_following_end());
}

#[test]
fn preserves_only_the_underlying_background_beneath_overlay_scrollbar_glyphs() {
    let background = "\x1b[42m";
    let border_foreground = "\x1b[31m";
    let content: Rc<RefCell<dyn Component>> = Rc::new(RefCell::new(BackgroundComponent {
        background: background.to_string(),
        border_foreground: border_foreground.to_string(),
    }));
    let scroll_view = Rc::new(RefCell::new(ScrollView::new(
        content,
        ScrollViewOptions {
            scrollbar: ScrollViewScrollbar::Auto,
            scrollbar_track_style: Rc::new(|text: &str| text.to_string()),
            scrollbar_thumb_style: Rc::new(|text: &str| text.to_string()),
            ..ScrollViewOptions::default()
        },
    )));
    render_layout_frame(&(Rc::clone(&scroll_view) as Rc<RefCell<dyn Component>>), 6, 4);
    scroll_view.borrow_mut().scroll_by(1, 0);
    let frame = render_layout_frame(&(Rc::clone(&scroll_view) as Rc<RefCell<dyn Component>>), 6, 4);

    let stripped: Vec<String> = frame.lines.iter().map(|line| strip_terminal_sequences(line)).collect();
    assert_eq!(stripped, vec!["xxxxx│", "xxxxx┃", "xxxxx┃", "xxxxx│"]);
    for line in &frame.lines {
        assert!(line.contains(background), "{line:?} lost the content background");
        assert!(!line.contains(border_foreground), "{line:?} kept the border foreground");
        assert!(
            line.contains(&format!("\x1b[0m\x1b]8;;\x07{background}")),
            "{line:?} did not restore the content background after the scrollbar glyph"
        );
    }
}

#[test]
fn updates_reserved_scrollbar_layout_at_runtime() {
    let scroll_view = Rc::new(RefCell::new(ScrollView::new(
        text("123456"),
        ScrollViewOptions {
            scrollbar: ScrollViewScrollbar::Always,
            ..ScrollViewOptions::default()
        },
    )));
    let render = || {
        let root = hstack(
            vec![StackChild::Component(Rc::clone(&scroll_view) as Rc<RefCell<dyn Component>>)],
            StackOptions {
                align: maho_tui::layout_node::StackAlign::Start,
                ..StackOptions::default()
            },
        );
        render_layout_frame(&root, 6, 2)
    };

    let always = render();
    assert_eq!(visible_lines(&always.lines), vec!["12345┃", "6    ┃"]);
    assert_eq!(child(&always, 0).rect.width, 6);
    assert_eq!(child(&always, 0).children[0].rect.width, 5);

    scroll_view.borrow_mut().set_scrollbar(ScrollViewScrollbar::Hidden);
    assert_eq!(child(&render(), 0).children[0].rect.width, 6);
    assert!(!scroll_view.borrow().is_scrollbar_visible());
}

#[test]
fn measures_nested_scroll_content_from_constrained_child_geometry() {
    let inner = Rc::new(RefCell::new(ScrollView::new(text("1\n2\n3\n4\n5\n6"), ScrollViewOptions::default())));
    let outer = Rc::new(RefCell::new(ScrollView::new(
        vstack(
            vec![
                entry(Rc::clone(&inner) as Rc<RefCell<dyn Component>>, basis(2)),
                StackChild::Component(text("tail")),
            ],
            StackOptions::default(),
        ),
        ScrollViewOptions::default(),
    )));
    render_layout_frame(&(Rc::clone(&outer) as Rc<RefCell<dyn Component>>), 10, 2);

    assert_eq!(inner.borrow().viewport_height(), 2);
    assert_eq!(outer.borrow_mut().scroll_by(10, 0), 9);
    assert_eq!(outer.borrow().scroll_top(), 1);
}

#[test]
fn rebuilds_geometry_after_content_changes() {
    let text_component = Rc::new(RefCell::new(Text::with_padding("one", 0, 0)));
    let root = vstack(
        vec![StackChild::Component(Rc::clone(&text_component) as Rc<RefCell<dyn Component>>)],
        StackOptions::default(),
    );
    let first = render_layout_frame(&root, 10, 4);
    text_component.borrow_mut().set_text("one\ntwo\nthree");
    let second = render_layout_frame(&root, 10, 4);

    assert_eq!(child(&first, 0).lines.as_ref().map(Vec::len), Some(1));
    assert_eq!(child(&second, 0).lines.as_ref().map(Vec::len), Some(3));
}

struct CountingComponent {
    lines: Vec<String>,
    count: Rc<RefCell<usize>>,
}

impl Component for CountingComponent {
    fn render(&mut self, _width: usize) -> Vec<String> {
        *self.count.borrow_mut() += 1;
        self.lines.clone()
    }
}

struct VirtualLinesComponent {
    line_count: usize,
    overrides: Vec<(usize, String)>,
}

impl Component for VirtualLinesComponent {
    fn render(&mut self, _width: usize) -> Vec<String> {
        let mut lines = vec![String::new(); self.line_count];
        for (index, value) in &self.overrides {
            lines[*index] = value.clone();
        }
        lines
    }
}

struct BackgroundComponent {
    background: String,
    border_foreground: String,
}

impl Component for BackgroundComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        (0..8)
            .map(|_| {
                format!(
                    "{}{}{}│\x1b[39m\x1b[49m",
                    self.background,
                    "x".repeat(width.saturating_sub(1)),
                    self.border_foreground
                )
            })
            .collect()
    }
}
