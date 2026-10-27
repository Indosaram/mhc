//! US keyboard layout for `Input.dispatchKeyEvent`.
//!
//! Port of the keyboard module of omowright `src/core.js` (`KEYBOARD_LAYOUT` with its aliases,
//! `splitKeyCombo`, `resolveKeyDescriptor`, the modifier mask and `ControlOrMeta`).

/// One key as CDP wants it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyDescriptor {
    pub key: String,
    pub code: String,
    pub key_code: u32,
    pub text: String,
    pub location: u32,
}

fn key(key: &str, code: &str, key_code: u32, text: &str, location: u32) -> KeyDescriptor {
    KeyDescriptor {
        key: key.to_owned(),
        code: code.to_owned(),
        key_code,
        text: text.to_owned(),
        location,
    }
}

const PUNCTUATION: &[(char, char, &str, u32)] = &[
    ('`', '~', "Backquote", 192),
    ('-', '_', "Minus", 189),
    ('=', '+', "Equal", 187),
    ('[', '{', "BracketLeft", 219),
    (']', '}', "BracketRight", 221),
    ('\\', '|', "Backslash", 220),
    (';', ':', "Semicolon", 186),
    ('\'', '"', "Quote", 222),
    (',', '<', "Comma", 188),
    ('.', '>', "Period", 190),
    ('/', '?', "Slash", 191),
];
const SHIFTED_DIGITS: [char; 10] = [')', '!', '@', '#', '$', '%', '^', '&', '*', '('];

