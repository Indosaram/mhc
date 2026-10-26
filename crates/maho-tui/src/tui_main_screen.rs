//! Port of senpi `packages/tui/src/tui-main-screen.ts`.

use std::cell::RefCell;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::rc::Rc;

use crate::image_stub::is_image_line;
use crate::mouse_input::{
    MouseClickSynthesizer, MouseTracking, SgrMouseEvent, is_mouse_sequence, parse_sgr_mouse_event,
    to_tui_mouse_event,
};
use crate::terminal::Terminal;
use crate::tui::{
    Component, MouseBlocker, TuiBase, TuiMouseDispatchTarget, TuiMouseEvent, TuiMouseEventResult,
    TuiMouseEventType, dispatch_mouse_event, retarget_mouse_event,
};

pub struct TuiMainScreenRenderState {
    pub previous_lines: Vec<String>,
    pub previous_width: i64,
    pub previous_height: i64,
    pub cursor_row: usize,
    pub hardware_cursor_row: usize,
    pub max_lines_rendered: usize,
    pub previous_viewport_top: usize,
}

struct MousePress {
    target: TuiMouseDispatchTarget,
    epoch: u64,
    frame: Vec<String>,
    components: Vec<Rc<RefCell<dyn Component>>>,
    x: i64,
    y: i64,
}

/// TUI implementation that renders into the terminal's main screen and scrollback.
pub struct TuiMainScreen {
    pub base: TuiBase,
    tracking_enabled: bool,
    yielded: bool,
    clicks: MouseClickSynthesizer,
    mouse_press: Option<MousePress>,
    layout_revision: u64,
    /// Fingerprint of the last committed `previous_lines` (see [`lines_fingerprint`]); only used
    /// to detect line changes for `layout_revision`, so a full copy is not kept.
    committed_mouse_lines: (usize, u64),
    committed_mouse_components: Vec<Rc<RefCell<dyn Component>>>,
    mouse_capture_token: Option<u64>,
}

/// Environment/platform predicate for whether mouse tracking is supported (senpi checks
/// `process.stdout.isTTY`, `TERMUX_VERSION`, and Windows Terminal via `WT_SESSION`; this port
/// takes the caller's already-resolved answer since terminal/TTY/env probing is
/// `terminal.rs`'s responsibility, not this renderer's).
pub struct MouseTrackingSupport {
    pub is_tty: bool,
    pub is_termux: bool,
    pub is_windows_without_wt: bool,
}

impl TuiMainScreen {
    pub fn new() -> Self {
        Self {
            base: TuiBase::new(),
            tracking_enabled: false,
            yielded: false,
            clicks: MouseClickSynthesizer::new(),
            mouse_press: None,
            layout_revision: 0,
            committed_mouse_lines: lines_fingerprint(&[]),
            committed_mouse_components: Vec::new(),
            mouse_capture_token: None,
        }
    }

    /// Enables or disables mouse tracking based on the platform/TTY predicate (senpi's
    /// `applyMouseTracking` override). Callers apply this themselves after
    /// [`TuiMainScreen::acquire_mouse_capture`]/[`TuiMainScreen::release_mouse_capture`]/
    /// [`TuiMainScreen::set_mouse_blocker`] report that tracking state changed, since this port
    /// has no virtual dispatch from `TuiBase` back onto its owning renderer.
    pub fn apply_mouse_tracking(
        &mut self,
        enabled: bool,
        support: &MouseTrackingSupport,
        terminal: &mut dyn Terminal,
    ) {
        self.yielded = false;
        let supported = support.is_tty && !support.is_termux && !support.is_windows_without_wt;
        let stopped = self.base.state.borrow().stopped;
        let next = enabled && supported && !stopped;
        self.clicks.cancel();
        self.mouse_press = None;
        if self.tracking_enabled == next {
            return;
        }
        self.tracking_enabled = next;
        terminal.write(if next {
            MouseTracking::INLINE
        } else {
            MouseTracking::DISABLE
        });
    }

