//! Port of senpi `packages/tui/src/tui-alt-screen.ts`.
//!
//! Uses `image_stub.rs` (todo 7's minimal Kitty-image shim) for placement/capability queries
//! instead of the real `terminal-image.ts` port (todo 9); every such call site is annotated
//! and the gap is tracked in `parity.d/7.md`. `render_layout_frame` (see `layout.rs`) already
//! paints scrollbars into its returned `LayoutFrame.lines`, so this file never repaints them -
//! only hit-tests scrollbar geometry for drag/click handling.

use std::cell::RefCell;
use std::rc::Rc;

use crate::alt_screen_search::{
    get_alt_screen_search_match_key, AltScreenSearchComponent, AltScreenSearchIndex, AltScreenSearchMatch,
    NavigationButtonStyle,
};
use crate::components::alt_screen_flash::AltScreenFlashContainer;
use crate::components::scroll_view::{ScrollToOptions, ScrollView, ScrollViewFollow, ScrollViewOptions};
use crate::image_stub::{self, ImageProtocol};
use crate::keybindings::get_keybindings;
use crate::layout::{
    get_layout_boxes_at, get_scroll_view_box, get_scroll_views_at, get_scrollbar_geometry, render_layout_frame,
    strip_osc133_zone_prefix, LayoutFrame,
};
use crate::mouse_input::{
    decode_mouse_button, is_mouse_sequence, parse_sgr_mouse_event, parse_wheel_event, to_tui_mouse_event, MouseTracking,
    SgrMouseEvent, ToTuiMouseEventExtra, WheelEvent,
};
use crate::terminal::Terminal;
use crate::tui::{
    can_receive_keys, composite_tui_line, dispatch_mouse_event, retarget_mouse_event, Component, Container, MouseBlocker,
    OverlayAnchor, OverlayHandle, OverlayMargin, OverlayOptions, SizeValue, TuiBase, TuiMouseButton, TuiMouseDispatchResult,
    TuiMouseDispatchTarget, TuiMouseEvent, TuiMouseEventType, CURSOR_MARKER,
};
use crate::utils::{slice_by_column, strip_terminal_sequences, truncate_to_width, visible_width, word_segments};

/// A double/triple-click word-selection joiner set: fullscreen owns mouse selection and
/// mirrors common terminal word-selection behavior by keeping paths and kebab-case tokens
/// whole (senpi's `TERMINAL_WORD_SELECTION_JOINERS`).
const WORD_SELECTION_JOINERS: [&str; 2] = ["/", "-"];
const DOUBLE_CLICK_INTERVAL_MS: i64 = 500;
const PAGE_SCROLL_OVERLAP: i64 = 4;

