//! Port of senpi `packages/tui/src/colors.ts`.

use std::sync::LazyLock;

use regex::Regex;

use crate::oklab::{
    linear_srgb_to_rgb, okhsl_to_rgb, oklab_to_linear_srgb, rgb_to_okhsl, rgb_to_oklab,
};
use crate::terminal_colors::RgbColor;

/// A concrete color. Every color can be converted to sRGB, so color math never fails.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Color {
    Indexed { index: u8 },
    Rgb { r: f64, g: f64, b: f64 },
    Oklch { l: f64, c: f64, h: f64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalColorMode {
    Color256,
    Truecolor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorMixSpace {
    #[default]
    Oklch,
    Srgb,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OklchChannels {
    pub l: f64,
    pub c: f64,
    pub h: f64,
}

/// OKHSL channels: hue in degrees, saturation and lightness 0-1. Saturation is relative to the most the
/// sRGB gamut allows at that hue and lightness, so every value is in gamut.
pub type OkhslChannels = crate::oklab::Okhsl;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TextAttributes {
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
    pub strikethrough: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct TextStyle {
    pub attributes: TextAttributes,
    pub fg: Option<Color>,
    pub bg: Option<Color>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColorError(pub String);

impl std::fmt::Display for ColorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ColorError {}

fn require_finite(value: f64, name: &str) -> Result<(), ColorError> {
    if !value.is_finite() {
        return Err(ColorError(format!("{name} must be finite")));
    }
    Ok(())
}

fn fmt_num(value: f64) -> String {
    // JS number formatting for the error messages.
    if value.fract() == 0.0 && value.abs() < 1e21 {
        format!("{}", value as i64)
    } else {
        format!("{value}")
    }
}

pub fn indexed_color(index: i64) -> Result<Color, ColorError> {
    if !(0..=255).contains(&index) {
        return Err(ColorError(format!(
            "ANSI color index must be an integer from 0 to 255: {index}"
        )));
    }
    Ok(Color::Indexed { index: index as u8 })
}

pub fn rgb_color(r: f64, g: f64, b: f64) -> Result<Color, ColorError> {
    for (name, value) in [("r", r), ("g", g), ("b", b)] {
        require_finite(value, name)?;
        if !(0.0..=255.0).contains(&value) {
            return Err(ColorError(format!(
                "{name} must be between 0 and 255: {}",
                fmt_num(value)
            )));
        }
    }
    Ok(Color::Rgb { r, g, b })
}

pub fn oklch_color(l: f64, c: f64, h: f64) -> Result<Color, ColorError> {
    require_finite(l, "l")?;
    require_finite(c, "c")?;
    require_finite(h, "h")?;
    if !(0.0..=1.0).contains(&l) {
        return Err(ColorError(format!(
            "l must be between 0 and 1: {}",
            fmt_num(l)
        )));
    }
    if c < 0.0 {
        return Err(ColorError(format!(
            "c must not be negative: {}",
            fmt_num(c)
        )));
    }
    Ok(Color::Oklch {
        l,
        c,
        h: ((h % 360.0) + 360.0) % 360.0,
    })
}

const NUMBER_PATTERN: &str = r"[+-]?(?:\d+(?:\.\d*)?|\.\d+)(?:e[+-]?\d+)?";
static OKLCH_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?i)^oklch\(\s*({NUMBER_PATTERN})(%)?\s+({NUMBER_PATTERN})\s+({NUMBER_PATTERN})(?:deg)?\s*\)$"
    ))
    .expect("valid regex")
});
static OKHSL_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?i)^okhsl\(\s*({NUMBER_PATTERN})(?:deg)?\s+({NUMBER_PATTERN})(%)?\s+({NUMBER_PATTERN})(%)?\s*\)$"
    ))
    .expect("valid regex")
});
static HEX_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^#([\da-f]{3}|[\da-f]{6})$").expect("valid regex"));

