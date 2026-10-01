//! Port of senpi packages/coding-agent/src/core/export-html/ansi-to-html.ts.
//!
//! Converts terminal ANSI color/style codes to HTML with inline styles.

use std::sync::OnceLock;

use regex::Regex;

const ANSI_COLORS: [&str; 16] = [
    "#000000", "#800000", "#008000", "#808000", "#000080", "#800080", "#008080", "#c0c0c0", "#808080",
    "#ff0000", "#00ff00", "#ffff00", "#0000ff", "#ff00ff", "#00ffff", "#ffffff",
];

fn color256_to_hex(index: i64) -> String {
    if (0..16).contains(&index) {
        return ANSI_COLORS[index as usize].to_owned();
    }
    if (16..232).contains(&index) {
        let cube_index = index - 16;
        let r = cube_index / 36;
        let g = (cube_index % 36) / 6;
        let b = cube_index % 6;
        let to_component = |n: i64| if n == 0 { 0 } else { 55 + n * 40 };
        let to_hex = |n: i64| format!("{:02x}", to_component(n));
        return format!("#{}{}{}", to_hex(r), to_hex(g), to_hex(b));
    }
    let gray = 8 + (index - 232) * 10;
    let gray_hex = format!("{gray:02x}");
    format!("#{gray_hex}{gray_hex}{gray_hex}")
}

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#039;")
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct TextStyle {
    fg: Option<String>,
    bg: Option<String>,
    bold: bool,
    dim: bool,
    italic: bool,
    underline: bool,
}

fn style_to_inline_css(style: &TextStyle) -> String {
    let mut parts = Vec::new();
    if let Some(fg) = &style.fg {
        parts.push(format!("color:{fg}"));
    }
    if let Some(bg) = &style.bg {
        parts.push(format!("background-color:{bg}"));
    }
    if style.bold {
        parts.push("font-weight:bold".to_owned());
    }
    if style.dim {
        parts.push("opacity:0.6".to_owned());
    }
    if style.italic {
        parts.push("font-style:italic".to_owned());
    }
    if style.underline {
        parts.push("text-decoration:underline".to_owned());
    }
    parts.join(";")
}

fn has_style(style: &TextStyle) -> bool {
    style.fg.is_some() || style.bg.is_some() || style.bold || style.dim || style.italic || style.underline
}

fn apply_sgr_code(params: &[i64], style: &mut TextStyle) {
    let mut i = 0usize;
    while i < params.len() {
        let code = params[i];
        if code == 0 {
            *style = TextStyle::default();
        } else if code == 1 {
            style.bold = true;
        } else if code == 2 {
            style.dim = true;
        } else if code == 3 {
            style.italic = true;
        } else if code == 4 {
            style.underline = true;
        } else if code == 22 {
            style.bold = false;
            style.dim = false;
        } else if code == 23 {
            style.italic = false;
        } else if code == 24 {
            style.underline = false;
        } else if (30..=37).contains(&code) {
            style.fg = Some(ANSI_COLORS[(code - 30) as usize].to_owned());
        } else if code == 38 {
            if params.get(i + 1) == Some(&5) && params.len() > i + 2 {
                style.fg = Some(color256_to_hex(params[i + 2]));
                i += 2;
            } else if params.get(i + 1) == Some(&2) && params.len() > i + 4 {
                style.fg = Some(format!("rgb({},{},{})", params[i + 2], params[i + 3], params[i + 4]));
                i += 4;
            }
        } else if code == 39 {
            style.fg = None;
        } else if (40..=47).contains(&code) {
            style.bg = Some(ANSI_COLORS[(code - 40) as usize].to_owned());
        } else if code == 48 {
            if params.get(i + 1) == Some(&5) && params.len() > i + 2 {
                style.bg = Some(color256_to_hex(params[i + 2]));
                i += 2;
            } else if params.get(i + 1) == Some(&2) && params.len() > i + 4 {
                style.bg = Some(format!("rgb({},{},{})", params[i + 2], params[i + 3], params[i + 4]));
                i += 4;
            }
        } else if code == 49 {
            style.bg = None;
        } else if (90..=97).contains(&code) {
            style.fg = Some(ANSI_COLORS[(code - 90 + 8) as usize].to_owned());
        } else if (100..=107).contains(&code) {
            style.bg = Some(ANSI_COLORS[(code - 100 + 8) as usize].to_owned());
        }
        i += 1;
    }
}

