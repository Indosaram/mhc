//! Port of senpi `packages/tui/test/mouse-input.test.ts`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use maho_tui::mouse_input::{
    decode_mouse_button, is_mouse_sequence, parse_sgr_mouse_event, parse_wheel_event, to_tui_mouse_event,
    MouseClickSynthesizer, MouseTracking, SgrMouseEvent, ToTuiMouseEventExtra,
};
use maho_tui::stdin_buffer::{Clock, StdinBuffer, StdinBufferOptions, StdinEvent, StdinInput};
use maho_tui::tui::{TuiMouseButton, TuiMouseEventType};

#[derive(Default)]
struct ManualClock(AtomicU64);

impl Clock for ManualClock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

impl ManualClock {
    fn advance(&self, ms: u64) {
        self.0.fetch_add(ms, Ordering::SeqCst);
    }
}

fn new_buffer(clock: &Arc<ManualClock>) -> StdinBuffer {
    StdinBuffer::with_clock(
        StdinBufferOptions::default(),
        Arc::clone(clock) as Arc<dyn Clock>,
    )
}

fn data_events(events: Vec<StdinEvent>) -> Vec<String> {
    events
        .into_iter()
        .filter_map(|event| match event {
            StdinEvent::Data(data) => Some(data),
            StdinEvent::Paste(_) => None,
        })
        .collect()
}

fn assert_parses(sequence: &str, button: i64, x: i64, y: i64, release: bool) {
    let event = parse_sgr_mouse_event(sequence).unwrap_or_else(|| panic!("{sequence} did not parse"));
    assert_eq!(event.button, button, "{sequence} button");
    assert_eq!(event.x, x, "{sequence} x");
    assert_eq!(event.y, y, "{sequence} y");
    assert_eq!(event.release, release, "{sequence} release");
}

#[test]
fn parses_sgr_mouse_press() {
    assert_parses("\x1b[<0;10;5M", 0, 9, 4, false);
}

#[test]
fn parses_sgr_mouse_release() {
    assert_parses("\x1b[<0;10;5m", 0, 9, 4, true);
}

#[test]
fn parses_sgr_wheel_up_sequence() {
    assert_parses("\x1b[<64;3;3M", 64, 2, 2, false);
}

#[test]
fn parses_sgr_modifier_button_sequence() {
    assert_parses("\x1b[<35;20;5M", 35, 19, 4, false);
}

#[test]
fn rejects_out_of_range_protocol_values() {
    for sequence in ["\x1b[<256;1;1M", "\x1b[<0;0;1M", "text"] {
        assert!(
            parse_sgr_mouse_event(sequence).is_none(),
            "{sequence} should not parse"
        );
    }
}

#[test]
fn decodes_buttons_and_modifiers() {
    let buttons: Vec<TuiMouseButton> = [0, 1, 2, 3].iter().map(|bit| decode_mouse_button(*bit)).collect();
    assert_eq!(
        buttons,
        vec![
            TuiMouseButton::Left,
            TuiMouseButton::Middle,
            TuiMouseButton::Right,
            TuiMouseButton::None
        ]
    );
    for bit in [4, 8, 16] {
        let event = to_tui_mouse_event(
            TuiMouseEventType::Press,
            SgrMouseEvent {
                button: bit,
                x: 9,
                y: 4,
                release: false,
            },
            80,
            24,
            ToTuiMouseEventExtra::default(),
        );
        assert_eq!(event.shift, bit == 4, "shift for {bit}");
        assert_eq!(event.alt, bit == 8, "alt for {bit}");
        assert_eq!(event.ctrl, bit == 16, "ctrl for {bit}");
    }
}

#[test]
fn recognizes_and_parses_wheel_protocols() {
    let up = parse_wheel_event("\x1b[<64;3;3M").expect("wheel up");
    assert_eq!((up.direction, up.x, up.y, up.button), (-1, 2, 2, 64));
    let down = parse_wheel_event("\x1b[<65;3;3M").expect("wheel down");
    assert_eq!((down.direction, down.x, down.y, down.button), (1, 2, 2, 65));
    assert!(is_mouse_sequence("\x1b[<0;1;1M"));
    assert!(is_mouse_sequence("\x1b[M !!"));
    assert!(!is_mouse_sequence("a"));
}

#[test]
fn reassembles_split_reads_before_keyboard_handling() {
    let clock = Arc::new(ManualClock::default());
    let mut buffer = new_buffer(&clock);
    let mut seen: Vec<String> = Vec::new();
    seen.extend(data_events(buffer.process(StdinInput::Text("\x1b[<0;1"))));
    assert!(seen.is_empty(), "partial fragment must not be emitted: {seen:?}");
    seen.extend(data_events(buffer.process(StdinInput::Text("0;5M"))));
    assert_eq!(seen, vec!["\x1b[<0;10;5M"]);
    assert_parses(&seen[0], 0, 9, 4, false);
    buffer.destroy();
}