/// An OKHSL color, converted to sRGB. Saturation is relative to the sRGB gamut at the hue and lightness,
/// so equal saturation looks equally colorful across hues and lightness.
pub fn okhsl_color(h: f64, s: f64, l: f64) -> Result<Color, ColorError> {
    require_finite(h, "h")?;
    require_finite(s, "s")?;
    require_finite(l, "l")?;
    if !(0.0..=1.0).contains(&s) {
        return Err(ColorError(format!(
            "s must be between 0 and 1: {}",
            fmt_num(s)
        )));
    }
    if !(0.0..=1.0).contains(&l) {
        return Err(ColorError(format!(
            "l must be between 0 and 1: {}",
            fmt_num(l)
        )));
    }
    let RgbColor { r, g, b } = okhsl_to_rgb(h, s, l);
    rgb_color(f64::from(r), f64::from(g), f64::from(b))
}

pub fn color_to_okhsl(color: Color) -> OkhslChannels {
    rgb_to_okhsl(color_to_rgb(color))
}

/// A `parseColor()` input: a theme string or an ANSI index.
#[derive(Debug, Clone, Copy)]
pub enum ColorInput<'a> {
    Text(&'a str),
    Index(i64),
}

pub fn parse_color(value: ColorInput<'_>) -> Result<Color, ColorError> {
    let value = match value {
        ColorInput::Index(index) => return indexed_color(index),
        ColorInput::Text(text) => text,
    };
    let number = |text: &str| text.parse::<f64>().unwrap_or(f64::NAN);

    if let Some(hex) = HEX_PATTERN.captures(value) {
        let digits = &hex[1];
        let digits = if digits.len() == 3 {
            digits.chars().flat_map(|digit| [digit, digit]).collect()
        } else {
            digits.to_string()
        };
        let channel = |range: std::ops::Range<usize>| {
            f64::from(u8::from_str_radix(&digits[range], 16).expect("hex digits"))
        };
        return rgb_color(channel(0..2), channel(2..4), channel(4..6));
    }

    if let Some(oklch) = OKLCH_PATTERN.captures(value) {
        let divisor = if oklch.get(2).is_some() { 100.0 } else { 1.0 };
        let lightness = number(&oklch[1]) / divisor;
        return oklch_color(lightness, number(&oklch[3]), number(&oklch[4]));
    }

    if let Some(okhsl) = OKHSL_PATTERN.captures(value) {
        let saturation = number(&okhsl[2]) / if okhsl.get(3).is_some() { 100.0 } else { 1.0 };
        let lightness = number(&okhsl[4]) / if okhsl.get(5).is_some() { 100.0 } else { 1.0 };
        return okhsl_color(number(&okhsl[1]), saturation, lightness);
    }

    Err(ColorError(format!("Invalid color value: {value}")))
}

const BASIC_COLORS: [[u8; 3]; 16] = [
    [0, 0, 0],
    [128, 0, 0],
    [0, 128, 0],
    [128, 128, 0],
    [0, 0, 128],
    [128, 0, 128],
    [0, 128, 128],
    [192, 192, 192],
    [128, 128, 128],
    [255, 0, 0],
    [0, 255, 0],
    [255, 255, 0],
    [0, 0, 255],
    [255, 0, 255],
    [0, 255, 255],
    [255, 255, 255],
];
const CUBE_VALUES: [f64; 6] = [0.0, 95.0, 135.0, 175.0, 215.0, 255.0];
static GRAY_VALUES: LazyLock<[f64; 24]> =
    LazyLock::new(|| std::array::from_fn(|index| 8.0 + index as f64 * 10.0));

#[derive(Debug, Clone, Copy, PartialEq)]
struct Rgb {
    r: f64,
    g: f64,
    b: f64,
}

fn indexed_to_rgb(index: u8) -> Rgb {
    let index = usize::from(index);
    if index < 16 {
        let [r, g, b] = BASIC_COLORS[index];
        return Rgb {
            r: f64::from(r),
            g: f64::from(g),
            b: f64::from(b),
        };
    }
    if index < 232 {
        let cube_index = index - 16;
        return Rgb {
            r: CUBE_VALUES[cube_index / 36],
            g: CUBE_VALUES[(cube_index % 36) / 6],
            b: CUBE_VALUES[cube_index % 6],
        };
    }
    let gray = 8.0 + (index - 232) as f64 * 10.0;
    Rgb {
        r: gray,
        g: gray,
        b: gray,
    }
}