    pub fn before_terminal_stop(&mut self, terminal: &mut dyn Terminal) {
        terminal.write(MouseTracking::DISABLE);
        self.tracking_enabled = false;
        self.yielded = false;
        self.clicks.cancel();
        self.mouse_press = None;
    }

    /// Acquires the base's mouse-capture lease and applies tracking itself when this is the
    /// first lease (senpi's `acquireMouseCapture` calling `this.applyMouseTracking` through
    /// virtual dispatch; this port has no virtual dispatch onto `TuiBase`, so the caller-side
    /// wrapper applies tracking after the lease-accounting call returns).
    pub fn acquire_mouse_capture(
        &mut self,
        reason: impl Into<String>,
        support: &MouseTrackingSupport,
        terminal: &mut dyn Terminal,
    ) {
        let (token, should_apply) = self.base.acquire_mouse_capture(reason);
        self.mouse_capture_token = Some(token);
        if should_apply {
            let enabled = self.base.mouse_capture_enabled();
            self.apply_mouse_tracking(enabled, support, terminal);
            self.base.calibrate_mouse_anchor(terminal);
        }
    }

    pub fn release_mouse_capture(
        &mut self,
        support: &MouseTrackingSupport,
        terminal: &mut dyn Terminal,
    ) {
        let Some(token) = self.mouse_capture_token.take() else {
            return;
        };
        if self.base.release_mouse_capture(token) {
            self.apply_mouse_tracking(false, support, terminal);
        }
    }

    pub fn set_mouse_blocker(
        &mut self,
        name: MouseBlocker,
        on: bool,
        support: &MouseTrackingSupport,
        terminal: &mut dyn Terminal,
    ) {
        if self.base.set_mouse_blocker(name, on) {
            let enabled = self.base.mouse_capture_enabled();
            self.apply_mouse_tracking(enabled, support, terminal);
        }
    }

    /// Call after every `TuiBase::do_render` (senpi's `TuiMainScreen.doRender` override):
    /// commits the mouse frame, refreshes the tracked mounted/overlay component set for
    /// click-target staleness detection, and recalibrates the CPR mouse anchor.
    pub fn note_render(&mut self, terminal: &mut dyn Terminal) {
        self.base.note_committed_mouse_frame(terminal);

        fn visit(
            component: &Rc<RefCell<dyn Component>>,
            out: &mut Vec<Rc<RefCell<dyn Component>>>,
        ) {
            out.push(Rc::clone(component));
            let component = component.borrow();
            if let Some(container) = component.as_container() {
                for child in &container.children {
                    visit(child, out);
                }
            }
        }
        let mut components = Vec::new();
        for root in self.base.get_mouse_layout_roots() {
            visit(&root, &mut components);
        }

        let lines_changed = {
            let fingerprint = lines_fingerprint(&self.base.state.borrow().previous_lines);
            let lines_changed = fingerprint != self.committed_mouse_lines;
            self.committed_mouse_lines = fingerprint;
            lines_changed
        };
        let components_changed = components.len() != self.committed_mouse_components.len()
            || components
                .iter()
                .zip(self.committed_mouse_components.iter())
                .any(|(a, b)| !Rc::ptr_eq(a, b));
        if lines_changed || components_changed {
            self.layout_revision += 1;
        }
        self.committed_mouse_components = components;
        self.base.calibrate_mouse_anchor(terminal);
    }

    /// Called before `TuiBase::do_render` when a pending external write requires appending a
    /// fresh working frame instead of overwriting a displaced old one (senpi's
    /// `mouseExternalWritePending` branch at the top of `doRender`).
    pub fn take_mouse_external_write_pending(
        &mut self,
        terminal: &mut dyn Terminal,
    ) -> Option<TuiMainScreenRenderState> {
        let pending = self.base.state.borrow().mouse_external_write_pending;
        if !pending {
            return None;
        }
        self.base.state.borrow_mut().mouse_external_write_pending = false;
        terminal.write("\r\n");
        self.restore_render_state(TuiMainScreenRenderState {
            previous_lines: Vec::new(),
            previous_width: 0,
            previous_height: 0,
            cursor_row: 0,
            hardware_cursor_row: 0,
            max_lines_rendered: 0,
            previous_viewport_top: 0,
        });
        Some(self.capture_render_state())
    }

