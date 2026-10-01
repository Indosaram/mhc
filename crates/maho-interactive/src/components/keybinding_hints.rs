use crate::theme::{Theme, ThemeColor};
use maho_tui::keybindings::get_keybindings;

pub fn format_key_text(key: &str, capitalize: bool) -> String {
    key.split('/').map(|key| key.split('+').map(|part| {
        let lower = part.to_lowercase();
        let display = if lower == "escape" { "esc" }
            else if cfg!(target_os = "macos") && lower == "alt" { "option" }
            else { part };
        if capitalize {
            let mut chars = display.chars();
            chars.next().map(|first| format!("{}{rest}", first.to_uppercase(), rest = chars.as_str()))
                .unwrap_or_default()
        } else { display.to_owned() }
    }).collect::<Vec<_>>().join("+")).collect::<Vec<_>>().join("/")
}

pub fn key_text(keybinding: &str) -> String {
    format_key_text(&get_keybindings().get_keys(keybinding).join("/"), false)
}

pub fn key_display_text(keybinding: &str) -> String {
    format_key_text(&get_keybindings().get_keys(keybinding).join("/"), true)
}

pub fn key_hint(theme: &Theme, keybinding: &str, description: &str) -> String {
    format!("{}{}", theme.fg(ThemeColor::Dim, &key_text(keybinding)),
        theme.fg(ThemeColor::Muted, &format!(" {description}")))
}

pub fn raw_key_hint(theme: &Theme, key: &str, description: &str) -> String {
    format!("{}{}", theme.fg(ThemeColor::Dim, &format_key_text(key, false)),
        theme.fg(ThemeColor::Muted, &format!(" {description}")))
}