fn is_in_srgb_gamut(linear: [f64; 3]) -> bool {
    let epsilon = 1e-7;
    linear
        .iter()
        .all(|channel| *channel >= -epsilon && *channel <= 1.0 + epsilon)
}

fn oklch_to_rgb(OklchChannels { l, c, h }: OklchChannels) -> RgbColor {
    // Gamut mapping keeps the hue fixed, so its direction is computed once and scaled by chroma.
    let radians = h.to_radians();
    let cos = radians.cos();
    let sin = radians.sin();
    let at_chroma = |chroma: f64| oklab_to_linear_srgb([l, chroma * cos, chroma * sin]);

    let direct = at_chroma(c);
    if is_in_srgb_gamut(direct) {
        return linear_srgb_to_rgb(direct);
    }

    // Reduce chroma until the color fits. The achromatic color is always in gamut, so it is the
    // fallback when no bisection step fits, e.g. `oklch(100% 0.3 150)` must map to white.
    let mut linear = at_chroma(0.0);
    let mut low = 0.0;
    let mut high = c;
    for _ in 0..20 {
        let chroma = (low + high) / 2.0;
        let candidate = at_chroma(chroma);
        if is_in_srgb_gamut(candidate) {
            low = chroma;
            linear = candidate;
        } else {
            high = chroma;
        }
    }
    linear_srgb_to_rgb(linear)
}

fn color_to_rgb_f64(color: Color) -> Rgb {
    match color {
        Color::Indexed { index } => indexed_to_rgb(index),
        Color::Rgb { r, g, b } => Rgb { r, g, b },
        Color::Oklch { l, c, h } => {
            let rgb = oklch_to_rgb(OklchChannels { l, c, h });
            Rgb {
                r: f64::from(rgb.r),
                g: f64::from(rgb.g),
                b: f64::from(rgb.b),
            }
        }
    }
}

/// sRGB channels of a color, rounded to whole channel values.
pub fn color_to_rgb(color: Color) -> RgbColor {
    let rgb = color_to_rgb_f64(color);
    RgbColor {
        r: rgb.r.round() as u8,
        g: rgb.g.round() as u8,
        b: rgb.b.round() as u8,
    }
}

pub fn color_to_oklch(color: Color) -> OklchChannels {
    if let Color::Oklch { l, c, h } = color {
        return OklchChannels { l, c, h };
    }
    let [l, a, b] = rgb_to_oklab(color_to_rgb(color));
    OklchChannels {
        l,
        c: a.hypot(b),
        h: (b.atan2(a).to_degrees() + 360.0) % 360.0,
    }
}

pub fn color_to_hex(color: Color) -> String {
    let rgb = color_to_rgb_f64(color);
    format!(
        "#{:02x}{:02x}{:02x}",
        rgb.r.round() as u8,
        rgb.g.round() as u8,
        rgb.b.round() as u8
    )
}

pub fn mix_colors(
    first: Color,
    second: Color,
    amount: f64,
    space: ColorMixSpace,
) -> Result<Color, ColorError> {
    require_finite(amount, "amount")?;
    if !(0.0..=1.0).contains(&amount) {
        return Err(ColorError(format!(
            "amount must be between 0 and 1: {}",
            fmt_num(amount)
        )));
    }

    if space == ColorMixSpace::Srgb {
        let a = color_to_rgb_f64(first);
        let b = color_to_rgb_f64(second);
        return rgb_color(
            a.r + (b.r - a.r) * amount,
            a.g + (b.g - a.g) * amount,
            a.b + (b.b - a.b) * amount,
        );
    }

    let a = color_to_oklch(first);
    let b = color_to_oklch(second);
    let first_hue = if a.c < 1e-7 { b.h } else { a.h };
    let second_hue = if b.c < 1e-7 { first_hue } else { b.h };
    let hue_delta = ((second_hue - first_hue + 540.0) % 360.0) - 180.0;
    oklch_color(
        a.l + (b.l - a.l) * amount,
        a.c + (b.c - a.c) * amount,
        first_hue + hue_delta * amount,
    )
}