fn ansi_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\x1b\[([\d;]*)m").expect("ansi regex"))
}

/// Converts ANSI-escaped text to HTML with inline styles.
pub fn ansi_to_html(text: &str) -> String {
    let mut style = TextStyle::default();
    let mut result = String::new();
    let mut last_index = 0usize;
    let mut in_span = false;

    for capture in ansi_regex().captures_iter(text) {
        let whole = capture.get(0).expect("match");
        let before = &text[last_index..whole.start()];
        if !before.is_empty() {
            result.push_str(&escape_html(before));
        }

        let param_str = capture.get(1).map(|m| m.as_str()).unwrap_or_default();
        let params: Vec<i64> = if param_str.is_empty() {
            vec![0]
        } else {
            param_str.split(';').map(|part| part.parse::<i64>().unwrap_or(0)).collect()
        };

        if in_span {
            result.push_str("</span>");
            in_span = false;
        }

        apply_sgr_code(&params, &mut style);

        if has_style(&style) {
            result.push_str(&format!("<span style=\"{}\">", style_to_inline_css(&style)));
            in_span = true;
        }

        last_index = whole.end();
    }

    let remaining = &text[last_index..];
    if !remaining.is_empty() {
        result.push_str(&escape_html(remaining));
    }
    if in_span {
        result.push_str("</span>");
    }
    result
}

/// Converts an array of ANSI-escaped lines to HTML; each line is wrapped in a div.
pub fn ansi_lines_to_html(lines: &[String]) -> String {
    lines
        .iter()
        .map(|line| {
            let converted = ansi_to_html(line);
            let body = if converted.is_empty() { "&nbsp;".to_owned() } else { converted };
            format!("<div class=\"ansi-line\">{body}</div>")
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_256_color_cube_index_maps_to_hex() {
        assert_eq!(color256_to_hex(16), "#000000");
        assert_eq!(color256_to_hex(231), "#ffffff");
    }

    #[test]
    fn grayscale_indexes_map_to_hex() {
        assert_eq!(color256_to_hex(232), "#080808");
        assert_eq!(color256_to_hex(255), "#eeeeee");
    }

    #[test]
    fn plain_text_is_html_escaped() {
        assert_eq!(ansi_to_html("a & b < c"), "a &amp; b &lt; c");
    }

    #[test]
    fn a_bold_sgr_wraps_the_text_in_a_styled_span() {
        assert_eq!(ansi_to_html("\x1b[1mhi\x1b[0m"), "<span style=\"font-weight:bold\">hi</span>");
    }

    #[test]
    fn a_standard_foreground_color_maps_to_the_palette() {
        assert_eq!(ansi_to_html("\x1b[31mx\x1b[39m"), "<span style=\"color:#800000\">x</span>");
    }

    #[test]
    fn an_rgb_sgr_emits_an_rgb_color() {
        assert_eq!(ansi_to_html("\x1b[38;2;1;2;3mx\x1b[0m"), "<span style=\"color:rgb(1,2,3)\">x</span>");
    }

    #[test]
    fn a_256_color_sgr_emits_a_hex_color() {
        assert_eq!(ansi_to_html("\x1b[38;5;9mx\x1b[0m"), "<span style=\"color:#ff0000\">x</span>");
    }

    #[test]
    fn lines_are_wrapped_in_divs_with_nbsp_for_empty() {
        assert_eq!(
            ansi_lines_to_html(&["".to_owned(), "hi".to_owned()]),
            "<div class=\"ansi-line\">&nbsp;</div><div class=\"ansi-line\">hi</div>"
        );
    }
}
