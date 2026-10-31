//! Port of senpi `packages/tui/test/layout.test.ts`.
//!
//! One senpi case is not ported: "omits gaps around invisible entries" builds a `VStack` entry
//! with a `visible` predicate, and `StackEntryOptions` (src/components/stack.rs, outside this
//! lane's owned paths) carries no `visible` field yet, although `StackLayoutEntry.visible` and
//! `visible_stack_entries` already exist. The scrollbar case drives senpi's 30 ms hide timer
//! through the explicit clock (`set_scrollbar_active(_, now_ms)`, `fire_hide_timer_if_due`).

use std::cell::RefCell;
use std::rc::Rc;

use maho_tui::components::h_stack::HStack;
use maho_tui::components::scroll_view::{
    ScrollView, ScrollViewFollow, ScrollViewOptions, ScrollViewScrollbar,
};
use maho_tui::components::stack::{StackChild, StackEntryOptions, StackOptions};
use maho_tui::components::text::Text;
use maho_tui::components::v_stack::VStack;
use maho_tui::layout::{LayoutBox, render_layout_frame};
use maho_tui::layout_node::StackBasis;
use maho_tui::terminal_image::{
    KittyEncodeOptions, KittyImageMetadata, encode_kitty, register_kitty_image_metadata,
};
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

    let heights: Vec<usize> = frame
        .root
        .children
        .iter()
        .map(|child| child.rect.height)
        .collect();
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
                ScrollView::new(
                    transcript,
                    ScrollViewOptions {
                        follow: ScrollViewFollow::End,
                        ..ScrollViewOptions::default()
                    },
                ),
                StackEntryOptions {
                    basis: Some(StackBasis::Fixed(0)),
                    grow: Some(1),
                    ..StackEntryOptions::default()
                },
            ),
            entry(
                text("dock"),
                StackEntryOptions {
                    basis: Some(StackBasis::Auto),
                    ..StackEntryOptions::default()
                },
            ),
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
    let scroll_view: Rc<RefCell<dyn Component>> = ScrollView::new(
        transcript,
        ScrollViewOptions {
            follow: ScrollViewFollow::End,
            ..ScrollViewOptions::default()
        },
    );
    let frame = render_layout_frame(&scroll_view, 10, 3);
    assert_eq!(
        visible_lines(&frame.lines),
        vec!["visible 1", "visible 2", "visible 3"]
    );
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
                entry(
                    text("b1\nb2\nb3"),
                    StackEntryOptions {
                        shrink: Some(0),
                        ..StackEntryOptions::default()
                    },
                ),
            ],
            StackOptions::default(),
        ),
        10,
        4,
    );
    let heights: Vec<usize> = frame
        .root
        .children
        .iter()
        .map(|child| child.rect.height)
        .collect();
    assert_eq!(heights, vec![1, 3]);
    assert_eq!(visible_lines(&frame.lines), vec!["a1", "b1", "b2", "b3"]);
}