fn find_closest(values: &[f64], target: f64) -> usize {
    let mut closest_index = 0;
    let mut closest_distance = f64::INFINITY;
    for (index, value) in values.iter().enumerate() {
        let distance = (target - value).abs();
        if distance < closest_distance {
            closest_index = index;
            closest_distance = distance;
        }
    }
    closest_index
}

fn color_distance(first: Rgb, second: Rgb) -> f64 {
    let dr = first.r - second.r;
    let dg = first.g - second.g;
    let db = first.b - second.b;
    dr * dr * 0.299 + dg * dg * 0.587 + db * db * 0.114
}

fn rgb_to_ansi256(color: Rgb) -> usize {
    let r_index = find_closest(&CUBE_VALUES, color.r);
    let g_index = find_closest(&CUBE_VALUES, color.g);
    let b_index = find_closest(&CUBE_VALUES, color.b);
    let cube_color = Rgb {
        r: CUBE_VALUES[r_index],
        g: CUBE_VALUES[g_index],
        b: CUBE_VALUES[b_index],
    };
    let cube_index = 16 + 36 * r_index + 6 * g_index + b_index;

    let gray = (0.299 * color.r + 0.587 * color.g + 0.114 * color.b).round();
    let gray_offset = find_closest(&*GRAY_VALUES, gray);
    let gray_value = GRAY_VALUES[gray_offset];
    let spread = color.r.max(color.g).max(color.b) - color.r.min(color.g).min(color.b);
    if spread < 10.0
        && color_distance(
            color,
            Rgb {
                r: gray_value,
                g: gray_value,
                b: gray_value,
            },
        ) < color_distance(color, cube_color)
    {
        return 232 + gray_offset;
    }
    cube_index
}

fn color_ansi(color: Color, mode: TerminalColorMode, background: bool) -> String {
    let code = if background { 48 } else { 38 };
    if let Color::Indexed { index } = color {
        return format!("\x1b[{code};5;{index}m");
    }

    let rgb = color_to_rgb_f64(color);
    if mode == TerminalColorMode::Truecolor {
        return format!(
            "\x1b[{code};2;{};{};{}m",
            rgb.r.round(),
            rgb.g.round(),
            rgb.b.round()
        );
    }
    format!("\x1b[{code};5;{}m", rgb_to_ansi256(rgb))
}

pub fn foreground_ansi(color: Color, mode: TerminalColorMode) -> String {
    color_ansi(color, mode, false)
}

pub fn background_ansi(color: Color, mode: TerminalColorMode) -> String {
    color_ansi(color, mode, true)
}

pub fn style_text(text: &str, options: &TextStyle, mode: TerminalColorMode) -> String {
    style_text_with_ansi(
        text,
        options.fg.map(|fg| foreground_ansi(fg, mode)).as_deref(),
        options.bg.map(|bg| background_ansi(bg, mode)).as_deref(),
        &options.attributes,
    )
}

