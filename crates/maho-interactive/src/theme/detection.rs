use super::theme::{TerminalTheme, ansi256_to_hex, get_theme_for_rgb_color, hex_to_rgb};
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalThemeDetection {
    pub theme: TerminalTheme,
    pub source: &'static str,
    pub detail: String,
    pub confidence: &'static str,
}
pub fn detect_terminal_background_from_env(colorfgbg: Option<&str>) -> TerminalThemeDetection {
    let background = colorfgbg.unwrap_or("").split(';').rev().find_map(|part| {
        let part = part.trim();
        let prefix: String = part
            .chars()
            .enumerate()
            .take_while(|(i, c)| c.is_ascii_digit() || (*i == 0 && matches!(c, '+' | '-')))
            .map(|(_, c)| c)
            .collect();
        prefix.parse::<u8>().ok()
    });
    if let Some(index) = background
        && let Ok(rgb) = hex_to_rgb(&ansi256_to_hex(index))
    {
        return TerminalThemeDetection {
            theme: get_theme_for_rgb_color(rgb),
            source: "COLORFGBG",
            detail: format!("background color index {index}"),
            confidence: "high",
        };
    }
    TerminalThemeDetection {
        theme: TerminalTheme::Dark,
        source: "fallback",
        detail: "no terminal background hint found".into(),
        confidence: "low",
    }
}
pub fn detect_terminal_theme_for_auto(
    color_scheme: Option<TerminalTheme>,
    background: Option<[u8; 3]>,
    colorfgbg: Option<&str>,
) -> TerminalTheme {
    color_scheme
        .or_else(|| background.map(get_theme_for_rgb_color))
        .unwrap_or_else(|| detect_terminal_background_from_env(colorfgbg).theme)
}
