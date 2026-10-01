//! Port of senpi `packages/coding-agent/src/modes/interactive/grok/chrome-tokens.ts`.

use std::rc::Rc;

use crate::theme::{Theme, ThemeBg, ThemeColor};

pub type ChromeTextStyle = Rc<dyn Fn(&str) -> String>;

/// Chrome-only semantic tokens. Foreground/status tokens delegate to the active interactive theme;
/// the panel token is the theme export's card background.
pub struct GrokChromeTokens {
    pub input_border: ChromeTextStyle,
    pub input_interior: ChromeTextStyle,
    pub surface: ChromeTextStyle,
    pub card_border: ChromeTextStyle,
    pub model_label: ChromeTextStyle,
    pub cwd: ChromeTextStyle,
    pub primary_text: ChromeTextStyle,
    pub muted_text: ChromeTextStyle,
    pub success: ChromeTextStyle,
    pub error: ChromeTextStyle,
    pub warning: ChromeTextStyle,
}

fn background_from_theme_export(hex: &str) -> Option<ChromeTextStyle> {
    let rest = hex.strip_prefix('#')?;
    if rest.len() != 6 || !rest.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let red = u8::from_str_radix(&rest[0..2], 16).ok()?;
    let green = u8::from_str_radix(&rest[2..4], 16).ok()?;
    let blue = u8::from_str_radix(&rest[4..6], 16).ok()?;
    Some(Rc::new(move |text: &str| {
        format!("\x1b[48;2;{red};{green};{blue}m{text}\x1b[49m")
    }))
}

fn fg_style(theme: &Theme, color: ThemeColor) -> ChromeTextStyle {
    let theme = theme.clone();
    Rc::new(move |text: &str| theme.fg(color, text))
}

pub fn get_grok_chrome_tokens(theme: &Theme) -> GrokChromeTokens {
    let export = theme.export_colors();
    let panel_background = export.get("cardBg").cloned().unwrap_or_default();
    let page_background = export.get("pageBg").cloned().unwrap_or_default();
    let input_interior = background_from_theme_export(&panel_background).unwrap_or_else(|| {
        let theme = theme.clone();
        Rc::new(move |text: &str| theme.bg(ThemeBg::ToolPendingBg, text))
    });
    let surface = background_from_theme_export(&page_background).unwrap_or_else(|| Rc::new(|text: &str| text.to_string()));
    GrokChromeTokens {
        input_border: fg_style(theme, ThemeColor::BorderAccent),
        input_interior,
        surface,
        card_border: fg_style(theme, ThemeColor::BorderMuted),
        model_label: fg_style(theme, ThemeColor::ThinkingText),
        cwd: fg_style(theme, ThemeColor::Dim),
        primary_text: fg_style(theme, ThemeColor::Text),
        muted_text: fg_style(theme, ThemeColor::Muted),
        success: fg_style(theme, ThemeColor::Success),
        error: fg_style(theme, ThemeColor::Error),
        warning: fg_style(theme, ThemeColor::Warning),
    }
}
