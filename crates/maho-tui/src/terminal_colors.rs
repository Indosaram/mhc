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
pub enum TerminalColorScheme {
    Dark,
    Light,
}

static OSC11_BACKGROUND_COLOR_RESPONSE_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^\x1b\]11;([^\x07\x1b]*)(?:\x07|\x1b\\)$").expect("valid regex")
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
}
