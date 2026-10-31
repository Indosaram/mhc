//! Port of senpi `packages/tui/src/terminal-colors.ts`.

use std::sync::LazyLock;

use regex::Regex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RgbColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OscColorTarget {
    Foreground,
    Background,
    Palette(u16),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OscColorResponse {
    pub target: OscColorTarget,
    pub rgb: Option<RgbColor>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TerminalColors {
    pub foreground: Option<RgbColor>,
    pub background: Option<RgbColor>,
    pub palette: Vec<Option<RgbColor>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalColorScheme {
    Dark,
    Light,
}

static OSC11_BACKGROUND_COLOR_RESPONSE_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^\x1b\]11;([^\x07\x1b]*)(?:\x07|\x1b\\)$").expect("valid regex")
});
static OSC_COLOR_RESPONSE_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^\x1b\](?:(1[01])|4;(\d{1,3}));([^\x07\x1b]*)(?:\x07|\x1b\\)$")
        .expect("valid regex")
});
static COLOR_SCHEME_REPORT_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:\x1b\[\?997;(1|2)n)+$").expect("valid regex"));
static HEX6: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^[0-9a-f]{6}$").expect("valid regex"));
static HEX12: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^[0-9a-f]{12}$").expect("valid regex"));
static HEX_CHANNEL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^[0-9a-f]+$").expect("valid regex"));
static RGB_PREFIX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^rgba?:").expect("valid regex"));

fn hex_to_rgb(hex: &str) -> RgbColor {
    let normalized = hex.strip_prefix('#').unwrap_or(hex);
    let channel =
        |range: std::ops::Range<usize>| u8::from_str_radix(&normalized[range], 16).unwrap_or(0);
    RgbColor {
        r: channel(0..2),
        g: channel(2..4),
        b: channel(4..6),
    }
}

/// Scales an N-digit hex channel to 0..=255 (`Math.round(value / (16^N - 1) * 255)`).
fn parse_osc_hex_channel(channel: &str) -> Option<u8> {
    if !HEX_CHANNEL.is_match(channel) {
        return None;
    }
    let max = 16f64.powi(i32::try_from(channel.len()).ok()?) - 1.0;
    if max <= 0.0 {
        return None;
    }
    let value = channel.chars().fold(0f64, |acc, c| {
        acc * 16.0 + f64::from(c.to_digit(16).unwrap_or(0))
    });
    let scaled = (value / max * 255.0).round();
    // The ratio is within [0, 1], so the rounded value fits in u8.
    Some(scaled.clamp(0.0, 255.0) as u8)
}

pub fn is_osc11_background_color_response(data: &str) -> bool {
    OSC11_BACKGROUND_COLOR_RESPONSE_PATTERN.is_match(data)
}

pub fn parse_osc11_background_color(data: &str) -> Option<RgbColor> {
    let captures = OSC11_BACKGROUND_COLOR_RESPONSE_PATTERN.captures(data)?;
    let value = captures.get(1).map_or("", |m| m.as_str()).trim();
    if let Some(hex) = value.strip_prefix('#') {
        if HEX6.is_match(hex) {
            return Some(hex_to_rgb(value));
        }
        if HEX12.is_match(hex) {
            return Some(RgbColor {
                r: parse_osc_hex_channel(&hex[0..4])?,
                g: parse_osc_hex_channel(&hex[4..8])?,
                b: parse_osc_hex_channel(&hex[8..12])?,
            });
        }
        return None;
    }
    let rgb_value = RGB_PREFIX.replace(value, "");
    let mut parts = rgb_value.split('/');
    let (red, green, blue) = (parts.next()?, parts.next()?, parts.next()?);
    Some(RgbColor {
        r: parse_osc_hex_channel(red)?,
        g: parse_osc_hex_channel(green)?,
        b: parse_osc_hex_channel(blue)?,
    })
}

pub fn parse_osc_color_response(data: &str) -> Option<OscColorResponse> {
    let captures = OSC_COLOR_RESPONSE_PATTERN.captures(data)?;
    let target = match captures.get(1).map(|m| m.as_str()) {
        Some("10") => OscColorTarget::Foreground,
        Some("11") => OscColorTarget::Background,
        _ => OscColorTarget::Palette(captures.get(2)?.as_str().parse().ok()?),
    };
    let rgb = parse_osc_color_value(captures.get(3)?.as_str());
    Some(OscColorResponse { target, rgb })
}