    fn apply_mouse_result(
        &mut self,
        result: Option<TuiMouseEventResult>,
        focus_target: Option<Rc<RefCell<dyn Component>>>,
        fallback_target: &Rc<RefCell<dyn Component>>,
    ) {
        let Some(result) = result else {
            return;
        };
        if !result.focus {
            return;
        }
        let target = self
            .base
            .resolve_mouse_focus_target(focus_target.as_ref().unwrap_or(fallback_target));
        if let Some(target) = target {
            self.base.set_focus(Some(target));
        }
    }

    /// Registered via `TuiBase::add_input_listener` (senpi's constructor-time
    /// `this.addInputListener((data) => this.handleMouseInput(data))`). `now_ms` is senpi's
    /// implicit `Date.now()` inside `MouseClickSynthesizer` - this port takes it explicitly so
    /// double-click detection stays testable without real timers.
    pub fn handle_mouse_input(
        &mut self,
        data: &str,
        now_ms: i64,
        terminal: &mut dyn Terminal,
    ) -> bool {
        if !is_mouse_sequence(data) {
            let cell_report = data
                .strip_prefix("\x1b[")
                .and_then(|s| s.strip_suffix('t'))
                .is_some_and(|s| {
                    !s.is_empty()
                        && s.split(';').all(|part| {
                            !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit())
                        })
                });
            let report = matches!(data, "\x1b[I" | "\x1b[O") || data.starts_with("\x1b[?")
                || cell_report
                || data.len() > 2
                    && (data.starts_with("\x1b]")
                        || data.starts_with("\x1bP")
                        || data.starts_with("\x1b_"));
            if self.yielded && !report && !crate::keys::is_key_release(data) {
                self.yielded = false;
                if self.base.mouse_capture_enabled() && !self.base.state.borrow().stopped {
                    self.tracking_enabled = true;
                    terminal.write(MouseTracking::INLINE);
                    self.base.request_render(false, 0);
                }
            }
            return false;
        }
        let raw = parse_sgr_mouse_event(data);
        if self.tracking_enabled && raw.as_ref().is_some_and(|raw| raw.button & 64 != 0) {
            terminal.write(MouseTracking::DISABLE);
            self.tracking_enabled = false;
            self.yielded = true;
            self.clicks.cancel();
            self.mouse_press = None;
            self.base.request_render(false, 0);
            return true;
        }
        let Some(raw) = raw else {
            self.clicks.cancel();
            self.mouse_press = None;
            return true;
        };
        if !self.tracking_enabled || raw.button != 0 {
            self.clicks.cancel();
            self.mouse_press = None;
            return true;
        }
        let frame_line = self.base.resolve_frame_line(raw.y + 1, terminal);
        let Some(frame_line) = frame_line else {
            self.clicks.cancel();
            self.mouse_press = None;
            return true;
        };
        if raw.x >= terminal.columns() as i64 {
            self.clicks.cancel();
            self.mouse_press = None;
            return true;
        }
        let event_type = if raw.release {
            TuiMouseEventType::Release
        } else {
            TuiMouseEventType::Press
        };
        let event = to_tui_mouse_event(
            event_type,
            raw,
            terminal.columns() as usize,
            terminal.rows() as usize,
            Default::default(),
        );

