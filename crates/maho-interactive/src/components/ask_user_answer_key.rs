//! Port of senpi `packages/coding-agent/src/modes/interactive/components/ask-user-answer-key.ts`.

use std::collections::{HashMap, HashSet};

use maho_core::keybindings::{QUESTION_ANSWER_FALLBACK_KEY, QUESTION_ANSWER_PRIMARY_KEY, host_platform};
use maho_tui::keybindings::{KeybindingsManager, get_keybindings};
use maho_tui::keys::decode_kitty_printable;

pub const ASK_USER_ANSWER_KEYBINDING: &str = "app.question.answer";

const KEY_DISPLAY_ALIASES: &[(&str, &str)] = &[("escape", "esc")];

fn format_key_part(part: &str, capitalize: bool, platform: &str) -> String {
    let lower = part.to_lowercase();
    let display = KEY_DISPLAY_ALIASES
        .iter()
        .find(|(from, _)| *from == lower)
        .map(|(_, to)| (*to).to_string())
        .unwrap_or_else(|| {
            if platform == "darwin" && lower == "alt" {
                "option".to_string()
            } else {
                part.to_string()
            }
        });
    if capitalize {
        let mut chars = display.chars();
        match chars.next() {
            Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
            None => display,
        }
    } else {
        display
    }
}

/// todo 33 `components/keybinding-hints.ts` owns this once it lands.
pub(crate) fn format_key_text(key: &str) -> String {
    let platform = host_platform();
    key.split('/')
        .map(|part| {
            part.split('+')
                .map(|piece| format_key_part(piece, false, &platform))
                .collect::<Vec<_>>()
                .join("+")
        })
        .collect::<Vec<_>>()
        .join("/")
}

pub(crate) fn format_keys(keys: &[String]) -> String {
    if keys.is_empty() {
        return String::new();
    }
    format_key_text(&keys.join("/"))
}

pub(crate) fn key_text(keybinding: &str) -> String {
    format_keys(&get_keybindings().get_keys(keybinding))
}

pub(crate) fn raw_key_hint(theme: &crate::theme::Theme, key: &str, description: &str) -> String {
    format!(
        "{}{}",
        theme.fg(crate::theme::ThemeColor::Dim, &format_key_text(key)),
        theme.fg(crate::theme::ThemeColor::Muted, &format!(" {description}"))
    )
}

pub(crate) fn key_hint(theme: &crate::theme::Theme, keybinding: &str, description: &str) -> String {
    format!(
        "{}{}",
        theme.fg(crate::theme::ThemeColor::Dim, &key_text(keybinding)),
        theme.fg(crate::theme::ThemeColor::Muted, &format!(" {description}"))
    )
}

pub fn ask_user_answer_key_hint(env: &HashMap<String, String>) -> String {
    let keys = get_keybindings().get_keys(ASK_USER_ANSWER_KEYBINDING);
    let term_program = env.get("TERM_PROGRAM").map(String::as_str).unwrap_or("");
    let preferred = if env.contains_key("TMUX")
        || ["Apple_Terminal", "WarpTerminal", "vscode"].contains(&term_program)
    {
        QUESTION_ANSWER_FALLBACK_KEY
    } else {
        QUESTION_ANSWER_PRIMARY_KEY
    };
    if keys.iter().any(|key| key == preferred) {
        format_key_text(preferred)
    } else {
        key_text(ASK_USER_ANSWER_KEYBINDING)
    }
}

// Letter keys whose Option-compose glyphs stand in for the bound `alt+<letter>` chord on darwin.
const DARWIN_OPTION_GLYPHS: &[(&str, [&str; 2])] = &[
    ("a", ["å", "Å"]),
    ("b", ["∫", "ı"]),
    ("c", ["ç", "Ç"]),
    ("d", ["∂", "Î"]),
    ("f", ["ƒ", "Ï"]),
    ("g", ["©", "˝"]),
    ("h", ["˙", "Ó"]),
    ("j", ["∆", "Ô"]),
    ("k", ["˚", "\u{f8ff}"]),
    ("l", ["¬", "Ò"]),
    ("m", ["µ", "Â"]),
    ("o", ["ø", "Ø"]),
    ("p", ["π", "∏"]),
    ("q", ["œ", "Œ"]),
    ("r", ["®", "‰"]),
    ("s", ["ß", "Í"]),
    ("t", ["†", "ˇ"]),
    ("v", ["√", "◊"]),
    ("w", ["∑", "„"]),
    ("x", ["≈", "˛"]),
    ("y", ["¥", "Á"]),
    ("z", ["Ω", "¸"]),
];

pub fn darwin_option_glyphs(keys: &[String]) -> HashSet<String> {
    let mut glyphs = HashSet::new();
    for key in keys {
        let Some(letter) = alt_letter(key) else {
            continue;
        };
        if let Some((_, pair)) = DARWIN_OPTION_GLYPHS.iter().find(|(name, _)| *name == letter) {
            glyphs.insert(pair[0].to_string());
            glyphs.insert(pair[1].to_string());
        }
    }
    glyphs
}

fn alt_letter(key: &str) -> Option<String> {
    let rest = key
        .strip_prefix("alt+")
        .or_else(|| key.strip_prefix("Alt+"))
        .or_else(|| key.strip_prefix("ALT+"))?;
    if rest.len() == 1 && rest.chars().all(|c| c.is_ascii_alphabetic()) {
        Some(rest.to_lowercase())
    } else {
        None
    }
}

pub fn matches_ask_user_answer_key(
    data: &str,
    platform: &str,
    keybindings: &KeybindingsManager,
) -> bool {
    if keybindings.matches(data, ASK_USER_ANSWER_KEYBINDING) {
        return true;
    }
    if platform != "darwin" {
        return false;
    }
    let glyphs = darwin_option_glyphs(&keybindings.get_keys(ASK_USER_ANSWER_KEYBINDING));
    match decode_kitty_printable(data) {
        Some(printable) => glyphs.contains(&printable),
        None => glyphs.contains(data),
    }
}

pub fn process_env() -> HashMap<String, String> {
    std::env::vars().collect()
}