/// senpi \`parseOscColorValue\`: a value that does not parse yields \`None\`, never a rejected reply.
fn parse_osc_color_value(raw_value: &str) -> Option<RgbColor> {
    let value = raw_value.trim();
    if let Some(hex) = value.strip_prefix('#') {
        if HEX6.is_match(hex) {
            return Some(hex_to_rgb(value));
        }
        if HEX12.is_match(hex) {
            return Some(RgbColor {
                r: parse_osc_hex_channel(&hex[0..4])?,
                g: parse_osc_hex_channel(&hex[4..8])?,
                b: parse_osc_hex_channel(&hex[8..12])?,
            });
        }
        return None;
    }
    let rgb_value = RGB_PREFIX.replace(value, "");
    let mut parts = rgb_value.split('/');
    let (r, g, b) = (parts.next()?, parts.next()?, parts.next()?);
    Some(RgbColor {
        r: parse_osc_hex_channel(r)?,
        g: parse_osc_hex_channel(g)?,
        b: parse_osc_hex_channel(b)?,
    })
}

pub fn is_terminal_color_da1(data: &str) -> bool {
    Regex::new(r"^\x1b\[\?[\d;]*c$").is_ok_and(|pattern| pattern.is_match(data))
}

pub fn parse_terminal_color_scheme_report(data: &str) -> Option<TerminalColorScheme> {
    let captures = COLOR_SCHEME_REPORT_PATTERN.captures(data)?;
    // Like JS `match`, the capture holds the last repetition.
    Some(if captures.get(1).map(|m| m.as_str()) == Some("2") {
        TerminalColorScheme::Light
    } else {
        TerminalColorScheme::Dark
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn rgb(r: u8, g: u8, b: u8) -> RgbColor {
        RgbColor { r, g, b }
    }

    #[test]
    fn parses_16_bit_osc_11_rgb_responses() {
        assert_eq!(
            parse_osc11_background_color("\x1b]11;rgb:0000/8000/ffff\x07"),
            Some(rgb(0, 128, 255))
        );
    }

    #[test]
    fn parses_osc_11_hex_responses() {
        assert_eq!(
            parse_osc11_background_color("\x1b]11;#ffffff\x1b\\"),
            Some(rgb(255, 255, 255))
        );
        assert_eq!(
            parse_osc11_background_color("\x1b]11;#000000\x07"),
            Some(rgb(0, 0, 0))
        );
    }

    #[test]
    fn rejects_non_strict_osc_11_responses() {
        assert_eq!(parse_osc11_background_color("x\x1b]11;#ffffff\x07"), None);
        assert_eq!(parse_osc11_background_color("\x1b]10;#ffffff\x07"), None);
        assert_eq!(parse_osc11_background_color("\x1b]11;#ffffff\x07x"), None);
    }

    #[test]
    fn parses_color_scheme_reports() {
        use TerminalColorScheme::{Dark, Light};
        assert_eq!(
            parse_terminal_color_scheme_report("\x1b[?997;1n"),
            Some(Dark)
        );
        assert_eq!(
            parse_terminal_color_scheme_report("\x1b[?997;2n"),
            Some(Light)
        );
        assert_eq!(
            parse_terminal_color_scheme_report("\x1b[?997;2n\x1b[?997;1n\x1b[?997;1n"),
            Some(Dark)
        );
        assert_eq!(
            parse_terminal_color_scheme_report("\x1b[?997;1n\x1b[?997;2n\x1b[?997;2n"),
            Some(Light)
        );
        assert_eq!(parse_terminal_color_scheme_report("\x1b[?997;3n"), None);
        assert_eq!(parse_terminal_color_scheme_report("\x1b[?996n"), None);
        assert_eq!(parse_terminal_color_scheme_report("x\x1b[?997;1n"), None);
    }

    #[test]
    fn recognizes_osc_11_background_color_responses() {
        assert!(is_osc11_background_color_response(
            "\x1b]11;rgb:0000/8000/ffff\x07"
        ));
        assert!(!is_osc11_background_color_response("\x1b]10;#ffffff\x07"));
    }

    #[test]
    fn parses_12_digit_hex_and_rgba_forms() {
        assert_eq!(
            parse_osc11_background_color("\x1b]11;#ffff00008000\x07"),
            Some(rgb(255, 0, 128))
        );
        assert_eq!(
            parse_osc11_background_color("\x1b]11;rgba:ff/00/80/ff\x07"),
            Some(rgb(255, 0, 128))
        );
        assert_eq!(parse_osc11_background_color("\x1b]11;#fff\x07"), None);
        assert_eq!(
            parse_osc11_background_color("\x1b]11;rgb:zz/00/00\x07"),
            None
        );
    }

    #[test]
    fn parses_osc_foreground_background_and_palette_replies() {
        assert_eq!(
            parse_osc_color_response("\x1b]10;#123456\x07"),
            Some(OscColorResponse {
                target: OscColorTarget::Foreground,
                rgb: Some(rgb(0x12, 0x34, 0x56))
            })
        );
        assert_eq!(
            parse_osc_color_response("\x1b]11;rgb:ffff/0000/8000\x1b\\"),
            Some(OscColorResponse {
                target: OscColorTarget::Background,
                rgb: Some(rgb(255, 0, 128))
            })
        );
        assert_eq!(
            parse_osc_color_response("\x1b]4;256;#abcdef\x07"),
            Some(OscColorResponse {
                target: OscColorTarget::Palette(256),
                rgb: Some(rgb(0xab, 0xcd, 0xef))
            })
        );
        assert_eq!(
            parse_osc_color_response("\x1b]4;1;rgb:ff/00/00/ff\x07"),
            Some(OscColorResponse {
                target: OscColorTarget::Palette(1),
                rgb: Some(rgb(255, 0, 0))
            })
        );
    }

    #[test]
    fn parses_default_and_palette_color_replies() {
        assert_eq!(
            parse_osc_color_response("\x1b]10;#123456\x07"),
            Some(OscColorResponse {
                target: OscColorTarget::Foreground,
                rgb: Some(rgb(0x12, 0x34, 0x56))
            })
        );
        assert_eq!(
            parse_osc_color_response("\x1b]11;rgb:ffff/0000/8000\x1b\\"),
            Some(OscColorResponse {
                target: OscColorTarget::Background,
                rgb: Some(rgb(255, 0, 128))
            })
        );
        assert_eq!(
            parse_osc_color_response("\x1b]4;15;#abcdef\x07"),
            Some(OscColorResponse {
                target: OscColorTarget::Palette(15),
                rgb: Some(rgb(0xab, 0xcd, 0xef))
            })
        );
        assert!(is_terminal_color_da1("\x1b[?1;2c"));
    }
}

#[cfg(test)]
mod query_terminal_colors_tests {
    //! Port of the `TUI.queryTerminalColors` cases of senpi `terminal-colors.test.ts`, driven
    //! through `TuiBase` with an explicit clock instead of real timers.

    use std::cell::RefCell;
    use std::rc::Rc;

    use super::{RgbColor, TerminalColors};
    use crate::terminal::{InputHandler, ResizeHandler, Terminal, TerminalError};
    use crate::tui::{Component, TuiBase};

    #[derive(Default)]
    struct TestTerminal {
        writes: Vec<String>,
    }

    impl Terminal for TestTerminal {
        fn start(&mut self, _on_input: InputHandler, _on_resize: ResizeHandler) {}
        fn stop(&mut self) -> Result<(), TerminalError> {
            Ok(())
        }
        fn drain_input(&mut self, _max_ms: u64, _idle_ms: u64) {}
        fn write(&mut self, data: &str) {
            self.writes.push(data.to_string());
        }
        fn columns(&self) -> u16 {
            80
        }
        fn rows(&self) -> u16 {
            24
        }
        fn kitty_protocol_active(&self) -> bool {
            false
        }
        fn move_by(&mut self, _lines: i64) {}
        fn hide_cursor(&mut self) {}
        fn show_cursor(&mut self) {}
        fn clear_line(&mut self) {}
        fn clear_from_cursor(&mut self) {}
        fn clear_screen(&mut self) {}
        fn set_title(&mut self, _title: &str) {}
        fn set_progress(&mut self, _active: bool) {}
    }

    struct InputRecorder(Rc<RefCell<Vec<String>>>);

    impl Component for InputRecorder {
        fn render(&mut self, _width: usize) -> Vec<String> {
            Vec::new()
        }
        fn handle_input(&mut self, data: &str) {
            self.0.borrow_mut().push(data.to_string());
        }
        fn has_input_handler(&self) -> bool {
            true
        }
    }

    const DA1: &str = "\x1b[?62;22c";
    const BLACK: RgbColor = RgbColor { r: 0, g: 0, b: 0 };
    const WHITE: RgbColor = RgbColor {
        r: 255,
        g: 255,
        b: 255,
    };

    fn palette_replies() -> Vec<String> {
        (0..16)
            .map(|index| format!("\x1b]4;{index};#000000\x07"))
            .collect()
    }

    fn setup() -> (TuiBase, TestTerminal, Rc<RefCell<Vec<String>>>) {
        let mut base = TuiBase::new();
        let inputs = Rc::new(RefCell::new(Vec::new()));
        let component: Rc<RefCell<dyn Component>> =
            Rc::new(RefCell::new(InputRecorder(Rc::clone(&inputs))));
        base.add_child(Rc::clone(&component));
        base.set_focus(Some(component));
        (base, TestTerminal::default(), inputs)
    }

    fn send(base: &mut TuiBase, data: &str) {
        base.handle_terminal_input(data, false);
    }

    #[test]
    fn queries_all_colors_in_one_write_and_consumes_the_replies() {
        let (mut base, mut terminal, inputs) = setup();
        let query = base.query_terminal_colors(&mut terminal, 1000, 0);
        let written = terminal.writes.last().cloned().unwrap_or_default();
        assert!(
            written.starts_with("\x1b]10;?\x07\x1b]11;?\x07\x1b]4;0;?\x07")
                && written.ends_with("\x1b[c")
        );

        send(&mut base, "x");
        send(&mut base, "\x1b]10;#ffffff\x07");
        send(&mut base, "\x1b]11;rgb:0000/0000/0000\x1b\\");
        for reply in palette_replies() {
            send(&mut base, &reply);
        }
        // Resolves once every reply arrived, without waiting for DA1.
        assert_eq!(
            query.result(),
            Some(TerminalColors {
                foreground: Some(WHITE),
                background: Some(BLACK),
                palette: vec![Some(BLACK); 16],
            })
        );
        send(&mut base, DA1);
        assert_eq!(*inputs.borrow(), ["x"]);
    }

    #[test]
    fn resolves_on_da1_with_the_replies_that_arrived_in_query_order() {
        let (mut base, mut terminal, _inputs) = setup();
        let first = base.query_terminal_colors(&mut terminal, 1000, 0);
        let second = base.query_terminal_colors(&mut terminal, 1000, 0);
        send(&mut base, "\x1b]11;#000000\x07");
        // An incomplete palette is dropped.
        for reply in &palette_replies()[..8] {
            send(&mut base, reply);
        }
        send(&mut base, DA1);
        send(&mut base, DA1);

        assert_eq!(
            first.result(),
            Some(TerminalColors {
                foreground: None,
                background: Some(BLACK),
                palette: Vec::new(),
            })
        );
        assert_eq!(second.result(), Some(TerminalColors::default()));
    }

    #[test]
    fn reports_late_replies_after_a_timeout_and_consumes_them_until_da1() {
        let (mut base, mut terminal, inputs) = setup();
        let late = Rc::new(RefCell::new(Vec::new()));
        let sink = Rc::clone(&late);
        let query = base.query_terminal_colors_with_late_reply(
            &mut terminal,
            1,
            0,
            Some(Box::new(move |colors| sink.borrow_mut().push(colors))),
        );
        base.poll_terminal_queries(5);
        assert_eq!(query.result().and_then(|colors| colors.background), None);
        assert!(query.result().is_some());

        send(&mut base, "\x1b]11;#ffffff\x07");
        send(&mut base, DA1);
        assert_eq!(
            *late.borrow(),
            [TerminalColors {
                foreground: None,
                background: Some(WHITE),
                palette: Vec::new(),
            }]
        );
        assert!(inputs.borrow().is_empty());

        // With no query pending, color replies are ordinary input again.
        send(&mut base, "\x1b]11;#ffffff\x07");
        assert_eq!(*inputs.borrow(), ["\x1b]11;#ffffff\x07"]);
    }
}
