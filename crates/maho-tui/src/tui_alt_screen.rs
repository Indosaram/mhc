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
    find_alt_screen_search_matches, get_alt_screen_search_match_key, AltScreenSearchComponent, AltScreenSearchMatch,
};
use crate::components::scroll_view::ScrollView;
use crate::image_stub::{self, ImageProtocol};
use crate::keybindings::get_keybindings;
use crate::layout::{get_scroll_view_box, get_scroll_views_at, get_scrollbar_geometry, render_layout_frame, LayoutFrame};
use crate::mouse_input::{is_mouse_sequence, parse_sgr_mouse_event, parse_wheel_event, MouseTracking, SgrMouseEvent};
use crate::terminal::Terminal;
use crate::tui::{can_receive_keys, Component, Focusable, MouseBlocker, TuiBase};
use crate::utils::{strip_terminal_sequences, visible_width, word_segments};

/// A double/triple-click word-selection joiner set: fullscreen owns mouse selection and
/// mirrors common terminal word-selection behavior by keeping paths and kebab-case tokens
/// whole (senpi's `TERMINAL_WORD_SELECTION_JOINERS`).
const WORD_SELECTION_JOINERS: [&str; 2] = ["/", "-"];
const DOUBLE_CLICK_INTERVAL_MS: i64 = 500;

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

const KITTY_IMAGE_BYTE_BUDGET: u64 = 64 * 1024 * 1024;

struct CachedKittyImage {
    generation: u64,
    bytes: u64,
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
    search_component: Option<Rc<RefCell<AltScreenSearchComponent>>>,
    search_query: String,
    search_matches: Vec<AltScreenSearchMatch>,
    search_selected_index: i64,
    search_visible: bool,
    kitty_image_cache: std::collections::HashMap<u32, CachedKittyImage>,
    kitty_image_cache_generation: u64,
    kitty_image_cache_bytes: u64,
}

pub struct MouseTrackingSupport {
    pub is_tty: bool,
    pub is_termux: bool,
    pub is_windows_without_wt: bool,
}

impl TuiAltScreen {
    pub fn new() -> Self {
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
            search_component: None,
            search_query: String::new(),
            search_matches: Vec::new(),
            search_selected_index: -1,
            search_visible: false,
            kitty_image_cache: std::collections::HashMap::new(),
            kitty_image_cache_generation: 0,
            kitty_image_cache_bytes: 0,
        }
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