/// (key, code, keyCode, text, location, aliases)
type NamedKey = (&'static str, &'static str, u32, &'static str, u32, &'static [&'static str]);
const NAMED: &[NamedKey] = &[
    ("Backspace", "Backspace", 8, "", 0, &[]),
    ("Tab", "Tab", 9, "\t", 0, &["\t"]),
    ("Enter", "Enter", 13, "\r", 0, &["Return", "\n", "\r"]),
    ("Escape", "Escape", 27, "", 0, &["Esc"]),
    ("Delete", "Delete", 46, "", 0, &["Del"]),
    ("Insert", "Insert", 45, "", 0, &["Ins"]),
    ("Home", "Home", 36, "", 0, &[]),
    ("End", "End", 35, "", 0, &[]),
    ("PageUp", "PageUp", 33, "", 0, &[]),
    ("PageDown", "PageDown", 34, "", 0, &[]),
    ("ArrowUp", "ArrowUp", 38, "", 0, &["Up"]),
    ("ArrowDown", "ArrowDown", 40, "", 0, &["Down"]),
    ("ArrowLeft", "ArrowLeft", 37, "", 0, &["Left"]),
    ("ArrowRight", "ArrowRight", 39, "", 0, &["Right"]),
    ("Shift", "ShiftLeft", 16, "", 1, &[]),
    ("Control", "ControlLeft", 17, "", 1, &["Ctrl"]),
    ("Alt", "AltLeft", 18, "", 1, &["Option"]),
    ("Meta", "MetaLeft", 91, "", 1, &["Command", "Cmd", "OS"]),
    ("ContextMenu", "ContextMenu", 93, "", 0, &[]),
    ("CapsLock", "CapsLock", 20, "", 0, &[]),
    ("NumLock", "NumLock", 144, "", 0, &[]),
    ("ScrollLock", "ScrollLock", 145, "", 0, &[]),
    ("Pause", "Pause", 19, "", 0, &[]),
    ("PrintScreen", "PrintScreen", 44, "", 0, &[]),
    ("Space", "Space", 32, " ", 0, &[" "]),
];

/// A key and, when it has one, its shifted form.
fn lookup_exact(name: &str) -> Option<(KeyDescriptor, Option<KeyDescriptor>)> {
    let mut chars = name.chars();
    if let (Some(c), None) = (chars.next(), chars.clone().next()) {
        if c.is_ascii_lowercase() || c.is_ascii_uppercase() {
            let lower = c.to_ascii_lowercase();
            let upper = c.to_ascii_uppercase();
            let code = format!("Key{upper}");
            let key_code = upper as u32;
            let base = key(&lower.to_string(), &code, key_code, &lower.to_string(), 0);
            let shifted = key(&upper.to_string(), &code, key_code, &upper.to_string(), 0);
            return Some(if c.is_ascii_uppercase() {
                (shifted.clone(), Some(shifted))
            } else {
                (base, Some(shifted))
            });
        }
        if let Some(digit) = c.to_digit(10) {
            let code = format!("Digit{c}");
            let shifted_char = SHIFTED_DIGITS[digit as usize];
            return Some((
                key(&c.to_string(), &code, 48 + digit, &c.to_string(), 0),
                Some(key(&shifted_char.to_string(), &code, 48 + digit, &shifted_char.to_string(), 0)),
            ));
        }
        if let Some(position) = SHIFTED_DIGITS.iter().position(|shifted| *shifted == c) {
            let digit = position as u32;
            let descriptor = key(&c.to_string(), &format!("Digit{digit}"), 48 + digit, &c.to_string(), 0);
            return Some((descriptor.clone(), Some(descriptor)));
        }
        for (base, shifted, code, key_code) in PUNCTUATION {
            if c == *base {
                return Some((
                    key(&base.to_string(), code, *key_code, &base.to_string(), 0),
                    Some(key(&shifted.to_string(), code, *key_code, &shifted.to_string(), 0)),
                ));
            }
            if c == *shifted {
                let descriptor = key(&shifted.to_string(), code, *key_code, &shifted.to_string(), 0);
                return Some((descriptor.clone(), Some(descriptor)));
            }
        }
    }
    for (name_key, code, key_code, text, location, aliases) in NAMED {
        if name == *name_key || name == *code || aliases.contains(&name) {
            return Some((key(name_key, code, *key_code, text, *location), None));
        }
    }
    if let Some(number) = name.strip_prefix('F').and_then(|n| n.parse::<u32>().ok())
        && (1..=12).contains(&number)
    {
        return Some((key(name, name, 111 + number, "", 0), None));
    }
    if let Some(digit) = name.strip_prefix("Numpad").and_then(|n| n.parse::<u32>().ok())
        && digit <= 9
    {
        return Some((key(&digit.to_string(), name, 96 + digit, &digit.to_string(), 3), None));
    }
    for (code, key_code, text) in [
        ("NumpadMultiply", 106, "*"),
        ("NumpadAdd", 107, "+"),
        ("NumpadSubtract", 109, "-"),
        ("NumpadDecimal", 110, "."),
        ("NumpadDivide", 111, "/"),
        ("NumpadEnter", 13, "\r"),
    ] {
        if name == code {
            return Some((key(text, code, key_code, text, 3), None));
        }
    }
    None
}

/// `ControlOrMeta` resolves to Meta on macOS and Control elsewhere.
fn resolve_smart_modifier(name: &str) -> String {
    if name.chars().count() == 1 {
        return name.to_owned();
    }
    let trimmed = name.trim();
    if trimmed.eq_ignore_ascii_case("controlormeta") {
        return if cfg!(target_os = "macos") { "Meta" } else { "Control" }.to_owned();
    }
    trimmed.to_owned()
}

fn lookup(name: &str) -> Option<(KeyDescriptor, Option<KeyDescriptor>)> {
    let name = resolve_smart_modifier(name);
    lookup_exact(&name)
        .or_else(|| {
            let mut chars = name.chars();
            let first = chars.next()?;
            let capitalized: String = first.to_uppercase().chain(chars.flat_map(char::to_lowercase)).collect();
            lookup_exact(&capitalized)
        })
        .or_else(|| (name.chars().count() > 1).then(|| lookup_exact(&name.to_lowercase())).flatten())
}

/// Whether `name` is a known key.
pub fn is_known(name: &str) -> bool {
    lookup(name).is_some()
}

pub const MODIFIERS: [&str; 4] = ["Alt", "Control", "Meta", "Shift"];

/// The canonical modifier name of `name`, if it is one.
pub fn modifier_name(name: &str) -> Option<&'static str> {
    let descriptor = lookup(name)?.0;
    MODIFIERS.iter().copied().find(|modifier| *modifier == descriptor.key)
}

/// CDP modifier bit mask (Alt=1, Control=2, Meta=4, Shift=8).
pub fn modifier_mask(modifiers: &[&str]) -> u32 {
    modifiers.iter().fold(0, |mask, modifier| {
        mask | match *modifier {
            "Alt" => 1,
            "Control" => 2,
            "Meta" => 4,
            "Shift" => 8,
            _ => 0,
        }
    })
}

/// The descriptor for `name` with modifiers held: Shift picks the shifted form, and any other
/// held modifier suppresses the text (omowright `resolveKeyDescriptor`).
pub fn resolve(name: &str, held: &[&str]) -> Result<KeyDescriptor, String> {
    let (base, shifted) = lookup(name).ok_or_else(|| format!("Unknown key: {name}"))?;
    let shift = held.contains(&"Shift");
    let mut descriptor = match (shift, shifted) {
        (true, Some(shifted)) => shifted,
        _ => base,
    };
    let only_shift = held.iter().all(|modifier| *modifier == "Shift");
    if !descriptor.text.is_empty() && !held.is_empty() && !only_shift {
        descriptor.text.clear();
    }
    Ok(descriptor)
}

/// `"Control+Shift+A"` → `["Control", "Shift", "A"]`; a lone or trailing `+` is the plus key
/// (omowright `splitKeyCombo`).
pub fn split_combo(combo: &str) -> Vec<String> {
    if combo.chars().count() == 1 && is_known(combo) {
        return vec![combo.to_owned()];
    }
    let trimmed = combo.trim();
    let mut parts = Vec::new();
    let mut current = String::new();
    for c in trimmed.chars() {
        if c != '+' {
            current.push(c);
            continue;
        }
        if !current.trim().is_empty() {
            parts.push(current.trim().to_owned());
            current.clear();
            continue;
        }
        parts.push("+".to_owned());
    }
    if !current.trim().is_empty() {
        parts.push(current.trim().to_owned());
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_digits_and_punctuation_have_codes_and_shifted_forms() {
        let a = resolve("a", &[]).ok();
        assert_eq!(a.as_ref().map(|k| (k.code.as_str(), k.key_code, k.text.as_str())), Some(("KeyA", 65, "a")));
        let shifted = resolve("a", &["Shift"]).ok();
        assert_eq!(shifted.map(|k| k.text), Some("A".into()));
        assert_eq!(resolve("A", &[]).ok().map(|k| k.key), Some("A".into()));
        assert_eq!(resolve("1", &["Shift"]).ok().map(|k| k.key), Some("!".into()));
        assert_eq!(resolve("?", &[]).ok().map(|k| (k.code, k.key_code)), Some(("Slash".into(), 191)));
    }

    #[test]
    fn named_keys_and_aliases_resolve() {
        assert_eq!(resolve("Return", &[]).ok().map(|k| (k.key, k.text)), Some(("Enter".into(), "\r".into())));
        assert_eq!(resolve("esc", &[]).ok().map(|k| k.key), Some("Escape".into()));
        assert_eq!(resolve("ctrl", &[]).ok().map(|k| (k.key, k.location)), Some(("Control".into(), 1)));
        assert_eq!(resolve("F5", &[]).ok().map(|k| k.key_code), Some(116));
        assert_eq!(resolve("Numpad7", &[]).ok().map(|k| (k.key, k.location)), Some(("7".into(), 3)));
        assert_eq!(resolve("Space", &[]).ok().map(|k| k.text), Some(" ".into()));
        assert!(resolve("Hyper", &[]).is_err());
    }

    #[test]
    fn modifiers_suppress_text_except_shift() {
        assert_eq!(resolve("a", &["Control"]).ok().map(|k| k.text), Some(String::new()));
        assert_eq!(modifier_mask(&["Control", "Shift"]), 10);
        assert_eq!(modifier_name("cmd"), Some("Meta"));
        assert_eq!(modifier_name("a"), None);
        let smart = modifier_name("ControlOrMeta");
        assert!(matches!(smart, Some("Meta" | "Control")));
    }

    #[test]
    fn combos_split_on_plus() {
        assert_eq!(split_combo("Control+Shift+a"), vec!["Control", "Shift", "a"]);
        assert_eq!(split_combo("+"), vec!["+"]);
        assert_eq!(split_combo("Control++"), vec!["Control", "+"]);
        assert_eq!(split_combo(" Enter "), vec!["Enter"]);
        assert!(split_combo("  ").is_empty());
    }
}
