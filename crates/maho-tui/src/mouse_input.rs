//! Port of senpi `packages/tui/src/mouse-input.ts`.

use crate::tui::{TuiMouseButton, TuiMouseEvent, TuiMouseEventType};

#[derive(Debug, Clone, Copy)]
pub struct SgrMouseEvent {
    pub button: i64,
    pub x: i64,
    pub y: i64,
    pub release: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct WheelEvent {
    pub direction: i64,
    pub x: i64,
    pub y: i64,
    pub button: i64,
}

pub struct MouseTracking;

impl MouseTracking {
    pub const BUTTON_MOTION: &'static str = "\x1b[?1000h\x1b[?1002h\x1b[?1004h\x1b[?1006h";
    pub const ALL_MOTION: &'static str = "\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1004h\x1b[?1006h";
    pub const INLINE: &'static str = "\x1b[?1006h\x1b[?1000h";
    pub const DISABLE: &'static str = "\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l";
}

/// Decode SGR's one-based wire coordinates into zero-based cells.
pub fn parse_sgr_mouse_event(data: &str) -> Option<SgrMouseEvent> {
    let rest = data.strip_prefix("\x1b[<")?;
    let (body, terminator) = if let Some(body) = rest.strip_suffix('M') {
        (body, false)
    } else if let Some(body) = rest.strip_suffix('m') {
        (body, true)
    } else {
        return None;
    };
    let mut parts = body.split(';');
    let button_str = parts.next()?;
    let x_str = parts.next()?;
    let y_str = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    if button_str.is_empty() || !button_str.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if x_str.is_empty() || !x_str.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if y_str.is_empty() || !y_str.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let button: i64 = button_str.parse().ok()?;
    let x: i64 = x_str.parse::<i64>().ok()?.checked_sub(1)?;
    let y: i64 = y_str.parse::<i64>().ok()?.checked_sub(1)?;
    if button > 255 || x < 0 || y < 0 {
        return None;
    }
    Some(SgrMouseEvent { button, x, y, release: terminator })
}

pub fn parse_wheel_event(data: &str) -> Option<WheelEvent> {
    let raw = parse_sgr_mouse_event(data).or_else(|| {
        if data.len() == 6 && data.starts_with("\x1b[M") {
            let bytes = data.as_bytes();
            Some(SgrMouseEvent {
                button: bytes[3] as i64 - 32,
                x: bytes[4] as i64 - 33,
                y: bytes[5] as i64 - 33,
                release: false,
            })
        } else {
            None
        }
    })?;
    if raw.button < 0 || raw.button > 255 || (raw.button & 64) == 0 {
        return None;
    }
    let direction = raw.button & 3;
    if direction != 0 && direction != 1 {
        return None;
    }
    Some(WheelEvent {
        direction: if direction == 0 { -1 } else { 1 },
        x: raw.x,
        y: raw.y,
        button: raw.button,
    })
}

pub fn is_mouse_sequence(data: &str) -> bool {
    data.starts_with("\x1b[<") || data.starts_with("\x1b[M")
}

pub fn decode_mouse_button(button: i64) -> TuiMouseButton {
    match button & 3 {
        0 => TuiMouseButton::Left,
        1 => TuiMouseButton::Middle,
        2 => TuiMouseButton::Right,
        _ => TuiMouseButton::None,
    }
}

#[derive(Default)]
pub struct ToTuiMouseEventExtra {
    pub wheel_delta: Option<i64>,
    pub click_count: Option<u32>,
}

pub fn to_tui_mouse_event(
    event_type: TuiMouseEventType,
    raw: SgrMouseEvent,
    columns: usize,
    rows: usize,
    extra: ToTuiMouseEventExtra,
) -> TuiMouseEvent {
    TuiMouseEvent {
        event_type,
        button: if matches!(event_type, TuiMouseEventType::Wheel) {
            TuiMouseButton::None
        } else {
            decode_mouse_button(raw.button)
        },
        x: raw.x,
        y: raw.y,
        screen_x: raw.x,
        screen_y: raw.y,
        width: columns.max(1),
        height: rows.max(1),
        shift: (raw.button & 4) != 0,
        alt: (raw.button & 8) != 0,
        ctrl: (raw.button & 16) != 0,
        wheel_delta: extra.wheel_delta,
        click_count: extra.click_count,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ClickPoint {
    x: i64,
    y: i64,
    target: usize,
    epoch: u64,
    time_ms: i64,
}

#[derive(Debug, Clone, Copy)]
struct LastClick {
    point: ClickPoint,
    count: u32,
}

/// Gesture state uses explicit timestamps so callers/tests need no timers.
pub struct MouseClickSynthesizer {
    pressed: Option<ClickPoint>,
    last: Option<LastClick>,
}

impl MouseClickSynthesizer {
    pub fn new() -> Self {
        Self { pressed: None, last: None }
    }

    pub fn press(&mut self, raw: SgrMouseEvent, target: usize, epoch: u64, now_ms: i64) {
        self.pressed = Some(ClickPoint { x: raw.x, y: raw.y, target, epoch, time_ms: now_ms });
    }

    pub fn release(&mut self, raw: SgrMouseEvent, target: usize, epoch: u64, now_ms: i64) -> Option<u32> {
        let press = self.pressed.take()?;
        if press.x != raw.x
            || press.y != raw.y
            || press.target != target
            || press.epoch != epoch
            || now_ms - press.time_ms > 500
            || now_ms < press.time_ms
        {
            self.last = None;
            return None;
        }
        let count = match self.last {
            Some(last)
                if last.point.x == raw.x
                    && last.point.y == raw.y
                    && last.point.target == target
                    && last.point.epoch == epoch
                    && now_ms - last.point.time_ms <= 500 =>
            {
                (last.count % 3) + 1
            }
            _ => 1,
        };
        self.last = Some(LastClick { point: ClickPoint { time_ms: now_ms, ..press }, count });
        Some(count)
    }

    pub fn cancel(&mut self) {
        self.pressed = None;
        self.last = None;
    }
}

impl Default for MouseClickSynthesizer {
    fn default() -> Self {
        Self::new()
    }
}
