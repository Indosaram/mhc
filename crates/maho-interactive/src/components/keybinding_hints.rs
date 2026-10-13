//! Port of `components/keybinding-hints.ts`.
use std::sync::{Arc, LazyLock};

use maho_tui::keybindings::{get_keybindings, KeybindingsConfig, KeybindingsManager};

use crate::theme::{Theme, ThemeColor};

/// senpi `getKeybindings()`: the process-wide manager `InteractiveMode` installs (user rebinds
/// included). Before any coding-agent manager is installed the global holds only `tui.*`
/// definitions, so the default app keybindings answer instead of an empty hint.
fn manager() -> Arc<KeybindingsManager> {
    static DEFAULT: LazyLock<Arc<KeybindingsManager>> = LazyLock::new(|| {
        Arc::new(KeybindingsManager::new(maho_core::keybindings::keybindings().clone(), KeybindingsConfig::default()))
    });
    let global = get_keybindings();
    if global.get_definition("app.interrupt").is_some() { global } else { Arc::clone(&*DEFAULT) }
}

fn format_key_part(part: &str, capitalize: bool) -> String {
    let lower = part.to_lowercase();
    let display = if lower == "escape" {
        String::from("esc")
    } else if cfg!(target_os = "macos") && lower == "alt" {
        String::from("option")
    } else {
        part.to_owned()
    };
    if !capitalize {
        return display;
    }
    let mut chars = display.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => display,
    }
}

pub fn format_key_text(key: &str, capitalize: bool) -> String {
    key.split('/')
        .map(|segment| {
            segment
                .split('+')
                .map(|part| format_key_part(part, capitalize))
                .collect::<Vec<_>>()
                .join("+")
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn format_keys(keys: &[String], capitalize: bool) -> String {
    if keys.is_empty() {
        return String::new();
    }
    format_key_text(&keys.join("/"), capitalize)
}

pub fn key_text(keybinding: &str) -> String {
    format_keys(&manager().get_keys(keybinding), false)
}

pub fn key_display_text(keybinding: &str) -> String {
    format_keys(&manager().get_keys(keybinding), true)
}

pub fn key_hint(keybinding: &str, description: &str, theme: &Theme) -> String {
    theme.fg(ThemeColor::Dim, &key_text(keybinding)) + &theme.fg(ThemeColor::Muted, &format!(" {description}"))
}

pub fn raw_key_hint(key: &str, description: &str, theme: &Theme) -> String {
    theme.fg(ThemeColor::Dim, &format_key_text(key, false)) + &theme.fg(ThemeColor::Muted, &format!(" {description}"))
}