    pub fn before_terminal_stop(&mut self, terminal: &mut dyn Terminal) {
        terminal.write(MouseTracking::DISABLE);
        self.tracking_enabled = false;
        self.clear_text_selection();
        self.stop_scrollbar_drag();
        self.stop_scrollbar_hover();
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

    /// Renders the full alt-screen frame (senpi's `TuiAltScreen.doRender`, which fully
    /// replaces `TuiBase.doRender` rather than calling `super.doRender()` - the alt screen
    /// always issues a full non-differential redraw).
    pub fn render_frame(&mut self, terminal: &mut dyn Terminal) -> Vec<String> {
        let width = (terminal.columns() as usize).max(1);
        let height = (terminal.rows() as usize).max(1);

        let Some(root) = self.layout_root.clone() else {
            self.current_layout = None;
            return vec![String::new(); height];
        };

        let frame = render_layout_frame(&root, width, height);
        self.refresh_search(&frame.lines);
        let mut lines = frame.lines.clone();

        if let (Some(anchor), Some(focus)) = (self.selection_anchor, self.selection_focus) {
            apply_selection_highlight(&mut lines, Selection { anchor, focus }, width);
        }

        if self.search_visible
            && let Some(search) = self.search_component.clone()
        {
            let search_width = width.clamp(20, 60);
            let search_lines = search.borrow_mut().render(search_width);
            let start_col = width.saturating_sub(search_width).saturating_sub(1);
            for (i, search_line) in search_lines.iter().enumerate() {
                if let Some(line) = lines.get_mut(i) {
                    *line = overlay_line(line, search_line, start_col, width);
                }
            }
        }

        while lines.len() < height {
            lines.push(String::new());
        }
        lines.truncate(height);
        self.current_layout = Some(frame);
        lines
    }

    fn refresh_search(&mut self, lines: &[String]) {
        if !self.search_visible {
            return;
        }
        self.search_matches = find_alt_screen_search_matches(lines, &self.search_query);
        if self.search_matches.is_empty() {
            self.search_selected_index = -1;
        } else if self.search_selected_index < 0 || self.search_selected_index as usize >= self.search_matches.len() {
            self.search_selected_index = 0;
        }
        if let Some(search) = &self.search_component {
            search.borrow_mut().set_result(self.search_selected_index, self.search_matches.len());
        }
    }

    /// Registered via `TuiBase::add_input_listener` (senpi's `handleMouseInput`).
    pub fn handle_mouse_input(&mut self, data: &str, now_ms: i64, terminal: &mut dyn Terminal) -> bool {
        if !is_mouse_sequence(data) {
            return false;
        }
        if let Some(wheel) = parse_wheel_event(data) {
            self.route_wheel(wheel.direction, wheel.x, wheel.y);
            return true;
        }
        let Some(raw) = parse_sgr_mouse_event(data) else {
            return true;
        };
        self.handle_mouse_event(raw, now_ms, terminal);
        true
    }

    fn route_wheel(&mut self, direction: i64, x: i64, y: i64) {
        let Some(frame) = self.current_layout.as_ref() else {
            return;
        };
        let mut remaining = direction * 3;
        let scroll_views = get_scroll_views_at(frame, x, y);
        let primary_scroll_view = frame.primary_scroll_view.clone();
        let mut seen: Vec<*const RefCell<ScrollView>> = Vec::new();
        for scroll_view in &scroll_views {
            seen.push(Rc::as_ptr(scroll_view));
            let contain = scroll_view.borrow().overscroll() == crate::components::scroll_view::Overscroll::Contain;
            remaining = scroll_view.borrow_mut().scroll_by(remaining, 0);
            if remaining == 0 || contain {
                break;
            }
        }
        if let Some(primary) = &primary_scroll_view
            && remaining != 0
            && !seen.contains(&Rc::as_ptr(primary))
        {
            primary.borrow_mut().scroll_by(remaining, 0);
        }
        // clippy-collapsible-if-guard: primary lookup stays a nested condition on purpose since
        // the outer `if let` binds `primary` and clippy's own suggested collapse is unavailable
        // when the inner condition references the outer binding across an `&&`; left as-is.
        self.set_scrollbar_hover(self.get_scrollbar_target_at(x, y).map(|(sv, _)| sv), 0);
        self.base.request_render(false, 0);
    }

    fn handle_mouse_event(&mut self, raw: SgrMouseEvent, now_ms: i64, terminal: &mut dyn Terminal) {
        // Handles left-button press/drag/release routing for scrollbar drag and text
        // selection. Motion bit 32 marks drag/move; senpi decodes the low 3 button bits, but
        // this port only supports button 0 (left) for selection/scrollbar as `Input`/`Text`
        // consumers do not expose right/middle-button behavior in this crate.
        if self.handle_scrollbar_mouse_event(&raw) {
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

    pub fn toggle_search(&mut self, visible: bool) {
        self.search_visible = visible;
        if let Some(search) = &self.search_component {
            search.borrow_mut().set_focused(visible);
        }
        if !visible {
            self.base.set_focus(None);
        }
        self.base.request_render(true, 0);
    }

    pub fn set_search_component(&mut self, search: Rc<RefCell<AltScreenSearchComponent>>) {
        self.search_component = Some(search);
    }

    pub fn on_search_query_change(&mut self, query: String) {
        self.search_query = query;
        self.search_selected_index = -1;
        self.base.request_render(false, 0);
    }

    pub fn navigate_search(&mut self, direction: i64) {
        if self.search_matches.is_empty() {
            return;
        }
        let len = self.search_matches.len() as i64;
        self.search_selected_index = ((self.search_selected_index.max(0) + direction) % len + len) % len;
        if let Some(search) = &self.search_component {
            search.borrow_mut().set_result(self.search_selected_index, self.search_matches.len());
        }
        self.base.request_render(false, 0);
    }

    pub fn current_search_match_key(&self) -> Option<String> {
        self.search_matches
            .get(self.search_selected_index.max(0) as usize)
            .map(get_alt_screen_search_match_key)
    }

    /// Records a rendered Kitty image placeholder for the byte-budget-bounded offscreen cache
    /// (senpi's `uploadedKittyImages` eviction policy). Uses `image_stub.rs`'s placeholder
    /// metadata extraction (todo 9 gap: real transmission byte accounting).
    pub fn note_rendered_image_line(&mut self, line: &str, protocol: ImageProtocol) {
        if protocol != ImageProtocol::Kitty {
            return;
        }
        let Some(placement) = image_stub::get_kitty_image_placement(line) else {
            return;
        };
        self.kitty_image_cache_generation += 1;
        let previous = self.kitty_image_cache.insert(
            placement.image_id,
            CachedKittyImage { generation: self.kitty_image_cache_generation, bytes: placement.estimated_decoded_bytes },
        );
        if let Some(previous) = previous {
            self.kitty_image_cache_bytes = self.kitty_image_cache_bytes.saturating_sub(previous.bytes);
        }
        self.kitty_image_cache_bytes += placement.estimated_decoded_bytes;
        self.evict_kitty_images_over_budget();
    }

    fn evict_kitty_images_over_budget(&mut self) {
        while self.kitty_image_cache_bytes > KITTY_IMAGE_BYTE_BUDGET {
            let Some((&oldest_id, _)) = self.kitty_image_cache.iter().min_by_key(|(_, cached)| cached.generation) else {
                break;
            };
            if let Some(cached) = self.kitty_image_cache.remove(&oldest_id) {
                self.kitty_image_cache_bytes = self.kitty_image_cache_bytes.saturating_sub(cached.bytes);
            } else {
                break;
            }
        }
    }

    pub fn handle_terminal_input(&mut self, data: &str, is_key_release: bool) {
        let kb = get_keybindings();
        if kb.matches(data, "tui.altScreen.search") {
            if !is_key_release {
                self.toggle_search(!self.search_visible);
            }
            return;
        }
        if self.search_visible && kb.matches(data, "tui.altScreen.searchNext") {
            if !is_key_release {
                self.navigate_search(1);
            }
            return;
        }
        if self.search_visible && kb.matches(data, "tui.altScreen.searchPrevious") {
            if !is_key_release {
                self.navigate_search(-1);
            }
            return;
        }
        if self.search_visible && kb.matches(data, "tui.altScreen.searchClose") {
            if !is_key_release {
                self.toggle_search(false);
            }
            return;
        }
        if !self.search_visible && kb.matches(data, "tui.altScreen.top") {
            if let Some(primary) = self.current_layout.as_ref().and_then(|f| f.primary_scroll_view.clone())
                && !is_key_release
            {
                primary.borrow_mut().scroll_to_start(0);
                self.base.request_render(false, 0);
            }
            return;
        }
        if !self.search_visible && kb.matches(data, "tui.altScreen.bottom") {
            if let Some(primary) = self.current_layout.as_ref().and_then(|f| f.primary_scroll_view.clone())
                && !is_key_release
            {
                primary.borrow_mut().scroll_to_end(0);
                self.base.request_render(false, 0);
            }
            return;
        }
        if !self.search_visible && kb.matches(data, "tui.altScreen.lineUp") {
            if let Some(primary) = self.current_layout.as_ref().and_then(|f| f.primary_scroll_view.clone())
                && !is_key_release
            {
                primary.borrow_mut().scroll_by(-1, 0);
                self.base.request_render(false, 0);
            }
            return;
        }
        if !self.search_visible && kb.matches(data, "tui.altScreen.lineDown") {
            if let Some(primary) = self.current_layout.as_ref().and_then(|f| f.primary_scroll_view.clone())
                && !is_key_release
            {
                primary.borrow_mut().scroll_by(1, 0);
                self.base.request_render(false, 0);
            }
            return;
        }
        self.base.handle_terminal_input(data, is_key_release);
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

fn overlay_line(base_line: &str, overlay: &str, start_col: usize, width: usize) -> String {
    let plain_base = strip_terminal_sequences(base_line);
    let before = slice_plain(&plain_base, 0, start_col);
    let overlay_width = visible_width(overlay);
    let after_col = (start_col + overlay_width).min(width);
    let after = slice_plain(&plain_base, after_col, width);
    format!("{before}{overlay}{after}")
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
