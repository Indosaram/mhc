//! Port of senpi `packages/tui/src/tui.ts`.
//!
//! senpi resolves `queryTerminalBackgroundColor`/`queryTerminalColorScheme` with a JS
//! `Promise` and schedules frames with `process.nextTick`/`setTimeout` on Node's event loop.
//! This crate has no event loop: `ProcessTerminal` (see `terminal.rs`, todo 6) exposes the
//! same asynchrony as a poll-driven [`crate::terminal::CursorQueryTicket`] plus an explicit
//! [`crate::stdin_buffer::Clock`], so the pending-query state below is polled from
//! [`TuiBase::poll`] (called once per driver loop iteration) instead of attached to promise
//! callbacks. Render scheduling collapses to the same call: [`TuiBase::request_render`] marks
//! a frame due at `now + min_render_interval_ms`, and [`TuiBase::poll`] renders once that
//! deadline has passed, mirroring `process.nextTick` (immediate) and the 16ms `setTimeout`
//! throttle faithfully without an event loop to hook into.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use crate::image_stub::{
    delete_kitty_image, extract_kitty_image_ids, extract_kitty_image_rows, get_capabilities,
    is_image_line,
};
use crate::terminal::Terminal;
use crate::terminal_colors::{
    is_osc11_background_color_response, parse_osc11_background_color,
    parse_terminal_color_scheme_report, RgbColor, TerminalColorScheme,
};
use crate::utils::{
    extract_segments, normalize_terminal_output, slice_by_column, slice_with_width, visible_width,
};

const MAX_RENDER_WRITE_CHARS: usize = 1024 * 1024;
const VIEWPORT_RENDER_OVERSCAN: usize = 16;
const MAX_SCROLL_DIFF_ROWS: usize = 4;
const SEGMENT_RESET: &str = "\x1b[0m\x1b]8;;\x07";
const FRAME_BEGIN: &str = "\x1b[?2026h\x1b[?7l";
const FRAME_END: &str = "\x1b[?7h\x1b[?2026l";
/// Zero-width APC marker: components emit this at the cursor position when focused.
pub const CURSOR_MARKER: &str = "\x1b_pi:c\x07";
const FAKE_CURSOR_START: &str = "\x1b[7m";
const FAKE_CURSOR_END: &str = "\x1b[27m";
const FAKE_CURSOR_RESET: &str = "\x1b[0m";
const ENABLE_TERMINAL_COLOR_SCHEME_NOTIFICATIONS: &str = "\x1b[?2031h";
const DISABLE_TERMINAL_COLOR_SCHEME_NOTIFICATIONS: &str = "\x1b[?2031l";
const QUERY_CELL_SIZE: &str = "\x1b[16t";
const QUERY_OSC11_BACKGROUND_COLOR: &str = "\x1b]11;?\x07";
const QUERY_TERMINAL_COLOR_SCHEME: &str = "\x1b[?996n";

/// senpi checks `process.env.TMUX` (any non-empty value) before toggling tmux focus reporting.
fn in_tmux() -> bool {
    crate::process_env::var("TMUX").is_some_and(|value| !value.is_empty())
}

/// senpi's `isTermuxSession`: `Boolean(process.env.TERMUX_VERSION)`.
fn is_termux_session() -> bool {
    crate::process_env::var("TERMUX_VERSION").is_some_and(|value| !value.is_empty())
}

/// senpi's `CSI 6 ; height ; width t` cell-size reply (`/^\x1b\[6;(\d+);(\d+)t$/`), as
/// `(height_px, width_px)`.
fn parse_cell_size_response(data: &str) -> Option<(u32, u32)> {
    let body = data.strip_prefix("\x1b[6;")?.strip_suffix('t')?;
    let (height, width) = body.split_once(';')?;
    let digits = |value: &str| !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit());
    if !digits(height) || !digits(width) {
        return None;
    }
    Some((height.parse().unwrap_or(u32::MAX), width.parse().unwrap_or(u32::MAX)))
}

/// Resolution of a terminal protocol query (senpi's `queryTerminalBackgroundColor` /
/// `queryTerminalColorScheme` promises). `result()` is `None` while pending and `Some(value)`
/// once the reply arrives or the query times out (then `value` is `None`); the driver loop
/// advances timeouts through [`TuiBase::poll_terminal_queries`].
#[derive(Clone)]
pub struct TerminalQueryTicket<T>(Rc<RefCell<Option<T>>>);

impl<T: Copy> TerminalQueryTicket<T> {
    fn pending() -> Self {
        Self(Rc::new(RefCell::new(None)))
    }

    pub fn result(&self) -> Option<T> {
        *self.0.borrow()
    }

    fn settle(&self, value: T) {
        let mut slot = self.0.borrow_mut();
        if slot.is_none() {
            *slot = Some(value);
        }
    }
}

struct PendingTerminalQuery<T> {
    ticket: TerminalQueryTicket<T>,
    deadline_ms: u64,
}

pub type TerminalColorSchemeListener = Box<dyn FnMut(TerminalColorScheme)>;

fn write_bounded(terminal: &mut dyn Terminal, data: &str) {
    let bytes = data.as_bytes();
    let mut offset = 0;
    while offset < bytes.len() {
        let mut end = (offset + MAX_RENDER_WRITE_CHARS).min(bytes.len());
        while end < bytes.len() && !data.is_char_boundary(end) {
            end -= 1;
        }
        if end <= offset {
            end = (offset + 1).min(bytes.len());
            while end < bytes.len() && !data.is_char_boundary(end) {
                end += 1;
            }
        }
        terminal.write(&data[offset..end]);
        offset = end;
    }
}

/// Normalized cell-based mouse event. Coordinates are zero-based.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TuiMouseEventType {
    Press,
    Release,
    Move,
    Drag,
    Click,
    Wheel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TuiMouseButton {
    Left,
    Middle,
    Right,
    None,
}

#[derive(Debug, Clone, Copy)]
pub struct TuiMouseEvent {
    pub event_type: TuiMouseEventType,
    pub button: TuiMouseButton,
    /// Coordinates local to the receiving component.
    pub x: i64,
    pub y: i64,
    /// Absolute terminal coordinates.
    pub screen_x: i64,
    pub screen_y: i64,
    /// Current component bounds.
    pub width: usize,
    pub height: usize,
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
    /// Logical lines. Negative values scroll up.
    pub wheel_delta: Option<i64>,
    /// Consecutive click count when `event_type` is `Click`.
    pub click_count: Option<u32>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct TuiMouseEventResult {
    /// Stop propagation and suppress renderer-level fallback behavior.
    pub handled: bool,
    /// Route subsequent drag/release events to this component. Implies handled.
    pub capture: bool,
    /// Give keyboard focus to this component. Implies handled.
    pub focus: bool,
    /// Explicit render request/suppression. `None` uses the event-type default.
    pub render: Option<bool>,
}

/// Internal target metadata used by containers and alternate-screen dispatch.
#[derive(Clone)]
pub struct TuiMouseDispatchTarget {
    pub component: Rc<RefCell<dyn Component>>,
    pub origin_x: i64,
    pub origin_y: i64,
    pub width: usize,
    pub height: usize,
}

/// Result of dispatching to a concrete component.
#[derive(Clone)]
pub struct TuiMouseDispatchResult {
    pub result: TuiMouseEventResult,
    pub target: TuiMouseDispatchTarget,
    /// Keyboard focus target, which may be a delegating parent container.
    pub focus_target: Option<Rc<RefCell<dyn Component>>>,
}

/// Dispatch an event to a component and retain the exact target and coordinate transform.
/// Containers use this when forwarding events to nested children.
pub fn dispatch_mouse_event(
    component: &Rc<RefCell<dyn Component>>,
    event: &TuiMouseEvent,
) -> Option<TuiMouseDispatchResult> {
    let result = component.borrow_mut().handle_mouse(event)?;
    if !result.handled && !result.capture && !result.focus {
        return None;
    }
    let focus_target = if result.focus {
        Some(Rc::clone(component))
    } else {
        None
    };
    Some(TuiMouseDispatchResult {
        result,
        focus_target,
        target: TuiMouseDispatchTarget {
            component: Rc::clone(component),
            origin_x: event.screen_x - event.x,
            origin_y: event.screen_y - event.y,
            width: event.width,
            height: event.height,
        },
    })
}

/// Recreate local coordinates for a previously dispatched mouse target.
pub fn retarget_mouse_event(event: &TuiMouseEvent, target: &TuiMouseDispatchTarget) -> TuiMouseEvent {
    TuiMouseEvent {
        x: event.screen_x - target.origin_x,
        y: event.screen_y - target.origin_y,
        width: target.width,
        height: target.height,
        ..*event
    }
}

/// Component interface - all components must implement this.
pub trait Component {
    /// Render the component to lines for the given viewport width.
    fn render(&mut self, width: usize) -> Vec<String>;

    /// Handler for keyboard input when the component has focus.
    fn handle_input(&mut self, _data: &str) {}

    /// `true` when this component defines [`Component::handle_input`] (senpi checks
    /// `typeof component.handleInput === "function"`; Rust has no such reflection, so
    /// implementors override this alongside `handle_input`).
    fn has_input_handler(&self) -> bool {
        false
    }

    /// Normalized mouse handler.
    fn handle_mouse(&mut self, _event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        None
    }

    /// If true, the component receives key release events (Kitty protocol).
    fn wants_key_release(&self) -> bool {
        false
    }

    /// Invalidate any cached rendering state.
    fn invalidate(&mut self) {}

    fn dispose(&mut self) {}

    /// `Some(focused)` when the component implements [`Focusable`] (senpi's `"focused" in
    /// component` duck-typed check).
    fn focusable_get(&self) -> Option<bool> {
        None
    }

    fn focusable_set(&mut self, _focused: bool) {}

    /// Downcast support for `instanceof Container` checks.
    fn as_container(&self) -> Option<&Container> {
        None
    }

    fn as_container_mut(&mut self) -> Option<&mut Container> {
        None
    }

    /// Downcast support for senpi's `component[LAYOUT_NODE]()` duck-typed check
    /// ([`crate::layout_node::LayoutComponent`]).
    fn as_layout_component(&self) -> Option<&dyn crate::layout_node::LayoutComponent> {
        None
    }

    /// senpi's `layout.test.ts` "paints only clipped rows from very large scroll content"
    /// relies on JavaScript's sparse arrays: a component reports a billion rows while
    /// materializing four. `Vec<String>` cannot be sparse, so a component whose content is
    /// virtual declares its row count here and answers rows through
    /// [`Component::render_virtual_row`]; the layout engine then never calls
    /// [`Component::render`] for it and reads only the rows it paints.
    fn virtual_line_count(&self, _width: usize) -> Option<usize> {
        None
    }

    /// Row `index` of a component that declared [`Component::virtual_line_count`].
    fn render_virtual_row(&mut self, _width: usize, _index: usize) -> String {
        String::new()
    }

    /// senpi `component.constructor.name`: the implementing type's name, used by render
    /// containment to label a component whose render panicked.
    fn component_name(&self) -> &'static str {
        std::any::type_name::<Self>()
    }
}

/// senpi `DIAGNOSTIC_LOG_MODE`: diagnostic logs hold rendered conversation text, so they are
/// private to the owner.
#[cfg(unix)]
const DIAGNOSTIC_LOG_MODE: u32 = 0o600;

/// senpi `renderErrorLoggedClasses`: component names whose render error was already logged.
static RENDER_ERROR_LOGGED_CLASSES: std::sync::Mutex<Option<HashSet<String>>> = std::sync::Mutex::new(None);

/// senpi `renderErrorLogDirectory`: render containment logs from module scope because
/// `Container` has no TUI instance, so the host-resolved log directory is published here.
static RENDER_ERROR_LOG_DIRECTORY: std::sync::Mutex<Option<std::path::PathBuf>> = std::sync::Mutex::new(None);

/// senpi `defaultDiagnosticLogDirectory`: `~/.senpi/agent`.
fn default_diagnostic_log_directory() -> std::path::PathBuf {
    dirs::home_dir()
        .map(|home| home.join(".senpi").join("agent"))
        .unwrap_or_else(|| std::path::PathBuf::from(".senpi/agent"))
}

/// senpi `new Date().toISOString()`.
fn diagnostic_timestamp() -> String {
    format!("{:.3}", jiff::Timestamp::now())
}

/// senpi `componentRenderErrorName`: the type name without its module path or generics.
fn component_render_error_name(component: &Rc<RefCell<dyn Component>>) -> String {
    let Ok(component) = component.try_borrow() else {
        return "AnonymousComponent".to_owned();
    };
    let full = component.component_name();
    let base = full.split('<').next().unwrap_or(full);
    match base.rsplit("::").next() {
        Some(name) if !name.is_empty() => name.to_owned(),
        _ => "AnonymousComponent".to_owned(),
    }
}

/// senpi `logRenderErrorOnce`: append one `render error` line per component name to
/// `<logDir>/senpi-debug.log`.
fn log_render_error_once(component: &Rc<RefCell<dyn Component>>, error: &(dyn std::any::Any + Send)) {
    let component_name = component_render_error_name(component);
    {
        let mut logged = RENDER_ERROR_LOGGED_CLASSES.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if !logged.get_or_insert_with(HashSet::new).insert(component_name.clone()) {
            return;
        }
    }
    let error_text = error
        .downcast_ref::<&str>()
        .map(|message| (*message).to_owned())
        .or_else(|| error.downcast_ref::<String>().cloned())
        .map_or_else(|| "panic".to_owned(), |message| format!("panic: {message}"));
    let log_directory = RENDER_ERROR_LOG_DIRECTORY
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
        .unwrap_or_else(default_diagnostic_log_directory);
    let log_path = log_directory.join("senpi-debug.log");
    let msg = format!("[{}] render error: {component_name}: {error_text}\n", diagnostic_timestamp());
    append_render_error_log_best_effort(&log_path, &msg);
}

/// `fs.appendFileSync`/`fs.writeFileSync` with `{ mode: DIAGNOSTIC_LOG_MODE }`.
fn open_diagnostic_log(log_path: &std::path::Path, append: bool) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.create(true);
    if append {
        options.append(true);
    } else {
        options.write(true).truncate(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(DIAGNOSTIC_LOG_MODE);
    }
    options.open(log_path)
}

/// senpi `appendRenderErrorLogBestEffort`.
fn append_render_error_log_best_effort(log_path: &std::path::Path, msg: &str) {
    use std::io::Write;
    if let Some(parent) = log_path.parent()
        && std::fs::create_dir_all(parent).is_err()
    {
        return;
    }
    if let Ok(mut file) = open_diagnostic_log(log_path, true)
        && file.write_all(msg.as_bytes()).is_ok()
    {
        chmod_diagnostic_log_best_effort(log_path);
    }
}

/// senpi `writeRenderDiagnosticBestEffort`.
fn write_render_diagnostic_best_effort(log_path: &std::path::Path, data: &str) -> bool {
    use std::io::Write;
    if let Some(parent) = log_path.parent()
        && std::fs::create_dir_all(parent).is_err()
    {
        return false;
    }
    let written = open_diagnostic_log(log_path, false).and_then(|mut file| file.write_all(data.as_bytes()));
    if written.is_err() {
        return false;
    }
    chmod_diagnostic_log_best_effort(log_path);
    true
}

/// senpi `formatOverWideRenderDiagnostic`.
fn format_over_wide_render_diagnostic(lines: &[String], terminal_width: usize, line_index: usize, line_width: usize) -> String {
    let mut parts = vec![
        format!("Crash at {}", diagnostic_timestamp()),
        format!("Terminal width: {terminal_width}"),
        format!("Line {line_index} visible width: {line_width}"),
        String::new(),
        "=== All rendered lines ===".to_owned(),
    ];
    parts.extend(lines.iter().enumerate().map(|(index, line)| format!("[{index}] (w={}) {line}", visible_width(line))));
    parts.push(String::new());
    parts.join("\n")
}

/// senpi `chmodDiagnosticLogBestEffort`: tighten a pre-existing file the open mode did not touch.
fn chmod_diagnostic_log_best_effort(log_path: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(log_path, std::fs::Permissions::from_mode(DIAGNOSTIC_LOG_MODE));
    }
    #[cfg(not(unix))]
    let _ = log_path;
}

/// The renderer locals senpi's `PI_TUI_DEBUG` dump reports for one differential frame.
struct TuiDebugFrame {
    first_changed: usize,
    viewport_top: usize,
    height: usize,
    line_diff: i64,
    hardware_cursor_row: usize,
    render_end: usize,
    final_cursor_row: usize,
    cursor_pos: Option<(usize, usize)>,
}

/// Interface for components that can receive focus and display a hardware cursor.
pub trait Focusable {
    fn focused(&self) -> bool;
    fn set_focused(&mut self, value: bool);
}

pub fn is_focusable(component: &Rc<RefCell<dyn Component>>) -> bool {
    component.borrow().focusable_get().is_some()
}

/// Only a component that can receive keys may own keyboard focus.
pub fn can_receive_keys(component: &Rc<RefCell<dyn Component>>) -> bool {
    component.borrow().has_input_handler()
}

pub type MouseLayoutCache = (usize, Vec<(Rc<RefCell<dyn Component>>, usize)>);

/// Container - a component that contains other components.
pub struct Container {
    pub children: Vec<Rc<RefCell<dyn Component>>>,
    disposed: bool,
    mouse_layout: Option<MouseLayoutCache>,
}

impl Default for Container {
    fn default() -> Self {
        Self::new()
    }
}

impl Container {
    pub fn new() -> Self {
        Self {
            children: Vec::new(),
            disposed: false,
            mouse_layout: None,
        }
    }