/// Like `style_text()`, but with precomputed color escape sequences, e.g. cached theme colors.
pub fn style_text_with_ansi(
    text: &str,
    fg_ansi: Option<&str>,
    bg_ansi: Option<&str>,
    options: &TextAttributes,
) -> String {
    // Resets are prepended so they close in reverse order of the opening sequences.
    let mut prefix = String::new();
    let mut suffix = String::new();
    if let Some(fg) = fg_ansi.filter(|ansi| !ansi.is_empty()) {
        prefix.push_str(fg);
        suffix = "\x1b[39m".to_string();
    }
    if let Some(bg) = bg_ansi.filter(|ansi| !ansi.is_empty()) {
        prefix.push_str(bg);
        suffix = format!("\x1b[49m{suffix}");
    }
    if options.bold {
        prefix.push_str("\x1b[1m");
    }
    if options.dim {
        prefix.push_str("\x1b[2m");
    }
    if options.bold || options.dim {
        suffix = format!("\x1b[22m{suffix}");
    }
    if options.italic {
        prefix.push_str("\x1b[3m");
        suffix = format!("\x1b[23m{suffix}");
    }
    if options.underline {
        prefix.push_str("\x1b[4m");
        suffix = format!("\x1b[24m{suffix}");
    }
    if options.inverse {
        prefix.push_str("\x1b[7m");
        suffix = format!("\x1b[27m{suffix}");
    }
    if options.strikethrough {
        prefix.push_str("\x1b[9m");
        suffix = format!("\x1b[29m{suffix}");
    }
    format!("{prefix}{text}{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(value: &str) -> Result<Color, ColorError> {
        parse_color(ColorInput::Text(value))
    }

    #[test]
    fn parses_hex_and_oklch_colors_and_rejects_everything_else() {
        assert_eq!(
            text("#abc").unwrap(),
            Color::Rgb {
                r: 170.0,
                g: 187.0,
                b: 204.0
            }
        );
        assert_eq!(
            text("oklch(62% 0.1 200)").unwrap(),
            Color::Oklch {
                l: 0.62,
                c: 0.1,
                h: 200.0
            }
        );
        assert!(text("").unwrap_err().0.contains("Invalid color value"));
        assert!(text("red").unwrap_err().0.contains("Invalid color value"));
    }

    #[test]
    fn gamut_maps_oklch_to_srgb_including_the_lightness_limits() {
        assert_eq!(
            color_to_rgb(oklch_color(0.627955, 0.257683, 29.2339).unwrap()),
            RgbColor { r: 255, g: 0, b: 0 }
        );
        assert_eq!(
            color_to_rgb(oklch_color(1.0, 0.3, 150.0).unwrap()),
            RgbColor {
                r: 255,
                g: 255,
                b: 255
            }
        );
        assert_eq!(
            color_to_rgb(oklch_color(0.0, 0.3, 150.0).unwrap()),
            RgbColor { r: 0, g: 0, b: 0 }
        );
    }

    #[test]
    fn parses_okhsl_colors_and_round_trips_them() {
        // Full saturation at the red cusp is pure sRGB red.
        assert_eq!(
            text("okhsl(29.23 100% 56.8%)").unwrap(),
            rgb_color(255.0, 0.0, 0.0).unwrap()
        );
        assert_eq!(
            text("OKHSL(250deg 60% 55%)").unwrap(),
            okhsl_color(250.0, 0.6, 0.55).unwrap()
        );
        assert!(
            text("okhsl(250 160% 55%)")
                .unwrap_err()
                .0
                .contains("s must be between 0 and 1")
        );
        for hex in ["#4f8eb3", "#20242a", "#f8f9fa"] {
            let okhsl = color_to_okhsl(text(hex).unwrap());
            assert_eq!(
                color_to_hex(okhsl_color(okhsl.h, okhsl.s, okhsl.l).unwrap()),
                hex
            );
        }
    }

    #[test]
    fn styles_text_and_closes_sequences_in_reverse_order() {
        let style = TextStyle {
            fg: Some(rgb_color(18.0, 52.0, 86.0).unwrap()),
            bg: Some(indexed_color(9).unwrap()),
            attributes: TextAttributes {
                bold: true,
                italic: true,
                ..TextAttributes::default()
            },
        };
        assert_eq!(
            style_text("Ready", &style, TerminalColorMode::Truecolor),
            "\x1b[38;2;18;52;86m\x1b[48;5;9m\x1b[1m\x1b[3mReady\x1b[23m\x1b[22m\x1b[49m\x1b[39m"
        );
        let fg_only = TextStyle {
            fg: Some(rgb_color(18.0, 52.0, 86.0).unwrap()),
            ..TextStyle::default()
        };
        let rendered = style_text("Ready", &fg_only, TerminalColorMode::Color256);
        let pattern = Regex::new(r"^\x1b\[38;5;\d+mReady\x1b\[39m$").unwrap();
        assert!(pattern.is_match(&rendered), "{rendered:?}");
    }
}