#[test]
fn keeps_owned_fragments_out_of_keyboard_handling_after_timeout_flush() {
    let clock = Arc::new(ManualClock::default());
    let mut buffer = new_buffer(&clock);
    let mut seen: Vec<String> = Vec::new();
    seen.extend(data_events(buffer.process(StdinInput::Text("\x1b[<0;1"))));
    assert!(buffer.flush().is_empty(), "flush must not release an owned fragment");
    seen.extend(data_events(buffer.process(StdinInput::Text("0;5M"))));
    assert_eq!(seen, vec!["\x1b[<0;10;5M"]);
    buffer.destroy();
}

#[test]
fn bounds_owned_fragment_buffering_and_discards_late_tails() {
    let clock = Arc::new(ManualClock::default());
    let mut buffer = new_buffer(&clock);
    let mut seen: Vec<String> = Vec::new();
    seen.extend(data_events(buffer.process(StdinInput::Text("\x1b[<0;1"))));
    clock.advance(750);
    data_events(buffer.fire_timer());
    assert_eq!(buffer.get_buffer(), "");
    assert!(seen.is_empty(), "expired fragment must not be emitted: {seen:?}");

    seen.extend(data_events(buffer.process(StdinInput::Text("0;5Ma"))));
    assert_eq!(seen, vec!["a"]);

    let long = format!("\x1b[<{}", "1".repeat(65));
    seen.extend(data_events(buffer.process(StdinInput::Text(&long))));
    assert_eq!(buffer.get_buffer(), "");

    seen.extend(data_events(buffer.process(StdinInput::Text(";1;1Mb"))));
    assert_eq!(seen, vec!["a", "b"]);
    buffer.destroy();
}

#[test]
fn resynchronizes_at_a_new_escape_without_leaking_its_protocol_tail() {
    let clock = Arc::new(ManualClock::default());
    let mut buffer = new_buffer(&clock);
    let long = format!("\x1b[<{}", "1".repeat(65));
    data_events(buffer.process(StdinInput::Text(&long)));
    let seen = data_events(buffer.process(StdinInput::Text("\x1b[<0;10;5M")));
    assert_eq!(seen, vec!["\x1b[<0;10;5M"]);
    buffer.destroy();
}

#[test]
fn synthesizes_click_chains_with_explicit_time() {
    let mut synthesizer = MouseClickSynthesizer::new();
    let target = 7usize;
    let raw = SgrMouseEvent {
        button: 0,
        x: 4,
        y: 2,
        release: false,
    };
    for i in 0..4i64 {
        synthesizer.press(raw, target, 1, 100 * i);
        let released = synthesizer.release(
            SgrMouseEvent { release: true, ..raw },
            target,
            1,
            100 * i + 1,
        );
        assert_eq!(released, Some(((i % 3) + 1) as u32), "click {i}");
    }
    synthesizer.press(raw, target, 1, 1000);
    assert_eq!(
        synthesizer.release(SgrMouseEvent { release: true, ..raw }, target, 1, 1001),
        Some(1)
    );
}

#[test]
fn rejects_stale_moved_cancelled_and_retargeted_gestures() {
    let mut synthesizer = MouseClickSynthesizer::new();
    let target = 7usize;
    let raw = SgrMouseEvent {
        button: 0,
        x: 4,
        y: 2,
        release: false,
    };
    let cases = [
        (SgrMouseEvent { x: 5, ..raw }, target, 1u64, 1i64),
        (raw, 8usize, 1, 1),
        (raw, target, 2, 1),
        (raw, target, 1, 501),
    ];
    for (release, identity, epoch, time) in cases {
        synthesizer.press(raw, target, 1, 0);
        assert_eq!(
            synthesizer.release(release, identity, epoch, time),
            None,
            "gesture {release:?} {identity} {epoch} {time} must be rejected"
        );
    }
    synthesizer.press(raw, target, 1, 0);
    synthesizer.cancel();
    assert_eq!(synthesizer.release(raw, target, 1, 1), None);
}

#[test]
fn mouse_tracking_sequences_match_senpi() {
    assert_eq!(
        MouseTracking::ALL_MOTION,
        "\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1004h\x1b[?1006h"
    );
    assert_eq!(MouseTracking::BUTTON_MOTION, "\x1b[?1000h\x1b[?1002h\x1b[?1004h\x1b[?1006h");
    assert_eq!(MouseTracking::DISABLE, "\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l");
}