    /// Same recursive dispatch as [`Component::handle_mouse`], but also returns the full
    /// [`TuiMouseDispatchResult`] (with the matched child's retarget handle) instead of just
    /// the [`TuiMouseEventResult`]. Used where the caller is not itself wrapped as
    /// `Rc<RefCell<dyn Component>>` and so cannot go through the top-level
    /// [`dispatch_mouse_event`] (senpi's `TuiBase extends Container`, where `this` is always
    /// such a handle; this crate's `TuiBase` holds its `Container` unwrapped).
    pub fn dispatch_mouse_with_target(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseDispatchResult> {
        if event.y < 0 || event.y >= event.height as i64 {
            return None;
        }
        let mouse_children = match &self.mouse_layout {
            Some((w, children)) if *w == event.width => children.clone(),
            _ => self
                .children
                .iter()
                .map(|c| {
                    let height = c.borrow_mut().render(event.width).len();
                    (Rc::clone(c), height)
                })
                .collect(),
        };
        let mut child_y: i64 = 0;
        for (child, child_height) in mouse_children {
            let child_height = child_height as i64;
            if event.y >= child_y && event.y < child_y + child_height {
                let sub_event = TuiMouseEvent {
                    y: event.y - child_y,
                    height: child_height as usize,
                    ..*event
                };
                return dispatch_mouse_event(&child, &sub_event);
            }
            child_y += child_height;
        }
        None
    }

    pub fn add_child(&mut self, component: Rc<RefCell<dyn Component>>) {
        self.children.push(component);
    }

    pub fn remove_child(&mut self, component: &Rc<RefCell<dyn Component>>) {
        if let Some(index) = self
            .children
            .iter()
            .position(|child| Rc::ptr_eq(child, component))
        {
            let removed = self.children.remove(index);
            removed.borrow_mut().dispose();
        }
    }

    pub fn detach_child(&mut self, component: &Rc<RefCell<dyn Component>>) {
        if let Some(index) = self
            .children
            .iter()
            .position(|child| Rc::ptr_eq(child, component))
        {
            self.children.remove(index);
        }
    }

    pub fn clear(&mut self) {
        for child in self.children.drain(..) {
            child.borrow_mut().dispose();
        }
    }

    pub fn detach_all(&mut self) {
        self.children.clear();
    }
}

impl Component for Container {
    fn render(&mut self, width: usize) -> Vec<String> {
        let mut lines = Vec::new();
        let mut mouse_children = Vec::with_capacity(self.children.len());
        for child in &self.children {
            let rendered = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| child.borrow_mut().render(width)));
            let child_lines = match rendered {
                Ok(child_lines) => child_lines,
                Err(error) => {
                    log_render_error_once(child, error.as_ref());
                    let component_name = component_render_error_name(child);
                    // Focus ownership stays unchanged; render containment must not steal or clear focus implicitly.
                    vec![format!("[render error: {component_name}]")]
                }
            };
            mouse_children.push((Rc::clone(child), child_lines.len()));
            lines.extend(child_lines);
        }
        self.mouse_layout = Some((width, mouse_children));
        lines
    }

    fn handle_input(&mut self, _data: &str) {}

    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        if event.y < 0 || event.y >= event.height as i64 {
            return None;
        }
        let mouse_children = match &self.mouse_layout {
            Some((w, children)) if *w == event.width => children.clone(),
            _ => self
                .children
                .iter()
                .map(|c| {
                    let height = c.borrow_mut().render(event.width).len();
                    (Rc::clone(c), height)
                })
                .collect(),
        };
        let mut child_y: i64 = 0;
        for (child, child_height) in mouse_children {
            let child_height = child_height as i64;
            if event.y >= child_y && event.y < child_y + child_height {
                let sub_event = TuiMouseEvent {
                    y: event.y - child_y,
                    height: child_height as usize,
                    ..*event
                };
                let result = dispatch_mouse_event(&child, &sub_event)?;
                return Some(result.result);
            }
            child_y += child_height;
        }
        None
    }

    fn invalidate(&mut self) {
        for child in &self.children {
            child.borrow_mut().invalidate();
        }
    }

    fn dispose(&mut self) {
        if self.disposed {
            return;
        }
        self.disposed = true;
        for child in &self.children {
            child.borrow_mut().dispose();
        }
    }

    fn as_container(&self) -> Option<&Container> {
        Some(self)
    }

    fn as_container_mut(&mut self) -> Option<&mut Container> {
        Some(self)
    }
}

/// Composite overlay content into a terminal line at a fixed column.
pub fn composite_tui_line(
    base_line: &str,
    overlay_line: &str,
    start_col: usize,
    overlay_width: usize,
    total_width: usize,
) -> String {
    composite_line_at(base_line, overlay_line, start_col, overlay_width, total_width)
}

fn composite_line_at(
    base_line: &str,
    overlay_line: &str,
    start_col: usize,
    overlay_width: usize,
    total_width: usize,
) -> String {
    if is_image_line(base_line) && visible_width(base_line) == 0 {
        return base_line.to_string();
    }
    let placeholder_index = base_line.find('\u{10eeee}');
    let protocol_end = placeholder_index.and_then(|idx| base_line[..idx].rfind("\x1b\\"));
    let protocol_prefix = protocol_end.map_or("", |end| &base_line[..end + 2]);

    let after_start = start_col + overlay_width;
    let base = extract_segments(
        base_line,
        start_col,
        after_start,
        total_width.saturating_sub(after_start),
        true,
    );
    let (overlay_text, overlay_actual_width) = slice_with_width(overlay_line, 0, overlay_width, true);

    let before_pad = start_col.saturating_sub(base.before_width);
    let overlay_pad = overlay_width.saturating_sub(overlay_actual_width);
    let actual_before_width = start_col.max(base.before_width);
    let actual_overlay_width = overlay_width.max(overlay_actual_width);
    let after_target = total_width.saturating_sub(actual_before_width + actual_overlay_width);
    let after_pad = after_target.saturating_sub(base.after_width);

    let result = format!(
        "{}{}{}{}{}{}{}{}",
        base.before,
        " ".repeat(before_pad),
        SEGMENT_RESET,
        overlay_text,
        " ".repeat(overlay_pad),
        SEGMENT_RESET,
        base.after,
        " ".repeat(after_pad),
    );

    let composited = if visible_width(&result) <= total_width {
        result
    } else {
        slice_by_column(&result, 0, total_width, true)
    };
    format!("{protocol_prefix}{composited}")
}

pub type TuiMode = &'static str;

#[derive(Debug, Clone, Copy, Default)]
pub struct TuiStopOptions {
    /// Leave renderer output in place for another TUI taking over the same terminal.
    pub preserve_screen: bool,
}

/// Anchor position for overlays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayAnchor {
    Center,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
    TopCenter,
    BottomCenter,
    LeftCenter,
    RightCenter,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct OverlayMargin {
    pub top: Option<i64>,
    pub right: Option<i64>,
    pub bottom: Option<i64>,
    pub left: Option<i64>,
}

/// Value that can be absolute (columns/rows) or a percentage of the reference size.
#[derive(Debug, Clone, Copy)]
pub enum SizeValue {
    Absolute(i64),
    Percent(f64),
}

fn parse_size_value(value: Option<SizeValue>, reference_size: i64) -> Option<i64> {
    match value? {
        SizeValue::Absolute(v) => Some(v),
        SizeValue::Percent(p) => Some(((reference_size as f64 * p) / 100.0).floor() as i64),
    }
}

/// Options for overlay positioning and sizing.
#[derive(Clone, Default)]
pub struct OverlayOptions {
    pub width: Option<SizeValue>,
    pub min_width: Option<i64>,
    pub max_height: Option<SizeValue>,
    pub anchor: Option<OverlayAnchor>,
    pub offset_x: Option<i64>,
    pub offset_y: Option<i64>,
    pub row: Option<SizeValue>,
    pub col: Option<SizeValue>,
    pub margin: Option<OverlayMargin>,
    /// If true, don't capture keyboard focus when shown.
    pub non_capturing: bool,
    /// Overlay is only rendered when this returns true, given current terminal dimensions.
    pub visible: Option<Rc<dyn Fn(u16, u16) -> bool>>,
}

/// Options for [`OverlayHandle::unfocus`].
pub struct OverlayUnfocusOptions {
    pub target: Option<Rc<RefCell<dyn Component>>>,
}

/// Last rendered terminal-relative overlay rectangle.
#[derive(Debug, Clone, Copy)]
pub struct OverlayBounds {
    pub row: i64,
    pub col: i64,
    pub width: usize,
    pub height: usize,
}

struct OverlayStackEntry {
    component: Rc<RefCell<dyn Component>>,
    options: Option<OverlayOptions>,
    pre_focus: Option<Rc<RefCell<dyn Component>>>,
    hidden: bool,
    focus_order: u64,
    bounds: Option<OverlayBounds>,
}

struct RenderedOverlayLayout {
    entry_component: Rc<RefCell<dyn Component>>,
    row: i64,
    col: i64,
    width: usize,
    height: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum OverlayFocusRestorePolicy {
    Clear,
    Preserve,
}

/// Handle returned by [`TuiBase::show_overlay`] for controlling the overlay.
pub struct OverlayHandle {
    tui: Rc<RefCell<TuiBaseState>>,
    component: Rc<RefCell<dyn Component>>,
}

fn ptr_eq_opt(a: &Option<Rc<RefCell<dyn Component>>>, b: &Rc<RefCell<dyn Component>>) -> bool {
    a.as_ref().is_some_and(|a| Rc::ptr_eq(a, b))
}

impl OverlayHandle {
    /// Permanently remove the overlay (cannot be shown again).
    pub fn hide(&self) {
        TuiBaseState::hide_overlay_component(&self.tui, &self.component);
    }

    pub fn set_hidden(&self, hidden: bool) {
        TuiBaseState::set_overlay_hidden(&self.tui, &self.component, hidden);
    }

    pub fn is_hidden(&self) -> bool {
        TuiBaseState::overlay_hidden(&self.tui, &self.component)
    }

    pub fn focus(&self) {
        TuiBaseState::focus_overlay(&self.tui, &self.component);
    }

    pub fn unfocus(&self, options: Option<OverlayUnfocusOptions>) {
        TuiBaseState::unfocus_overlay(&self.tui, &self.component, options);
    }

    pub fn is_focused(&self) -> bool {
        self.tui
            .borrow()
            .focused_component
            .as_ref()
            .is_some_and(|f| Rc::ptr_eq(f, &self.component))
    }

    pub fn get_bounds(&self) -> Option<OverlayBounds> {
        TuiBaseState::overlay_bounds(&self.tui, &self.component)
    }
}

struct ViewportInsertScrollPlan {
    viewport_top: usize,
    region_bottom: usize,
    inserted_rows: Vec<String>,
    mutated_rows: Vec<(usize, String)>,
}

pub(crate) struct NormalizedLinesResult {
    pub(crate) lines: Vec<String>,
    first_raw_changed: Option<usize>,
    compare_end_exclusive: usize,
    bounded: bool,
}

/// The `Symbol.for("@earendil-works/pi-tui/viewport")` marker becomes a trait; renderers that
/// implement [`ViewportTui`] additionally expose `set_layout_root`.
pub trait ViewportTui {
    fn set_layout_root(&mut self, component: Option<Rc<RefCell<dyn Component>>>);
}

/// Shared render/overlay/focus/mouse state behind every [`TuiBase`] renderer (senpi's fields
/// live directly on the `TuiBase` class; here they are split into a `Rc<RefCell<_>>` so
/// [`OverlayHandle`] closures - which in JS just close over `this` - can reach the same state).
pub struct TuiBaseState {
    pub previous_lines: Vec<String>,
    previous_raw_lines: Vec<String>,
    normalize_memo: std::collections::HashMap<String, String>,
    pub previous_kitty_image_ids: HashSet<u32>,
    pub previous_width: i64,
    pub previous_height: i64,
    focused_component: Option<Rc<RefCell<dyn Component>>>,
    render_requested: bool,
    render_due_at_ms: Option<u64>,
    last_render_at_ms: u64,
    min_render_interval_ms: u64,
    input_render_pending: bool,
    pub cursor_row: usize,
    pub hardware_cursor_row: usize,
    show_hardware_cursor: bool,
    clear_on_shrink: bool,
    pub max_lines_rendered: usize,
    pub previous_viewport_top: usize,
    full_redraw_count: u64,
    mux_viewport_repaint_count: u64,
    reset_render_state_pending: bool,
    over_wide_crash_dump_written: bool,
    pub stopped: bool,
    last_cursor_visibility: Option<bool>,
    focus_order_counter: u64,
    overlay_stack: Vec<OverlayStackEntry>,
    rendered_overlay_layouts: Vec<RenderedOverlayLayout>,
    overlay_focus_restore: OverlayFocusRestore,
    pub placement_epoch: u64,
    mouse_leases: std::collections::HashMap<u64, String>,
    next_mouse_lease_token: u64,
    mouse_blockers: HashSet<MouseBlocker>,
    pub anchor: MouseAnchor,
    mouse_committed_line_count: usize,
    mouse_anchor_pending: bool,
    pub mouse_external_write_pending: bool,
    pending_cursor_query: Option<(crate::terminal::CursorQueryTicket, MouseAnchorQueryContext)>,
}

/// Result a [`TuiInputListener`] returns to consume input or rewrite it before it reaches the
/// focused component (senpi's `{ consume?: boolean; data?: string } | void`).
#[derive(Debug, Clone, Default)]
pub struct TuiInputListenerResult {
    pub consume: bool,
    pub data: Option<String>,
}

pub type TuiInputListener = Box<dyn FnMut(&str) -> Option<TuiInputListenerResult>>;

/// One of the three reasons mouse tracking is force-disabled regardless of lease count
/// (senpi's `mouseBlockers` string-literal set).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseBlocker {
    Suspended,
    ExternalEditor,
    ShuttingDown,
}

/// CPR-calibrated mapping from absolute screen rows to committed-frame line indices (senpi's
/// `anchor` field). `kind: Unknown` means [`TuiBase::resolve_frame_line`] cannot answer until
/// the next successful calibration; `epoch`/`rows`/`columns` detect staleness from a resize or
/// placement change.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MouseAnchorKind {
    Unknown,
    Cleared,
    Viewport,
    Cpr,
}

#[derive(Debug, Clone, Copy)]
pub struct MouseAnchor {
    pub kind: MouseAnchorKind,
    pub frame_top_screen_row: Option<i64>,
    pub epoch: u64,
    pub rows: u16,
    pub columns: u16,
}

impl Default for MouseAnchor {
    fn default() -> Self {
        Self { kind: MouseAnchorKind::Unknown, frame_top_screen_row: None, epoch: 0, rows: 0, columns: 0 }
    }
}

#[derive(Debug, Clone, Copy)]
struct MouseAnchorQueryContext {
    epoch: u64,
    rows: u16,
    columns: u16,
    hardware_cursor_row: usize,
    line_count: usize,
}

enum OverlayFocusResume {
    RestoreOverlay,
    FocusTarget(Option<Rc<RefCell<dyn Component>>>),
}

enum OverlayFocusRestore {
    Inactive,
    Eligible {
        overlay_component: Rc<RefCell<dyn Component>>,
    },
    Blocked {
        overlay_component: Rc<RefCell<dyn Component>>,
        blocked_by: Rc<RefCell<dyn Component>>,
        resume: OverlayFocusResume,
    },
}

impl Default for TuiBaseState {
    fn default() -> Self {
        Self {
            previous_lines: Vec::new(),
            previous_raw_lines: Vec::new(),
            normalize_memo: std::collections::HashMap::new(),
            previous_kitty_image_ids: HashSet::new(),
            previous_width: 0,
            previous_height: 0,
            focused_component: None,
            render_requested: false,
            render_due_at_ms: None,
            last_render_at_ms: 0,
            min_render_interval_ms: 16,
            input_render_pending: false,
            cursor_row: 0,
            hardware_cursor_row: 0,
            show_hardware_cursor: crate::process_env::var("PI_HARDWARE_CURSOR").as_deref() == Some("1"),
            clear_on_shrink: crate::process_env::var("PI_CLEAR_ON_SHRINK").as_deref() == Some("1"),
            max_lines_rendered: 0,
            previous_viewport_top: 0,
            full_redraw_count: 0,
            mux_viewport_repaint_count: 0,
            reset_render_state_pending: false,
            over_wide_crash_dump_written: false,
            stopped: false,
            last_cursor_visibility: None,
            focus_order_counter: 0,
            overlay_stack: Vec::new(),
            rendered_overlay_layouts: Vec::new(),
            overlay_focus_restore: OverlayFocusRestore::Inactive,
            placement_epoch: 0,
            mouse_leases: std::collections::HashMap::new(),
            next_mouse_lease_token: 0,
            mouse_blockers: HashSet::new(),
            anchor: MouseAnchor::default(),
            mouse_committed_line_count: 0,
            mouse_anchor_pending: false,
            mouse_external_write_pending: false,
            pending_cursor_query: None,
        }
    }
}

impl TuiBaseState {
    fn is_overlay_visible(entry: &OverlayStackEntry, columns: u16, rows: u16) -> bool {
        if entry.hidden {
            return false;
        }
        match entry.options.as_ref().and_then(|o| o.visible.as_ref()) {
            Some(visible) => visible(columns, rows),
            None => true,
        }
    }

    fn topmost_visible_overlay_index(&self, columns: u16, rows: u16) -> Option<usize> {
        let mut topmost: Option<(usize, u64)> = None;
        for (index, overlay) in self.overlay_stack.iter().enumerate() {
            if overlay.options.as_ref().is_some_and(|o| o.non_capturing) {
                continue;
            }
            if !Self::is_overlay_visible(overlay, columns, rows) {
                continue;
            }
            if topmost.is_none_or(|(_, order)| overlay.focus_order > order) {
                topmost = Some((index, overlay.focus_order));
            }
        }
        topmost.map(|(index, _)| index)
    }

    fn clear_overlay_focus_restore_for(&mut self, component: &Rc<RefCell<dyn Component>>) {
        let clear = match &self.overlay_focus_restore {
            OverlayFocusRestore::Eligible { overlay_component }
            | OverlayFocusRestore::Blocked {
                overlay_component, ..
            } => Rc::ptr_eq(overlay_component, component),
            OverlayFocusRestore::Inactive => false,
        };
        if clear {
            self.overlay_focus_restore = OverlayFocusRestore::Inactive;
        }
    }