#[test]
fn includes_nested_minimum_sizes_in_intrinsic_stack_measurement() {
    let dock = vstack(
        vec![
            StackChild::Component(text("top1\ntop2\ntop3")),
            entry(
                text("selector"),
                StackEntryOptions {
                    min_size: Some(3),
                    ..StackEntryOptions::default()
                },
            ),
            StackChild::Component(text("below")),
            entry(
                text("footer"),
                StackEntryOptions {
                    min_size: Some(1),
                    ..StackEntryOptions::default()
                },
            ),
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
        vec![
            "body", "top1", "top2", "top3", "selector", "", "", "below", "footer"
        ]
    );
}

#[test]
fn composes_horizontal_children_at_allocated_widths() {
    let frame = render_layout_frame(
        &hstack(
            vec![
                entry(
                    text("left"),
                    StackEntryOptions {
                        basis: Some(StackBasis::Fixed(6)),
                        shrink: Some(0),
                        ..StackEntryOptions::default()
                    },
                ),
                entry(
                    text("right"),
                    StackEntryOptions {
                        basis: Some(StackBasis::Fixed(6)),
                        shrink: Some(0),
                        ..StackEntryOptions::default()
                    },
                ),
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
                entry(
                    text("hidden"),
                    StackEntryOptions {
                        basis: Some(StackBasis::Fixed(0)),
                        shrink: Some(0),
                        ..StackEntryOptions::default()
                    },
                ),
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
fn crops_kitty_images_at_a_scroll_views_lower_boundary() {
    let image_id = 124;
    let image_line = encode_kitty(
        "AAAA",
        &KittyEncodeOptions {
            columns: Some(2),
            rows: Some(3),
            image_id: Some(image_id),
            move_cursor: false,
            virtual_placement: false,
        },
    );
    register_kitty_image_metadata(KittyImageMetadata {
        image_id,
        columns: 2,
        rows: 3,
        width_px: 100,
        height_px: 100,
    });
    let transcript_lines: Rc<RefCell<dyn Component>> = Rc::new(RefCell::new(StaticLines(vec![
        "one".to_string(),
        "two".to_string(),
        image_line,
        String::new(),
        String::new(),
    ])));
    let transcript = ScrollView::new(transcript_lines, ScrollViewOptions::default());
    let frame = render_layout_frame(
        &vstack(
            vec![
                entry(
                    transcript as Rc<RefCell<dyn Component>>,
                    StackEntryOptions {
                        basis: Some(StackBasis::Fixed(0)),
                        grow: Some(1),
                        ..StackEntryOptions::default()
                    },
                ),
                StackChild::Component(text("dock")),
            ],
            StackOptions::default(),
        ),
        20,
        4,
    );

    assert!(
        frame.lines[2].contains("y=0,h=34,r=1"),
        "{:?}",
        frame.lines[2]
    );
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
    render_layout_frame(
        &(Rc::clone(&scroll_view) as Rc<RefCell<dyn Component>>),
        10,
        3,
    );

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
fn renders_a_proportional_glyph_scrollbar_with_an_expanded_active_thumb() {
    let source_lines = [
        "abcd界", "abcde2", "abcde3", "abcde4", "abcde5", "abcde6", "abcde7", "abcde8",
    ];
    let content_background = "\x1b[42m";
    let track_color = "\x1b[38;5;2m";
    let thumb_color = "\x1b[38;5;1m";
    let track_style: Rc<dyn Fn(&str) -> String> =
        Rc::new(|text: &str| format!("\x1b[38;5;2m{text}\x1b[39m"));
    let thumb_style: Rc<dyn Fn(&str) -> String> =
        Rc::new(|text: &str| format!("\x1b[38;5;1m{text}\x1b[39m"));
    let content = Rc::new(RefCell::new(Text::with_padding(
        source_lines.join("\n"),
        0,
        0,
    )));
    content
        .borrow_mut()
        .set_custom_bg_fn(Some(Rc::new(|text: &str| {
            format!("\x1b[42m{text}\x1b[49m")
        })));
    let content: Rc<RefCell<dyn Component>> = content;
    let scroll_view = ScrollView::new(
        Rc::clone(&content),
        ScrollViewOptions {
            scrollbar: ScrollViewScrollbar::Auto,
            scrollbar_track_style: Rc::clone(&track_style),
            scrollbar_thumb_style: Rc::clone(&thumb_style),
            scrollbar_hide_delay_ms: 10,
            ..ScrollViewOptions::default()
        },
    );
    let root = Rc::clone(&scroll_view) as Rc<RefCell<dyn Component>>;
    let render = || render_layout_frame(&root, 6, 4).lines;
    let visible = |lines: &[String]| -> Vec<String> {
        lines
            .iter()
            .map(|line| strip_terminal_sequences(line))
            .collect()
    };
    let contains = |lines: &[String], needle: &str| -> Vec<bool> {
        lines.iter().map(|line| line.contains(needle)).collect()
    };

    let lines = render();
    assert_eq!(visible(&lines), source_lines[..4].to_vec());

    scroll_view.borrow_mut().scroll_by(2, 0);
    let lines = render();
    assert_eq!(
        visible(&lines),
        vec!["abcde│", "abcde┃", "abcde┃", "abcde│"]
    );
    assert_eq!(
        contains(&lines, track_color),
        vec![true, false, false, true]
    );
    assert_eq!(
        contains(&lines, thumb_color),
        vec![false, true, true, false]
    );

    scroll_view.borrow_mut().set_scrollbar_active(true, 0);
    let lines = render();
    assert_eq!(
        visible(&lines),
        vec!["abcde│", "abcde█", "abcde█", "abcde│"]
    );
    assert_eq!(
        contains(&lines, thumb_color),
        vec![false, true, true, false]
    );
    assert!(lines[1].rfind(content_background) < lines[1].rfind(thumb_color));

    // senpi releases the active thumb, then waits past its 10 ms hide delay.
    scroll_view.borrow_mut().set_scrollbar_active(false, 0);
    assert!(scroll_view.borrow_mut().fire_hide_timer_if_due(30));
    let lines = render();
    assert_eq!(visible(&lines), source_lines[2..6].to_vec());

    scroll_view.borrow_mut().scroll_to_end(30);
    let lines = render();
    assert_eq!(
        visible(&lines),
        vec!["abcde│", "abcde│", "abcde┃", "abcde┃"]
    );

    scroll_view.borrow_mut().scroll_to_start(30);
    let lines = render();
    assert_eq!(visible(&lines)[0], "abcd ┃");

    let has_glyph = |line: &String| {
        let stripped = strip_terminal_sequences(line);
        stripped.contains('│') || stripped.contains('┃')
    };

    let followed_content = Rc::new(RefCell::new(Text::with_padding(
        source_lines.join("\n"),
        0,
        0,
    )));
    let followed = ScrollView::new(
        Rc::clone(&followed_content) as Rc<RefCell<dyn Component>>,
        ScrollViewOptions {
            follow: ScrollViewFollow::End,
            scrollbar: ScrollViewScrollbar::Auto,
            scrollbar_track_style: Rc::clone(&track_style),
            scrollbar_thumb_style: Rc::clone(&thumb_style),
            ..ScrollViewOptions::default()
        },
    );
    let followed_root = Rc::clone(&followed) as Rc<RefCell<dyn Component>>;
    render_layout_frame(&followed_root, 6, 4);
    assert_eq!(followed.borrow().scroll_top(), 4);
    followed_content
        .borrow_mut()
        .set_text(format!("{}\nabcde9", source_lines.join("\n")));
    let growth_frame = render_layout_frame(&followed_root, 6, 4);
    assert_eq!(followed.borrow().scroll_top(), 5);
    assert!(!growth_frame.lines.iter().any(has_glyph));

    let fitting_content: Rc<RefCell<dyn Component>> =
        Rc::new(RefCell::new(Text::with_padding("1\n2", 0, 0)));
    let automatic = ScrollView::new(
        Rc::clone(&fitting_content),
        ScrollViewOptions {
            scrollbar: ScrollViewScrollbar::Auto,
            scrollbar_thumb_style: Rc::clone(&thumb_style),
            ..ScrollViewOptions::default()
        },
    );
    let automatic_root = Rc::clone(&automatic) as Rc<RefCell<dyn Component>>;
    render_layout_frame(&automatic_root, 6, 4);
    automatic.borrow_mut().scroll_by(1, 0);
    assert!(
        !render_layout_frame(&automatic_root, 6, 4)
            .lines
            .iter()
            .any(has_glyph)
    );

    let always_fitting = ScrollView::new(
        Rc::clone(&fitting_content),
        ScrollViewOptions {
            scrollbar: ScrollViewScrollbar::Always,
            scrollbar_thumb_style: Rc::clone(&thumb_style),
            ..ScrollViewOptions::default()
        },
    );
    let always_fitting_frame =
        render_layout_frame(&(always_fitting as Rc<RefCell<dyn Component>>), 6, 4);
    assert_eq!(always_fitting_frame.root.children[0].rect.width, 5);
    assert!(
        visible(&always_fitting_frame.lines)
            .iter()
            .all(|line| line.ends_with('┃'))
    );

    let always_overflowing = ScrollView::new(
        Rc::clone(&content),
        ScrollViewOptions {
            scrollbar: ScrollViewScrollbar::Always,
            scrollbar_track_style: Rc::clone(&track_style),
            scrollbar_thumb_style: Rc::clone(&thumb_style),
            ..ScrollViewOptions::default()
        },
    );
    let always_overflowing_frame =
        render_layout_frame(&(always_overflowing as Rc<RefCell<dyn Component>>), 6, 4);
    assert_eq!(always_overflowing_frame.root.children[0].rect.width, 5);
    let overflowing = visible(&always_overflowing_frame.lines);
    assert_eq!(
        overflowing
            .iter()
            .filter(|line| line.ends_with('┃'))
            .count(),
        2
    );
    assert_eq!(
        overflowing
            .iter()
            .filter(|line| line.ends_with('│'))
            .count(),
        2
    );
    let reset = "\x1b[0m\x1b]8;;\x07";
    // JavaScript's `lastIndexOf(needle, from)`: the last match starting at or before `from`.
    let last_index_before = |line: &str, needle: &str, from: usize| -> i64 {
        line.match_indices(needle)
            .map(|(index, _)| index)
            .filter(|index| *index <= from)
            .last()
            .map_or(-1, |index| index as i64)
    };
    let last_index =
        |line: &str, needle: &str| -> i64 { line.rfind(needle).map_or(-1, |index| index as i64) };
    for line in &always_overflowing_frame.lines {
        let scrollbar_style_index =
            last_index(line, track_color).max(last_index(line, thumb_color));
        assert!(scrollbar_style_index >= 0, "{line:?}");
        let reserved_column_reset_index =
            last_index_before(line, reset, scrollbar_style_index as usize);
        assert!(
            reserved_column_reset_index > last_index(line, content_background),
            "{line:?}"
        );
    }

    let thumb_height_for = |content_height: usize| -> usize {
        let body = vec!["x"; content_height].join("\n");
        let sized = ScrollView::new(
            Rc::new(RefCell::new(Text::with_padding(body, 0, 0))),
            ScrollViewOptions {
                scrollbar: ScrollViewScrollbar::Auto,
                scrollbar_thumb_style: Rc::clone(&thumb_style),
                ..ScrollViewOptions::default()
            },
        );
        let sized_root = Rc::clone(&sized) as Rc<RefCell<dyn Component>>;
        render_layout_frame(&sized_root, 6, 20);
        sized.borrow_mut().scroll_by(1, 0);
        render_layout_frame(&sized_root, 6, 20)
            .lines
            .iter()
            .filter(|line| strip_terminal_sequences(line).ends_with('┃'))
            .count()
    };
    assert_eq!(thumb_height_for(21), 19);
    assert_eq!(thumb_height_for(40), 10);
    assert_eq!(thumb_height_for(100), 4);
    assert_eq!(thumb_height_for(400), 2);
}

#[test]
fn preserves_only_the_underlying_background_beneath_overlay_scrollbar_glyphs() {
    let background = "\x1b[42m";
    let border_foreground = "\x1b[31m";
    let content: Rc<RefCell<dyn Component>> = Rc::new(RefCell::new(BackgroundComponent {
        background: background.to_string(),
        border_foreground: border_foreground.to_string(),
    }));
    let scroll_view = ScrollView::new(
        content,
        ScrollViewOptions {
            scrollbar: ScrollViewScrollbar::Auto,
            scrollbar_track_style: Rc::new(|text: &str| text.to_string()),
            scrollbar_thumb_style: Rc::new(|text: &str| text.to_string()),
            ..ScrollViewOptions::default()
        },
    );
    render_layout_frame(
        &(Rc::clone(&scroll_view) as Rc<RefCell<dyn Component>>),
        6,
        4,
    );
    scroll_view.borrow_mut().scroll_by(1, 0);
    let frame = render_layout_frame(
        &(Rc::clone(&scroll_view) as Rc<RefCell<dyn Component>>),
        6,
        4,
    );

    let stripped: Vec<String> = frame
        .lines
        .iter()
        .map(|line| strip_terminal_sequences(line))
        .collect();
    assert_eq!(stripped, vec!["xxxxx│", "xxxxx┃", "xxxxx┃", "xxxxx│"]);
    for line in &frame.lines {
        assert!(
            line.contains(background),
            "{line:?} lost the content background"
        );
        assert!(
            !line.contains(border_foreground),
            "{line:?} kept the border foreground"
        );
        assert!(
            line.contains(&format!("\x1b[0m\x1b]8;;\x07{background}")),
            "{line:?} did not restore the content background after the scrollbar glyph"
        );
    }
}

#[test]
fn updates_reserved_scrollbar_layout_at_runtime() {
    let scroll_view = ScrollView::new(
        text("123456"),
        ScrollViewOptions {
            scrollbar: ScrollViewScrollbar::Always,
            ..ScrollViewOptions::default()
        },
    );
    let render = || {
        let root = hstack(
            vec![StackChild::Component(
                Rc::clone(&scroll_view) as Rc<RefCell<dyn Component>>
            )],
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

    scroll_view
        .borrow_mut()
        .set_scrollbar(ScrollViewScrollbar::Hidden);
    assert_eq!(child(&render(), 0).children[0].rect.width, 6);
    assert!(!scroll_view.borrow().is_scrollbar_visible());
}

#[test]
fn measures_nested_scroll_content_from_constrained_child_geometry() {
    let inner = ScrollView::new(text("1\n2\n3\n4\n5\n6"), ScrollViewOptions::default());
    let outer = ScrollView::new(
        vstack(
            vec![
                entry(Rc::clone(&inner) as Rc<RefCell<dyn Component>>, basis(2)),
                StackChild::Component(text("tail")),
            ],
            StackOptions::default(),
        ),
        ScrollViewOptions::default(),
    );
    render_layout_frame(&(Rc::clone(&outer) as Rc<RefCell<dyn Component>>), 10, 2);

    assert_eq!(inner.borrow().viewport_height(), 2);
    assert_eq!(outer.borrow_mut().scroll_by(10, 0), 9);
    assert_eq!(outer.borrow().scroll_top(), 1);
}

#[test]
fn rebuilds_geometry_after_content_changes() {
    let text_component = Rc::new(RefCell::new(Text::with_padding("one", 0, 0)));
    let root = vstack(
        vec![StackChild::Component(
            Rc::clone(&text_component) as Rc<RefCell<dyn Component>>
        )],
        StackOptions::default(),
    );
    let first = render_layout_frame(&root, 10, 4);
    text_component.borrow_mut().set_text("one\ntwo\nthree");
    let second = render_layout_frame(&root, 10, 4);

    assert_eq!(
        child(&first, 0).lines.as_ref().map(|lines| lines.len()),
        Some(1)
    );
    assert_eq!(
        child(&second, 0).lines.as_ref().map(|lines| lines.len()),
        Some(3)
    );
}

struct StaticLines(Vec<String>);

impl Component for StaticLines {
    fn render(&mut self, _width: usize) -> Vec<String> {
        self.0.clone()
    }
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
        // senpi materializes only the four assigned indices of a sparse array of
        // `line_count` entries; this port declares the count through
        // `virtual_line_count` and answers rows through `render_virtual_row`, so the
        // layout engine never calls `render` (see `Component::virtual_line_count`).
        unreachable!("VirtualLinesComponent is read through the virtual-line seam")
    }

    fn virtual_line_count(&self, _width: usize) -> Option<usize> {
        Some(self.line_count)
    }

    fn render_virtual_row(&mut self, _width: usize, index: usize) -> String {
        self.overrides
            .iter()
            .find(|(row, _)| *row == index)
            .map(|(_, value)| value.clone())
            .unwrap_or_default()
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