        if !raw.release {
            self.handle_mouse_press(raw, event, frame_line, now_ms, terminal);
        } else {
            self.handle_mouse_release(raw, event, now_ms, terminal);
        }
        true
    }

    fn handle_mouse_press(
        &mut self,
        raw: SgrMouseEvent,
        event: TuiMouseEvent,
        frame_line: usize,
        now_ms: i64,
        terminal: &mut dyn Terminal,
    ) {
        let (hit, overlay_result) = self.base.dispatch_mouse_to_overlay(&event);
        let placement_epoch = self.base.state.borrow().placement_epoch;
        let previous_lines_len = self.base.state.borrow().previous_lines.len();

        let result = if let Some(overlay_result) = overlay_result {
            Some(overlay_result)
        } else if hit {
            None
        } else {
            let retargeted = TuiMouseEvent {
                y: frame_line as i64,
                height: previous_lines_len,
                ..event
            };
            self.base.dispatch_self_mouse_event(&retargeted)
        };

        self.mouse_press = None;
        if let Some(result) = result {
            let focus_target = result
                .focus_target
                .clone()
                .unwrap_or_else(|| Rc::clone(&result.target.component));
            let press_key = component_key(&result.target.component);
            self.apply_mouse_result(
                Some(result.result),
                Some(focus_target),
                &result.target.component,
            );
            self.mouse_press = Some(MousePress {
                target: result.target,
                epoch: placement_epoch,
                frame: self.base.state.borrow().previous_lines.clone(),
                components: self.collect_mouse_components(),
                x: raw.x,
                y: raw.y,
            });
            self.clicks.press(raw, press_key, placement_epoch, now_ms);
        } else {
            self.clicks.cancel();
        }
        self.base.request_render(false, 0);
        let _ = terminal;
    }

    fn handle_mouse_release(
        &mut self,
        raw: SgrMouseEvent,
        event: TuiMouseEvent,
        now_ms: i64,
        terminal: &mut dyn Terminal,
    ) {
        let press = self.mouse_press.take();
        let placement_epoch = self.base.state.borrow().placement_epoch;
        let Some(press) = press else {
            self.clicks.cancel();
            return;
        };
        let layout_unchanged = self.press_layout_unchanged(&press);
        if press.epoch != placement_epoch
            || !layout_unchanged
            || press.x != raw.x
            || press.y != raw.y
        {
            self.clicks.cancel();
            return;
        }

        let Some(count) = self.clicks.release(
            raw,
            component_key(&press.target.component),
            placement_epoch,
            now_ms,
        ) else {
            return;
        };
        let click = TuiMouseEvent {
            event_type: TuiMouseEventType::Click,
            click_count: Some(count),
            ..event
        };
        let focus_before_click = self.base.get_focused_component();
        let retargeted = retarget_mouse_event(&click, &press.target);
        let click_result = dispatch_mouse_event(&press.target.component, &retargeted);
        let focus_unchanged = match (&self.base.get_focused_component(), &focus_before_click) {
            (Some(a), Some(b)) => Rc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        if focus_unchanged {
            let focus_target = click_result.as_ref().and_then(|r| r.focus_target.clone());
            self.apply_mouse_result(
                click_result.map(|r| r.result),
                focus_target,
                &press.target.component,
            );
        }
        self.base.request_render(false, 0);
        let _ = terminal;
    }

    fn press_layout_unchanged(&self, press: &MousePress) -> bool {
        let components = self.collect_mouse_components();
        press.frame == self.base.state.borrow().previous_lines
            && components.len() == press.components.len()
            && components
                .iter()
                .zip(&press.components)
                .all(|(a, b)| Rc::ptr_eq(a, b))
    }

    pub fn mouse_capture_state(&self) -> &'static str {
        if self.yielded {
            "yielded"
        } else if self.tracking_enabled {
            "armed"
        } else {
            "off"
        }
    }
    fn collect_mouse_components(&self) -> Vec<Rc<RefCell<dyn Component>>> {
        fn visit(
            component: &Rc<RefCell<dyn Component>>,
            out: &mut Vec<Rc<RefCell<dyn Component>>>,
        ) {
            out.push(Rc::clone(component));
            if let Some(container) = component.borrow().as_container() {
                for child in &container.children {
                    visit(child, out);
                }
            }
        }
        let mut components = Vec::new();
        for root in self.base.get_mouse_layout_roots() {
            visit(&root, &mut components);
        }
        components
    }

    pub fn capture_render_state(&self) -> TuiMainScreenRenderState {
        let state = self.base.state.borrow();
        TuiMainScreenRenderState {
            previous_lines: state.previous_lines.clone(),
            previous_width: state.previous_width,
            previous_height: state.previous_height,
            cursor_row: state.cursor_row,
            hardware_cursor_row: state.hardware_cursor_row,
            max_lines_rendered: state.max_lines_rendered,
            previous_viewport_top: state.previous_viewport_top,
        }
    }

    pub fn restore_render_state(&mut self, restore_state: TuiMainScreenRenderState) {
        let mut state = self.base.state.borrow_mut();
        state.previous_lines = restore_state
            .previous_lines
            .into_iter()
            .map(|line| {
                if is_image_line(&line) {
                    String::new()
                } else {
                    line
                }
            })
            .collect();
        state.previous_kitty_image_ids = Default::default();
        state.previous_width = restore_state.previous_width;
        state.previous_height = restore_state.previous_height;
        state.cursor_row = restore_state.cursor_row;
        state.hardware_cursor_row = restore_state.hardware_cursor_row;
        state.max_lines_rendered = restore_state.max_lines_rendered;
        state.previous_viewport_top = restore_state.previous_viewport_top;
    }
}