    fn retarget_overlay_pre_focus(&mut self, removed: &Rc<RefCell<dyn Component>>, removed_pre_focus: Option<Rc<RefCell<dyn Component>>>) {
        for overlay in &mut self.overlay_stack {
            if ptr_eq_opt(&overlay.pre_focus, removed) {
                overlay.pre_focus = removed_pre_focus.clone();
            }
        }
    }

    fn set_focus_component(&mut self, component: Option<Rc<RefCell<dyn Component>>>) {
        if let Some(previous) = &self.focused_component
            && previous.borrow().focusable_get().is_some()
        {
            previous.borrow_mut().focusable_set(false);
        }
        self.focused_component = component.clone();
        if let Some(next) = &component
            && next.borrow().focusable_get().is_some()
        {
            next.borrow_mut().focusable_set(true);
        }
    }

    fn hide_overlay_component(this: &Rc<RefCell<Self>>, component: &Rc<RefCell<dyn Component>>) {
        let mut state = this.borrow_mut();
        let Some(index) = state
            .overlay_stack
            .iter()
            .position(|entry| Rc::ptr_eq(&entry.component, component))
        else {
            return;
        };
        state.clear_overlay_focus_restore_for(component);
        let entry = state.overlay_stack.remove(index);
        state.retarget_overlay_pre_focus(component, entry.pre_focus.clone());
        let was_focused = state
            .focused_component
            .as_ref()
            .is_some_and(|f| Rc::ptr_eq(f, component));
        if was_focused {
            let fallback = state
                .topmost_visible_overlay_index(0, 0)
                .map(|i| Rc::clone(&state.overlay_stack[i].component))
                .or_else(|| entry.pre_focus.clone());
            state.set_focus_component(fallback);
        }
        state.render_requested = true;
        state.render_due_at_ms = Some(0);
    }

    fn set_overlay_hidden(this: &Rc<RefCell<Self>>, component: &Rc<RefCell<dyn Component>>, hidden: bool) {
        let mut state = this.borrow_mut();
        let Some(index) = state
            .overlay_stack
            .iter()
            .position(|entry| Rc::ptr_eq(&entry.component, component))
        else {
            return;
        };
        if state.overlay_stack[index].hidden == hidden {
            return;
        }
        state.overlay_stack[index].hidden = hidden;
        if hidden {
            state.clear_overlay_focus_restore_for(component);
            let was_focused = state
                .focused_component
                .as_ref()
                .is_some_and(|f| Rc::ptr_eq(f, component));
            if was_focused {
                let pre_focus = state.overlay_stack[index].pre_focus.clone();
                let fallback = state
                    .topmost_visible_overlay_index(0, 0)
                    .map(|i| Rc::clone(&state.overlay_stack[i].component))
                    .or(pre_focus);
                state.set_focus_component(fallback);
            }
        } else {
            let non_capturing = state.overlay_stack[index]
                .options
                .as_ref()
                .is_some_and(|o| o.non_capturing);
            if !non_capturing {
                state.focus_order_counter += 1;
                state.overlay_stack[index].focus_order = state.focus_order_counter;
                state.set_focus_component(Some(Rc::clone(component)));
            }
        }
        state.render_requested = true;
        state.render_due_at_ms = Some(0);
    }

    fn overlay_hidden(this: &Rc<RefCell<Self>>, component: &Rc<RefCell<dyn Component>>) -> bool {
        this.borrow()
            .overlay_stack
            .iter()
            .find(|entry| Rc::ptr_eq(&entry.component, component))
            .is_some_and(|entry| entry.hidden)
    }

    fn focus_overlay(this: &Rc<RefCell<Self>>, component: &Rc<RefCell<dyn Component>>) {
        let mut state = this.borrow_mut();
        let Some(index) = state
            .overlay_stack
            .iter()
            .position(|entry| Rc::ptr_eq(&entry.component, component))
        else {
            return;
        };
        state.focus_order_counter += 1;
        state.overlay_stack[index].focus_order = state.focus_order_counter;
        state.set_focus_component(Some(Rc::clone(component)));
        state.render_requested = true;
        state.render_due_at_ms = Some(0);
    }

    fn unfocus_overlay(
        this: &Rc<RefCell<Self>>,
        component: &Rc<RefCell<dyn Component>>,
        options: Option<OverlayUnfocusOptions>,
    ) {
        let mut state = this.borrow_mut();
        let Some(index) = state
            .overlay_stack
            .iter()
            .position(|entry| Rc::ptr_eq(&entry.component, component))
        else {
            return;
        };
        let is_focused = state
            .focused_component
            .as_ref()
            .is_some_and(|f| Rc::ptr_eq(f, component));
        let has_pending_restore = matches!(
            &state.overlay_focus_restore,
            OverlayFocusRestore::Eligible { overlay_component }
            | OverlayFocusRestore::Blocked { overlay_component, .. }
                if Rc::ptr_eq(overlay_component, &state.overlay_stack[index].component)
        );
        if !is_focused && !has_pending_restore {
            return;
        }

        let blocked_on_this_entry_focused_on_blocker = match &state.overlay_focus_restore {
            OverlayFocusRestore::Blocked {
                overlay_component,
                blocked_by,
                ..
            } => {
                Rc::ptr_eq(overlay_component, &state.overlay_stack[index].component)
                    && ptr_eq_opt(&state.focused_component, blocked_by)
            }
            _ => false,
        };
        if blocked_on_this_entry_focused_on_blocker {
            if let OverlayFocusRestore::Blocked { blocked_by, .. } =
                std::mem::replace(&mut state.overlay_focus_restore, OverlayFocusRestore::Inactive)
                && let Some(options) = options
            {
                state.overlay_focus_restore = OverlayFocusRestore::Blocked {
                    overlay_component: Rc::clone(&state.overlay_stack[index].component),
                    blocked_by,
                    resume: OverlayFocusResume::FocusTarget(options.target),
                };
            }
            state.render_requested = true;
            state.render_due_at_ms = Some(0);
            return;
        }

        state.clear_overlay_focus_restore_for(component);
        if is_focused || options.is_some() {
            let pre_focus = state.overlay_stack[index].pre_focus.clone();
            let topmost = state.topmost_visible_overlay_index(0, 0);
            let fallback_target = match topmost {
                Some(i) if !Rc::ptr_eq(&state.overlay_stack[i].component, component) => {
                    Some(Rc::clone(&state.overlay_stack[i].component))
                }
                _ => pre_focus,
            };
            let target = if options.is_some() {
                options.and_then(|o| o.target)
            } else {
                fallback_target
            };
            state.set_focus_component(target);
        }
        state.render_requested = true;
        state.render_due_at_ms = Some(0);
    }

    fn overlay_bounds(
        this: &Rc<RefCell<Self>>,
        component: &Rc<RefCell<dyn Component>>,
    ) -> Option<OverlayBounds> {
        this.borrow()
            .overlay_stack
            .iter()
            .find(|entry| Rc::ptr_eq(&entry.component, component))
            .and_then(|entry| entry.bounds)
    }
}

/// Base renderer shared by [`crate::tui_main_screen::TuiMainScreen`] and
/// [`crate::tui_alt_screen::TuiAltScreen`]. Owns the [`Container`] children (via
/// [`TuiBase::container`]), differential render loop, overlay stack, and keyboard focus.
pub struct TuiBase {
    container: Container,
    pub state: Rc<RefCell<TuiBaseState>>,
    pub on_debug: Option<Box<dyn FnMut()>>,
    input_listeners: Vec<(u64, TuiInputListener)>,
    next_input_listener_id: u64,
    /// Overrides [`TuiBase`]'s mounted-roots concept (senpi's `getMountedRoots` protected
    /// override); `None` uses `self.container.children`. [`crate::tui_alt_screen::TuiAltScreen`]
    /// sets this to its single layout root once one is assigned - there is no virtual dispatch
    /// from `TuiBase` back onto a subclass override in this port.
    mounted_roots_override: Option<Vec<Rc<RefCell<dyn Component>>>>,
    /// senpi's `mode !== "fullscreen"` guard: main-screen renderers consume tmux focus events to
    /// refresh capabilities; the alt screen owns focus reports itself and clears this.
    pub(crate) consume_tmux_focus_events: bool,
    terminal_color_scheme_notifications_enabled: bool,
    terminal_color_scheme_listeners: Vec<(u64, TerminalColorSchemeListener)>,
    next_terminal_color_scheme_listener_id: u64,
    pending_osc11_background_replies: usize,
    pending_osc11_background_queries: std::collections::VecDeque<PendingTerminalQuery<Option<RgbColor>>>,
    pending_terminal_color_scheme_queries: Vec<PendingTerminalQuery<Option<TerminalColorScheme>>>,
    /// senpi's `#muxDetector` (`TuiConstructorOptions.muxDetector`, default
    /// [`crate::mux::is_multiplexer_session`]).
    mux_detector: Box<dyn Fn() -> bool>,
    /// senpi `logDirectory`: directory for debug/crash logs (`PI_DEBUG_REDRAW`, render errors),
    /// defaulting to `~/.senpi/agent`.
    log_directory: std::path::PathBuf,
}

impl TuiBase {
    pub fn new() -> Self {
        Self {
            container: Container::new(),
            state: Rc::new(RefCell::new(TuiBaseState::default())),
            on_debug: None,
            input_listeners: Vec::new(),
            next_input_listener_id: 0,
            mounted_roots_override: None,
            consume_tmux_focus_events: true,
            terminal_color_scheme_notifications_enabled: false,
            terminal_color_scheme_listeners: Vec::new(),
            next_terminal_color_scheme_listener_id: 0,
            pending_osc11_background_replies: 0,
            pending_osc11_background_queries: std::collections::VecDeque::new(),
            pending_terminal_color_scheme_queries: Vec::new(),
            mux_detector: Box::new(crate::mux::is_multiplexer_session),
            log_directory: default_diagnostic_log_directory(),
        }
    }

    /// senpi's `logDirectory` constructor argument: the host-resolved log directory, also
    /// published for module-scope render containment (`renderErrorLogDirectory`).
    pub fn set_log_directory(&mut self, log_directory: std::path::PathBuf) {
        *RENDER_ERROR_LOG_DIRECTORY.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(log_directory.clone());
        self.log_directory = log_directory;
    }

    /// senpi `doRender`'s `logRedraw` (`PI_DEBUG_REDRAW=1`): append each full-render reason to
    /// `<logDirectory>/pi-debug.log`.
    fn log_redraw(&self, reason: &str, new_lines_len: usize, height: usize) {
        if crate::process_env::var("PI_DEBUG_REDRAW").as_deref() != Some("1") {
            return;
        }
        let previous_lines_len = self.state.borrow().previous_lines.len();
        let log_path = self.log_directory.join("pi-debug.log");
        let msg = format!(
            "[{}] fullRender: {reason} (prev={previous_lines_len}, new={new_lines_len}, height={height})\n",
            diagnostic_timestamp(),
        );
        append_render_error_log_best_effort(&log_path, &msg);
    }

    /// senpi's `muxDetector` constructor option: overrides multiplexer detection for the
    /// scrollback-preserving render policy.
    pub fn set_mux_detector(&mut self, detector: Box<dyn Fn() -> bool>) {
        self.mux_detector = detector;
    }

    /// The renderer-state half of senpi's `start()` that precedes `beforeTerminalStart`.
    pub fn prepare_start(&mut self) {
        let mut state = self.state.borrow_mut();
        state.stopped = false;
        state.render_requested = false;
        state.input_render_pending = false;
        state.last_cursor_visibility = None;
    }

    /// The output half of senpi's `start()` that follows `terminal.start(...)`: tmux focus
    /// reporting, hidden cursor, color-scheme notifications, the cell-size query, and a render.
    pub fn after_terminal_start(&mut self, terminal: &mut dyn Terminal) {
        if in_tmux() {
            terminal.write(crate::tmux_focus::ENABLE_FOCUS_REPORTING);
        }
        self.set_cursor_visibility(false, terminal);
        if self.terminal_color_scheme_notifications_enabled {
            terminal.write(ENABLE_TERMINAL_COLOR_SCHEME_NOTIFICATIONS);
        }
        self.query_cell_size(terminal);
        self.request_render(false, 0);
    }

    /// senpi's `#setCursorVisibility`.
    fn set_cursor_visibility(&mut self, visible: bool, terminal: &mut dyn Terminal) {
        {
            let mut state = self.state.borrow_mut();
            if state.last_cursor_visibility == Some(visible) {
                return;
            }
            state.last_cursor_visibility = Some(visible);
        }
        if visible {
            terminal.show_cursor();
        } else {
            terminal.hide_cursor();
        }
    }

    /// senpi's `queryCellSize`: only terminals with an image protocol need the pixel cell size.
    fn query_cell_size(&mut self, terminal: &mut dyn Terminal) {
        if crate::terminal_image::get_capabilities().images.is_none() {
            return;
        }
        terminal.write(QUERY_CELL_SIZE);
    }

    /// The part of senpi's `stop()` that precedes `beforeTerminalStop`.
    pub fn begin_stop(&mut self, terminal: &mut dyn Terminal) {
        {
            let mut state = self.state.borrow_mut();
            state.stopped = true;
            state.render_requested = false;
            state.input_render_pending = false;
            state.render_due_at_ms = None;
        }
        if self.terminal_color_scheme_notifications_enabled {
            terminal.write(DISABLE_TERMINAL_COLOR_SCHEME_NOTIFICATIONS);
        }
    }

    /// The part of senpi's `stop()` between `beforeTerminalStop` and `terminal.stop()`: drop
    /// mouse leases, move the cursor below the last frame (unless another renderer keeps the
    /// screen), show the cursor, and disable tmux focus reporting.
    pub fn finish_stop_output(&mut self, terminal: &mut dyn Terminal, preserve_screen: bool) {
        self.reset_mouse_capture_state();
        let (line_count, hardware_cursor_row, cursor_hidden) = {
            let state = self.state.borrow();
            (state.previous_lines.len(), state.hardware_cursor_row, state.last_cursor_visibility == Some(false))
        };
        if !preserve_screen && line_count > 0 {
            if cursor_hidden {
                terminal.write(" ");
            }
            let line_diff = line_count as i64 - hardware_cursor_row as i64;
            if line_diff > 0 {
                terminal.write(&format!("\x1b[{line_diff}B"));
            } else if line_diff < 0 {
                terminal.write(&format!("\x1b[{}A", -line_diff));
            }
            terminal.write("\r\n");
        }
        self.state.borrow_mut().last_cursor_visibility = None;
        self.set_cursor_visibility(true, terminal);
        if in_tmux() {
            terminal.write(crate::tmux_focus::DISABLE_FOCUS_REPORTING);
        }
    }

    /// senpi's `resetMouseCaptureState`.
    fn reset_mouse_capture_state(&mut self) {
        let mut state = self.state.borrow_mut();
        state.mouse_leases.clear();
        state.mouse_blockers.clear();
        state.placement_epoch += 1;
    }

    /// senpi's `onTerminalColorSchemeChange`; returns the id for
    /// [`TuiBase::remove_terminal_color_scheme_listener`].
    pub fn on_terminal_color_scheme_change(&mut self, listener: TerminalColorSchemeListener) -> u64 {
        let id = self.next_terminal_color_scheme_listener_id;
        self.next_terminal_color_scheme_listener_id += 1;
        self.terminal_color_scheme_listeners.push((id, listener));
        id
    }

    pub fn remove_terminal_color_scheme_listener(&mut self, id: u64) {
        self.terminal_color_scheme_listeners.retain(|(listener_id, _)| *listener_id != id);
    }

    /// senpi's `setTerminalColorSchemeNotifications` (`CSI ? 2031 h/l`).
    pub fn set_terminal_color_scheme_notifications(&mut self, enabled: bool, terminal: &mut dyn Terminal) {
        if self.terminal_color_scheme_notifications_enabled == enabled {
            return;
        }
        self.terminal_color_scheme_notifications_enabled = enabled;
        if !self.state.borrow().stopped {
            terminal.write(if enabled {
                ENABLE_TERMINAL_COLOR_SCHEME_NOTIFICATIONS
            } else {
                DISABLE_TERMINAL_COLOR_SCHEME_NOTIFICATIONS
            });
        }
    }

    /// senpi's `queryTerminalBackgroundColor` (OSC 11). The ticket settles with the parsed color,
    /// or `None` once `now_ms + timeout_ms` passes in [`TuiBase::poll_terminal_queries`].
    pub fn query_terminal_background_color(&mut self, terminal: &mut dyn Terminal, timeout_ms: u64, now_ms: u64) -> TerminalQueryTicket<Option<RgbColor>> {
        let ticket = TerminalQueryTicket::pending();
        self.pending_osc11_background_queries.push_back(PendingTerminalQuery { ticket: ticket.clone(), deadline_ms: now_ms.saturating_add(timeout_ms) });
        self.pending_osc11_background_replies += 1;
        terminal.write(QUERY_OSC11_BACKGROUND_COLOR);
        ticket
    }

    /// senpi's `queryTerminalColorScheme` (DSR 996): settles with the next color-scheme report,
    /// or `None` on timeout.
    pub fn query_terminal_color_scheme(&mut self, terminal: &mut dyn Terminal, timeout_ms: u64, now_ms: u64) -> TerminalQueryTicket<Option<TerminalColorScheme>> {
        let ticket = TerminalQueryTicket::pending();
        self.pending_terminal_color_scheme_queries.push(PendingTerminalQuery { ticket: ticket.clone(), deadline_ms: now_ms.saturating_add(timeout_ms) });
        terminal.write(QUERY_TERMINAL_COLOR_SCHEME);
        ticket
    }