/// senpi's `OSC133_PROMPT_START`: an OSC 133 `A` (prompt start) marker at the start of a line.
fn is_osc133_prompt_start(line: &str) -> bool {
    line.starts_with("\x1b]133;A\x07") || line.starts_with("\x1b]133;A\x1b\\")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct SelectionPoint {
    row: usize,
    col: usize,
}

#[derive(Debug, Clone, Copy)]
struct Selection {
    anchor: SelectionPoint,
    focus: SelectionPoint,
}

impl Selection {
    fn ordered(&self) -> (SelectionPoint, SelectionPoint) {
        if self.anchor <= self.focus {
            (self.anchor, self.focus)
        } else {
            (self.focus, self.anchor)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum SelectionGranularity {
    Character,
    Word,
    Line,
}

struct LastClick {
    timestamp_ms: i64,
    count: u32,
    row: usize,
    word_start: usize,
    word_end: usize,
}

struct ScrollbarDrag {
    scroll_view: Rc<RefCell<ScrollView>>,
    grab_offset: i64,
}

/// senpi's `lastComponentClick`.
struct LastComponentClick {
    timestamp_ms: i64,
    count: u32,
    component: Rc<RefCell<dyn Component>>,
    x: i64,
    y: i64,
}

const ALT_WHEEL_SCROLL_MULTIPLIER: i64 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchSelectionMode {
    Query,
    Retain,
    Next,
    Previous,
}

/// senpi's `ActiveSearch`. The component's query callback cannot borrow the renderer, so it
/// stores the new query in `pending_query`, which the renderer drains (senpi calls
/// `updateSearchQuery` directly from the callback).
struct ActiveSearch {
    component: Rc<RefCell<AltScreenSearchComponent>>,
    index: AltScreenSearchIndex,
    overlay: OverlayHandle,
    pending_query: Rc<RefCell<Option<String>>>,
    query: String,
    matches: Vec<AltScreenSearchMatch>,
    selected_index: i64,
    selected_key: Option<String>,
    anchor_row: usize,
    selection_mode: SearchSelectionMode,
}

const MAX_CACHED_OFFSCREEN_KITTY_IMAGES: usize = 16;
const MAX_CACHED_OFFSCREEN_KITTY_TRANSMISSION_BYTES: u64 = 32 * 1024 * 1024;
const MAX_CACHED_OFFSCREEN_KITTY_DECODED_BYTES: u64 = 64 * 1024 * 1024;

const ENTER_ALT_SCREEN: &str = "\x1b[?1049h";
const EXIT_ALT_SCREEN: &str = "\x1b[?1049l";
const DISABLE_AUTOWRAP: &str = "\x1b[?7l";
const ENABLE_AUTOWRAP: &str = "\x1b[?7h";
const ENABLE_ALL_MOTION_MOUSE: &str = "\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1004h\x1b[?1006h";
const ENABLE_BUTTON_MOTION_MOUSE: &str = "\x1b[?1000h\x1b[?1002h\x1b[?1006h";
const DISABLE_MOUSE: &str = "\x1b[?1006l\x1b[?1004l\x1b[?1003l\x1b[?1002l\x1b[?1000l";
const BEGIN_SYNCHRONIZED_OUTPUT: &str = "\x1b[?2026h";
const END_SYNCHRONIZED_OUTPUT: &str = "\x1b[?2026l";
const DELETE_ALL_KITTY_IMAGES: &str = "\x1b_Ga=d,d=A,q=2\x1b\\";
const DELETE_ALL_KITTY_PLACEMENTS: &str = "\x1b_Ga=d,d=a,q=2\x1b\\";

#[derive(Debug, Clone, Copy)]
struct CachedKittyImage {
    transmission_generation: u64,
    transmission_bytes: u64,
    estimated_decoded_bytes: u64,
}

/// TUI implementation that switches the terminal to the alternate screen buffer and owns the
/// whole viewport: renders one Component tree as `layout_root`, scrolls it, and layers text
/// selection, a scrollbar, and a find-in-transcript search overlay on top.
pub struct TuiAltScreen {
    pub base: TuiBase,
    layout_root: Option<Rc<RefCell<dyn Component>>>,
    current_layout: Option<LayoutFrame>,
    tracking_enabled: bool,
    mouse_capture_token: Option<u64>,
    selection_anchor: Option<SelectionPoint>,
    selection_focus: Option<SelectionPoint>,
    selection_granularity: SelectionGranularity,
    selection_press_active: bool,
    last_click: Option<LastClick>,
    scrollbar_drag: Option<ScrollbarDrag>,
    scrollbar_hover: Option<Rc<RefCell<ScrollView>>>,
    active_search: Option<ActiveSearch>,
    /// senpi's `searchNavigationButtonStyle` option.
    search_navigation_button_style: NavigationButtonStyle,
    /// senpi's `wheelScrollLines` option (default 1).
    wheel_scroll_lines: i64,
    mouse_capture: Option<TuiMouseDispatchTarget>,
    mouse_press_target: Option<TuiMouseDispatchTarget>,
    mouse_press_point: Option<(i64, i64)>,
    mouse_press_moved: bool,
    last_component_click: Option<LastComponentClick>,
    /// senpi's `uploadedKittyImages` `Map`: insertion order is the eviction order, and a
    /// re-rendered image moves to the end.
    uploaded_kitty_images: Vec<(u32, CachedKittyImage)>,
    saved_capabilities: Option<crate::terminal_image::TerminalCapabilities>,
    alt_screen_active: bool,
    previous_screen: Vec<String>,
    previous_screen_width: usize,
    previous_screen_height: usize,
    last_document: Vec<String>,
    full_redraw_count: u64,
    image_protocol: ImageProtocol,
    flashes: AltScreenFlashContainer,
    scroll_to_end_indicator: Option<Rc<dyn Fn() -> String>>,
    /// senpi's `scrollToEndIndicatorRect`: `(row, column, width)` of the last painted pill.
    scroll_to_end_indicator_rect: Option<(i64, i64, i64)>,
    /// senpi's `implicitDocument`: renders the base container's children when no layout root
    /// is set. Its children are refreshed from `base.children()` before each layout pass because
    /// this port's `TuiBase` owns its `Container` by value.
    implicit_document: Rc<RefCell<Container>>,
    /// senpi's `implicitScrollView`: a primary follow-end view around `implicit_document`.
    implicit_scroll_view: Rc<RefCell<ScrollView>>,
}

pub struct MouseTrackingSupport {
    pub is_tty: bool,
    pub is_termux: bool,
    pub is_windows_without_wt: bool,
}

/// Search-highlight column ranges for one screen row: `(start_col, end_col, is_current)`.
type SearchHighlightRanges = (usize, Vec<(usize, usize, bool)>);

impl TuiAltScreen {
    pub fn new() -> Self {
        let implicit_document = Rc::new(RefCell::new(Container::new()));
        let implicit_scroll_view = ScrollView::new(
            Rc::clone(&implicit_document) as Rc<RefCell<dyn Component>>,
            ScrollViewOptions { follow: ScrollViewFollow::End, primary: true, ..Default::default() },
        );
        Self {
            base: TuiBase::new(),
            layout_root: None,
            current_layout: None,
            tracking_enabled: false,
            mouse_capture_token: None,
            selection_anchor: None,
            selection_focus: None,
            selection_granularity: SelectionGranularity::Character,
            selection_press_active: false,
            last_click: None,
            scrollbar_drag: None,
            scrollbar_hover: None,
            active_search: None,
            search_navigation_button_style: Rc::new(|text: &str, _hovered: bool| text.to_string()),
            wheel_scroll_lines: 1,
            mouse_capture: None,
            mouse_press_target: None,
            mouse_press_point: None,
            mouse_press_moved: false,
            last_component_click: None,
            uploaded_kitty_images: Vec::new(),
            saved_capabilities: None,
            alt_screen_active: false,
            previous_screen: Vec::new(),
            previous_screen_width: 0,
            previous_screen_height: 0,
            last_document: Vec::new(),
            full_redraw_count: 0,
            image_protocol: ImageProtocol::None,
            flashes: AltScreenFlashContainer::new(),
            scroll_to_end_indicator: None,
            scroll_to_end_indicator_rect: None,
            implicit_document,
            implicit_scroll_view,
        }
    }

    /// senpi's `getPrimaryScrollView`: the laid-out primary view, else the implicit one.
    fn get_primary_scroll_view(&self) -> Rc<RefCell<ScrollView>> {
        self.current_layout
            .as_ref()
            .and_then(|layout| layout.primary_scroll_view.clone())
            .unwrap_or_else(|| Rc::clone(&self.implicit_scroll_view))
    }

    /// senpi's `layoutRoot ?? implicitScrollView`.
    fn get_render_root(&mut self) -> Rc<RefCell<dyn Component>> {
        if let Some(root) = &self.layout_root {
            return Rc::clone(root);
        }
        let children = self.base.children().to_vec();
        let mut document = self.implicit_document.borrow_mut();
        document.detach_all();
        for child in children {
            document.add_child(child);
        }
        drop(document);
        Rc::clone(&self.implicit_scroll_view) as Rc<RefCell<dyn Component>>
    }

    /// Sets the single component the alt screen renders and dispatches mouse/focus against
    /// (senpi's `layoutRoot` setter, which also changes `getMountedRoots()`'s answer).
    pub fn set_layout_root(&mut self, root: Option<Rc<RefCell<dyn Component>>>) {
        self.layout_root = root.clone();
        self.base.set_mounted_roots_override(root.map(|r| vec![r]));
        self.current_layout = None;
        self.base.request_render(true, 0);
    }

    pub fn apply_mouse_tracking(&mut self, enabled: bool, support: &MouseTrackingSupport, terminal: &mut dyn Terminal) {
        let supported = support.is_tty && !support.is_termux && !support.is_windows_without_wt;
        let stopped = self.base.state.borrow().stopped;
        let next = enabled && supported && !stopped;
        if self.tracking_enabled == next {
            return;
        }
        self.tracking_enabled = next;
        terminal.write(if next { MouseTracking::ALL_MOTION } else { MouseTracking::DISABLE });
    }

    /// senpi's `beforeTerminalStop`: close search, drop transient gesture state, and hand the
    /// mouse/autowrap modes back before the alt screen is left.
    pub fn before_terminal_stop(&mut self, terminal: &mut dyn Terminal, mouse_enabled: bool) {
        self.close_search();
        self.clear_text_selection();
        self.clear_component_mouse_gesture();
        self.stop_scrollbar_hover();
        self.stop_scrollbar_drag();
        self.flashes.dispose();
        self.tracking_enabled = false;
        if !self.alt_screen_active {
            return;
        }
        terminal.write(&format!(
            "{BEGIN_SYNCHRONIZED_OUTPUT}{}{}{ENABLE_AUTOWRAP}{END_SYNCHRONIZED_OUTPUT}",
            self.delete_alt_screen_kitty_images(),
            if mouse_enabled { DISABLE_MOUSE } else { "" }
        ));
        self.uploaded_kitty_images.clear();
    }

    pub fn acquire_mouse_capture(&mut self, reason: impl Into<String>, support: &MouseTrackingSupport, terminal: &mut dyn Terminal) {
        let (token, should_apply) = self.base.acquire_mouse_capture(reason);
        self.mouse_capture_token = Some(token);
        if should_apply {
            let enabled = self.base.mouse_capture_enabled();
            self.apply_mouse_tracking(enabled, support, terminal);
        }
    }

    pub fn release_mouse_capture(&mut self, support: &MouseTrackingSupport, terminal: &mut dyn Terminal) {
        let Some(token) = self.mouse_capture_token.take() else {
            return;
        };
        if self.base.release_mouse_capture(token) {
            self.apply_mouse_tracking(false, support, terminal);
        }
    }

    pub fn set_mouse_blocker(&mut self, name: MouseBlocker, on: bool, support: &MouseTrackingSupport, terminal: &mut dyn Terminal) {
        if self.base.set_mouse_blocker(name, on) {
            let enabled = self.base.mouse_capture_enabled();
            self.apply_mouse_tracking(enabled, support, terminal);
        }
    }

    fn clear_text_selection(&mut self) {
        self.selection_press_active = false;
        self.selection_anchor = None;
        self.selection_focus = None;
        self.selection_granularity = SelectionGranularity::Character;
        self.last_click = None;
    }

    fn stop_scrollbar_drag(&mut self) {
        self.scrollbar_drag = None;
    }

    fn set_scrollbar_hover(&mut self, scroll_view: Option<Rc<RefCell<ScrollView>>>, now_ms: u64) {
        let changed = match (&self.scrollbar_hover, &scroll_view) {
            (Some(a), Some(b)) => !Rc::ptr_eq(a, b),
            (None, None) => false,
            _ => true,
        };
        if !changed {
            return;
        }
        if let Some(previous) = self.scrollbar_hover.take() {
            previous.borrow_mut().set_scrollbar_active(false, now_ms);
        }
        if let Some(next) = &scroll_view {
            next.borrow_mut().set_scrollbar_active(true, now_ms);
        }
        self.scrollbar_hover = scroll_view;
    }

    fn stop_scrollbar_hover(&mut self) {
        self.set_scrollbar_hover(None, 0);
    }

    fn get_scrollbar_target_at(&self, x: i64, y: i64) -> Option<(Rc<RefCell<ScrollView>>, crate::layout::ScrollbarGeometry)> {
        let frame = self.current_layout.as_ref()?;
        if self.base.has_overlay() {
            return None;
        }
        for scroll_view in get_scroll_views_at(frame, x, y) {
            let Some(boxed) = get_scroll_view_box(frame, &scroll_view) else {
                continue;
            };
            let Some(geometry) = get_scrollbar_geometry(boxed, true) else {
                continue;
            };
            if x == geometry.column && y >= geometry.track_top && y < geometry.track_top + geometry.track_height as i64 {
                return Some((scroll_view, geometry));
            }
        }
        None
    }

    /// senpi's `TuiAltScreen.doRender`, which fully replaces `TuiBase.doRender` rather than
    /// calling `super.doRender()`: the alt screen always paints a whole viewport into the
    /// alternate buffer and diffs it against the previous frame.
    pub fn do_render(&mut self, terminal: &mut dyn Terminal) {
        if self.base.state.borrow().stopped || !self.alt_screen_active {
            return;
        }
        let width = (terminal.columns() as usize).max(1);
        let height = (terminal.rows() as usize).max(1);
        let root = self.get_render_root();
        self.apply_pending_search_query();

        let mut next_layout = render_layout_frame(&root, width, height);
        if self.refresh_search(&next_layout) {
            next_layout = render_layout_frame(&root, width, height);
        }

        let mut screen: Vec<String> = next_layout
            .lines
            .iter()
            .map(|line| strip_osc133_zone_prefix(line).to_string())
            .collect();
        screen = self.apply_search_highlights(screen, &next_layout);
        screen = self.composite_scroll_to_end_indicator(screen, &next_layout, width);
        screen = self.base.composite_overlays(screen, terminal.columns(), terminal.rows());
        if screen.len() > height {
            let excess = screen.len() - height;
            screen.drain(0..excess);
        }
        screen = self.apply_selection(screen, &next_layout);
        screen = self.composite_flashes(screen, width, height);

        let cursor_pos = self.base.extract_cursor_position(&mut screen, height);
        screen = TuiBase::apply_line_reset_result(&mut self.base.state.borrow_mut(), &screen).lines;
        screen = screen
            .into_iter()
            .map(|line| {
                if image_stub::is_image_line(&line) || visible_width(&line) <= width {
                    line
                } else {
                    slice_by_column(&line, 0, width, true)
                }
            })
            .collect();

        let full_redraw = self.previous_screen.is_empty()
            || self.previous_screen_width != width
            || self.previous_screen_height != height;
        let images_need_redraw = screen.iter().enumerate().any(|(row, line)| {
            let previous = self.previous_screen.get(row).map(String::as_str).unwrap_or("");
            line != previous && (image_stub::is_image_line(line) || image_stub::is_image_line(previous))
        });

        let redraw_images = full_redraw || images_need_redraw;
        let had_uploaded_kitty_images = !self.uploaded_kitty_images.is_empty();
        let (prepared_lines, evicted_image_deletion) = if redraw_images && self.image_protocol == ImageProtocol::Kitty {
            self.prepare_kitty_screen(&screen)
        } else {
            (screen.clone(), String::new())
        };

        let mut buffer = String::from(BEGIN_SYNCHRONIZED_OUTPUT);
        if full_redraw {
            self.full_redraw_count += 1;
            let clear_images = if self.image_protocol == ImageProtocol::Kitty && had_uploaded_kitty_images {
                DELETE_ALL_KITTY_PLACEMENTS.to_string()
            } else {
                self.delete_alt_screen_kitty_images()
            };
            buffer.push_str(&clear_images);
            buffer.push_str("\x1b[2J");
        } else if images_need_redraw {
            if self.image_protocol == ImageProtocol::Iterm2 {
                buffer.push_str("\x1b[2J");
            } else if self.image_protocol == ImageProtocol::Kitty {
                buffer.push_str(DELETE_ALL_KITTY_PLACEMENTS);
            }
        }
        buffer.push_str(&evicted_image_deletion);

        for row in 0..height {
            let line = screen.get(row).map(String::as_str).unwrap_or("");
            let previous = self.previous_screen.get(row).map(String::as_str).unwrap_or("");
            if !full_redraw && !images_need_redraw && line == previous {
                continue;
            }
            let painted = prepared_lines.get(row).map(String::as_str).unwrap_or("");
            buffer.push_str(&format!("\x1b[{};1H\x1b[2K{painted}", row + 1));
        }

        if let Some((row, col)) = cursor_pos {
            buffer.push_str(&format!("\x1b[{};{}H", row + 1, col.min(width) + 1));
            buffer.push_str(if self.base.get_show_hardware_cursor() { "\x1b[?25h" } else { "\x1b[?25l" });
        } else {
            buffer.push_str("\x1b[?25l");
        }
        buffer.push_str(END_SYNCHRONIZED_OUTPUT);
        terminal.write(&buffer);

        self.previous_screen = screen;
        self.previous_screen_width = width;
        self.previous_screen_height = height;
        self.current_layout = Some(next_layout);
    }

    /// senpi's `beforeTerminalStart`: enter the alternate screen, disable autowrap, enable mouse
    /// reporting, clear, and reset the diff state.
    pub fn before_terminal_start(&mut self, terminal: &mut dyn Terminal, mouse_enabled: bool, multiplexer: bool) {
        self.clear_text_selection();
        self.clear_component_mouse_gesture();
        self.last_component_click = None;
        self.stop_scrollbar_hover();
        self.stop_scrollbar_drag();
        self.flashes.dispose();
        self.alt_screen_active = true;
        let capabilities = crate::terminal_image::get_capabilities();
        self.image_protocol = image_stub::get_capabilities().images;
        self.uploaded_kitty_images.clear();
        // iTerm2 inline images are placed relative to the scrolling main buffer; in the alternate
        // screen they break the viewport, so senpi disables them until the alt screen is left.
        if capabilities.images == Some(crate::terminal_capabilities::ImageProtocol::Iterm2) {
            self.saved_capabilities = Some(capabilities);
            crate::terminal_image::set_capabilities(crate::terminal_image::TerminalCapabilities { images: None, ..capabilities });
            self.base.invalidate();
        }
        self.reset_render_state();
        let mouse_sequence = if multiplexer { ENABLE_BUTTON_MOTION_MOUSE } else { ENABLE_ALL_MOTION_MOUSE };
        terminal.write(&format!(
            "{ENTER_ALT_SCREEN}{DISABLE_AUTOWRAP}{}\x1b[2J\x1b[H\x1b[?25l",
            if mouse_enabled { mouse_sequence } else { "" }
        ));
    }

    /// senpi's `afterTerminalStop`: leave the alternate screen, either keeping the alt frame on
    /// screen or repainting the alt document into the main screen's scrollback.
    pub fn after_terminal_stop(&mut self, terminal: &mut dyn Terminal, preserve_screen: bool) {
        if !self.alt_screen_active {
            return;
        }
        self.alt_screen_active = false;
        if preserve_screen {
            terminal.write(&format!("{BEGIN_SYNCHRONIZED_OUTPUT}{EXIT_ALT_SCREEN}\x1b[?25h{END_SYNCHRONIZED_OUTPUT}"));
        } else {
            self.write_last_document(terminal);
        }
        if let Some(capabilities) = self.saved_capabilities.take() {
            crate::terminal_image::set_capabilities(capabilities);
        }
    }

    /// The non-preserving half of senpi's `afterTerminalStop`: repaint the alt document into the
    /// main screen's scrollback.
    fn write_last_document(&mut self, terminal: &mut dyn Terminal) {
        let width = (terminal.columns() as usize).max(1);
        let document_lines: Vec<String> = self
            .base
            .render_mounted(width)
            .iter()
            .map(|line| strip_osc133_zone_prefix(line).to_string())
            .collect();
        let without_cursor: Vec<String> = document_lines.iter().map(|line| line.replace(CURSOR_MARKER, "")).collect();
        let normalized = TuiBase::apply_line_reset_result(&mut self.base.state.borrow_mut(), &without_cursor).lines;
        self.last_document = normalized
            .into_iter()
            .map(|line| {
                if image_stub::is_image_line(&line) || visible_width(&line) <= width {
                    line
                } else {
                    slice_by_column(&line, 0, width, true)
                }
            })
            .collect();
        let mut buffer = format!("{BEGIN_SYNCHRONIZED_OUTPUT}{EXIT_ALT_SCREEN}{DISABLE_AUTOWRAP}");
        for (row, line) in self.last_document.iter().enumerate() {
            if row > 0 {
                buffer.push_str("\r\n");
            }
            buffer.push_str(&format!("\r\x1b[2K{line}"));
        }
        buffer.push_str(&format!("\x1b[0m{ENABLE_AUTOWRAP}\r\n\x1b[?25h{END_SYNCHRONIZED_OUTPUT}"));
        terminal.write(&buffer);
    }

    pub fn full_redraws(&self) -> u64 {
        self.full_redraw_count
    }

    pub fn is_alt_screen_active(&self) -> bool {
        self.alt_screen_active
    }

    pub fn set_image_protocol(&mut self, protocol: ImageProtocol) {
        self.image_protocol = protocol;
    }

    pub fn set_scroll_to_end_indicator(&mut self, indicator: Option<Rc<dyn Fn() -> String>>) {
        self.scroll_to_end_indicator = indicator;
    }

    pub fn flashes_mut(&mut self) -> &mut AltScreenFlashContainer {
        &mut self.flashes
    }

    /// senpi's `viewportTop`.
    pub fn viewport_top(&self) -> usize {
        self.get_primary_scroll_view().borrow().scroll_top()
    }

    /// senpi's `isFollowingOutput`.
    pub fn is_following_output(&self) -> bool {
        self.get_primary_scroll_view().borrow().is_following_end()
    }

    pub fn scroll_by(&mut self, lines: i64) {
        self.get_primary_scroll_view().borrow_mut().scroll_by(lines, 0);
        self.base.request_render(false, 0);
    }

    pub fn scroll_to_top(&mut self) {
        self.get_primary_scroll_view().borrow_mut().scroll_to_start(0);
        self.base.request_render(false, 0);
    }

    pub fn scroll_to_bottom(&mut self) {
        self.get_primary_scroll_view().borrow_mut().scroll_to_end(0);
        self.base.request_render(false, 0);
    }

    /// senpi's `scrollToPrompt`: jump the primary scroll view to the previous/next OSC 133
    /// prompt-start line.
    fn scroll_to_prompt(&mut self, direction: i64) {
        let Some(frame) = self.current_layout.as_ref() else {
            return;
        };
        let scroll_view = self.get_primary_scroll_view();
        let Some(lines) = get_scroll_view_box(frame, &scroll_view).and_then(|boxed| boxed.scroll_content_lines.as_ref()) else {
            return;
        };
        let mut row = scroll_view.borrow().scroll_top() as i64 + direction;
        let mut target = None;
        while row >= 0 && (row as usize) < lines.len() {
            if is_osc133_prompt_start(&lines[row as usize]) {
                target = Some(row);
                break;
            }
            row += direction;
        }
        if let Some(target) = target {
            scroll_view.borrow_mut().scroll_to(target, Default::default(), 0);
            self.base.request_render(false, 0);
        }
    }

    /// senpi's `resetRenderState`.
    pub fn reset_render_state(&mut self) {
        self.previous_screen.clear();
        self.previous_screen_width = 0;
        self.previous_screen_height = 0;
        self.current_layout = None;
    }

    fn delete_alt_screen_kitty_images(&self) -> String {
        if self.image_protocol == ImageProtocol::Kitty {
            DELETE_ALL_KITTY_IMAGES.to_string()
        } else {
            String::new()
        }
    }

    fn apply_search_text_highlight(&self, text: &str, current: bool) -> String {
        if current {
            format!("\x1b[1;7m{text}\x1b[22;27m")
        } else {
            format!("\x1b[4m{text}\x1b[24m")
        }
    }

    /// senpi's `applySearchHighlights`: underline every visible match and reverse the current one.
    fn apply_search_highlights(&mut self, screen: Vec<String>, layout: &LayoutFrame) -> Vec<String> {
        let Some(search) = self.active_search.as_ref() else {
            return screen;
        };
        if search.selected_index < 0 || search.matches.is_empty() {
            return screen;
        }
        let scroll_view = layout.primary_scroll_view.clone().unwrap_or_else(|| Rc::clone(&self.implicit_scroll_view));
        let Some(boxed) = get_scroll_view_box(layout, &scroll_view) else {
            return screen;
        };
        let scroll_top = scroll_view.borrow().scroll_top() as i64;
        let scrollbar_column = get_scrollbar_geometry(boxed, false).map(|geometry| geometry.column);
        let min_row = boxed.rect.y.max(boxed.clip.y).max(0);
        let max_row = (boxed.rect.y + boxed.rect.height as i64)
            .min(boxed.clip.y + boxed.clip.height as i64)
            .min(screen.len() as i64);
        let min_column = boxed.rect.x.max(boxed.clip.x).max(0);
        let max_column = (boxed.rect.x + boxed.rect.width as i64)
            .min(boxed.clip.x + boxed.clip.width as i64)
            .min(layout.width as i64)
            .min(scrollbar_column.unwrap_or(i64::MAX));
        let min_content_row = scroll_top + min_row - boxed.rect.y;
        let max_content_row = scroll_top + max_row - boxed.rect.y - 1;
        let first_visible = search.matches.partition_point(|search_match| {
            search_match.segments.last().map(|segment| segment.row as i64).unwrap_or(-1) < min_content_row
        });

        let mut ranges_by_row: Vec<SearchHighlightRanges> = Vec::new();
        for (match_index, search_match) in search.matches.iter().enumerate().skip(first_visible) {
            if search_match.segments.first().map(|segment| segment.row as i64).unwrap_or(0) > max_content_row {
                break;
            }
            for segment in &search_match.segments {
                let row = boxed.rect.y + segment.row as i64 - scroll_top;
                if row < min_row || row >= max_row {
                    continue;
                }
                let start_col = min_column.max(boxed.rect.x + segment.start_col as i64);
                let end_col = max_column.min(boxed.rect.x + segment.end_col as i64);
                if end_col <= start_col {
                    continue;
                }
                let row = row as usize;
                let range = (start_col as usize, end_col as usize, match_index as i64 == search.selected_index);
                match ranges_by_row.iter_mut().find(|(candidate, _)| *candidate == row) {
                    Some((_, ranges)) => ranges.push(range),
                    None => ranges_by_row.push((row, vec![range])),
                }
            }
        }

        let mut result = screen;
        for (row, mut ranges) in ranges_by_row {
            let Some(line) = result.get(row).cloned() else {
                continue;
            };
            if image_stub::is_image_line(&line) {
                continue;
            }
            let line_width = visible_width(&line);
            let mut line = line;
            ranges.sort_by_key(|range| std::cmp::Reverse(range.0));
            for (start_col, end_col, current) in ranges {
                let start_col = start_col.min(line_width);
                let end_col = end_col.min(line_width);
                if end_col <= start_col {
                    continue;
                }
                let before = slice_by_column(&line, 0, start_col, true);
                let highlighted = slice_by_column(&line, start_col, end_col - start_col, true);
                let after = slice_by_column(&line, end_col, line_width.saturating_sub(end_col), true);
                line = format!("{before}{}{after}", self.apply_search_text_highlight(&highlighted, current));
            }
            result[row] = line;
        }
        result
    }

    /// senpi's `compositeScrollToEndIndicator`; records the pill's rect for click handling.
    fn composite_scroll_to_end_indicator(&mut self, screen: Vec<String>, layout: &LayoutFrame, width: usize) -> Vec<String> {
        self.scroll_to_end_indicator_rect = None;
        let Some(indicator) = self.scroll_to_end_indicator.clone() else {
            return screen;
        };
        let scroll_view = layout.primary_scroll_view.clone().unwrap_or_else(|| Rc::clone(&self.implicit_scroll_view));
        if !scroll_view.borrow().follows_end() || scroll_view.borrow().is_following_end() {
            return screen;
        }
        let Some(boxed) = get_scroll_view_box(layout, &scroll_view) else {
            return screen;
        };
        let clip = boxed.clip;
        if clip.width == 0 || clip.height == 0 {
            return screen;
        }
        let row = clip.y + clip.height as i64 - 1;
        if row < 0 || row as usize >= screen.len() || image_stub::is_image_line(&screen[row as usize]) {
            return screen;
        }
        let scrollbar_column = get_scrollbar_geometry(boxed, true).map(|geometry| geometry.column);
        let available_width = ((scrollbar_column.unwrap_or(clip.x + clip.width as i64)) - clip.x).max(0) as usize;
        let text = truncate_to_width(&indicator(), available_width, "", false);
        let text_width = visible_width(&text);
        if text_width == 0 {
            return screen;
        }
        let column = clip.x + ((available_width.saturating_sub(text_width)) / 2) as i64;
        let mut result = screen;
        let base = result[row as usize].clone();
        result[row as usize] = composite_tui_line(&base, &text, column.max(0) as usize, text_width, width);
        self.scroll_to_end_indicator_rect = Some((row, column, text_width as i64));
        result
    }

    /// senpi's `handleScrollToEndIndicatorMouseEvent`: a primary press on the pill jumps to the end.
    fn handle_scroll_to_end_indicator_mouse_event(&mut self, event: &SgrMouseEvent) -> bool {
        let Some((row, column, width)) = self.scroll_to_end_indicator_rect else {
            return false;
        };
        if event.release || (event.button & 32) != 0 || (event.button & 3) != 0 {
            return false;
        }
        if event.y != row || event.x < column || event.x >= column + width {
            return false;
        }
        self.scroll_to_bottom();
        true
    }

    /// senpi's `compositeFlashes`.
    fn composite_flashes(&mut self, screen: Vec<String>, width: usize, height: usize) -> Vec<String> {
        let rendered = self.flashes.render(width);
        let flash_lines: Vec<String> = if rendered.len() > height {
            rendered[rendered.len() - height..].to_vec()
        } else {
            rendered
        };
        if flash_lines.is_empty() {
            return screen;
        }
        let mut result = screen;
        while result.len() < height {
            result.push(String::new());
        }
        for (row, line) in flash_lines.iter().enumerate() {
            let flash_width = visible_width(line);
            if flash_width == 0 {
                continue;
            }
            let base = result.get(row).cloned().unwrap_or_default();
            result[row] = composite_tui_line(&base, line, width.saturating_sub(flash_width), flash_width, width);
        }
        result
    }

    /// senpi's `applySelection`. The scroll-view-anchored mapping senpi performs when the
    /// selection started inside a scroll view is not representable in this port's `Selection`
    /// type (it carries no scroll-view reference); see `parity.d/7.md`.
    fn apply_selection(&mut self, screen: Vec<String>, _layout: &LayoutFrame) -> Vec<String> {
        let (Some(anchor), Some(focus)) = (self.selection_anchor, self.selection_focus) else {
            return screen;
        };
        let mut lines = screen;
        let width = self.current_layout.as_ref().map(|layout| layout.width).unwrap_or(0);
        apply_selection_highlight(&mut lines, Selection { anchor, focus }, width);
        lines
    }

    /// senpi's `toggleSearch`: open the find bar as a focused top-right overlay, or close it.
    pub fn toggle_search(&mut self) {
        if self.active_search.is_some() {
            self.close_search();
            return;
        }
        let pending_query: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
        let sink = Rc::clone(&pending_query);
        let component = Rc::new(RefCell::new(AltScreenSearchComponent::new(
            Box::new(move |query: &str| {
                *sink.borrow_mut() = Some(query.to_string());
            }),
            Some(Rc::clone(&self.search_navigation_button_style)),
            cfg!(target_os = "macos"),
        )));
        let anchor_row = self.get_primary_scroll_view().borrow().scroll_top();
        let overlay = self.base.show_overlay(
            Rc::clone(&component) as Rc<RefCell<dyn Component>>,
            Some(OverlayOptions {
                anchor: Some(OverlayAnchor::TopRight),
                width: Some(SizeValue::Percent(40.0)),
                min_width: Some(32),
                margin: Some(OverlayMargin { top: Some(1), right: Some(1), bottom: Some(1), left: Some(1) }),
                ..Default::default()
            }),
        );
        self.active_search = Some(ActiveSearch {
            component,
            index: AltScreenSearchIndex::new(),
            overlay,
            pending_query,
            query: String::new(),
            matches: Vec::new(),
            selected_index: -1,
            selected_key: None,
            anchor_row,
            selection_mode: SearchSelectionMode::Query,
        });
    }

    /// senpi's `closeSearch`.
    fn close_search(&mut self) {
        let Some(search) = self.active_search.take() else {
            return;
        };
        search.overlay.hide();
        self.base.request_render(false, 0);
    }

    /// Applies a query the search component reported through its callback.
    fn apply_pending_search_query(&mut self) {
        let pending = self.active_search.as_ref().and_then(|search| search.pending_query.borrow_mut().take());
        if let Some(query) = pending {
            self.update_search_query(query);
        }
    }

    /// senpi's `updateSearchQuery`: re-anchor at the current match (or viewport top) so the
    /// next refresh selects the first match at or below it.
    fn update_search_query(&mut self, query: String) {
        let primary_top = self.get_primary_scroll_view().borrow().scroll_top();
        let Some(search) = self.active_search.as_mut() else {
            return;
        };
        if query == search.query {
            return;
        }
        let selected_row = usize::try_from(search.selected_index)
            .ok()
            .and_then(|index| search.matches.get(index))
            .and_then(|selected| selected.segments.first())
            .map(|segment| segment.row);
        search.anchor_row = selected_row.unwrap_or(primary_top);
        search.query = query;
        search.selection_mode = SearchSelectionMode::Query;
        search.component.borrow_mut().set_result(-1, 0);
        self.base.request_render(false, 0);
    }

    /// senpi's `navigateSearch`: select the next/previous match on the next refresh.
    pub fn navigate_search(&mut self, direction: i64) {
        let Some(search) = self.active_search.as_mut() else {
            return;
        };
        if search.query.is_empty() {
            return;
        }
        search.selection_mode = if direction < 0 { SearchSelectionMode::Previous } else { SearchSelectionMode::Next };
        self.base.request_render(false, 0);
    }

    /// senpi's `getSearchNavigationDirectionAt`.
    fn get_search_navigation_direction_at(&self, x: i64, y: i64) -> Option<i64> {
        let search = self.active_search.as_ref()?;
        let bounds = search.overlay.get_bounds()?;
        if x < bounds.col || x >= bounds.col + bounds.width as i64 || y < bounds.row || y >= bounds.row + bounds.height as i64 {
            return None;
        }
        search.component.borrow().get_navigation_direction_at(y - bounds.row, x - bounds.col)
    }

    /// senpi's `handleSearchMouseEvent`: hover and click the find bar's prev/next buttons.
    fn handle_search_mouse_event(&mut self, event: &SgrMouseEvent) -> bool {
        let Some(search) = self.active_search.as_ref() else {
            return false;
        };
        let component = Rc::clone(&search.component);
        let direction = self.get_search_navigation_direction_at(event.x, event.y);
        if component.borrow_mut().set_hovered_navigation_direction(direction) {
            self.base.request_render(false, 0);
        }
        let Some(direction) = direction else {
            return false;
        };
        if event.release || (event.button & 32) != 0 || (event.button & 3) != 0 {
            return false;
        }
        self.navigate_search(direction);
        true
    }

    /// senpi's `refreshSearch`: incrementally re-match the primary scroll view, resolve the
    /// selection for the pending mode, and reveal it. Returns whether the viewport moved, in
    /// which case the caller lays the frame out again.
    fn refresh_search(&mut self, layout: &LayoutFrame) -> bool {
        let scroll_view = layout.primary_scroll_view.clone().unwrap_or_else(|| Rc::clone(&self.implicit_scroll_view));
        let Some(search) = self.active_search.as_mut() else {
            return false;
        };
        let boxed = get_scroll_view_box(layout, &scroll_view);
        let lines = boxed.and_then(|boxed| boxed.scroll_content_lines.as_ref());
        let Some(lines) = lines.filter(|_| !search.query.trim().is_empty()) else {
            search.matches.clear();
            search.selected_index = -1;
            search.selected_key = None;
            search.selection_mode = SearchSelectionMode::Retain;
            search.component.borrow_mut().set_result(-1, 0);
            return false;
        };

        let should_reveal_selection = search.selection_mode != SearchSelectionMode::Retain;
        let result = search.index.search(lines, &search.query);
        search.matches = result.matches;
        if !result.changed && search.selection_mode == SearchSelectionMode::Retain {
            return false;
        }

        let matches = &search.matches;
        let len = matches.len() as i64;
        let exact_index: i64 = if result.changed {
            search
                .selected_key
                .as_ref()
                .and_then(|key| matches.iter().position(|candidate| &get_alt_screen_search_match_key(candidate) == key))
                .map_or(-1, |index| index as i64)
        } else {
            search.selected_index
        };
        let mut selected_index = -1i64;
        if len > 0 {
            selected_index = match search.selection_mode {
                SearchSelectionMode::Query => {
                    let first_at_anchor = matches.partition_point(|candidate| {
                        candidate.segments.first().map_or(0, |segment| segment.row) < search.anchor_row
                    }) as i64;
                    if first_at_anchor < len { first_at_anchor } else { 0 }
                }
                SearchSelectionMode::Next => {
                    let base_index = if exact_index >= 0 { exact_index } else { search.selected_index.min(len - 1) };
                    if base_index < 0 { 0 } else { (base_index + 1) % len }
                }
                SearchSelectionMode::Previous => {
                    let base_index = if exact_index >= 0 { exact_index } else { search.selected_index.min(len - 1) };
                    if base_index < 0 { len - 1 } else { (base_index - 1 + len) % len }
                }
                SearchSelectionMode::Retain => {
                    if exact_index >= 0 { exact_index } else { search.selected_index.max(0).min(len - 1) }
                }
            };
        }

        search.selected_index = selected_index;
        let selected = usize::try_from(selected_index).ok().and_then(|index| matches.get(index));
        search.selected_key = selected.map(get_alt_screen_search_match_key);
        search.selection_mode = SearchSelectionMode::Retain;
        search.component.borrow_mut().set_result(selected_index, matches.len());
        if !should_reveal_selection {
            return false;
        }

        let first_segment = selected.and_then(|selected| selected.segments.first());
        let last_segment = selected.and_then(|selected| selected.segments.last());
        let (Some(first_segment), Some(last_segment)) = (first_segment, last_segment) else {
            return false;
        };
        let viewport_height = scroll_view.borrow().viewport_height();
        if viewport_height == 0 {
            return false;
        }
        let before = scroll_view.borrow().scroll_top();
        let visible_bottom = before + viewport_height - 1;
        let mut target = before as i64;
        if first_segment.row < before || last_segment.row > visible_bottom {
            target = first_segment.row as i64 - (viewport_height / 3) as i64;
        }
        scroll_view.borrow_mut().scroll_to(target, ScrollToOptions { disable_follow: true }, 0);
        scroll_view.borrow().scroll_top() != before
    }

    /// Registered via `TuiBase::add_input_listener` (senpi's `handleMouseInput`).
    pub fn handle_mouse_input(&mut self, data: &str, now_ms: i64, terminal: &mut dyn Terminal) -> bool {
        if !is_mouse_sequence(data) {
            return false;
        }
        if let Some(wheel) = parse_wheel_event(data) {
            let wheel_delta = wheel.direction * self.get_wheel_scroll_lines(wheel.button);
            let event = self.create_mouse_event(
                TuiMouseEventType::Wheel,
                wheel.button,
                wheel.x,
                wheel.y,
                ToTuiMouseEventExtra { wheel_delta: Some(wheel_delta), click_count: None },
                terminal,
            );
            let (hit, overlay_result) = self.base.dispatch_mouse_to_overlay(&event);
            let result = overlay_result.or_else(|| if hit { None } else { self.dispatch_mouse_to_layout(&event) });
            if let Some(result) = result {
                if self.apply_mouse_dispatch_result(&event, &result, true) {
                    self.base.request_render(false, 0);
                }
                return true;
            }
            if self.should_defer_viewport_input_to_overlay() {
                // senpi returns `undefined` so the focused overlay receives the raw sequence.
                self.base.handle_terminal_input(data, false);
                return true;
            }
            self.route_wheel(wheel);
            return true;
        }
        let Some(raw) = parse_sgr_mouse_event(data) else {
            return true;
        };
        self.handle_mouse_event(raw, now_ms, terminal);
        true
    }

    /// Sets senpi's `wheelScrollLines` option (logical lines per wheel event, at least 1).
    pub fn set_wheel_scroll_lines(&mut self, lines: i64) {
        self.wheel_scroll_lines = lines.max(1);
    }

    /// senpi's `getWheelScrollLines`: SGR button bit 3 (value 8) is the Alt modifier.
    fn get_wheel_scroll_lines(&self, button: i64) -> i64 {
        if (button & 8) != 0 {
            self.wheel_scroll_lines * ALT_WHEEL_SCROLL_MULTIPLIER
        } else {
            self.wheel_scroll_lines
        }
    }

    /// senpi's `createMouseEvent`.
    fn create_mouse_event(
        &self,
        event_type: TuiMouseEventType,
        button: i64,
        x: i64,
        y: i64,
        extra: ToTuiMouseEventExtra,
        terminal: &dyn Terminal,
    ) -> TuiMouseEvent {
        to_tui_mouse_event(
            event_type,
            SgrMouseEvent { button, x, y, release: event_type == TuiMouseEventType::Release },
            terminal.columns() as usize,
            terminal.rows() as usize,
            extra,
        )
    }

    /// senpi's `dispatchMouseToLayout`: offer the event to every component on the visual hit
    /// path, deepest first. Layout containers that only forward to `Container::handle_mouse`
    /// (stacks and scroll views, i.e. layout components that are containers) are skipped because
    /// their children are visited through their own layout boxes.
    fn dispatch_mouse_to_layout(&self, event: &TuiMouseEvent) -> Option<TuiMouseDispatchResult> {
        let frame = self.current_layout.as_ref()?;
        let targets: Vec<(Rc<RefCell<dyn Component>>, crate::layout::LayoutRect)> = get_layout_boxes_at(frame, event.screen_x, event.screen_y)
            .into_iter()
            .map(|boxed| (Rc::clone(&boxed.component), boxed.rect))
            .collect();
        let mut visited: Vec<*const ()> = Vec::new();
        for (component, rect) in targets {
            let key = Rc::as_ptr(&component) as *const ();
            if visited.contains(&key) {
                continue;
            }
            let forwards_to_container = {
                let borrowed = component.borrow();
                borrowed.as_layout_component().is_some() && borrowed.as_container().is_some()
            };
            if forwards_to_container {
                continue;
            }
            visited.push(key);
            let local = TuiMouseEvent {
                x: event.screen_x - rect.x,
                y: event.screen_y - rect.y,
                width: rect.width,
                height: rect.height,
                ..*event
            };
            if let Some(result) = dispatch_mouse_event(&component, &local) {
                return Some(result);
            }
        }
        None
    }

    /// senpi's `applyMouseDispatchResult`: apply focus and capture, and report whether to render.
    fn apply_mouse_dispatch_result(&mut self, event: &TuiMouseEvent, result: &TuiMouseDispatchResult, apply_focus: bool) -> bool {
        let focus_target = if apply_focus && result.result.focus {
            self.base
                .resolve_mouse_focus_target(result.focus_target.as_ref().unwrap_or(&result.target.component))
        } else {
            None
        };
        let focus_changed = focus_target.as_ref().is_some_and(|target| {
            !self.base.get_focused_component().is_some_and(|focused| Rc::ptr_eq(&focused, target))
        });
        if let Some(target) = focus_target {
            self.base.set_focus(Some(target));
        }
        if result.result.capture {
            self.mouse_capture = Some(result.target.clone());
        }
        result.result.render.unwrap_or(
            focus_changed
                || matches!(
                    event.event_type,
                    TuiMouseEventType::Press | TuiMouseEventType::Click | TuiMouseEventType::Drag | TuiMouseEventType::Wheel
                ),
        )
    }

    /// senpi's `getComponentClickCount`: double/triple clicks on the same component cell.
    fn get_component_click_count(&mut self, target: &TuiMouseDispatchTarget, x: i64, y: i64, now_ms: i64) -> u32 {
        let count = match &self.last_component_click {
            Some(previous)
                if now_ms - previous.timestamp_ms <= DOUBLE_CLICK_INTERVAL_MS
                    && Rc::ptr_eq(&previous.component, &target.component)
                    && previous.x == x
                    && previous.y == y =>
            {
                (previous.count % 3) + 1
            }
            _ => 1,
        };
        self.last_component_click = Some(LastComponentClick {
            timestamp_ms: now_ms,
            count,
            component: Rc::clone(&target.component),
            x,
            y,
        });
        count
    }

    /// senpi's `clearComponentMouseGesture`.
    fn clear_component_mouse_gesture(&mut self) {
        self.mouse_capture = None;
        self.mouse_press_target = None;
        self.mouse_press_point = None;
        self.mouse_press_moved = false;
    }

    fn route_wheel(&mut self, wheel: WheelEvent) {
        let mut remaining = wheel.direction * self.get_wheel_scroll_lines(wheel.button);
        let scroll_views = self
            .current_layout
            .as_ref()
            .map(|frame| get_scroll_views_at(frame, wheel.x, wheel.y))
            .unwrap_or_default();
        let mut seen: Vec<*const RefCell<ScrollView>> = Vec::new();
        for scroll_view in &scroll_views {
            seen.push(Rc::as_ptr(scroll_view));
            let contain = scroll_view.borrow().overscroll() == crate::components::scroll_view::Overscroll::Contain;
            remaining = scroll_view.borrow_mut().scroll_by(remaining, 0);
            if remaining == 0 || contain {
                break;
            }
        }
        let primary = self.get_primary_scroll_view();
        if remaining != 0 && !seen.contains(&Rc::as_ptr(&primary)) {
            primary.borrow_mut().scroll_by(remaining, 0);
        }
        self.update_scrollbar_hover(wheel.x, wheel.y);
        self.base.request_render(false, 0);
    }

    fn update_scrollbar_hover(&mut self, x: i64, y: i64) {
        self.set_scrollbar_hover(self.get_scrollbar_target_at(x, y).map(|(scroll_view, _)| scroll_view), 0);
    }

    /// senpi's `handleMouseEvent`: captured/pressed component gestures (with click synthesis)
    /// first, then the find bar, overlays, scrollbars, layout components, and finally text
    /// selection.
    fn handle_mouse_event(&mut self, raw: SgrMouseEvent, now_ms: i64, terminal: &mut dyn Terminal) {
        let is_motion = (raw.button & 32) != 0;
        let event_type = if raw.release {
            TuiMouseEventType::Release
        } else if is_motion {
            if decode_mouse_button(raw.button) == TuiMouseButton::None { TuiMouseEventType::Move } else { TuiMouseEventType::Drag }
        } else {
            TuiMouseEventType::Press
        };
        let event = self.create_mouse_event(event_type, raw.button, raw.x, raw.y, ToTuiMouseEventExtra::default(), terminal);

        if let Some(target) = self.mouse_capture.clone().or_else(|| self.mouse_press_target.clone()) {
            if self.mouse_press_point.is_some_and(|point| point != (raw.x, raw.y)) {
                self.mouse_press_moved = true;
                self.last_component_click = None;
            }
            let mut render = false;
            if let Some(target_result) = dispatch_mouse_event(&target.component, &retarget_mouse_event(&event, &target)) {
                render = self.apply_mouse_dispatch_result(&event, &target_result, true);
            }
            if raw.release {
                if !self.mouse_press_moved && self.mouse_press_point == Some((raw.x, raw.y)) {
                    let click_count = self.get_component_click_count(&target, raw.x, raw.y, now_ms);
                    let click_event = self.create_mouse_event(
                        TuiMouseEventType::Click,
                        raw.button,
                        raw.x,
                        raw.y,
                        ToTuiMouseEventExtra { wheel_delta: None, click_count: Some(click_count) },
                        terminal,
                    );
                    // A click handler that moves focus owns the outcome; only re-apply the click
                    // target's focus when the handler left focus untouched.
                    let focus_before_click = self.base.get_focused_component();
                    let click_result = dispatch_mouse_event(&target.component, &retarget_mouse_event(&click_event, &target));
                    if let Some(click_result) = click_result {
                        let focus_after_click = self.base.get_focused_component();
                        let keep_handler_focus = match (&focus_before_click, &focus_after_click) {
                            (Some(before), Some(after)) => !Rc::ptr_eq(before, after),
                            (None, None) => false,
                            _ => true,
                        };
                        render = self.apply_mouse_dispatch_result(&click_event, &click_result, !keep_handler_focus) || render;
                    }
                }
                self.clear_component_mouse_gesture();
            }
            if render {
                self.base.request_render(false, 0);
            }
            return;
        }

        if self.handle_search_mouse_event(&raw) {
            return;
        }

        let (hit, overlay_result) = self.base.dispatch_mouse_to_overlay(&event);
        if !hit {
            if self.handle_scroll_to_end_indicator_mouse_event(&raw) {
                return;
            }
            let scrollbar_handled = self.handle_scrollbar_mouse_event(&raw);
            if self.scrollbar_drag.is_none() {
                self.update_scrollbar_hover(raw.x, raw.y);
            }
            if scrollbar_handled {
                return;
            }
        } else {
            self.stop_scrollbar_hover();
        }

        let result = overlay_result.or_else(|| if hit { None } else { self.dispatch_mouse_to_layout(&event) });
        if let Some(result) = result {
            let render = self.apply_mouse_dispatch_result(&event, &result, true);
            if event_type == TuiMouseEventType::Press {
                self.clear_text_selection();
                self.mouse_press_target = Some(result.target.clone());
                self.mouse_press_point = Some((raw.x, raw.y));
                self.mouse_press_moved = false;
            }
            if render {
                self.base.request_render(false, 0);
            }
            return;
        }

        self.handle_selection_mouse_event(raw, now_ms, terminal);
    }

    fn handle_scrollbar_mouse_event(&mut self, event: &SgrMouseEvent) -> bool {
        if let Some(drag) = &self.scrollbar_drag {
            if event.release {
                self.stop_scrollbar_drag();
                return true;
            }
            let Some(frame) = &self.current_layout else {
                return true;
            };
            let Some(boxed) = get_scroll_view_box(frame, &drag.scroll_view) else {
                return true;
            };
            if let Some(geometry) = get_scrollbar_geometry(boxed, false) {
                let scroll_view = Rc::clone(&drag.scroll_view);
                let grab_offset = drag.grab_offset;
                scroll_scrollbar_to_pointer(&scroll_view, &geometry, event.y, grab_offset);
            }
            self.base.request_render(false, 0);
            return true;
        }

        if event.release || (event.button & 32) != 0 || (event.button & 3) != 0 {
            return false;
        }
        let Some((scroll_view, geometry)) = self.get_scrollbar_target_at(event.x, event.y) else {
            return false;
        };
        self.clear_text_selection();
        self.set_scrollbar_hover(Some(Rc::clone(&scroll_view)), 0);
        let on_thumb = event.y >= geometry.thumb_top && event.y < geometry.thumb_top + geometry.thumb_height as i64;
        let grab_offset = if on_thumb { event.y - geometry.thumb_top } else { (geometry.thumb_height / 2) as i64 };
        if !on_thumb {
            scroll_scrollbar_to_pointer(&scroll_view, &geometry, event.y, grab_offset);
        }
        self.scrollbar_drag = Some(ScrollbarDrag { scroll_view, grab_offset });
        self.base.request_render(false, 0);
        true
    }

    fn get_selection_point(&self, event: &SgrMouseEvent, terminal: &dyn Terminal) -> SelectionPoint {
        SelectionPoint {
            row: (event.y.max(0) as usize).min((terminal.rows() as usize).saturating_sub(1)),
            col: (event.x.max(0) as usize).min((terminal.columns() as usize).saturating_sub(1)),
        }
    }

    fn get_selection_source_line(&self, point: SelectionPoint) -> String {
        self.current_layout
            .as_ref()
            .and_then(|frame| frame.lines.get(point.row))
            .cloned()
            .unwrap_or_default()
    }

    fn get_word_selection(&self, point: SelectionPoint) -> Option<(SelectionPoint, SelectionPoint)> {
        let line = strip_terminal_sequences(&self.get_selection_source_line(point));
        let segments = word_segments(&line);
        struct Seg {
            start: usize,
            end: usize,
            selectable: bool,
            joiner: bool,
        }
        let mut cols = Vec::with_capacity(segments.len());
        let mut start_col = 0usize;
        for seg in &segments {
            let width = visible_width(seg.segment);
            let joiner = WORD_SELECTION_JOINERS.contains(&seg.segment);
            cols.push(Seg { start: start_col, end: start_col + width, selectable: seg.is_word_like || joiner, joiner });
            start_col += width;
        }
        let clicked_index = cols.iter().position(|s| point.col >= s.start && point.col < s.end)?;
        let can_join = |left: &Seg, right: &Seg| left.selectable && right.selectable && (left.joiner || right.joiner);
        let mut selection_start = cols[clicked_index].start;
        let mut selection_end = cols[clicked_index].end;
        let mut index = clicked_index;
        while index > 0 && can_join(&cols[index - 1], &cols[index]) {
            index -= 1;
            selection_start = cols[index].start;
        }
        let mut index = clicked_index;
        while index < cols.len() - 1 && can_join(&cols[index], &cols[index + 1]) {
            index += 1;
            selection_end = cols[index].end;
        }
        Some((SelectionPoint { row: point.row, col: selection_start }, SelectionPoint { row: point.row, col: selection_end }))
    }

    fn get_line_selection(&self, point: SelectionPoint) -> (SelectionPoint, SelectionPoint) {
        let width = visible_width(&self.get_selection_source_line(point));
        (SelectionPoint { row: point.row, col: 0 }, SelectionPoint { row: point.row, col: width })
    }

    fn update_selection_focus(&mut self, point: SelectionPoint) {
        if self.selection_granularity == SelectionGranularity::Character {
            self.selection_focus = Some(point);
            return;
        }
        let range = if self.selection_granularity == SelectionGranularity::Word {
            self.get_word_selection(point)
        } else {
            Some(self.get_line_selection(point))
        };
        let Some((range_start, range_end)) = range else {
            return;
        };
        let Some(anchor) = self.selection_anchor else {
            return;
        };
        let Some(focus) = self.selection_focus else {
            return;
        };
        let initial_start = anchor.min(focus);
        let initial_end = anchor.max(focus);
        if range_start < initial_start {
            self.selection_anchor = Some(initial_end);
            self.selection_focus = Some(range_start);
        } else {
            self.selection_anchor = Some(initial_start);
            self.selection_focus = Some(range_end);
        }
    }

    fn get_click_count(&mut self, point: SelectionPoint, word: Option<(SelectionPoint, SelectionPoint)>, now_ms: i64) -> u32 {
        let count = match (&self.last_click, word) {
            (Some(previous), Some((word_start, word_end)))
                if now_ms - previous.timestamp_ms <= DOUBLE_CLICK_INTERVAL_MS
                    && previous.row == point.row
                    && previous.word_start == word_start.col
                    && previous.word_end == word_end.col =>
            {
                (previous.count % 3) + 1
            }
            _ => 1,
        };
        self.last_click = word.map(|(word_start, word_end)| LastClick {
            timestamp_ms: now_ms,
            count,
            row: point.row,
            word_start: word_start.col,
            word_end: word_end.col,
        });
        count
    }

    fn handle_selection_mouse_event(&mut self, event: SgrMouseEvent, now_ms: i64, terminal: &mut dyn Terminal) {
        let button = event.button & 3;
        if button != 0 && !(event.release && button == 3) {
            return;
        }
        let point = self.get_selection_point(&event, terminal);

        if event.release {
            if !self.selection_press_active {
                return;
            }
            self.selection_press_active = false;
            if self.selection_anchor.is_none() {
                return;
            }
            self.update_selection_focus(point);
            self.base.request_render(false, 0);
            return;
        }

        if (event.button & 32) != 0 {
            if !self.selection_press_active || self.selection_anchor.is_none() {
                return;
            }
            self.update_selection_focus(point);
            self.base.request_render(false, 0);
            return;
        }

        self.selection_press_active = true;
        let anchor = point;
        let word = self.get_word_selection(anchor);
        let click_count = self.get_click_count(anchor, word, now_ms);
        let range = if click_count == 2 {
            word
        } else if click_count == 3 {
            Some(self.get_line_selection(anchor))
        } else {
            None
        };
        self.selection_granularity = if range.is_some() {
            if click_count == 2 {
                SelectionGranularity::Word
            } else {
                SelectionGranularity::Line
            }
        } else {
            SelectionGranularity::Character
        };
        let (range_start, range_end) = range.unwrap_or((anchor, anchor));
        self.selection_anchor = Some(range_start);
        self.selection_focus = Some(range_end);
        self.base.request_render(false, 0);

        if let Some(root) = &self.layout_root
            && can_receive_keys(root)
        {
            self.base.set_focus(Some(Rc::clone(root)));
        }
    }

    fn get_selection_bounds(&self) -> Option<(SelectionPoint, SelectionPoint)> {
        let anchor = self.selection_anchor?;
        let focus = self.selection_focus?;
        if anchor == focus {
            return None;
        }
        Some(if anchor < focus { (anchor, focus) } else { (focus, anchor) })
    }

    pub fn has_active_selection(&self) -> bool {
        self.get_selection_bounds().is_some()
    }

    fn get_active_selection_text(&self) -> Option<String> {
        let (start, end) = self.get_selection_bounds()?;
        let frame = self.current_layout.as_ref()?;
        let mut lines = Vec::new();
        for row in start.row..=end.row.min(frame.lines.len().saturating_sub(1)) {
            let line = frame.lines.get(row).cloned().unwrap_or_default();
            let plain = strip_terminal_sequences(&line);
            let row_start_col = if row == start.row { start.col } else { 0 };
            let row_end_col = if row == end.row { end.col } else { visible_width(&plain) };
            if row_end_col <= row_start_col {
                lines.push(String::new());
                continue;
            }
            lines.push(slice_plain(&plain, row_start_col, row_end_col).trim_end().to_string());
        }
        let text = lines.join("\n");
        if text.is_empty() { None } else { Some(text) }
    }

    pub fn copy_active_selection_to_clipboard(&mut self, terminal: &mut dyn Terminal) -> bool {
        let Some(text) = self.get_active_selection_text() else {
            return false;
        };
        self.copy_text_to_clipboard(&text, terminal);
        true
    }

    fn copy_text_to_clipboard(&mut self, text: &str, terminal: &mut dyn Terminal) {
        let encoded = base64_encode::encode_base64(text.as_bytes());
        terminal.write(&format!("\x1b]52;c;{encoded}\x07"));
    }

    pub fn current_search_match_key(&self) -> Option<String> {
        let search = self.active_search.as_ref()?;
        let index = usize::try_from(search.selected_index).ok()?;
        search.matches.get(index).map(get_alt_screen_search_match_key)
    }

    /// senpi's `prepareKittyScreen`: swap an already-uploaded image's full transmission for its
    /// placement-only replacement line, and evict offscreen uploads once any cache cap is exceeded.
    fn prepare_kitty_screen(&mut self, screen: &[String]) -> (Vec<String>, String) {
        let mut visible_image_ids: Vec<u32> = Vec::new();
        let lines: Vec<String> = screen
            .iter()
            .map(|line| {
                let Some(placement) = image_stub::get_kitty_image_placement(line) else {
                    return line.clone();
                };
                visible_image_ids.push(placement.image_id);
                let cached_image = self
                    .uploaded_kitty_images
                    .iter()
                    .position(|(id, _)| *id == placement.image_id)
                    .map(|index| self.uploaded_kitty_images.remove(index).1);
                self.uploaded_kitty_images.push((
                    placement.image_id,
                    CachedKittyImage {
                        transmission_generation: placement.transmission_generation,
                        transmission_bytes: placement.transmission_bytes,
                        estimated_decoded_bytes: placement.estimated_decoded_bytes,
                    },
                ));
                if cached_image.is_some_and(|cached| cached.transmission_generation == placement.transmission_generation) {
                    placement.replacement_line
                } else {
                    line.clone()
                }
            })
            .collect();

        let mut offscreen_count = 0usize;
        let mut offscreen_transmission_bytes = 0u64;
        let mut offscreen_decoded_bytes = 0u64;
        for (image_id, cached) in &self.uploaded_kitty_images {
            if visible_image_ids.contains(image_id) {
                continue;
            }
            offscreen_count += 1;
            offscreen_transmission_bytes += cached.transmission_bytes;
            offscreen_decoded_bytes += cached.estimated_decoded_bytes;
        }

        let mut evicted_image_deletion = String::new();
        let mut index = 0;
        while index < self.uploaded_kitty_images.len() {
            if offscreen_count <= MAX_CACHED_OFFSCREEN_KITTY_IMAGES
                && offscreen_transmission_bytes <= MAX_CACHED_OFFSCREEN_KITTY_TRANSMISSION_BYTES
                && offscreen_decoded_bytes <= MAX_CACHED_OFFSCREEN_KITTY_DECODED_BYTES
            {
                break;
            }
            let (image_id, cached) = self.uploaded_kitty_images[index];
            if visible_image_ids.contains(&image_id) {
                index += 1;
                continue;
            }
            evicted_image_deletion.push_str(&image_stub::delete_kitty_image(image_id));
            self.uploaded_kitty_images.remove(index);
            offscreen_count -= 1;
            offscreen_transmission_bytes -= cached.transmission_bytes;
            offscreen_decoded_bytes -= cached.estimated_decoded_bytes;
        }
        (lines, evicted_image_deletion)
    }

    /// senpi's `shouldDeferViewportInputToOverlay`: a focused overlay other than the find bar
    /// owns the keyboard.
    fn should_defer_viewport_input_to_overlay(&self) -> bool {
        self.base.is_overlay_focused() && !self.active_search.as_ref().is_some_and(|search| search.overlay.is_focused())
    }

    pub fn handle_terminal_input(&mut self, data: &str, is_key_release: bool) {
        let kb = get_keybindings();
        if kb.matches(data, "tui.altScreen.search") {
            if !is_key_release {
                self.toggle_search();
            }
            return;
        }
        if self.active_search.as_ref().is_some_and(|search| search.overlay.is_focused()) {
            if kb.matches(data, "tui.altScreen.searchNext") {
                if !is_key_release {
                    self.navigate_search(1);
                }
                return;
            }
            if kb.matches(data, "tui.altScreen.searchPrevious") {
                if !is_key_release {
                    self.navigate_search(-1);
                }
                return;
            }
            if kb.matches(data, "tui.altScreen.searchClose") {
                if !is_key_release {
                    self.close_search();
                }
                return;
            }
        }
        if self.should_defer_viewport_input_to_overlay() {
            self.base.handle_terminal_input(data, is_key_release);
            return;
        }
        let viewport_height = self.get_primary_scroll_view().borrow().viewport_height() as i64;
        if kb.matches(data, "tui.altScreen.pageUp") {
            if !is_key_release {
                self.scroll_by(-(viewport_height - PAGE_SCROLL_OVERLAP).max(1));
            }
            return;
        }
        if kb.matches(data, "tui.altScreen.pageDown") {
            if !is_key_release {
                self.scroll_by((viewport_height - PAGE_SCROLL_OVERLAP).max(1));
            }
            return;
        }
        if kb.matches(data, "tui.altScreen.halfPageUp") {
            if !is_key_release {
                self.scroll_by(-(viewport_height / 2).max(1));
            }
            return;
        }
        if kb.matches(data, "tui.altScreen.halfPageDown") {
            if !is_key_release {
                self.scroll_by((viewport_height / 2).max(1));
            }
            return;
        }
        if kb.matches(data, "tui.altScreen.lineUp") {
            if !is_key_release {
                self.scroll_by(-1);
            }
            return;
        }
        if kb.matches(data, "tui.altScreen.lineDown") {
            if !is_key_release {
                self.scroll_by(1);
            }
            return;
        }
        if kb.matches(data, "tui.altScreen.previousPrompt") {
            if !is_key_release {
                self.scroll_to_prompt(-1);
            }
            return;
        }
        if kb.matches(data, "tui.altScreen.nextPrompt") {
            if !is_key_release {
                self.scroll_to_prompt(1);
            }
            return;
        }
        if kb.matches(data, "tui.altScreen.top") {
            if !is_key_release {
                self.scroll_to_top();
            }
            return;
        }
        if kb.matches(data, "tui.altScreen.bottom") {
            if !is_key_release {
                self.scroll_to_bottom();
            }
            return;
        }
        self.base.handle_terminal_input(data, is_key_release);
        self.apply_pending_search_query();
    }
}

impl Default for TuiAltScreen {
    fn default() -> Self {
        Self::new()
    }
}

fn scroll_scrollbar_to_pointer(scroll_view: &Rc<RefCell<ScrollView>>, geometry: &crate::layout::ScrollbarGeometry, pointer_y: i64, grab_offset: i64) {
    let max_thumb_offset = geometry.track_height.saturating_sub(geometry.thumb_height);
    let thumb_offset = (pointer_y - geometry.track_top - grab_offset).max(0).min(max_thumb_offset as i64);
    let scroll_top = if max_thumb_offset == 0 {
        0
    } else {
        ((thumb_offset as f64 / max_thumb_offset as f64) * geometry.max_scroll_top as f64).round() as i64
    };
    scroll_view.borrow_mut().scroll_to(scroll_top, Default::default(), 0);
}

fn apply_selection_highlight(lines: &mut [String], selection: Selection, width: usize) {
    let (start, end) = selection.ordered();
    for row in start.row..=end.row.min(lines.len().saturating_sub(1)) {
        let Some(line) = lines.get(row) else {
            continue;
        };
        let plain = strip_terminal_sequences(line);
        let line_width = visible_width(&plain);
        let row_start_col = if row == start.row { start.col } else { 0 };
        let row_end_col = (if row == end.row { end.col } else { width }).min(line_width);
        if row_start_col >= row_end_col {
            continue;
        }
        lines[row] = highlight_range(&plain, row_start_col, row_end_col);
    }
}

fn highlight_range(plain: &str, start_col: usize, end_col: usize) -> String {
    let width = visible_width(plain);
    let before = slice_plain(plain, 0, start_col);
    let middle = slice_plain(plain, start_col, end_col);
    let after = slice_plain(plain, end_col, width);
    format!("{before}\x1b[7m{middle}\x1b[27m{after}")
}

fn slice_plain(plain: &str, start_col: usize, end_col: usize) -> String {
    crate::utils::slice_by_column(plain, start_col, end_col.saturating_sub(start_col), true)
}

/// Minimal RFC 4648 base64 encoder for OSC 52 clipboard payloads; no external crate is pulled
/// in for one small, dependency-free encode.
mod base64_encode {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    pub fn encode_base64(data: &[u8]) -> String {
        let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
        for chunk in data.chunks(3) {
            let b0 = chunk[0];
            let b1 = *chunk.get(1).unwrap_or(&0);
            let b2 = *chunk.get(2).unwrap_or(&0);
            out.push(ALPHABET[(b0 >> 2) as usize] as char);
            out.push(ALPHABET[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize] as char);
            out.push(if chunk.len() > 1 { ALPHABET[(((b1 & 0x0f) << 2) | (b2 >> 6)) as usize] as char } else { '=' });
            out.push(if chunk.len() > 2 { ALPHABET[(b2 & 0x3f) as usize] as char } else { '=' });
        }
        out
    }
}