impl Default for TuiMainScreen {
    fn default() -> Self {
        Self::new()
    }
}

/// Allocation-free `(len, hash)` fingerprint of a committed frame's lines; `Hash` for a slice of
/// strings covers the length and per-line boundaries, so line splits are distinguished.
fn lines_fingerprint(lines: &[String]) -> (usize, u64) {
    let mut hasher = DefaultHasher::new();
    lines.hash(&mut hasher);
    (lines.len(), hasher.finish())
}

/// Pointer identity for [`MouseClickSynthesizer`]'s target-equality checks.
fn component_key(component: &Rc<RefCell<dyn Component>>) -> usize {
    Rc::as_ptr(component) as *const () as usize
}

#[cfg(test)]
mod tests {
    use super::lines_fingerprint;

    #[test]
    fn release_rejects_changed_frame_even_without_note_render() {
        use super::*;
        struct Target;
        impl Component for Target {
            fn render(&mut self, _: usize) -> Vec<String> {
                vec![]
            }
        }
        let screen = TuiMainScreen::new();
        let target: Rc<RefCell<dyn Component>> = Rc::new(RefCell::new(Target));
        let press = MousePress {
            target: TuiMouseDispatchTarget {
                component: target,
                origin_x: 0,
                origin_y: 0,
                width: 80,
                height: 1,
            },
            epoch: 0,
            frame: vec!["before".into()],
            components: vec![],
            x: 0,
            y: 0,
        };
        screen.base.state.borrow_mut().previous_lines = vec!["after".into()];
        assert!(!screen.press_layout_unchanged(&press));
        screen.base.state.borrow_mut().previous_lines = vec!["before".into()];
        assert!(screen.press_layout_unchanged(&press));
        let mut stale = press;
        stale.components.push(Rc::new(RefCell::new(Target)));
        assert!(!screen.press_layout_unchanged(&stale));
    }

    #[test]
    fn lines_fingerprint_tracks_line_content_and_boundaries() {
        let a = vec!["ab".to_string(), "c".to_string()];
        assert_eq!(lines_fingerprint(&a), lines_fingerprint(&a.clone()));
        assert_ne!(
            lines_fingerprint(&a),
            lines_fingerprint(&["a".to_string(), "bc".to_string()])
        );
        assert_ne!(
            lines_fingerprint(&a),
            lines_fingerprint(&["ab".to_string(), "d".to_string()])
        );
        assert_ne!(
            lines_fingerprint(&a),
            lines_fingerprint(&["abc".to_string()])
        );
        assert_ne!(lines_fingerprint(&[]), lines_fingerprint(&[String::new()]));
    }
}