    /// Settles every terminal query whose timeout has passed (senpi's per-query `setTimeout`);
    /// call once per driver loop iteration.
    pub fn poll_terminal_queries(&mut self, now_ms: u64) {
        for query in &self.pending_osc11_background_queries {
            if query.deadline_ms <= now_ms {
                query.ticket.settle(None);
            }
        }
        self.pending_terminal_color_scheme_queries.retain(|query| {
            if query.deadline_ms <= now_ms {
                query.ticket.settle(None);
                return false;
            }
            true
        });
    }

    /// senpi's `consumeOsc11BackgroundResponse`: a reply is consumed while any OSC 11 query is
    /// outstanding, even after that query timed out.
    fn consume_osc11_background_response(&mut self, data: &str) -> bool {
        if self.pending_osc11_background_replies == 0 || !is_osc11_background_color_response(data) {
            return false;
        }
        let rgb = parse_osc11_background_color(data);
        self.pending_osc11_background_replies -= 1;
        if let Some(query) = self.pending_osc11_background_queries.pop_front() {
            query.ticket.settle(rgb);
        }
        true
    }

    /// senpi's `consumeTerminalColorSchemeReport`.
    fn consume_terminal_color_scheme_report(&mut self, data: &str) -> bool {
        let Some(scheme) = parse_terminal_color_scheme_report(data) else {
            return false;
        };
        for query in self.pending_terminal_color_scheme_queries.drain(..) {
            query.ticket.settle(Some(scheme));
        }
        for (_, listener) in &mut self.terminal_color_scheme_listeners {
            listener(scheme);
        }
        true
    }

    /// senpi's `consumeCellSizeResponse`.
    fn consume_cell_size_response(&mut self, data: &str) -> bool {
        let Some((height_px, width_px)) = parse_cell_size_response(data) else {
            return false;
        };
        if height_px == 0 || width_px == 0 {
            return true;
        }
        crate::terminal_image::set_cell_dimensions(crate::terminal_capabilities::CellDimensions { width_px, height_px });
        self.invalidate();
        self.request_render(false, 0);
        true
    }

    /// Renders the mounted roots (senpi's `TuiBase.render`); the alt screen needs it for its
    /// `afterTerminalStop` hand-back to the main screen.
    pub(crate) fn render_mounted(&mut self, width: usize) -> Vec<String> {
        match self.mounted_roots_override.clone() {
            Some(roots) => {
                let mut lines = Vec::new();
                for root in roots {
                    lines.extend(root.borrow_mut().render(width));
                }
                lines
            }
            None => self.container.render(width),
        }
    }

    /// Sets the mounted-roots override (senpi's `getMountedRoots` protected override).
    pub fn set_mounted_roots_override(&mut self, roots: Option<Vec<Rc<RefCell<dyn Component>>>>) {
        self.mounted_roots_override = roots;
    }

    fn mounted_roots(&self) -> Vec<Rc<RefCell<dyn Component>>> {
        self.mounted_roots_override.clone().unwrap_or_else(|| self.container.children.clone())
    }

    /// Registers a listener invoked with every input chunk before it reaches the focused
    /// component; returns the id to pass to [`TuiBase::remove_input_listener`].
    pub fn add_input_listener(&mut self, listener: TuiInputListener) -> u64 {
        let id = self.next_input_listener_id;
        self.next_input_listener_id += 1;
        self.input_listeners.push((id, listener));
        id
    }

    pub fn remove_input_listener(&mut self, id: u64) {
        self.input_listeners.retain(|(listener_id, _)| *listener_id != id);
    }

    pub fn children(&self) -> &[Rc<RefCell<dyn Component>>] {
        &self.container.children
    }

    pub fn add_child(&mut self, component: Rc<RefCell<dyn Component>>) {
        self.container.add_child(component);
    }

    pub fn remove_child(&mut self, component: &Rc<RefCell<dyn Component>>) {
        self.container.remove_child(component);
    }

    pub fn detach_child(&mut self, component: &Rc<RefCell<dyn Component>>) {
        self.container.detach_child(component);
    }

    pub fn clear(&mut self) {
        self.container.clear();
    }

    pub fn get_focused_component(&self) -> Option<Rc<RefCell<dyn Component>>> {
        self.state.borrow().focused_component.clone()
    }

    pub fn set_focus(&mut self, component: Option<Rc<RefCell<dyn Component>>>) {
        self.set_focus_internal(component, OverlayFocusRestorePolicy::Clear);
    }

    fn set_focus_internal(
        &mut self,
        component: Option<Rc<RefCell<dyn Component>>>,
        overlay_focus_restore: OverlayFocusRestorePolicy,
    ) {
        let mut state = self.state.borrow_mut();
        let previous_focus = state.focused_component.clone();
        let mut next_focus = component;

        let previous_focused_overlay_index = previous_focus.as_ref().and_then(|previous| {
            state.overlay_stack.iter().position(|entry| {
                Rc::ptr_eq(&entry.component, previous)
                    && TuiBaseState::is_overlay_visible(entry, 0, 0)
            })
        });
        let next_focus_is_overlay = next_focus.as_ref().is_some_and(|next| {
            state
                .overlay_stack
                .iter()
                .any(|entry| Rc::ptr_eq(&entry.component, next))
        });

        if let Some(next) = next_focus.clone() {
            if !next_focus_is_overlay {
                let blocked_match = matches!(
                    &state.overlay_focus_restore,
                    OverlayFocusRestore::Blocked { blocked_by, .. }
                        if ptr_eq_opt(&previous_focus, blocked_by)
                );
                if blocked_match {
                    if let OverlayFocusRestore::Blocked {
                        overlay_component,
                        blocked_by,
                        resume,
                    } = std::mem::replace(&mut state.overlay_focus_restore, OverlayFocusRestore::Inactive)
                    {
                        let mounted = self.is_component_mounted_locked(&state, &blocked_by);
                        let resume_is_target = matches!(resume, OverlayFocusResume::FocusTarget(_));
                        if resume_is_target || !mounted {
                            next_focus = match resume {
                                OverlayFocusResume::RestoreOverlay => Some(overlay_component),
                                OverlayFocusResume::FocusTarget(target) => target,
                            };
                        } else {
                            state.overlay_focus_restore = OverlayFocusRestore::Blocked {
                                overlay_component,
                                blocked_by: next,
                                resume,
                            };
                        }
                    }
                } else if let Some(overlay_index) = previous_focused_overlay_index {
                    let is_current_restore_target = matches!(
                        &state.overlay_focus_restore,
                        OverlayFocusRestore::Eligible { overlay_component }
                        | OverlayFocusRestore::Blocked { overlay_component, .. }
                            if Rc::ptr_eq(overlay_component, &state.overlay_stack[overlay_index].component)
                    ) && !matches!(&state.overlay_focus_restore, OverlayFocusRestore::Inactive);
                    let is_ancestor = self.is_overlay_focus_ancestor_locked(
                        &state,
                        overlay_index,
                        &next,
                    );
                    if is_current_restore_target && !is_ancestor {
                        state.overlay_focus_restore = OverlayFocusRestore::Blocked {
                            overlay_component: Rc::clone(&state.overlay_stack[overlay_index].component),
                            blocked_by: next,
                            resume: OverlayFocusResume::RestoreOverlay,
                        };
                    }
                }
            }
        } else {
            let blocked_match = matches!(
                &state.overlay_focus_restore,
                OverlayFocusRestore::Blocked { blocked_by, .. }
                    if ptr_eq_opt(&previous_focus, blocked_by)
            );
            if blocked_match {
                if let OverlayFocusRestore::Blocked {
                    overlay_component,
                    resume,
                    ..
                } = std::mem::replace(&mut state.overlay_focus_restore, OverlayFocusRestore::Inactive)
                {
                    next_focus = match resume {
                        OverlayFocusResume::RestoreOverlay => Some(overlay_component),
                        OverlayFocusResume::FocusTarget(target) => target,
                    };
                }
            } else if overlay_focus_restore == OverlayFocusRestorePolicy::Clear {
                state.overlay_focus_restore = OverlayFocusRestore::Inactive;
            }
        }

        state.set_focus_component(next_focus.clone());

        if let Some(next) = &next_focus
            && let Some(index) = state
                .overlay_stack
                .iter()
                .position(|entry| Rc::ptr_eq(&entry.component, next) && TuiBaseState::is_overlay_visible(entry, 0, 0))
        {
            state.overlay_focus_restore = OverlayFocusRestore::Eligible {
                overlay_component: Rc::clone(&state.overlay_stack[index].component),
            };
        }
    }

    fn is_component_mounted_locked(&self, _state: &TuiBaseState, component: &Rc<RefCell<dyn Component>>) -> bool {
        self.mounted_roots()
            .iter()
            .any(|child| Self::contains_component(child, component))
    }

    fn contains_component(root: &Rc<RefCell<dyn Component>>, target: &Rc<RefCell<dyn Component>>) -> bool {
        if Rc::ptr_eq(root, target) {
            return true;
        }
        let root_ref = root.borrow();
        match root_ref.as_container() {
            Some(container) => container
                .children
                .iter()
                .any(|child| Self::contains_component(child, target)),
            None => false,
        }
    }

    /// Dispatch to the base container's children directly (senpi's `dispatchMouseEvent(this,
    /// event)` - `TuiBase extends Container` so `this` is itself the dispatched-to
    /// `Component`; here the base [`Container`] is unwrapped, so this returns the matched
    /// child's own [`TuiMouseDispatchResult`] via [`Container::dispatch_mouse_with_target`]
    /// instead of a handle to the base itself).
    pub fn dispatch_self_mouse_event(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseDispatchResult> {
        self.container.dispatch_mouse_with_target(event)
    }

    /// Mounted roots plus every currently-rendered overlay's component (senpi's
    /// `getMouseLayoutRoots`): the set mouse dispatch and focus-target resolution walk.
    pub fn get_mouse_layout_roots(&self) -> Vec<Rc<RefCell<dyn Component>>> {
        let mut roots: Vec<Rc<RefCell<dyn Component>>> = self.mounted_roots();
        for layout in &self.state.borrow().rendered_overlay_layouts {
            roots.push(Rc::clone(&layout.entry_component));
        }
        roots
    }

    /// Keyboard focus owner for a clicked component: the overlay that owns it, else the
    /// component itself when it can receive keys, else the nearest surrounding component that
    /// can. `None` when nothing in that chain can - a mouse-only control that owned focus would
    /// swallow every later keystroke.
    pub fn resolve_mouse_focus_target(&self, component: &Rc<RefCell<dyn Component>>) -> Option<Rc<RefCell<dyn Component>>> {
        let state = self.state.borrow();
        for overlay in state.overlay_stack.iter().rev() {
            if TuiBaseState::is_overlay_visible(overlay, 0, 0) && Self::contains_component(&overlay.component, component) {
                return Some(Rc::clone(&overlay.component));
            }
        }
        drop(state);
        if can_receive_keys(component) {
            return Some(Rc::clone(component));
        }
        self.find_key_focus_owner(component)
    }

    /// Deepest mounted ancestor of `target` that can receive keys, excluding `target` itself.
    fn find_key_focus_owner(&self, target: &Rc<RefCell<dyn Component>>) -> Option<Rc<RefCell<dyn Component>>> {
        fn walk(node: &Rc<RefCell<dyn Component>>, target: &Rc<RefCell<dyn Component>>, path: &mut Vec<Rc<RefCell<dyn Component>>>) -> bool {
            path.push(Rc::clone(node));
            if Rc::ptr_eq(node, target) {
                return true;
            }
            let children = node.borrow().as_container().map(|c| c.children.clone());
            if let Some(children) = children {
                for child in &children {
                    if walk(child, target, path) {
                        return true;
                    }
                }
            }
            path.pop();
            false
        }

        for root in self.get_mouse_layout_roots() {
            let mut path = Vec::new();
            if !walk(&root, target, &mut path) {
                continue;
            }
            for candidate in path.iter().rev().skip(1) {
                if can_receive_keys(candidate) {
                    return Some(Rc::clone(candidate));
                }
            }
            return None;
        }
        None
    }

    /// Host-owned intent to enable mouse tracking; a replacement renderer starts with no
    /// leases. Returns `(lease_token, should_apply_tracking)`: the caller applies tracking
    /// itself (senpi dispatches through the `applyMouseTracking` virtual override, which this
    /// port cannot express as a plain callback without borrowing `self` twice - see
    /// `TuiMainScreen::acquire_mouse_capture`) using [`TuiBase::mouse_capture_enabled`], then
    /// calls [`TuiBase::calibrate_mouse_anchor`].
    pub fn acquire_mouse_capture(&mut self, reason: impl Into<String>) -> (u64, bool) {
        let mut state = self.state.borrow_mut();
        let token = state.next_mouse_lease_token;
        state.next_mouse_lease_token += 1;
        state.mouse_leases.insert(token, reason.into());
        let first = state.mouse_leases.len() == 1;
        (token, first)
    }

    /// Returns whether the caller should now disable tracking (the lease pool became empty).
    pub fn release_mouse_capture(&mut self, token: u64) -> bool {
        let mut state = self.state.borrow_mut();
        let removed = state.mouse_leases.remove(&token).is_some();
        removed && state.mouse_leases.is_empty()
    }

    pub fn mouse_capture_enabled(&self) -> bool {
        let state = self.state.borrow();
        !state.mouse_leases.is_empty() && state.mouse_blockers.is_empty()
    }

    /// Returns whether tracking state actually changed (the caller re-applies tracking using
    /// [`TuiBase::mouse_capture_enabled`] when it did).
    pub fn set_mouse_blocker(&mut self, name: MouseBlocker, on: bool) -> bool {
        let mut state = self.state.borrow_mut();
        let had = state.mouse_blockers.contains(&name);
        if had == on {
            return false;
        }
        if on {
            state.mouse_blockers.insert(name);
        } else {
            state.mouse_blockers.remove(&name);
        }
        state.placement_epoch += 1;
        true
    }

    /// Called after a full (non-differential) render publishes new geometry (senpi's
    /// `noteFullRender`).
    pub fn note_full_render(&mut self, clear: bool, terminal: &mut dyn Terminal) {
        {
            let mut state = self.state.borrow_mut();
            state.placement_epoch += 1;
            let epoch = state.placement_epoch;
            state.anchor = MouseAnchor {
                kind: if clear { MouseAnchorKind::Cleared } else { MouseAnchorKind::Unknown },
                frame_top_screen_row: if clear { Some(0) } else { None },
                epoch,
                rows: terminal.rows(),
                columns: terminal.columns(),
            };
            state.mouse_committed_line_count = state.previous_lines.len();
        }
        self.note_committed_mouse_frame(terminal);
        self.calibrate_mouse_anchor(terminal);
    }

    /// Called only after the renderer has published its geometry and bytes (senpi's
    /// `noteCommittedMouseFrame`).
    pub fn note_committed_mouse_frame(&mut self, terminal: &mut dyn Terminal) {
        let mut state = self.state.borrow_mut();
        if state.mouse_committed_line_count != state.previous_lines.len() {
            state.placement_epoch += 1;
        }
        state.mouse_committed_line_count = state.previous_lines.len();
        if state.previous_lines.iter().any(|line| crate::image_stub::is_image_line(line)) {
            state.placement_epoch += 1;
            state.anchor.kind = MouseAnchorKind::Unknown;
            return;
        }
        let rows = terminal.rows();
        let columns = terminal.columns();
        if state.previous_lines.len() >= rows as usize {
            let epoch = state.placement_epoch;
            state.anchor = MouseAnchor { kind: MouseAnchorKind::Viewport, frame_top_screen_row: None, epoch, rows, columns };
        } else if state.anchor.epoch != state.placement_epoch || state.anchor.rows != rows || state.anchor.columns != columns {
            let epoch = state.placement_epoch;
            state.anchor = MouseAnchor { kind: MouseAnchorKind::Unknown, frame_top_screen_row: None, epoch, rows, columns };
        }
    }

    /// Never block a frame on terminal protocol negotiation or a missing reply (senpi's
    /// `calibrateMouseAnchor`). Issues a [`crate::terminal::CursorQueryTicket`] and polls it
    /// from [`TuiBase::poll_mouse_anchor_query`] on the next tick instead of awaiting a promise.
    pub fn calibrate_mouse_anchor(&mut self, terminal: &mut dyn Terminal) {
        let ticket_context = {
            let state = self.state.borrow();
            if !self.mouse_capture_enabled()
                || state.stopped
                || state.mouse_anchor_pending
                || state.mouse_external_write_pending
                || state.previous_lines.is_empty()
                || state.previous_lines.iter().any(|line| crate::image_stub::is_image_line(line))
            {
                return;
            }
            if state.anchor.kind != MouseAnchorKind::Unknown
                && state.anchor.epoch == state.placement_epoch
                && state.anchor.rows == terminal.rows()
                && state.anchor.columns == terminal.columns()
            {
                return;
            }
            MouseAnchorQueryContext {
                epoch: state.placement_epoch,
                rows: terminal.rows(),
                columns: terminal.columns(),
                hardware_cursor_row: state.hardware_cursor_row,
                line_count: state.previous_lines.len(),
            }
        };
        let Some(ticket) = terminal.query_cursor_position() else {
            return;
        };
        let mut state = self.state.borrow_mut();
        state.mouse_anchor_pending = true;
        state.pending_cursor_query = Some((ticket, ticket_context));
    }

    /// Polls the in-flight [`crate::terminal::CursorQueryTicket`] from
    /// [`TuiBase::calibrate_mouse_anchor`]; call once per driver loop iteration.
    pub fn poll_mouse_anchor_query(&mut self) {
        let settled = {
            let state = self.state.borrow();
            state
                .pending_cursor_query
                .as_ref()
                .and_then(|(ticket, ctx)| ticket.result().map(|position| (position, *ctx)))
        };
        let Some((position, ctx)) = settled else {
            return;
        };
        let mut state = self.state.borrow_mut();
        state.mouse_anchor_pending = false;
        state.pending_cursor_query = None;
        let Some(position) = position else {
            return;
        };
        let stale = state.stopped
            || state.mouse_leases.is_empty()
            || !state.mouse_blockers.is_empty()
            || ctx.epoch != state.placement_epoch
            || ctx.hardware_cursor_row != state.hardware_cursor_row
            || ctx.line_count != state.previous_lines.len();
        if stale {
            return;
        }
        let top = position.row as i64 - 1 - ctx.hardware_cursor_row as i64;
        let line_count = ctx.line_count as i64;
        if top < 0
            || top + line_count > ctx.rows as i64
            || position.column < 1
            || position.column as i64 > ctx.columns as i64 + 1
            || position.page.is_some_and(|page| page != 1)
        {
            return;
        }
        state.anchor = MouseAnchor {
            kind: MouseAnchorKind::Cpr,
            frame_top_screen_row: Some(top),
            epoch: ctx.epoch,
            rows: ctx.rows,
            columns: ctx.columns,
        };
    }

    /// Input rows are one-based; the returned committed frame line is zero-based (senpi's
    /// `resolveFrameLine`).
    pub fn resolve_frame_line(&self, screen_row: i64, terminal: &dyn Terminal) -> Option<usize> {
        let state = self.state.borrow();
        let anchor = state.anchor;
        if anchor.kind == MouseAnchorKind::Unknown
            || anchor.epoch != state.placement_epoch
            || anchor.rows != terminal.rows()
            || anchor.columns != terminal.columns()
            || screen_row < 1
            || screen_row > anchor.rows as i64
        {
            return None;
        }
        let line = if anchor.kind == MouseAnchorKind::Viewport {
            state.previous_viewport_top as i64 + screen_row - 1
        } else {
            screen_row - 1 - anchor.frame_top_screen_row.unwrap_or(0)
        };
        if line >= 0 && (line as usize) < state.previous_lines.len() {
            Some(line as usize)
        } else {
            None
        }
    }

    fn is_overlay_focus_ancestor_locked(
        &self,
        state: &TuiBaseState,
        overlay_index: usize,
        component: &Rc<RefCell<dyn Component>>,
    ) -> bool {
        let mut visited: Vec<*const RefCell<dyn Component>> = Vec::new();
        let mut current = state.overlay_stack[overlay_index].pre_focus.clone();
        while let Some(node) = current {
            let ptr = Rc::as_ptr(&node);
            if visited.contains(&ptr) {
                break;
            }
            visited.push(ptr);
            if Rc::ptr_eq(&node, component) {
                return true;
            }
            current = state
                .overlay_stack
                .iter()
                .find(|overlay| Rc::ptr_eq(&overlay.component, &node))
                .and_then(|overlay| overlay.pre_focus.clone());
        }
        false
    }

    /// Show an overlay component with configurable positioning and sizing. Returns a handle to
    /// control the overlay's visibility.
    pub fn show_overlay(
        &mut self,
        component: Rc<RefCell<dyn Component>>,
        options: Option<OverlayOptions>,
    ) -> OverlayHandle {
        let non_capturing = options.as_ref().is_some_and(|o| o.non_capturing);
        {
            let mut state = self.state.borrow_mut();
            state.focus_order_counter += 1;
            let entry = OverlayStackEntry {
                component: Rc::clone(&component),
                options,
                pre_focus: state.focused_component.clone(),
                hidden: false,
                focus_order: state.focus_order_counter,
                bounds: None,
            };
            let visible = TuiBaseState::is_overlay_visible(&entry, 0, 0);
            state.overlay_stack.push(entry);
            drop(state);
            if !non_capturing && visible {
                self.set_focus(Some(Rc::clone(&component)));
            }
        }
        let mut state = self.state.borrow_mut();
        state.render_requested = true;
        state.render_due_at_ms = Some(0);
        drop(state);
        OverlayHandle {
            tui: Rc::clone(&self.state),
            component,
        }
    }

    /// Hide the topmost overlay and restore previous focus.
    pub fn hide_overlay(&mut self) {
        let component = {
            let state = self.state.borrow();
            state.overlay_stack.last().map(|e| Rc::clone(&e.component))
        };
        if let Some(component) = component {
            TuiBaseState::hide_overlay_component(&self.state, &component);
        }
    }

    /// `true` when there are any visible overlays.
    pub fn has_overlay(&self) -> bool {
        let state = self.state.borrow();
        state
            .overlay_stack
            .iter()
            .any(|entry| TuiBaseState::is_overlay_visible(entry, 0, 0))
    }

    /// `true` when keyboard focus sits on a visible overlay (senpi's protected `isOverlayFocused`).
    pub fn is_overlay_focused(&self) -> bool {
        let state = self.state.borrow();
        state.focused_component.as_ref().is_some_and(|focused| {
            state
                .overlay_stack
                .iter()
                .any(|entry| Rc::ptr_eq(&entry.component, focused) && TuiBaseState::is_overlay_visible(entry, 0, 0))
        })
    }

    pub fn invalidate(&mut self) {
        for root in self.mounted_roots() {
            root.borrow_mut().invalidate();
        }
        let state = self.state.borrow();
        for overlay in &state.overlay_stack {
            overlay.component.borrow_mut().invalidate();
        }
    }

    /// Request a render. `force` mirrors senpi's forced `renderNow`/`requestRender(true)` path
    /// (immediate, next poll tick); otherwise the request is throttled to
    /// `min_render_interval_ms` after the last render, mirroring the 16ms `setTimeout` cap.
    pub fn request_render(&mut self, force: bool, now_ms: u64) {
        self.request_render_from(force, "unknown", now_ms);
    }

    /// senpi's `requestRender(force, source)`. An `"input"`/`"editor.input"` source is
    /// expedited: the frame is due immediately instead of after the throttle interval, so
    /// keystrokes echo without waiting for the next throttled frame.
    pub fn request_render_from(&mut self, force: bool, source: &str, now_ms: u64) {
        if force {
            self.reset_forced_render_state();
        }
        let mut state = self.state.borrow_mut();
        if force {
            state.input_render_pending = false;
            state.render_requested = true;
            state.render_due_at_ms = Some(now_ms);
            return;
        }
        if source == "input" || source == "editor.input" {
            if !state.input_render_pending {
                state.input_render_pending = true;
                state.render_requested = true;
                state.render_due_at_ms = Some(now_ms);
            }
            return;
        }
        if state.render_requested {
            return;
        }
        state.render_requested = true;
        let elapsed = now_ms.saturating_sub(state.last_render_at_ms);
        let delay = state.min_render_interval_ms.saturating_sub(elapsed);
        state.render_due_at_ms = Some(now_ms + delay);
    }

    /// senpi's `setMaxRenderFps`: cap the render scheduler at `fps` frames per second, clamped
    /// to 30-120fps (120fps yields an 8ms minimum interval). Never calling this keeps the
    /// historic 16ms (~60fps) throttle.
    pub fn set_max_render_fps(&mut self, fps: f64) {
        let clamped = fps.round().clamp(30.0, 120.0);
        self.state.borrow_mut().min_render_interval_ms = (1000.0 / clamped).floor() as u64;
    }

    /// `true` once [`TuiBase::request_render`] has a due frame at or before `now_ms`; the
    /// caller's poll loop then calls [`TuiBase::render_now`].
    pub fn render_due(&self, now_ms: u64) -> bool {
        let state = self.state.borrow();
        state.render_requested && state.render_due_at_ms.is_some_and(|due| due <= now_ms)
    }

    /// senpi's `scheduleRender`/`commitExpeditedRender` timer callback: when a frame is due at
    /// `now_ms`, clear the request and record `now_ms` as the last render so the next
    /// non-input request is throttled to `min_render_interval_ms`. Returns whether the caller
    /// should render now.
    pub fn take_due_render(&mut self, now_ms: u64) -> bool {
        if !self.render_due(now_ms) {
            return false;
        }
        let mut state = self.state.borrow_mut();
        state.render_requested = false;
        state.input_render_pending = false;
        state.render_due_at_ms = None;
        state.last_render_at_ms = now_ms;
        true
    }

    /// The current throttle interval (16ms unless [`TuiBase::set_max_render_fps`] changed it).
    pub fn min_render_interval_ms(&self) -> u64 {
        self.state.borrow().min_render_interval_ms
    }

    pub fn get_show_hardware_cursor(&self) -> bool {
        self.state.borrow().show_hardware_cursor
    }

    pub fn set_show_hardware_cursor(&mut self, enabled: bool) {
        let changed = {
            let mut state = self.state.borrow_mut();
            if state.show_hardware_cursor == enabled {
                false
            } else {
                state.show_hardware_cursor = enabled;
                true
            }
        };
        if changed {
            self.request_render(false, 0);
        }
    }

    pub fn get_clear_on_shrink(&self) -> bool {
        self.state.borrow().clear_on_shrink
    }

    pub fn set_clear_on_shrink(&mut self, enabled: bool) {
        self.state.borrow_mut().clear_on_shrink = enabled;
    }

    pub fn full_redraws(&self) -> u64 {
        self.state.borrow().full_redraw_count
    }

    pub fn mux_viewport_repaints(&self) -> u64 {
        self.state.borrow().mux_viewport_repaint_count
    }

    /// Takes the pending renderer `resetRenderState()` request left by
    /// [`TuiBase::reset_forced_render_state`].
    pub fn take_reset_render_state_pending(&mut self) -> bool {
        std::mem::take(&mut self.state.borrow_mut().reset_render_state_pending)
    }

    /// Reset renderer-side state for the next `start()`; mirrors senpi's `stop()` clearing the
    /// diff cache so a stopped-then-restarted (or handed-off) renderer starts clean.
    pub fn reset_for_stop(&mut self) {
        let mut state = self.state.borrow_mut();
        state.previous_lines.clear();
        state.previous_raw_lines.clear();
        state.previous_kitty_image_ids.clear();
        state.previous_width = 0;
        state.previous_height = 0;
        state.cursor_row = 0;
        state.hardware_cursor_row = 0;
        state.max_lines_rendered = 0;
        state.previous_viewport_top = 0;
        state.last_cursor_visibility = None;
        state.render_requested = false;
        state.input_render_pending = false;
    }

    /// Force the next render to repaint from a clean slate (senpi's `resetForcedRenderState`).
    /// senpi's first step, the renderer's `resetRenderState()` override, is deferred through
    /// `reset_render_state_pending`: there is no virtual dispatch from `TuiBase` back onto
    /// [`crate::tui_alt_screen::TuiAltScreen`], which consumes the flag before its next frame.
    pub fn reset_forced_render_state(&mut self) {
        let mut state = self.state.borrow_mut();
        state.reset_render_state_pending = true;
        state.previous_lines.clear();
        state.previous_raw_lines.clear();
        state.previous_width = -1;
        state.previous_height = -1;
        state.cursor_row = 0;
        state.hardware_cursor_row = 0;
        state.max_lines_rendered = 0;
        state.previous_viewport_top = 0;
    }

    /// The terminal-protocol head of senpi's `handleTerminalInput`: tmux focus events (main
    /// screen only) refresh capabilities and repaint, and OSC 11 background replies,
    /// color-scheme reports and cell-size replies are consumed. Returns the input left for the
    /// renderer and focused component, or `None` when the chunk was fully consumed.
    pub fn consume_terminal_protocol_input(&mut self, data: &str) -> Option<String> {
        let mut data = data.to_string();
        if self.consume_tmux_focus_events {
            let focus = crate::tmux_focus::consume_tmux_focus_event(&data);
            if focus.event.is_some() {
                crate::terminal_image::reset_capabilities_cache();
                self.invalidate();
                self.request_render(true, 0);
                if focus.data.is_empty() {
                    return None;
                }
                data = focus.data;
            }
        }
        if self.consume_osc11_background_response(&data)
            || self.consume_terminal_color_scheme_report(&data)
            || self.consume_cell_size_response(&data)
        {
            return None;
        }
        Some(data)
    }

    /// The focus half of senpi's `handleTerminalInput` that runs before input is delivered:
    /// redirect focus away from a hidden overlay, and restore focus to an eligible overlay.
    /// Hosts that deliver input to their own root (instead of through
    /// [`TuiBase::handle_terminal_input`]) call this first, then check the focused component.
    pub fn apply_input_focus_rules(&mut self) {
        {
            let state = self.state.borrow_mut();
            let focused_overlay_visible = state
                .focused_component
                .as_ref()
                .and_then(|focused| {
                    state
                        .overlay_stack
                        .iter()
                        .find(|entry| Rc::ptr_eq(&entry.component, focused))
                })
                .map(|entry| TuiBaseState::is_overlay_visible(entry, 0, 0));
            if focused_overlay_visible == Some(false) {
                let fallback = state
                    .topmost_visible_overlay_index(0, 0)
                    .map(|i| Rc::clone(&state.overlay_stack[i].component));
                drop(state);
                if let Some(fallback) = fallback {
                    self.set_focus(Some(fallback));
                } else {
                    let pre_focus = self
                        .state
                        .borrow()
                        .focused_component
                        .as_ref()
                        .and_then(|focused| {
                            self.state
                                .borrow()
                                .overlay_stack
                                .iter()
                                .find(|entry| Rc::ptr_eq(&entry.component, focused))
                                .and_then(|entry| entry.pre_focus.clone())
                        });
                    self.set_focus_internal(pre_focus, OverlayFocusRestorePolicy::Preserve);
                }
            }
        }

        {
            let focus_is_overlay = {
                let state = self.state.borrow();
                state.focused_component.as_ref().is_some_and(|focused| {
                    state
                        .overlay_stack
                        .iter()
                        .any(|entry| Rc::ptr_eq(&entry.component, focused))
                })
            };
            if !focus_is_overlay {
                let action = {
                    let state = self.state.borrow();
                    match &state.overlay_focus_restore {
                        OverlayFocusRestore::Eligible { overlay_component } => {
                            Some(Some(Rc::clone(overlay_component)))
                        }
                        OverlayFocusRestore::Blocked {
                            blocked_by, resume, ..
                        } if !ptr_eq_opt(&state.focused_component, blocked_by) => match resume {
                            OverlayFocusResume::RestoreOverlay => {
                                if let OverlayFocusRestore::Blocked { overlay_component, .. } =
                                    &state.overlay_focus_restore
                                {
                                    Some(Some(Rc::clone(overlay_component)))
                                } else {
                                    None
                                }
                            }
                            OverlayFocusResume::FocusTarget(target) => Some(target.clone()),
                        },
                        _ => None,
                    }
                };
                if let Some(target) = action {
                    if matches!(
                        self.state.borrow().overlay_focus_restore,
                        OverlayFocusRestore::Blocked { .. }
                    ) {
                        self.state.borrow_mut().overlay_focus_restore = OverlayFocusRestore::Inactive;
                    }
                    self.set_focus(target);
                }
            }
        }
    }

    /// Route input to the focused component, applying the same global handling order as
    /// senpi's `handleTerminalInput` (terminal-protocol replies, debug key, overlay-visibility
    /// redirect, focus-restore, then the focused component).
    pub fn handle_terminal_input(&mut self, data: &str, is_key_release: bool) {
        let Some(data) = self.consume_terminal_protocol_input(data) else {
            return;
        };
        let data = data.as_str();
        if crate::keys::matches_key(data, "shift+ctrl+d")
            && let Some(on_debug) = &mut self.on_debug
        {
            on_debug();
            return;
        }

        self.apply_input_focus_rules();

        let mut owned_data = data.to_string();
        if !self.input_listeners.is_empty() {
            let mut current = owned_data.clone();
            let mut consumed = false;
            for (_, listener) in &mut self.input_listeners {
                if let Some(result) = listener(&current) {
                    if result.consume {
                        consumed = true;
                        break;
                    }
                    if let Some(new_data) = result.data {
                        current = new_data;
                    }
                }
            }
            if consumed || current.is_empty() {
                return;
            }
            owned_data = current;
        }
        let data = owned_data.as_str();

        let focused = self.state.borrow().focused_component.clone();
        if let Some(focused) = focused {
            let wants_release = focused.borrow().wants_key_release();
            if is_key_release && !wants_release {
                return;
            }
            focused.borrow_mut().handle_input(data);
            self.request_render_from(false, "input", 0);
        }
    }

    fn resolve_anchor_row(anchor: OverlayAnchor, height: i64, avail_height: i64, margin_top: i64) -> i64 {
        use OverlayAnchor::*;
        match anchor {
            TopLeft | TopCenter | TopRight => margin_top,
            BottomLeft | BottomCenter | BottomRight => margin_top + avail_height - height,
            LeftCenter | Center | RightCenter => margin_top + (avail_height - height) / 2,
        }
    }

    fn resolve_anchor_col(anchor: OverlayAnchor, width: i64, avail_width: i64, margin_left: i64) -> i64 {
        use OverlayAnchor::*;
        match anchor {
            TopLeft | LeftCenter | BottomLeft => margin_left,
            TopRight | RightCenter | BottomRight => margin_left + avail_width - width,
            TopCenter | Center | BottomCenter => margin_left + (avail_width - width) / 2,
        }
    }

    fn resolve_overlay_layout(
        options: Option<&OverlayOptions>,
        overlay_height: i64,
        term_width: i64,
        term_height: i64,
    ) -> (i64, i64, i64, Option<i64>) {
        let margin = options.and_then(|o| o.margin).unwrap_or_default();
        let margin_top = margin.top.unwrap_or(0).max(0);
        let margin_right = margin.right.unwrap_or(0).max(0);
        let margin_bottom = margin.bottom.unwrap_or(0).max(0);
        let margin_left = margin.left.unwrap_or(0).max(0);

        let avail_width = (term_width - margin_left - margin_right).max(1);
        let avail_height = (term_height - margin_top - margin_bottom).max(1);

        let mut width = options
            .and_then(|o| parse_size_value(o.width, term_width))
            .unwrap_or_else(|| 80i64.min(avail_width));
        if let Some(min_width) = options.and_then(|o| o.min_width) {
            width = width.max(min_width);
        }
        width = width.max(1).min(avail_width);

        let mut max_height = options.and_then(|o| parse_size_value(o.max_height, term_height));
        if let Some(mh) = max_height {
            max_height = Some(mh.max(1).min(avail_height));
        }

        let effective_height = max_height.map_or(overlay_height, |mh| overlay_height.min(mh));

        let anchor = options.and_then(|o| o.anchor).unwrap_or(OverlayAnchor::Center);
        let mut row = match options.and_then(|o| o.row) {
            Some(value) => match value {
                SizeValue::Percent(percent) => {
                    let max_row = (avail_height - effective_height).max(0);
                    margin_top + ((max_row as f64) * (percent / 100.0)).floor() as i64
                }
                SizeValue::Absolute(v) => v,
            },
            None => Self::resolve_anchor_row(anchor, effective_height, avail_height, margin_top),
        };
        let mut col = match options.and_then(|o| o.col) {
            Some(value) => match value {
                SizeValue::Percent(percent) => {
                    let max_col = (avail_width - width).max(0);
                    margin_left + ((max_col as f64) * (percent / 100.0)).floor() as i64
                }
                SizeValue::Absolute(v) => v,
            },
            None => Self::resolve_anchor_col(anchor, width, avail_width, margin_left),
        };

        if let Some(offset_y) = options.and_then(|o| o.offset_y) {
            row += offset_y;
        }
        if let Some(offset_x) = options.and_then(|o| o.offset_x) {
            col += offset_x;
        }

        row = row.max(margin_top).min(term_height - margin_bottom - effective_height);
        col = col.max(margin_left).min(term_width - margin_right - width);

        (width, row, col, max_height)
    }

    /// Composite all overlays into content lines (sorted by focus order, higher = on top).
    pub(crate) fn composite_overlays(&mut self, lines: Vec<String>, term_width: u16, term_height: u16) -> Vec<String> {
        let has_overlays = !self.state.borrow().overlay_stack.is_empty();
        if !has_overlays {
            self.state.borrow_mut().rendered_overlay_layouts.clear();
            return lines;
        }
        let mut result = lines;

        let mut visible_entries: Vec<usize> = {
            let mut state = self.state.borrow_mut();
            for entry in &mut state.overlay_stack {
                entry.bounds = None;
            }
            let mut indices: Vec<usize> = (0..state.overlay_stack.len())
                .filter(|&i| TuiBaseState::is_overlay_visible(&state.overlay_stack[i], term_width, term_height))
                .collect();
            indices.sort_by_key(|&i| state.overlay_stack[i].focus_order);
            indices
        };

        struct Rendered {
            component: Rc<RefCell<dyn Component>>,
            overlay_lines: Vec<String>,
            row: i64,
            col: i64,
            width: usize,
        }
        let mut rendered: Vec<Rendered> = Vec::new();
        let mut min_lines_needed = result.len() as i64;

        for &index in &visible_entries {
            let (component, options) = {
                let state = self.state.borrow();
                (
                    Rc::clone(&state.overlay_stack[index].component),
                    state.overlay_stack[index].options.clone(),
                )
            };
            let (width, _, _, max_height) =
                Self::resolve_overlay_layout(options.as_ref(), 0, term_width as i64, term_height as i64);
            let mut overlay_lines = component.borrow_mut().render(width.max(1) as usize);
            if let Some(max_height) = max_height {
                let max_height = max_height.max(0) as usize;
                if overlay_lines.len() > max_height {
                    overlay_lines.truncate(max_height);
                }
            }
            let (width, row, col, _) = Self::resolve_overlay_layout(
                options.as_ref(),
                overlay_lines.len() as i64,
                term_width as i64,
                term_height as i64,
            );
            {
                let mut state = self.state.borrow_mut();
                state.overlay_stack[index].bounds = Some(OverlayBounds {
                    row,
                    col,
                    width: width.max(0) as usize,
                    height: overlay_lines.len(),
                });
            }
            min_lines_needed = min_lines_needed.max(row + overlay_lines.len() as i64);
            rendered.push(Rendered {
                component,
                overlay_lines,
                row,
                col,
                width: width.max(0) as usize,
            });
        }
        visible_entries.clear();

        {
            let mut state = self.state.borrow_mut();
            state.rendered_overlay_layouts = rendered
                .iter()
                .map(|r| RenderedOverlayLayout {
                    entry_component: Rc::clone(&r.component),
                    row: r.row,
                    col: r.col,
                    width: r.width,
                    height: r.overlay_lines.len(),
                })
                .collect();
        }

        let working_height = (result.len() as i64)
            .max(term_height as i64)
            .max(min_lines_needed) as usize;
        while result.len() < working_height {
            result.push(String::new());
        }
        let viewport_start = working_height.saturating_sub(term_height as usize) as i64;

        for r in &rendered {
            for (i, overlay_line) in r.overlay_lines.iter().enumerate() {
                let idx = viewport_start + r.row + i as i64;
                if idx >= 0 && (idx as usize) < result.len() {
                    let truncated = if visible_width(overlay_line) > r.width {
                        slice_by_column(overlay_line, 0, r.width, true)
                    } else {
                        overlay_line.clone()
                    };
                    result[idx as usize] =
                        composite_line_at(&result[idx as usize], &truncated, r.col.max(0) as usize, r.width, term_width as usize);
                }
            }
        }

        result
    }

    /// Find and extract the cursor position from rendered lines, stripping the marker.
    pub(crate) fn extract_cursor_position(
        &self,
        lines: &mut [String],
        height: usize,
    ) -> Option<(usize, usize)> {
        let viewport_top = lines.len().saturating_sub(height);
        for row in (viewport_top..lines.len()).rev() {
            let line = &lines[row];
            if let Some(marker_index) = line.find(CURSOR_MARKER) {
                let before_marker = &line[..marker_index];
                let col = visible_width(before_marker);
                let mut after_marker = line[marker_index + CURSOR_MARKER.len()..].to_string();
                let show_hardware_cursor = self.state.borrow().show_hardware_cursor;
                if show_hardware_cursor && after_marker.starts_with(FAKE_CURSOR_START) {
                    let fake_cursor_end = after_marker[FAKE_CURSOR_START.len()..]
                        .find(FAKE_CURSOR_END)
                        .map(|i| i + FAKE_CURSOR_START.len());
                    let fake_cursor_reset = after_marker[FAKE_CURSOR_START.len()..]
                        .find(FAKE_CURSOR_RESET)
                        .map(|i| i + FAKE_CURSOR_START.len());
                    match (fake_cursor_reset, fake_cursor_end) {
                        (Some(reset), end) if end.is_none_or(|end| reset < end) => {
                            after_marker = after_marker[FAKE_CURSOR_START.len()..].to_string();
                        }
                        (_, Some(end)) => {
                            after_marker = format!(
                                "{}{}",
                                &after_marker[FAKE_CURSOR_START.len()..end],
                                &after_marker[end + FAKE_CURSOR_END.len()..]
                            );
                        }
                        _ => {}
                    }
                }
                let before_owned = before_marker.to_string();
                lines[row] = format!("{before_owned}{after_marker}");
                return Some((row, col));
            }
        }
        None
    }

    /// Whether `normalize_terminal_output` can change `line` (TAB or Thai/Lao AM vowel);
    /// otherwise the normalized form is just `line` + `SEGMENT_RESET` and needs no memo.
    fn needs_terminal_normalization(line: &str) -> bool {
        line.contains(['\t', '\u{0e33}', '\u{0eb3}'])
    }

    fn normalize_line(state: &mut TuiBaseState, line: &str) -> (String, bool) {
        if is_image_line(line) {
            return (line.to_string(), false);
        }
        if !Self::needs_terminal_normalization(line) {
            return (format!("{line}{SEGMENT_RESET}"), true);
        }
        if let Some(cached) = state.normalize_memo.get(line) {
            return (cached.clone(), false);
        }
        let normalized = format!("{}{}", normalize_terminal_output(line), SEGMENT_RESET);
        state.normalize_memo.insert(line.to_string(), normalized.clone());
        (normalized, true)
    }

    pub(crate) fn apply_line_reset_result(state: &mut TuiBaseState, lines: &[String]) -> NormalizedLinesResult {
        let mut previous_memo = std::mem::take(&mut state.normalize_memo);
        let mut next_memo = std::collections::HashMap::new();
        let mut normalized_lines = Vec::with_capacity(lines.len());
        for line in lines {
            if is_image_line(line) {
                normalized_lines.push(line.clone());
                continue;
            }
            if !Self::needs_terminal_normalization(line) {
                normalized_lines.push(format!("{line}{SEGMENT_RESET}"));
                continue;
            }
            let normalized = next_memo
                .get(line)
                .cloned()
                .or_else(|| previous_memo.remove(line))
                .unwrap_or_else(|| format!("{}{}", normalize_terminal_output(line), SEGMENT_RESET));
            next_memo.insert(line.clone(), normalized.clone());
            normalized_lines.push(normalized);
        }
        state.normalize_memo = next_memo;
        NormalizedLinesResult {
            compare_end_exclusive: normalized_lines.len(),
            lines: normalized_lines,
            first_raw_changed: None,
            bounded: false,
        }
    }

    fn apply_viewport_line_resets(
        state: &mut TuiBaseState,
        raw_lines: &[String],
        viewport_top: usize,
        height: usize,
        stable_dimensions: bool,
    ) -> NormalizedLinesResult {
        if !crate::mux::viewport_render_enabled()
            || !stable_dimensions
            || state.previous_lines.is_empty()
            || state.previous_lines.len() != raw_lines.len()
            || state.previous_raw_lines.len() != raw_lines.len()
        {
            return Self::apply_line_reset_result(state, raw_lines);
        }

        let window_start = viewport_top.saturating_sub(VIEWPORT_RENDER_OVERSCAN);
        let window_end = (viewport_top + height + VIEWPORT_RENDER_OVERSCAN).min(raw_lines.len());
        let mut first_raw_changed: Option<usize> = None;
        for (i, raw_line) in raw_lines.iter().enumerate() {
            if *raw_line == state.previous_raw_lines[i] {
                continue;
            }
            if first_raw_changed.is_none() {
                first_raw_changed = Some(i);
            }
            if i < window_start || i >= window_end {
                return Self::apply_line_reset_result(state, raw_lines);
            }
        }

        // Nothing changed: `do_render` takes the no-change path without needing the lines.
        if first_raw_changed.is_none() {
            return NormalizedLinesResult {
                lines: Vec::new(),
                first_raw_changed,
                compare_end_exclusive: window_end,
                bounded: true,
            };
        }

        let mut lines = state.previous_lines.clone();
        for i in window_start..window_end {
            if raw_lines[i] == state.previous_raw_lines[i] {
                continue;
            }
            // The replaced raw row will not be looked up again; keep the memo from growing.
            state.normalize_memo.remove(state.previous_raw_lines[i].as_str());
            let (normalized, _) = Self::normalize_line(state, &raw_lines[i]);
            lines[i] = normalized;
        }

        NormalizedLinesResult {
            lines,
            first_raw_changed,
            compare_end_exclusive: window_end,
            bounded: true,
        }
    }

    fn collect_kitty_image_ids(lines: &[String]) -> HashSet<u32> {
        let mut ids = HashSet::new();
        for line in lines {
            for id in extract_kitty_image_ids(line) {
                ids.insert(id);
            }
        }
        ids
    }

    fn delete_kitty_images(ids: impl IntoIterator<Item = u32>) -> String {
        let mut buffer = String::new();
        for id in ids {
            buffer.push_str(&delete_kitty_image(id));
        }
        buffer
    }

    fn get_kitty_image_reserved_rows(lines: &[String], index: usize, max_index: usize) -> usize {
        let rows = extract_kitty_image_rows(&lines[index]);
        if rows <= 1 {
            return 1;
        }
        let max_rows = rows.min(max_index + 1 - index).min(lines.len() - index);
        let mut reserved_rows = 1;
        while reserved_rows < max_rows {
            let line = lines.get(index + reserved_rows).map(String::as_str).unwrap_or("");
            if is_image_line(line) || visible_width(line) > 0 {
                break;
            }
            reserved_rows += 1;
        }
        reserved_rows
    }

    /// Apply the differential rendering algorithm and write the resulting frame(s) to
    /// `terminal`. Called by [`crate::tui_main_screen::TuiMainScreen::do_render`] and
    /// [`crate::tui_alt_screen::TuiAltScreen::do_render`] once per due frame.
    #[allow(clippy::too_many_lines)]
    pub fn do_render(&mut self, terminal: &mut dyn Terminal) {
        if self.state.borrow().stopped {
            return;
        }
        let width = terminal.columns() as usize;
        let height = terminal.rows() as usize;

        let (previous_width, previous_height, previous_viewport_top, previous_height_i) = {
            let state = self.state.borrow();
            (
                state.previous_width,
                state.previous_height,
                state.previous_viewport_top,
                state.previous_height,
            )
        };
        let width_changed = previous_width != 0 && previous_width != width as i64;
        let height_changed = previous_height != 0 && previous_height != height as i64;
        if width_changed || height_changed {
            self.state.borrow_mut().placement_epoch += 1;
        }
        let previous_buffer_length = if previous_height_i > 0 {
            previous_viewport_top + previous_height as usize
        } else {
            height
        };
        let mut prev_viewport_top = if height_changed {
            previous_buffer_length.saturating_sub(height)
        } else {
            previous_viewport_top
        };
        let mut viewport_top = prev_viewport_top;
        let mut hardware_cursor_row = self.state.borrow().hardware_cursor_row;

        let mut new_lines = self.container.render(width);
        if !self.state.borrow().overlay_stack.is_empty() {
            new_lines = self.composite_overlays(new_lines, terminal.columns(), terminal.rows());
        }

        let cursor_pos = self.extract_cursor_position(&mut new_lines, height);
        let raw_lines = std::mem::take(&mut new_lines);

        let preserve_mux_scrollback = self.should_preserve_mux_scrollback();
        let mut normalized = {
            let mut state = self.state.borrow_mut();
            Self::apply_viewport_line_resets(
                &mut state,
                &raw_lines,
                prev_viewport_top,
                height,
                !width_changed && !height_changed,
            )
        };
        if normalized.bounded && normalized.first_raw_changed.is_none() {
            // Bounded and unchanged: the normalized frame equals `previous_lines`. Only the
            // clear-on-shrink redraw needs the lines; otherwise this is the no-change path below.
            let clear_on_shrink_needed = {
                let state = self.state.borrow();
                state.clear_on_shrink
                    && raw_lines.len() < state.max_lines_rendered
                    && state.overlay_stack.is_empty()
            };
            if !clear_on_shrink_needed {
                self.position_hardware_cursor(terminal, cursor_pos, raw_lines.len());
                let mut state = self.state.borrow_mut();
                state.previous_viewport_top = prev_viewport_top;
                state.previous_height = height as i64;
                return;
            }
            normalized.lines = self.state.borrow().previous_lines.clone();
        }
        new_lines = std::mem::take(&mut normalized.lines);

        let previous_lines_len = self.state.borrow().previous_lines.len();

        // First render: emit everything without clearing.
        if previous_lines_len == 0 && !width_changed && !height_changed {
            self.log_redraw("first render", new_lines.len(), height);
            self.full_render(terminal, new_lines, raw_lines, cursor_pos, width, height, false, false);
            return;
        }

        if width_changed {
            self.log_redraw(&format!("terminal width changed ({previous_width} -> {width})"), new_lines.len(), height);
            self.full_render(terminal, new_lines, raw_lines, cursor_pos, width, height, true, !preserve_mux_scrollback);
            return;
        }

        // Height changes normally need a full re-render to keep the visible viewport aligned,
        // but Termux changes height when the software keyboard shows or hides; there a full
        // redraw would replay the entire history on every toggle, so fall through to the diff.
        if height_changed && !is_termux_session() {
            self.log_redraw(&format!("terminal height changed ({previous_height} -> {height})"), new_lines.len(), height);
            if preserve_mux_scrollback {
                if let Err((new_lines, raw_lines)) =
                    self.render_mux_viewport_repaint(terminal, new_lines, raw_lines, cursor_pos, width, height, None)
                {
                    self.full_render(terminal, new_lines, raw_lines, cursor_pos, width, height, true, false);
                }
            } else {
                self.full_render(terminal, new_lines, raw_lines, cursor_pos, width, height, true, true);
            }
            return;
        }

        let clear_on_shrink_needed = {
            let state = self.state.borrow();
            state.clear_on_shrink
                && new_lines.len() < state.max_lines_rendered
                && state.overlay_stack.is_empty()
        };
        if clear_on_shrink_needed {
            let max_lines_rendered = self.state.borrow().max_lines_rendered;
            self.log_redraw(&format!("clearOnShrink (maxLinesRendered={max_lines_rendered})"), new_lines.len(), height);
            self.full_render(terminal, new_lines, raw_lines, cursor_pos, width, height, true, !preserve_mux_scrollback);
            return;
        }

        let max_lines = new_lines.len().max(previous_lines_len);
        let diff_scan_start = if normalized.bounded {
            normalized.first_raw_changed.unwrap_or(0)
        } else {
            0
        };
        let diff_scan_end_exclusive = if normalized.bounded {
            normalized.compare_end_exclusive
        } else {
            max_lines
        };
        let mut first_changed: Option<usize> = None;
        let mut last_changed = 0usize;
        {
            let state = self.state.borrow();
            for i in diff_scan_start..diff_scan_end_exclusive {
                let old_line = state.previous_lines.get(i).map(String::as_str).unwrap_or("");
                let new_line = new_lines.get(i).map(String::as_str).unwrap_or("");
                if old_line != new_line {
                    if first_changed.is_none() {
                        first_changed = Some(i);
                    }
                    last_changed = i;
                }
            }
        }
        let appended_lines = new_lines.len() > previous_lines_len;
        let line_count_delta = new_lines.len() as i64 - previous_lines_len as i64;
        if appended_lines {
            if first_changed.is_none() {
                first_changed = Some(previous_lines_len);
            }
            last_changed = new_lines.len().saturating_sub(1);
        }

        let needs_kitty_expansion = first_changed.is_some_and(|fc| {
            let state = self.state.borrow();
            !state.previous_kitty_image_ids.is_empty() || {
                let end = last_changed.min(new_lines.len().saturating_sub(1));
                (fc..=end).any(|i| {
                    let line = new_lines.get(i).map(String::as_str).unwrap_or("");
                    is_image_line(line) || !extract_kitty_image_ids(line).is_empty()
                })
            }
        });
        if needs_kitty_expansion
            && let Some(fc) = first_changed
        {
            let (expanded_first, expanded_last) =
                self.expand_changed_range_for_kitty_images(fc, last_changed, &new_lines);
            first_changed = Some(expanded_first);
            last_changed = expanded_last;
        }

        let Some(first_changed) = first_changed else {
            self.position_hardware_cursor(terminal, cursor_pos, new_lines.len());
            let mut state = self.state.borrow_mut();
            state.previous_viewport_top = prev_viewport_top;
            state.previous_height = height as i64;
            return;
        };

        let append_start = appended_lines && first_changed == previous_lines_len && first_changed > 0;
        let insert_scroll_plan =
            self.create_viewport_insert_scroll_plan(&new_lines, prev_viewport_top, height, line_count_delta);
        if let Some(plan) = insert_scroll_plan {
            self.render_viewport_insert_scroll(terminal, plan, new_lines, raw_lines, cursor_pos, width, height);
            return;
        }

        // All changes are in deleted lines: nothing to render, just clear.
        if first_changed >= new_lines.len() {
            self.render_trailing_deletion(
                terminal,
                new_lines,
                raw_lines,
                cursor_pos,
                first_changed,
                last_changed,
                width,
                height,
                prev_viewport_top,
                hardware_cursor_row,
                preserve_mux_scrollback,
            );
            return;
        }

        let mut first_changed = first_changed;
        if first_changed < prev_viewport_top {
            if new_lines.len() < previous_lines_len {
                viewport_top = new_lines.len().saturating_sub(height);
            }
            if new_lines.len() > previous_lines_len {
                let max_viewport_top = new_lines.len().saturating_sub(height);
                viewport_top = max_viewport_top.min((prev_viewport_top as i64 + line_count_delta).max(0) as usize);
            }

            let mut first_visible_changed: Option<usize> = None;
            let mut last_visible_changed = 0usize;
            {
                let state = self.state.borrow();
                for row in 0..height {
                    let previous_line = state
                        .previous_lines
                        .get(prev_viewport_top + row)
                        .map(String::as_str)
                        .unwrap_or("");
                    let next_line = new_lines.get(viewport_top + row).map(String::as_str).unwrap_or("");
                    if previous_line != next_line {
                        if first_visible_changed.is_none() {
                            first_visible_changed = Some(row);
                        }
                        last_visible_changed = row;
                    }
                }
            }

            let Some(first_visible_changed) = first_visible_changed else {
                if line_count_delta != 0 {
                    if preserve_mux_scrollback {
                        // Above-viewport scrollback may stay stale in mux panes; only the visible viewport is repainted.
                        if let Err((new_lines, raw_lines)) = self.render_mux_viewport_repaint(
                            terminal, new_lines, raw_lines, cursor_pos, width, height, Some(viewport_top),
                        ) {
                            self.full_render(terminal, new_lines, raw_lines, cursor_pos, width, height, true, false);
                        }
                    } else {
                        self.render_scrollback_replay(
                            terminal, new_lines, raw_lines, cursor_pos, width, height, prev_viewport_top,
                            hardware_cursor_row,
                        );
                    }
                    return;
                }
                let mut state = self.state.borrow_mut();
                state.cursor_row = new_lines.len().saturating_sub(1);
                state.max_lines_rendered = state.max_lines_rendered.max(new_lines.len());
                state.previous_lines = new_lines;
                state.previous_raw_lines = raw_lines;
                state.previous_width = width as i64;
                state.previous_height = height as i64;
                state.previous_viewport_top = viewport_top;
                return;
            };

            if viewport_top != prev_viewport_top {
                if line_count_delta != 0 {
                    if preserve_mux_scrollback {
                        if let Err((new_lines, raw_lines)) = self.render_mux_viewport_repaint(
                            terminal, new_lines, raw_lines, cursor_pos, width, height, Some(viewport_top),
                        ) {
                            self.full_render(terminal, new_lines, raw_lines, cursor_pos, width, height, true, false);
                        }
                    } else {
                        self.render_scrollback_replay(
                            terminal, new_lines, raw_lines, cursor_pos, width, height, prev_viewport_top,
                            hardware_cursor_row,
                        );
                    }
                    return;
                }
                self.render_viewport_shift(
                    terminal, new_lines, raw_lines, cursor_pos, width, height, prev_viewport_top,
                    viewport_top, hardware_cursor_row,
                );
                return;
            }

            first_changed = viewport_top + first_visible_changed;
            last_changed = (viewport_top + last_visible_changed).min(new_lines.len().saturating_sub(1));
        }

        self.render_changed_range(
            terminal, new_lines, raw_lines, cursor_pos, width, height, first_changed, last_changed,
            append_start, &mut prev_viewport_top, &mut viewport_top, &mut hardware_cursor_row,
            needs_kitty_expansion,
        );
    }

    fn expand_changed_range_for_kitty_images(
        &self,
        first_changed: usize,
        last_changed: usize,
        new_lines: &[String],
    ) -> (usize, usize) {
        let mut expanded_first = first_changed;
        let mut expanded_last = last_changed;
        let mut expand_for = |lines: &[String]| {
            for i in 0..lines.len() {
                if extract_kitty_image_ids(&lines[i]).is_empty() {
                    continue;
                }
                let block_end = i + Self::get_kitty_image_reserved_rows(lines, i, lines.len() - 1) - 1;
                if i >= first_changed || (i <= last_changed && block_end >= first_changed) {
                    expanded_first = expanded_first.min(i);
                    expanded_last = expanded_last.max(block_end);
                }
            }
        };
        expand_for(&self.state.borrow().previous_lines);
        expand_for(new_lines);
        (expanded_first, expanded_last)
    }

    fn delete_changed_kitty_images(&self, first_changed: usize, last_changed: usize) -> String {
        let state = self.state.borrow();
        if last_changed < first_changed {
            return String::new();
        }
        let mut ids = HashSet::new();
        let max_line = last_changed.min(state.previous_lines.len().saturating_sub(1));
        for i in first_changed..=max_line {
            if let Some(line) = state.previous_lines.get(i) {
                for id in extract_kitty_image_ids(line) {
                    ids.insert(id);
                }
            }
        }
        Self::delete_kitty_images(ids)
    }

    fn get_viewport_rows(lines: &[String], viewport_top: usize, height: usize) -> Vec<String> {
        (0..height)
            .map(|row| lines.get(viewport_top + row).cloned().unwrap_or_default())
            .collect()
    }

    /// senpi's `shouldPreserveMuxScrollback`.
    fn should_preserve_mux_scrollback(&self) -> bool {
        (self.mux_detector)() && !crate::mux::use_legacy_mux_render()
    }

    fn create_viewport_insert_scroll_plan(
        &self,
        new_lines: &[String],
        prev_viewport_top: usize,
        height: usize,
        line_count_delta: i64,
    ) -> Option<ViewportInsertScrollPlan> {
        if line_count_delta <= 0
            || line_count_delta >= height as i64
            || !self.state.borrow().overlay_stack.is_empty()
        {
            return None;
        }
        let line_count_delta = line_count_delta as usize;
        let max_viewport_top = new_lines.len().saturating_sub(height);
        let viewport_top = max_viewport_top.min(prev_viewport_top + line_count_delta);
        if viewport_top <= prev_viewport_top {
            return None;
        }

        let previous_visible = {
            let state = self.state.borrow();
            Self::get_viewport_rows(&state.previous_lines, prev_viewport_top, height)
        };
        let next_visible = Self::get_viewport_rows(new_lines, viewport_top, height);
        if previous_visible.iter().any(|l| is_image_line(l)) || next_visible.iter().any(|l| is_image_line(l)) {
            return None;
        }

        let mut stable_suffix_rows = 0usize;
        while stable_suffix_rows < height
            && previous_visible[height - stable_suffix_rows - 1] == next_visible[height - stable_suffix_rows - 1]
        {
            stable_suffix_rows += 1;
        }

        let region_height = height - stable_suffix_rows;
        if region_height == 0 || region_height < line_count_delta {
            return None;
        }

        let mut mutated_rows = Vec::new();
        for row in 0..region_height.saturating_sub(line_count_delta) {
            if previous_visible[row + line_count_delta] != next_visible[row] {
                mutated_rows.push((row, next_visible[row].clone()));
                if mutated_rows.len() > MAX_SCROLL_DIFF_ROWS {
                    return None;
                }
            }
        }

        Some(ViewportInsertScrollPlan {
            viewport_top,
            region_bottom: region_height - 1,
            inserted_rows: next_visible[region_height - line_count_delta..region_height].to_vec(),
            mutated_rows,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn render_viewport_insert_scroll(
        &mut self,
        terminal: &mut dyn Terminal,
        plan: ViewportInsertScrollPlan,
        new_lines: Vec<String>,
        raw_lines: Vec<String>,
        cursor_pos: Option<(usize, usize)>,
        width: usize,
        height: usize,
    ) {
        let _ = height;
        let mut buffer = FRAME_BEGIN.to_string();
        let region_top = 0;
        let region_bottom = plan.region_bottom;
        buffer += &format!("\x1b[{};{}r", region_top + 1, region_bottom + 1);
        buffer += &format!("\x1b[{};1H", region_bottom + 1);
        buffer += &"\n".repeat(plan.inserted_rows.len());
        buffer += "\x1b[r";

        let first_inserted_screen_row = region_bottom + 1 - plan.inserted_rows.len();
        let mut final_painted_screen_row = first_inserted_screen_row + plan.inserted_rows.len() - 1;
        for (index, row) in plan.inserted_rows.iter().enumerate() {
            let screen_row = first_inserted_screen_row + index;
            buffer += &format!("\x1b[{};1H\x1b[2K{SEGMENT_RESET}", screen_row + 1);
            buffer += row;
        }
        for (screen_row, content) in &plan.mutated_rows {
            buffer += &format!("\x1b[{};1H\x1b[2K{SEGMENT_RESET}", screen_row + 1);
            buffer += content;
            final_painted_screen_row = *screen_row;
        }

        let final_cursor_row = plan.viewport_top + final_painted_screen_row;
        buffer = self.finish_frame(buffer, cursor_pos, new_lines.len(), final_cursor_row);
        write_bounded(terminal, &buffer);

        let kitty_image_ids = Self::collect_kitty_image_ids(&new_lines);
        let mut state = self.state.borrow_mut();
        state.cursor_row = new_lines.len().saturating_sub(1);
        state.max_lines_rendered = state.max_lines_rendered.max(new_lines.len());
        state.previous_viewport_top = plan.viewport_top;
        state.previous_lines = new_lines;
        state.previous_raw_lines = raw_lines;
        state.previous_kitty_image_ids = kitty_image_ids;
        state.previous_width = width as i64;
        state.previous_height = height as i64;
        state.placement_epoch += 1;
    }

    #[allow(clippy::too_many_arguments)]
    fn render_scrollback_replay(
        &mut self,
        terminal: &mut dyn Terminal,
        new_lines: Vec<String>,
        raw_lines: Vec<String>,
        cursor_pos: Option<(usize, usize)>,
        width: usize,
        height: usize,
        prev_viewport_top: usize,
        hardware_cursor_row: usize,
    ) {
        let mut buffer = FRAME_BEGIN.to_string();
        let previous_kitty_ids = self.state.borrow().previous_kitty_image_ids.clone();
        buffer += &Self::delete_kitty_images(previous_kitty_ids);
        if !self.should_preserve_mux_scrollback() {
            buffer += "\x1b[3J";
        }

        let current_screen_row = hardware_cursor_row
            .saturating_sub(prev_viewport_top)
            .min(height.saturating_sub(1));
        if current_screen_row > 0 {
            buffer += &format!("\x1b[{current_screen_row}A");
        }

        let buffer_length = height.max(new_lines.len());
        for row in 0..buffer_length {
            if row > 0 {
                buffer += "\r\n";
            }
            buffer += &format!("\r\x1b[2K{SEGMENT_RESET}");
            buffer += new_lines.get(row).map(String::as_str).unwrap_or("");
        }

        buffer = self.finish_frame(buffer, cursor_pos, new_lines.len(), buffer_length.saturating_sub(1));
        write_bounded(terminal, &buffer);

        let kitty_image_ids = Self::collect_kitty_image_ids(&new_lines);
        let mut state = self.state.borrow_mut();
        state.cursor_row = new_lines.len().saturating_sub(1);
        state.max_lines_rendered = new_lines.len();
        state.previous_viewport_top = buffer_length.saturating_sub(height);
        state.previous_lines = new_lines;
        state.previous_raw_lines = raw_lines;
        state.previous_kitty_image_ids = kitty_image_ids;
        state.previous_width = width as i64;
        state.previous_height = height as i64;
        state.placement_epoch += 1;
    }

    /// senpi's `renderMuxViewportRepaint`: repaint only the visible rows (no `2J`/`3J`) so a
    /// multiplexer pane keeps its scrollback. `viewport_top` defaults to the bottom of
    /// `new_lines`. Returns the lines back as `Err` without writing when a visible row is an
    /// image line, so the caller can fall back to a full render.
    #[allow(clippy::too_many_arguments)]
    fn render_mux_viewport_repaint(
        &mut self,
        terminal: &mut dyn Terminal,
        new_lines: Vec<String>,
        raw_lines: Vec<String>,
        cursor_pos: Option<(usize, usize)>,
        width: usize,
        height: usize,
        viewport_top: Option<usize>,
    ) -> Result<(), (Vec<String>, Vec<String>)> {
        let viewport_top = viewport_top.unwrap_or_else(|| new_lines.len().saturating_sub(height));
        let (previous_viewport_top, hardware_cursor_row) = {
            let state = self.state.borrow();
            let previous_visible = Self::get_viewport_rows(&state.previous_lines, state.previous_viewport_top, height);
            let next_visible = Self::get_viewport_rows(&new_lines, viewport_top, height);
            if previous_visible.iter().any(|line| is_image_line(line))
                || next_visible.iter().any(|line| is_image_line(line))
            {
                return Err((new_lines, raw_lines));
            }
            (state.previous_viewport_top, state.hardware_cursor_row)
        };

        let mut buffer = FRAME_BEGIN.to_string();
        let current_screen_row = hardware_cursor_row
            .saturating_sub(previous_viewport_top)
            .min(height.saturating_sub(1));
        if current_screen_row > 0 {
            buffer += &format!("\x1b[{current_screen_row}A");
        }

        for row in 0..height {
            if row > 0 {
                buffer += "\r\n";
            }
            buffer += &format!("\r\x1b[2K{SEGMENT_RESET}");
            buffer += new_lines.get(viewport_top + row).map(String::as_str).unwrap_or("");
        }

        let final_cursor_row = viewport_top + height.saturating_sub(1);
        buffer = self.finish_frame(buffer, cursor_pos, new_lines.len(), final_cursor_row);
        write_bounded(terminal, &buffer);

        let kitty_image_ids = Self::collect_kitty_image_ids(&new_lines);
        let mut state = self.state.borrow_mut();
        state.mux_viewport_repaint_count += 1;
        state.cursor_row = new_lines.len().saturating_sub(1);
        state.max_lines_rendered = new_lines.len();
        state.previous_viewport_top = viewport_top;
        state.previous_lines = new_lines;
        state.previous_raw_lines = raw_lines;
        state.previous_kitty_image_ids = kitty_image_ids;
        state.previous_width = width as i64;
        state.previous_height = height as i64;
        state.placement_epoch += 1;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn render_viewport_shift(
        &mut self,
        terminal: &mut dyn Terminal,
        new_lines: Vec<String>,
        raw_lines: Vec<String>,
        cursor_pos: Option<(usize, usize)>,
        width: usize,
        height: usize,
        prev_viewport_top: usize,
        viewport_top: usize,
        hardware_cursor_row: usize,
    ) {
        let previous_viewport_bottom = {
            let state = self.state.borrow();
            (state.previous_lines.len().saturating_sub(1)).min(prev_viewport_top + height - 1)
        };
        let mut buffer = FRAME_BEGIN.to_string();
        buffer += &self.delete_changed_kitty_images(prev_viewport_top, previous_viewport_bottom);

        let current_screen_row = hardware_cursor_row
            .saturating_sub(prev_viewport_top)
            .min(height.saturating_sub(1));
        if current_screen_row > 0 {
            buffer += &format!("\x1b[{current_screen_row}A");
        }

        for row in 0..height {
            if row > 0 {
                buffer += "\r\n";
            }
            buffer += &format!("\r\x1b[2K{SEGMENT_RESET}");
            buffer += new_lines.get(viewport_top + row).map(String::as_str).unwrap_or("");
        }

        let final_cursor_row = viewport_top + height.saturating_sub(1);
        buffer = self.finish_frame(buffer, cursor_pos, new_lines.len(), final_cursor_row);
        write_bounded(terminal, &buffer);

        let kitty_image_ids = Self::collect_kitty_image_ids(&new_lines);
        let mut state = self.state.borrow_mut();
        state.cursor_row = new_lines.len().saturating_sub(1);
        state.max_lines_rendered = state.max_lines_rendered.max(new_lines.len());
        state.previous_viewport_top = viewport_top;
        state.previous_lines = new_lines;
        state.previous_raw_lines = raw_lines;
        state.previous_kitty_image_ids = kitty_image_ids;
        state.previous_width = width as i64;
        state.previous_height = height as i64;
    }

    #[allow(clippy::too_many_arguments)]
    fn render_trailing_deletion(
        &mut self,
        terminal: &mut dyn Terminal,
        new_lines: Vec<String>,
        raw_lines: Vec<String>,
        cursor_pos: Option<(usize, usize)>,
        first_changed: usize,
        last_changed: usize,
        width: usize,
        height: usize,
        prev_viewport_top: usize,
        hardware_cursor_row: usize,
        preserve_mux_scrollback: bool,
    ) {
        let previous_lines_len = self.state.borrow().previous_lines.len();
        if previous_lines_len > new_lines.len() {
            let target_row = new_lines.len().saturating_sub(1);
            if target_row < prev_viewport_top {
                self.log_redraw(&format!("deleted lines moved viewport up ({target_row} < {prev_viewport_top})"), new_lines.len(), height);
                self.full_render(terminal, new_lines, raw_lines, cursor_pos, width, height, true, !preserve_mux_scrollback);
                return;
            }
            let extra_lines = previous_lines_len - new_lines.len();
            if extra_lines > height {
                self.log_redraw(&format!("extraLines > height ({extra_lines} > {height})"), new_lines.len(), height);
                self.full_render(terminal, new_lines, raw_lines, cursor_pos, width, height, true, !preserve_mux_scrollback);
                return;
            }
            let mut buffer = FRAME_BEGIN.to_string();
            buffer += &self.delete_changed_kitty_images(first_changed, last_changed);
            let current_screen_row = hardware_cursor_row as i64 - prev_viewport_top as i64;
            let target_screen_row = target_row as i64 - prev_viewport_top as i64;
            let line_diff = target_screen_row - current_screen_row;
            if line_diff > 0 {
                buffer += &format!("\x1b[{line_diff}B");
            } else if line_diff < 0 {
                buffer += &format!("\x1b[{}A", -line_diff);
            }
            buffer += "\r";
            let clear_start_offset = if new_lines.is_empty() { 0 } else { 1 };
            if extra_lines > 0 && clear_start_offset > 0 {
                buffer += &format!("\x1b[{clear_start_offset}B");
            }
            for i in 0..extra_lines {
                buffer += &format!("\r\x1b[2K{SEGMENT_RESET}");
                if i < extra_lines - 1 {
                    buffer += "\x1b[1B";
                }
            }
            let move_back = (extra_lines + clear_start_offset).saturating_sub(1);
            if move_back > 0 {
                buffer += &format!("\x1b[{move_back}A");
            }
            buffer = self.finish_frame(buffer, cursor_pos, new_lines.len(), target_row);
            write_bounded(terminal, &buffer);
            self.state.borrow_mut().cursor_row = target_row;
        } else {
            self.position_hardware_cursor(terminal, cursor_pos, new_lines.len());
        }
        let kitty_image_ids = Self::collect_kitty_image_ids(&new_lines);
        let mut state = self.state.borrow_mut();
        state.previous_lines = new_lines;
        state.previous_raw_lines = raw_lines;
        state.previous_kitty_image_ids = kitty_image_ids;
        state.previous_width = width as i64;
        state.previous_height = height as i64;
        state.previous_viewport_top = prev_viewport_top;
    }

    /// The `PI_TUI_DEBUG` frame dump of senpi `doRender`.
    fn write_tui_debug_dump(&self, frame: TuiDebugFrame, new_lines: &[String], buffer: &str) {
        use std::hash::{BuildHasher, Hasher};
        let debug_dir = std::path::Path::new("/tmp/tui");
        if std::fs::create_dir_all(debug_dir).is_err() {
            return;
        }
        let suffix = std::collections::hash_map::RandomState::new().build_hasher().finish();
        let debug_path = debug_dir.join(format!("render-{}-{suffix:x}.log", jiff::Timestamp::now().as_millisecond()));
        let state = self.state.borrow();
        let cursor_pos = frame.cursor_pos.map_or_else(
            || "null".to_owned(),
            |(row, col)| serde_json::json!({ "row": row, "col": col }).to_string(),
        );
        let pretty = |lines: &[String]| serde_json::to_string_pretty(lines).unwrap_or_default();
        let debug_data = [
            format!("firstChanged: {}", frame.first_changed),
            format!("viewportTop: {}", frame.viewport_top),
            format!("cursorRow: {}", state.cursor_row),
            format!("height: {}", frame.height),
            format!("lineDiff: {}", frame.line_diff),
            format!("hardwareCursorRow: {}", frame.hardware_cursor_row),
            format!("renderEnd: {}", frame.render_end),
            format!("finalCursorRow: {}", frame.final_cursor_row),
            format!("cursorPos: {cursor_pos}"),
            format!("newLines.length: {}", new_lines.len()),
            format!("previousLines.length: {}", state.previous_lines.len()),
            String::new(),
            "=== newLines ===".to_owned(),
            pretty(new_lines),
            String::new(),
            "=== previousLines ===".to_owned(),
            pretty(&state.previous_lines),
            String::new(),
            "=== buffer ===".to_owned(),
            serde_json::Value::String(buffer.to_owned()).to_string(),
        ]
        .join("\n");
        write_render_diagnostic_best_effort(&debug_path, &debug_data);
    }

    #[allow(clippy::too_many_arguments)]
    fn render_changed_range(
        &mut self,
        terminal: &mut dyn Terminal,
        mut new_lines: Vec<String>,
        raw_lines: Vec<String>,
        cursor_pos: Option<(usize, usize)>,
        width: usize,
        height: usize,
        first_changed: usize,
        last_changed: usize,
        append_start: bool,
        prev_viewport_top: &mut usize,
        viewport_top: &mut usize,
        hardware_cursor_row: &mut usize,
        needs_kitty_expansion: bool,
    ) {
        let mut buffer = FRAME_BEGIN.to_string();
        buffer += &self.delete_changed_kitty_images(first_changed, last_changed);
        let prev_viewport_bottom = *prev_viewport_top + height - 1;
        let move_target_row = if append_start { first_changed - 1 } else { first_changed };
        if move_target_row > prev_viewport_bottom {
            let current_screen_row = (*hardware_cursor_row as i64 - *prev_viewport_top as i64)
                .clamp(0, height as i64 - 1) as usize;
            let move_to_bottom = height - 1 - current_screen_row;
            if move_to_bottom > 0 {
                buffer += &format!("\x1b[{move_to_bottom}B");
            }
            let scroll = move_target_row - prev_viewport_bottom;
            buffer += &"\r\n".repeat(scroll);
            *prev_viewport_top += scroll;
            *viewport_top += scroll;
            *hardware_cursor_row = move_target_row;
        }

        let current_screen_row = *hardware_cursor_row as i64 - *prev_viewport_top as i64;
        let target_screen_row = move_target_row as i64 - *viewport_top as i64;
        let line_diff = target_screen_row - current_screen_row;
        if line_diff > 0 {
            buffer += &format!("\x1b[{line_diff}B");
        } else if line_diff < 0 {
            buffer += &format!("\x1b[{}A", -line_diff);
        }
        buffer += if append_start { "\r\n" } else { "\r" };

        let render_end = last_changed.min(new_lines.len().saturating_sub(1));
        let mut i = first_changed;
        while i <= render_end {
            if i > first_changed {
                buffer += "\r\n";
            }
            let line = new_lines[i].clone();
            let is_image = is_image_line(&line);
            let image_reserved_rows = if is_image {
                Self::get_kitty_image_reserved_rows(&new_lines, i, render_end)
            } else {
                1
            };
            if image_reserved_rows > 1 {
                buffer += &format!("\x1b[2K{SEGMENT_RESET}");
                for _ in 1..image_reserved_rows {
                    buffer += &format!("\r\n\x1b[2K{SEGMENT_RESET}");
                }
                buffer += &format!("\x1b[{}A", image_reserved_rows - 1);
                buffer += &line;
                buffer += &format!("\x1b[{}B", image_reserved_rows - 1);
                i += image_reserved_rows;
                continue;
            }

            buffer += &format!("\x1b[2K{SEGMENT_RESET}");
            let line_width = visible_width(&line);
            if !is_image && line_width > width {
                self.log_over_wide_render(&new_lines, width, i, line_width);
                let truncated_line = format!("{}{}", slice_by_column(&line, 0, width, true), SEGMENT_RESET);
                new_lines[i] = truncated_line.clone();
                buffer += &truncated_line;
                i += 1;
                continue;
            }
            buffer += &line;
            i += 1;
        }

        let mut final_cursor_row = render_end;
        let previous_lines_len = self.state.borrow().previous_lines.len();
        if previous_lines_len > new_lines.len() {
            if render_end < new_lines.len().saturating_sub(1) {
                let move_down = new_lines.len() - 1 - render_end;
                buffer += &format!("\x1b[{move_down}B");
                final_cursor_row = new_lines.len().saturating_sub(1);
            }
            let extra_lines = previous_lines_len - new_lines.len();
            for _ in new_lines.len()..previous_lines_len {
                buffer += &format!("\r\n\x1b[2K{SEGMENT_RESET}");
            }
            buffer += &format!("\x1b[{extra_lines}A");
        }

        buffer = self.finish_frame(buffer, cursor_pos, new_lines.len(), final_cursor_row);

        // senpi `PI_TUI_DEBUG=1`: dump each differential frame to `/tmp/tui/render-*.log`.
        if crate::process_env::var("PI_TUI_DEBUG").as_deref() == Some("1") {
            self.write_tui_debug_dump(TuiDebugFrame {
                first_changed, viewport_top: *viewport_top, height, line_diff,
                hardware_cursor_row: *hardware_cursor_row, render_end, final_cursor_row, cursor_pos,
            }, &new_lines, &buffer);
        }

        write_bounded(terminal, &buffer);

        let kitty_image_ids = needs_kitty_expansion.then(|| Self::collect_kitty_image_ids(&new_lines));
        let mut state = self.state.borrow_mut();
        state.cursor_row = new_lines.len().saturating_sub(1);
        state.max_lines_rendered = state.max_lines_rendered.max(new_lines.len());
        state.previous_viewport_top = (*prev_viewport_top).max(final_cursor_row.saturating_sub(height) + 1);
        state.previous_lines = new_lines;
        state.previous_raw_lines = raw_lines;
        if let Some(kitty_image_ids) = kitty_image_ids {
            state.previous_kitty_image_ids = kitty_image_ids;
        }
        state.previous_width = width as i64;
        state.previous_height = height as i64;
    }

    #[allow(clippy::too_many_arguments)]
    fn full_render(
        &mut self,
        terminal: &mut dyn Terminal,
        new_lines: Vec<String>,
        raw_lines: Vec<String>,
        cursor_pos: Option<(usize, usize)>,
        width: usize,
        height: usize,
        clear: bool,
        clear_scrollback: bool,
    ) {
        self.state.borrow_mut().full_redraw_count += 1;
        let mut buffer = FRAME_BEGIN.to_string();
        if clear {
            let previous_kitty_ids = self.state.borrow().previous_kitty_image_ids.clone();
            buffer += &Self::delete_kitty_images(previous_kitty_ids);
            buffer += "\x1b[2J\x1b[H";
            if clear_scrollback
                && !self.should_preserve_mux_scrollback()
                && crate::native_platform::node_platform() != "win32"
            {
                buffer += "\x1b[3J";
            }
        } else {
            buffer += &format!("\r\x1b[2K{SEGMENT_RESET}");
        }
        let mut i = 0;
        while i < new_lines.len() {
            if i > 0 {
                buffer += "\r\n";
            }
            let line = &new_lines[i];
            let is_image = is_image_line(line);
            let image_reserved_rows = if is_image {
                Self::get_kitty_image_reserved_rows(&new_lines, i, new_lines.len() - 1)
            } else {
                1
            };
            if image_reserved_rows > 1 && image_reserved_rows <= height {
                for _ in 1..image_reserved_rows {
                    buffer += "\r\n";
                }
                buffer += &format!("\x1b[{}A", image_reserved_rows - 1);
                buffer += line;
                buffer += &format!("\x1b[{}B", image_reserved_rows - 1);
                i += image_reserved_rows;
                continue;
            }
            buffer += line;
            i += 1;
        }
        let final_cursor_row = new_lines.len().saturating_sub(1);
        buffer = self.finish_frame(buffer, cursor_pos, new_lines.len(), final_cursor_row);
        write_bounded(terminal, &buffer);

        let mut state = self.state.borrow_mut();
        state.cursor_row = new_lines.len().saturating_sub(1);
        if clear {
            state.max_lines_rendered = new_lines.len();
        } else {
            state.max_lines_rendered = state.max_lines_rendered.max(new_lines.len());
        }
        let buffer_length = height.max(new_lines.len());
        state.previous_viewport_top = buffer_length.saturating_sub(height);
        state.previous_kitty_image_ids = Self::collect_kitty_image_ids(&new_lines);
        state.previous_lines = new_lines;
        state.previous_raw_lines = raw_lines;
        state.previous_width = width as i64;
        state.previous_height = height as i64;
        state.placement_epoch += 1;
    }

    fn build_hardware_cursor_sequence(&mut self, cursor_pos: Option<(usize, usize)>, total_lines: usize) -> String {
        let mut buffer = String::new();
        let mut state = self.state.borrow_mut();
        match cursor_pos {
            None if total_lines == 0 => {}
            None => {
                if state.last_cursor_visibility != Some(false) {
                    buffer += "\x1b[?25l";
                    state.last_cursor_visibility = Some(false);
                }
            }
            Some((row, col)) => {
                let target_row = row.min(total_lines.saturating_sub(1));
                let row_delta = target_row as i64 - state.hardware_cursor_row as i64;
                if row_delta > 0 {
                    buffer += &format!("\x1b[{row_delta}B");
                } else if row_delta < 0 {
                    buffer += &format!("\x1b[{}A", -row_delta);
                }
                buffer += &format!("\x1b[{}G", col + 1);
                state.hardware_cursor_row = target_row;
                let show = state.show_hardware_cursor;
                if state.last_cursor_visibility != Some(show) {
                    buffer += if show { "\x1b[?25h" } else { "\x1b[?25l" };
                    state.last_cursor_visibility = Some(show);
                }
            }
        }
        buffer
    }

    fn finish_frame(
        &mut self,
        buffer: String,
        cursor_pos: Option<(usize, usize)>,
        total_lines: usize,
        hardware_cursor_row: usize,
    ) -> String {
        self.state.borrow_mut().hardware_cursor_row = hardware_cursor_row;
        format!(
            "{}{}{}",
            buffer,
            self.build_hardware_cursor_sequence(cursor_pos, total_lines),
            FRAME_END
        )
    }

    fn position_hardware_cursor(&mut self, terminal: &mut dyn Terminal, cursor_pos: Option<(usize, usize)>, total_lines: usize) {
        let buffer = self.build_hardware_cursor_sequence(cursor_pos, total_lines);
        if !buffer.is_empty() {
            terminal.write(&format!("{FRAME_BEGIN}{buffer}{FRAME_END}"));
        }
    }

    /// Best-effort diagnostic for a component that rendered a line wider than the terminal
    /// (senpi's `formatOverWideRenderDiagnostic` + `writeRenderDiagnosticBestEffort`, gated by
    /// `overWideCrashDumpWritten` so only the first occurrence per session is logged unless
    /// `PI_TUI_STRICT_RENDER=1`). senpi additionally `stop()`s and throws under strict mode;
    /// that would panic across this crate's `unsafe_code = "forbid"`/`unwrap_used = "deny"`
    /// lint policy, so this port always truncates and logs instead of aborting the process.
    fn log_over_wide_render(&mut self, lines: &[String], terminal_width: usize, line_index: usize, line_width: usize) {
        let strict = crate::process_env::var("PI_TUI_STRICT_RENDER").as_deref() == Some("1");
        let already_written = self.state.borrow().over_wide_crash_dump_written;
        if !strict && already_written {
            return;
        }
        let body = format_over_wide_render_diagnostic(lines, terminal_width, line_index, line_width);
        let crash_log_path = default_diagnostic_log_directory().join("senpi-crash.log");
        write_render_diagnostic_best_effort(&crash_log_path, &body);
        self.state.borrow_mut().over_wide_crash_dump_written = true;
    }

    /// Dispatch to the visually topmost overlay under the pointer (senpi's
    /// `dispatchMouseToOverlay`). Callers - the alt-screen and main-screen mouse routers -
    /// consult this before falling back to the base [`Container::handle_mouse`] dispatch.
    pub fn dispatch_mouse_to_overlay(&mut self, event: &TuiMouseEvent) -> (bool, Option<TuiMouseDispatchResult>) {
        struct OverlayHitLayout {
            component: Rc<RefCell<dyn Component>>,
            row: i64,
            col: i64,
            width: usize,
            height: usize,
        }
        let layouts: Vec<OverlayHitLayout> = {
            let state = self.state.borrow();
            state
                .rendered_overlay_layouts
                .iter()
                .map(|l| OverlayHitLayout {
                    component: Rc::clone(&l.entry_component),
                    row: l.row,
                    col: l.col,
                    width: l.width,
                    height: l.height,
                })
                .collect()
        };
        for OverlayHitLayout { component, row, col, width, height } in layouts.into_iter().rev() {
            if event.screen_x < col
                || event.screen_x >= col + width as i64
                || event.screen_y < row
                || event.screen_y >= row + height as i64
            {
                continue;
            }
            let sub_event = TuiMouseEvent {
                x: event.screen_x - col,
                y: event.screen_y - row,
                width,
                height,
                ..*event
            };
            let result = dispatch_mouse_event(&component, &sub_event);
            return match result {
                Some(mut r) if r.result.focus => {
                    r.focus_target = Some(Rc::clone(&component));
                    (true, Some(r))
                }
                other => (true, other),
            };
        }
        (false, None)
    }
}

impl Default for TuiBase {
    fn default() -> Self {
        Self::new()
    }
}

#[allow(unused)]
fn ensure_get_capabilities_used() -> bool {
    get_capabilities().images_supported
}
