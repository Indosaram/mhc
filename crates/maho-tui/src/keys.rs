//! Keyboard input handling for terminal applications (port of senpi `keys.ts`).
//!
//! Supports both legacy terminal sequences and the Kitty keyboard protocol.
//! See: https://sw.kovidgoyal.net/kitty/keyboard-protocol/
//!
//! API:
//! - [`matches_key`] - check if input matches a key identifier
//! - [`parse_key`] - parse input and return the key identifier
//! - [`Key`] - helpers for building key identifiers

use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, Ordering};

use regex::Regex;

use crate::process_env;

#[cfg(not(test))]
static KITTY_PROTOCOL_ACTIVE: AtomicBool = AtomicBool::new(false);

// Unit tests run on parallel threads of one process; a per-thread flag keeps each test's
// protocol state isolated (JS has a single module-level flag and a single thread).
#[cfg(test)]
thread_local! {
    static KITTY_PROTOCOL_ACTIVE: AtomicBool = const { AtomicBool::new(false) };
}

/// Set the global Kitty keyboard protocol state (called by the terminal after detection).
pub fn set_kitty_protocol_active(active: bool) {
    #[cfg(not(test))]
    KITTY_PROTOCOL_ACTIVE.store(active, Ordering::SeqCst);
    #[cfg(test)]
    KITTY_PROTOCOL_ACTIVE.with(|flag| flag.store(active, Ordering::SeqCst));
}

pub fn is_kitty_protocol_active() -> bool {
    #[cfg(not(test))]
    return KITTY_PROTOCOL_ACTIVE.load(Ordering::SeqCst);
    #[cfg(test)]
    KITTY_PROTOCOL_ACTIVE.with(|flag| flag.load(Ordering::SeqCst))
}

fn kitty_active() -> bool {
    is_kitty_protocol_active()
}

/// A key identifier such as `"ctrl+c"`, `"escape"` or `"shift+ctrl+p"`.
pub type KeyId = String;

/// Helpers for building key identifiers, mirroring senpi's `Key` object.
pub struct Key;

macro_rules! key_consts {
    ($($name:ident = $value:expr),* $(,)?) => {
        $(pub const $name: &'static str = $value;)*
    };
}

macro_rules! key_mods {
    ($($name:ident = $prefix:expr),* $(,)?) => {
        $(pub fn $name(key: &str) -> String { format!(concat!($prefix, "{}"), key) })*
    };
}

#[allow(non_upper_case_globals)]
impl Key {
    key_consts!(
        escape = "escape",
        esc = "esc",
        enter = "enter",
        return_ = "return",
        tab = "tab",
        space = "space",
        backspace = "backspace",
        delete = "delete",
        insert = "insert",
        clear = "clear",
        home = "home",
        end = "end",
        pageUp = "pageUp",
        pageDown = "pageDown",
        up = "up",
        down = "down",
        left = "left",
        right = "right",
        f1 = "f1",
        f2 = "f2",
        f3 = "f3",
        f4 = "f4",
        f5 = "f5",
        f6 = "f6",
        f7 = "f7",
        f8 = "f8",
        f9 = "f9",
        f10 = "f10",
        f11 = "f11",
        f12 = "f12",
        backtick = "`",
        hyphen = "-",
        equals = "=",
        leftbracket = "[",
        rightbracket = "]",
        backslash = "\\",
        semicolon = ";",
        quote = "'",
        comma = ",",
        period = ".",
        slash = "/",
        exclamation = "!",
        at = "@",
        hash = "#",
        dollar = "$",
        percent = "%",
        caret = "^",
        ampersand = "&",
        asterisk = "*",
        leftparen = "(",
        rightparen = ")",
        underscore = "_",
        plus = "+",
        pipe = "|",
        tilde = "~",
        leftbrace = "{",
        rightbrace = "}",
        colon = ":",
        lessthan = "<",
        greaterthan = ">",
        question = "?",
    );
    key_mods!(
        ctrl = "ctrl+",
        shift = "shift+",
        alt = "alt+",
        super_ = "super+",
        ctrl_shift = "ctrl+shift+",
        shift_ctrl = "shift+ctrl+",
        ctrl_alt = "ctrl+alt+",
        alt_ctrl = "alt+ctrl+",
        shift_alt = "shift+alt+",
        alt_shift = "alt+shift+",
        ctrl_super = "ctrl+super+",
        super_ctrl = "super+ctrl+",
        shift_super = "shift+super+",
        super_shift = "super+shift+",
        alt_super = "alt+super+",
        super_alt = "super+alt+",
        ctrl_shift_alt = "ctrl+shift+alt+",
        ctrl_shift_super = "ctrl+shift+super+",
    );
}

const SYMBOL_KEYS: &[char] = &[
    '`', '-', '=', '[', ']', '\\', ';', '\'', ',', '.', '/', '!', '@', '#', '$', '%', '^', '&',
    '*', '(', ')', '_', '+', '|', '~', '{', '}', ':', '<', '>', '?',
];

fn is_symbol_key(key: &str) -> bool {
    let mut chars = key.chars();
    matches!((chars.next(), chars.next()), (Some(c), None) if SYMBOL_KEYS.contains(&c))
}

/// `SYMBOL_KEYS.has(String.fromCharCode(cp))`: fromCharCode truncates to a UTF-16 unit.
fn is_symbol_code_unit(cp: i64) -> bool {
    let unit = cp.rem_euclid(0x10000) as u32;
    char::from_u32(unit).is_some_and(|c| SYMBOL_KEYS.contains(&c))
}

mod modifiers {
    pub const SHIFT: i64 = 1;
    pub const ALT: i64 = 2;
    pub const CTRL: i64 = 4;
    pub const SUPER: i64 = 8;
}
use modifiers::{ALT, CTRL, SHIFT, SUPER};

const LOCK_MASK: i64 = 64 + 128;

const CP_ESCAPE: i64 = 27;
const CP_TAB: i64 = 9;
const CP_ENTER: i64 = 13;
const CP_SPACE: i64 = 32;
const CP_BACKSPACE: i64 = 127;
const CP_KP_ENTER: i64 = 57414;

const ARROW_UP: i64 = -1;
const ARROW_DOWN: i64 = -2;
const ARROW_RIGHT: i64 = -3;
const ARROW_LEFT: i64 = -4;

const FN_DELETE: i64 = -10;
const FN_INSERT: i64 = -11;
const FN_PAGE_UP: i64 = -12;
const FN_PAGE_DOWN: i64 = -13;
const FN_HOME: i64 = -14;
const FN_END: i64 = -15;

fn normalize_kitty_functional_codepoint(codepoint: i64) -> i64 {
    match codepoint {
        57399 => 48,
        57400 => 49,
        57401 => 50,
        57402 => 51,
        57403 => 52,
        57404 => 53,
        57405 => 54,
        57406 => 55,
        57407 => 56,
        57408 => 57,
        57409 => 46,
        57410 => 47,
        57411 => 42,
        57412 => 45,
        57413 => 43,
        57415 => 61,
        57416 => 44,
        57417 => ARROW_LEFT,
        57418 => ARROW_RIGHT,
        57419 => ARROW_UP,
        57420 => ARROW_DOWN,
        57421 => FN_PAGE_UP,
        57422 => FN_PAGE_DOWN,
        57423 => FN_HOME,
        57424 => FN_END,
        57425 => FN_INSERT,
        57426 => FN_DELETE,
        other => other,
    }
}

fn normalize_shifted_letter_identity_codepoint(codepoint: i64, modifier: i64) -> i64 {
    let effective = modifier & !LOCK_MASK;
    if (effective & SHIFT) != 0 && (65..=90).contains(&codepoint) {
        return codepoint + 32;
    }
    codepoint
}

#[derive(Clone, Copy)]
enum LegacyKey {
    Up,
    Down,
    Right,
    Left,
    Home,
    End,
    Insert,
    Delete,
    PageUp,
    PageDown,
    Clear,
}

fn legacy_key_sequences(key: LegacyKey) -> &'static [&'static str] {
    match key {
        LegacyKey::Up => &["\x1b[A", "\x1bOA"],
        LegacyKey::Down => &["\x1b[B", "\x1bOB"],
        LegacyKey::Right => &["\x1b[C", "\x1bOC"],
        LegacyKey::Left => &["\x1b[D", "\x1bOD"],
        LegacyKey::Home => &["\x1b[H", "\x1bOH", "\x1b[1~", "\x1b[7~"],
        LegacyKey::End => &["\x1b[F", "\x1bOF", "\x1b[4~", "\x1b[8~"],
        LegacyKey::Insert => &["\x1b[2~"],
        LegacyKey::Delete => &["\x1b[3~"],
        LegacyKey::PageUp => &["\x1b[5~", "\x1b[[5~"],
        LegacyKey::PageDown => &["\x1b[6~", "\x1b[[6~"],
        LegacyKey::Clear => &["\x1b[E", "\x1bOE"],
    }
}

fn legacy_function_key_sequences(key: &str) -> &'static [&'static str] {
    match key {
        "f1" => &["\x1bOP", "\x1b[11~", "\x1b[[A"],
        "f2" => &["\x1bOQ", "\x1b[12~", "\x1b[[B"],
        "f3" => &["\x1bOR", "\x1b[13~", "\x1b[[C"],
        "f4" => &["\x1bOS", "\x1b[14~", "\x1b[[D"],
        "f5" => &["\x1b[15~", "\x1b[[E"],
        "f6" => &["\x1b[17~"],
        "f7" => &["\x1b[18~"],
        "f8" => &["\x1b[19~"],
        "f9" => &["\x1b[20~"],
        "f10" => &["\x1b[21~"],
        "f11" => &["\x1b[23~"],
        "f12" => &["\x1b[24~"],
        _ => &[],
    }
}

fn legacy_shift_sequences(key: LegacyKey) -> &'static [&'static str] {
    match key {
        LegacyKey::Up => &["\x1b[a"],
        LegacyKey::Down => &["\x1b[b"],
        LegacyKey::Right => &["\x1b[c"],
        LegacyKey::Left => &["\x1b[d"],
        LegacyKey::Clear => &["\x1b[e"],
        LegacyKey::Insert => &["\x1b[2$"],
        LegacyKey::Delete => &["\x1b[3$"],
        LegacyKey::PageUp => &["\x1b[5$"],
        LegacyKey::PageDown => &["\x1b[6$"],
        LegacyKey::Home => &["\x1b[7$"],
        LegacyKey::End => &["\x1b[8$"],
    }
}

fn legacy_ctrl_sequences(key: LegacyKey) -> &'static [&'static str] {
    match key {
        LegacyKey::Up => &["\x1bOa"],
        LegacyKey::Down => &["\x1bOb"],
        LegacyKey::Right => &["\x1bOc"],
        LegacyKey::Left => &["\x1bOd"],
        LegacyKey::Clear => &["\x1bOe"],
        LegacyKey::Insert => &["\x1b[2^"],
        LegacyKey::Delete => &["\x1b[3^"],
        LegacyKey::PageUp => &["\x1b[5^"],
        LegacyKey::PageDown => &["\x1b[6^"],
        LegacyKey::Home => &["\x1b[7^"],
        LegacyKey::End => &["\x1b[8^"],
    }
}

fn legacy_sequence_key_id(data: &str) -> Option<&'static str> {
    Some(match data {
        "\x1bOA" => "up",
        "\x1bOB" => "down",
        "\x1bOC" => "right",
        "\x1bOD" => "left",
        "\x1bOH" => "home",
        "\x1bOF" => "end",
        "\x1b[E" => "clear",
        "\x1bOE" => "clear",
        "\x1bOe" => "ctrl+clear",
        "\x1b[e" => "shift+clear",
        "\x1b[2~" => "insert",
        "\x1b[2$" => "shift+insert",
        "\x1b[2^" => "ctrl+insert",
        "\x1b[3$" => "shift+delete",
        "\x1b[3^" => "ctrl+delete",
        "\x1b[[5~" => "pageUp",
        "\x1b[[6~" => "pageDown",
        "\x1b[a" => "shift+up",
        "\x1b[b" => "shift+down",
        "\x1b[c" => "shift+right",
        "\x1b[d" => "shift+left",
        "\x1bOa" => "ctrl+up",
        "\x1bOb" => "ctrl+down",
        "\x1bOc" => "ctrl+right",
        "\x1bOd" => "ctrl+left",
        "\x1b[5$" => "shift+pageUp",
        "\x1b[6$" => "shift+pageDown",
        "\x1b[7$" => "shift+home",
        "\x1b[8$" => "shift+end",
        "\x1b[5^" => "ctrl+pageUp",
        "\x1b[6^" => "ctrl+pageDown",
        "\x1b[7^" => "ctrl+home",
        "\x1b[8^" => "ctrl+end",
        "\x1bOP" => "f1",
        "\x1bOQ" => "f2",
        "\x1bOR" => "f3",
        "\x1bOS" => "f4",
        "\x1b[11~" => "f1",
        "\x1b[12~" => "f2",
        "\x1b[13~" => "f3",
        "\x1b[14~" => "f4",
        "\x1b[[A" => "f1",
        "\x1b[[B" => "f2",
        "\x1b[[C" => "f3",
        "\x1b[[D" => "f4",
        "\x1b[[E" => "f5",
        "\x1b[15~" => "f5",
        "\x1b[17~" => "f6",
        "\x1b[18~" => "f7",
        "\x1b[19~" => "f8",
        "\x1b[20~" => "f9",
        "\x1b[21~" => "f10",
        "\x1b[23~" => "f11",
        "\x1b[24~" => "f12",
        "\x1bb" => "alt+left",
        "\x1bf" => "alt+right",
        "\x1bp" => "alt+up",
        "\x1bn" => "alt+down",
        _ => return None,
    })
}

fn matches_legacy_sequence(data: &str, sequences: &[&str]) -> bool {
    sequences.contains(&data)
}

fn matches_legacy_modifier_sequence(data: &str, key: LegacyKey, modifier: i64) -> bool {
    if modifier == SHIFT {
        return matches_legacy_sequence(data, legacy_shift_sequences(key));
    }
    if modifier == CTRL {
        return matches_legacy_sequence(data, legacy_ctrl_sequences(key));
    }
    false
}

/// Event types from the Kitty keyboard protocol (flag 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyEventType {
    Press,
    Repeat,
    Release,
}

struct ParsedKittySequence {
    codepoint: i64,
    base_layout_key: Option<i64>,
    modifier: i64,
}

struct ParsedModifyOtherKeysSequence {
    codepoint: i64,
    modifier: i64,
}

const RELEASE_MARKERS: [&str; 8] = [":3u", ":3~", ":3A", ":3B", ":3C", ":3D", ":3H", ":3F"];
const REPEAT_MARKERS: [&str; 8] = [":2u", ":2~", ":2A", ":2B", ":2C", ":2D", ":2H", ":2F"];

/// Whether the data is a Kitty key release event (flag 2). Bracketed paste is never a release.
pub fn is_key_release(data: &str) -> bool {
    if data.contains("\x1b[200~") {
        return false;
    }
    RELEASE_MARKERS.iter().any(|m| data.contains(m))
}

/// Whether the data is a Kitty key repeat event (flag 2). Bracketed paste is never a repeat.
pub fn is_key_repeat(data: &str) -> bool {
    if data.contains("\x1b[200~") {
        return false;
    }
    REPEAT_MARKERS.iter().any(|m| data.contains(m))
}

/// `parseInt(str, 10)` for an all-ASCII-digit string; saturates instead of going to float.
fn parse_int(digits: &str) -> i64 {
    digits.parse::<i64>().unwrap_or(i64::MAX)
}

static KITTY_CSI_U_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\x1b\[([0-9]+)(?::([0-9]*))?(?::([0-9]+))?(?:;([0-9]+))?(?::([0-9]+))?u$")
        .expect("valid regex")
});
static ARROW_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\x1b\[1;([0-9]+)(?::([0-9]+))?([ABCD])$").expect("valid regex"));
static FUNC_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\x1b\[([0-9]+)(?:;([0-9]+))?(?::([0-9]+))?~$").expect("valid regex")
});
static HOME_END_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\x1b\[1;([0-9]+)(?::([0-9]+))?([HF])$").expect("valid regex"));
static MODIFY_OTHER_KEYS_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\x1b\[27;([0-9]+);([0-9]+)~$").expect("valid regex"));

fn parse_kitty_sequence(data: &str) -> Option<ParsedKittySequence> {
    if let Some(m) = KITTY_CSI_U_REGEX.captures(data) {
        let codepoint = parse_int(&m[1]);
        let base_layout_key = m.get(3).map(|g| parse_int(g.as_str()));
        let mod_value = m.get(4).map_or(1, |g| parse_int(g.as_str()));
        return Some(ParsedKittySequence {
            codepoint,
            base_layout_key,
            modifier: mod_value - 1,
        });
    }
    if let Some(m) = ARROW_REGEX.captures(data) {
        let mod_value = parse_int(&m[1]);
        let codepoint = match &m[3] {
            "A" => ARROW_UP,
            "B" => ARROW_DOWN,
            "C" => ARROW_RIGHT,
            _ => ARROW_LEFT,
        };
        return Some(ParsedKittySequence {
            codepoint,
            base_layout_key: None,
            modifier: mod_value - 1,
        });
    }
    if let Some(m) = FUNC_REGEX.captures(data) {
        let key_num = parse_int(&m[1]);
        let mod_value = m.get(2).map_or(1, |g| parse_int(g.as_str()));
        let codepoint = match key_num {
            2 => Some(FN_INSERT),
            3 => Some(FN_DELETE),
            5 => Some(FN_PAGE_UP),
            6 => Some(FN_PAGE_DOWN),
            7 => Some(FN_HOME),
            8 => Some(FN_END),
            _ => None,
        };
        if let Some(codepoint) = codepoint {
            return Some(ParsedKittySequence {
                codepoint,
                base_layout_key: None,
                modifier: mod_value - 1,
            });
        }
    }
    if let Some(m) = HOME_END_REGEX.captures(data) {
        let mod_value = parse_int(&m[1]);
        let codepoint = if &m[3] == "H" { FN_HOME } else { FN_END };
        return Some(ParsedKittySequence {
            codepoint,
            base_layout_key: None,
            modifier: mod_value - 1,
        });
    }
    None
}

fn matches_kitty_sequence(data: &str, expected_codepoint: i64, expected_modifier: i64) -> bool {
    let Some(parsed) = parse_kitty_sequence(data) else {
        return false;
    };
    let actual_mod = parsed.modifier & !LOCK_MASK;
    let expected_mod = expected_modifier & !LOCK_MASK;
    if actual_mod != expected_mod {
        return false;
    }

    let normalized_codepoint = normalize_shifted_letter_identity_codepoint(
        normalize_kitty_functional_codepoint(parsed.codepoint),
        parsed.modifier,
    );
    let normalized_expected = normalize_shifted_letter_identity_codepoint(
        normalize_kitty_functional_codepoint(expected_codepoint),
        expected_modifier,
    );
    if normalized_codepoint == normalized_expected {
        return true;
    }

    // Fall back to the base layout key (non-Latin layouts) only when the reported codepoint
    // is not already a Latin letter or known symbol; remapped layouts must not false-match.
    if parsed.base_layout_key == Some(expected_codepoint) {
        let cp = normalized_codepoint;
        let is_latin_letter = (97..=122).contains(&cp);
        if !is_latin_letter && !is_symbol_code_unit(cp) {
            return true;
        }
    }
    false
}

fn parse_modify_other_keys_sequence(data: &str) -> Option<ParsedModifyOtherKeysSequence> {
    let m = MODIFY_OTHER_KEYS_REGEX.captures(data)?;
    Some(ParsedModifyOtherKeysSequence {
        modifier: parse_int(&m[1]) - 1,
        codepoint: parse_int(&m[2]),
    })
}

fn matches_modify_other_keys(data: &str, expected_keycode: i64, expected_modifier: i64) -> bool {
    parse_modify_other_keys_sequence(data)
        .is_some_and(|p| p.codepoint == expected_keycode && p.modifier == expected_modifier)
}

fn is_windows_terminal_session() -> bool {
    let set = |name: &str| process_env::var(name).is_some_and(|v| !v.is_empty());
    set("WT_SESSION") && !set("SSH_CONNECTION") && !set("SSH_CLIENT") && !set("SSH_TTY")
}

/// Raw 0x08 is Ctrl+Backspace on Windows Terminal and plain Backspace elsewhere.
fn matches_raw_backspace(data: &str, expected_modifier: i64) -> bool {
    if data == "\x7f" {
        return expected_modifier == 0;
    }
    if data != "\x08" {
        return false;
    }
    if is_windows_terminal_session() {
        expected_modifier == CTRL
    } else {
        expected_modifier == 0
    }
}

/// Control character for a key (`code & 0x1f`); `-` maps like `_`.
fn raw_ctrl_char(key: &str) -> Option<char> {
    let lower = key.to_lowercase();
    let c = lower.chars().next()?;
    if c.is_ascii_lowercase() || matches!(c, '[' | '\\' | ']' | '_') {
        return char::from_u32(u32::from(c) & 0x1f);
    }
    if c == '-' {
        return Some('\x1f');
    }
    None
}

fn is_digit_key(key: &str) -> bool {
    ("0"..="9").contains(&key)
}

fn matches_printable_modify_other_keys(
    data: &str,
    expected_keycode: i64,
    expected_modifier: i64,
) -> bool {
    if expected_modifier == 0 {
        return false;
    }
    let Some(parsed) = parse_modify_other_keys_sequence(data) else {
        return false;
    };
    if parsed.modifier != expected_modifier {
        return false;
    }
    normalize_shifted_letter_identity_codepoint(parsed.codepoint, parsed.modifier)
        == normalize_shifted_letter_identity_codepoint(expected_keycode, expected_modifier)
}

fn format_key_name_with_modifiers(key_name: &str, modifier: i64) -> Option<String> {
    let effective = modifier & !LOCK_MASK;
    let supported = SHIFT | CTRL | ALT | SUPER;
    if (effective & !supported) != 0 {
        return None;
    }
    let mut mods = Vec::new();
    if effective & SHIFT != 0 {
        mods.push("shift");
    }
    if effective & CTRL != 0 {
        mods.push("ctrl");
    }
    if effective & ALT != 0 {
        mods.push("alt");
    }
    if effective & SUPER != 0 {
        mods.push("super");
    }
    if mods.is_empty() {
        Some(key_name.to_string())
    } else {
        Some(format!("{}+{}", mods.join("+"), key_name))
    }
}

struct ParsedKeyId {
    key: String,
    ctrl: bool,
    shift: bool,
    alt: bool,
    super_: bool,
}

fn parse_key_id(key_id: &str) -> Option<ParsedKeyId> {
    let lower = key_id.to_lowercase();
    let parts: Vec<&str> = lower.split('+').collect();
    let key = *parts.last()?;
    if key.is_empty() {
        return None;
    }
    Some(ParsedKeyId {
        key: key.to_string(),
        ctrl: parts.contains(&"ctrl"),
        shift: parts.contains(&"shift"),
        alt: parts.contains(&"alt"),
        super_: parts.contains(&"super"),
    })
}

fn matches_navigation(data: &str, legacy: LegacyKey, codepoint: i64, modifier: i64) -> bool {
    if modifier == 0 {
        return matches_legacy_sequence(data, legacy_key_sequences(legacy))
            || matches_kitty_sequence(data, codepoint, 0);
    }
    if matches_legacy_modifier_sequence(data, legacy, modifier) {
        return true;
    }
    matches_kitty_sequence(data, codepoint, modifier)
}

/// Match raw terminal input against a key identifier (e.g. `"ctrl+c"`, `"shift+enter"`).
pub fn matches_key(data: &str, key_id: &str) -> bool {
    let Some(parsed) = parse_key_id(key_id) else {
        return false;
    };
    let mut modifier = 0;
    if parsed.shift {
        modifier |= SHIFT;
    }
    if parsed.alt {
        modifier |= ALT;
    }
    if parsed.ctrl {
        modifier |= CTRL;
    }
    if parsed.super_ {
        modifier |= SUPER;
    }
    let key = parsed.key.as_str();
    let kitty = kitty_active();

    match key {
        "escape" | "esc" => {
            if modifier != 0 {
                return false;
            }
            return data == "\x1b"
                || matches_kitty_sequence(data, CP_ESCAPE, 0)
                || matches_modify_other_keys(data, CP_ESCAPE, 0);
        }
        "space" => {
            if !kitty {
                if modifier == CTRL && data == "\x00" {
                    return true;
                }
                if modifier == ALT && data == "\x1b " {
                    return true;
                }
            }
            if modifier == 0 {
                return data == " "
                    || matches_kitty_sequence(data, CP_SPACE, 0)
                    || matches_modify_other_keys(data, CP_SPACE, 0);
            }
            return matches_kitty_sequence(data, CP_SPACE, modifier)
                || matches_modify_other_keys(data, CP_SPACE, modifier);
        }
        "tab" => {
            if modifier == SHIFT {
                return data == "\x1b[Z"
                    || matches_kitty_sequence(data, CP_TAB, SHIFT)
                    || matches_modify_other_keys(data, CP_TAB, SHIFT);
            }
            if modifier == 0 {
                return data == "\t" || matches_kitty_sequence(data, CP_TAB, 0);
            }
            return matches_kitty_sequence(data, CP_TAB, modifier)
                || matches_modify_other_keys(data, CP_TAB, modifier);
        }
        "enter" | "return" => {
            if modifier == SHIFT {
                if matches_kitty_sequence(data, CP_ENTER, SHIFT)
                    || matches_kitty_sequence(data, CP_KP_ENTER, SHIFT)
                {
                    return true;
                }
                if matches_modify_other_keys(data, CP_ENTER, SHIFT) {
                    return true;
                }
                // With Kitty active, `\x1b\r` (kitty mapping) and `\n` (ghostty mapping) are shift+enter.
                if kitty {
                    return data == "\x1b\r" || data == "\n";
                }
                return false;
            }
            if modifier == ALT {
                if matches_kitty_sequence(data, CP_ENTER, ALT)
                    || matches_kitty_sequence(data, CP_KP_ENTER, ALT)
                {
                    return true;
                }
                if matches_modify_other_keys(data, CP_ENTER, ALT) {
                    return true;
                }
                if !kitty {
                    return data == "\x1b\r";
                }
                return false;
            }
            if modifier == 0 {
                return data == "\r"
                    || (!kitty && data == "\n")
                    || data == "\x1bOM"
                    || matches_kitty_sequence(data, CP_ENTER, 0)
                    || matches_kitty_sequence(data, CP_KP_ENTER, 0);
            }
            return matches_kitty_sequence(data, CP_ENTER, modifier)
                || matches_kitty_sequence(data, CP_KP_ENTER, modifier)
                || matches_modify_other_keys(data, CP_ENTER, modifier);
        }
        "backspace" => {
            if modifier == ALT {
                if data == "\x1b\x7f" || data == "\x1b\x08" {
                    return true;
                }
                return matches_kitty_sequence(data, CP_BACKSPACE, ALT)
                    || matches_modify_other_keys(data, CP_BACKSPACE, ALT);
            }
            if modifier == CTRL {
                if matches_raw_backspace(data, CTRL) {
                    return true;
                }
                return matches_kitty_sequence(data, CP_BACKSPACE, CTRL)
                    || matches_modify_other_keys(data, CP_BACKSPACE, CTRL);
            }
            if modifier == 0 {
                return matches_raw_backspace(data, 0)
                    || matches_kitty_sequence(data, CP_BACKSPACE, 0)
                    || matches_modify_other_keys(data, CP_BACKSPACE, 0);
            }
            return matches_kitty_sequence(data, CP_BACKSPACE, modifier)
                || matches_modify_other_keys(data, CP_BACKSPACE, modifier);
        }
        "insert" => return matches_navigation(data, LegacyKey::Insert, FN_INSERT, modifier),
        "delete" => return matches_navigation(data, LegacyKey::Delete, FN_DELETE, modifier),
        "clear" => {
            if modifier == 0 {
                return matches_legacy_sequence(data, legacy_key_sequences(LegacyKey::Clear));
            }
            return matches_legacy_modifier_sequence(data, LegacyKey::Clear, modifier);
        }
        "home" => return matches_navigation(data, LegacyKey::Home, FN_HOME, modifier),
        "end" => return matches_navigation(data, LegacyKey::End, FN_END, modifier),
        "pageup" => return matches_navigation(data, LegacyKey::PageUp, FN_PAGE_UP, modifier),
        "pagedown" => return matches_navigation(data, LegacyKey::PageDown, FN_PAGE_DOWN, modifier),
        "up" => {
            if modifier == ALT {
                return data == "\x1bp" || matches_kitty_sequence(data, ARROW_UP, ALT);
            }
            return matches_navigation(data, LegacyKey::Up, ARROW_UP, modifier);
        }
        "down" => {
            if modifier == ALT {
                return data == "\x1bn" || matches_kitty_sequence(data, ARROW_DOWN, ALT);
            }
            return matches_navigation(data, LegacyKey::Down, ARROW_DOWN, modifier);
        }
        "left" => {
            if modifier == ALT {
                return data == "\x1b[1;3D"
                    || (!kitty && data == "\x1bB")
                    || data == "\x1bb"
                    || matches_kitty_sequence(data, ARROW_LEFT, ALT);
            }
            if modifier == CTRL {
                return data == "\x1b[1;5D"
                    || matches_legacy_modifier_sequence(data, LegacyKey::Left, CTRL)
                    || matches_kitty_sequence(data, ARROW_LEFT, CTRL);
            }
            return matches_navigation(data, LegacyKey::Left, ARROW_LEFT, modifier);
        }
        "right" => {
            if modifier == ALT {
                return data == "\x1b[1;3C"
                    || (!kitty && data == "\x1bF")
                    || data == "\x1bf"
                    || matches_kitty_sequence(data, ARROW_RIGHT, ALT);
            }
            if modifier == CTRL {
                return data == "\x1b[1;5C"
                    || matches_legacy_modifier_sequence(data, LegacyKey::Right, CTRL)
                    || matches_kitty_sequence(data, ARROW_RIGHT, CTRL);
            }
            return matches_navigation(data, LegacyKey::Right, ARROW_RIGHT, modifier);
        }
        "f1" | "f2" | "f3" | "f4" | "f5" | "f6" | "f7" | "f8" | "f9" | "f10" | "f11" | "f12" => {
            if modifier != 0 {
                return false;
            }
            return matches_legacy_sequence(data, legacy_function_key_sequences(key));
        }
        _ => {}
    }

    let is_letter = key.len() == 1 && ("a"..="z").contains(&key);
    let is_digit = key.len() == 1 && is_digit_key(key);
    if key.chars().count() == 1 && (is_letter || is_digit || is_symbol_key(key)) {
        let codepoint = key.chars().next().map_or(0, |c| i64::from(u32::from(c)));
        let raw_ctrl = raw_ctrl_char(key);

        if modifier == CTRL + ALT
            && !kitty
            && let Some(rc) = raw_ctrl
        {
            // Legacy ctrl+alt+key is ESC + control char; otherwise fall through for CSI-u/tmux forms.
            if data
                .strip_prefix('\x1b')
                .is_some_and(|rest| rest.len() == 1 && rest.starts_with(rc))
            {
                return true;
            }
        }

        if modifier == ALT
            && !kitty
            && (is_letter || is_digit || is_symbol_key(key))
            && data.strip_prefix('\x1b') == Some(key)
        {
            return true;
        }

        if modifier == CTRL {
            if let Some(rc) = raw_ctrl
                && data.len() == rc.len_utf8()
                && data.starts_with(rc)
            {
                return true;
            }
            return matches_kitty_sequence(data, codepoint, CTRL)
                || matches_printable_modify_other_keys(data, codepoint, CTRL);
        }

        if modifier == SHIFT + CTRL {
            return matches_kitty_sequence(data, codepoint, SHIFT + CTRL)
                || matches_printable_modify_other_keys(data, codepoint, SHIFT + CTRL);
        }

        if modifier == SHIFT {
            if is_letter && data == key.to_uppercase() {
                return true;
            }
            return matches_kitty_sequence(data, codepoint, SHIFT)
                || matches_printable_modify_other_keys(data, codepoint, SHIFT);
        }

        if modifier != 0 {
            return matches_kitty_sequence(data, codepoint, modifier)
                || matches_printable_modify_other_keys(data, codepoint, modifier);
        }

        return data == key || matches_kitty_sequence(data, codepoint, 0);
    }

    false
}

fn char_string(cp: i64) -> String {
    u32::try_from(cp)
        .ok()
        .and_then(char::from_u32)
        .map(String::from)
        .unwrap_or_default()
}

fn format_parsed_key(
    codepoint: i64,
    modifier: i64,
    base_layout_key: Option<i64>,
) -> Option<String> {
    let normalized = normalize_kitty_functional_codepoint(codepoint);
    let identity = normalize_shifted_letter_identity_codepoint(normalized, modifier);

    // The codepoint is authoritative for Latin letters, digits and symbols (remapped layouts);
    // only other codepoints fall back to the base layout key.
    let is_latin_letter = (97..=122).contains(&identity);
    let is_digit = (48..=57).contains(&identity);
    let effective = if is_latin_letter || is_digit || is_symbol_code_unit(identity) {
        identity
    } else {
        base_layout_key.unwrap_or(identity)
    };

    let key_name: String = match effective {
        CP_ESCAPE => "escape".into(),
        CP_TAB => "tab".into(),
        CP_ENTER | CP_KP_ENTER => "enter".into(),
        CP_SPACE => "space".into(),
        CP_BACKSPACE => "backspace".into(),
        FN_DELETE => "delete".into(),
        FN_INSERT => "insert".into(),
        FN_HOME => "home".into(),
        FN_END => "end".into(),
        FN_PAGE_UP => "pageUp".into(),
        FN_PAGE_DOWN => "pageDown".into(),
        ARROW_UP => "up".into(),
        ARROW_DOWN => "down".into(),
        ARROW_LEFT => "left".into(),
        ARROW_RIGHT => "right".into(),
        48..=57 | 97..=122 => char_string(effective),
        _ if is_symbol_code_unit(effective) => char_string(effective.rem_euclid(0x10000)),
        _ => return None,
    };
    format_key_name_with_modifiers(&key_name, modifier)
}

/// Parse raw terminal input into a key identifier, if recognized.
pub fn parse_key(data: &str) -> Option<String> {
    if let Some(kitty) = parse_kitty_sequence(data) {
        return format_parsed_key(kitty.codepoint, kitty.modifier, kitty.base_layout_key);
    }
    if let Some(mok) = parse_modify_other_keys_sequence(data) {
        return format_parsed_key(mok.codepoint, mok.modifier, None);
    }

    let kitty = kitty_active();
    // With Kitty active, `\x1b\r` and `\n` are custom terminal mappings for shift+enter.
    if kitty && (data == "\x1b\r" || data == "\n") {
        return Some("shift+enter".into());
    }

    if let Some(id) = legacy_sequence_key_id(data) {
        return Some(id.into());
    }

    let fixed = match data {
        "\x1b" => Some("escape"),
        "\x1c" => Some("ctrl+\\"),
        "\x1d" => Some("ctrl+]"),
        "\x1f" => Some("ctrl+-"),
        "\x1b\x1b" => Some("ctrl+alt+["),
        "\x1b\x1c" => Some("ctrl+alt+\\"),
        "\x1b\x1d" => Some("ctrl+alt+]"),
        "\x1b\x1f" => Some("ctrl+alt+-"),
        "\t" => Some("tab"),
        "\r" | "\x1bOM" => Some("enter"),
        "\n" if !kitty => Some("enter"),
        "\x00" => Some("ctrl+space"),
        " " => Some("space"),
        "\x7f" => Some("backspace"),
        "\x08" => Some(if is_windows_terminal_session() {
            "ctrl+backspace"
        } else {
            "backspace"
        }),
        "\x1b[Z" => Some("shift+tab"),
        "\x1b\r" if !kitty => Some("alt+enter"),
        "\x1b " if !kitty => Some("alt+space"),
        "\x1b\x7f" | "\x1b\x08" => Some("alt+backspace"),
        "\x1bB" if !kitty => Some("alt+left"),
        "\x1bF" if !kitty => Some("alt+right"),
        _ => None,
    };
    if let Some(id) = fixed {
        return Some(id.into());
    }

    let units: Vec<u16> = data.encode_utf16().collect();
    if !kitty && units.len() == 2 && units[0] == 0x1b {
        let code = units[1];
        if (1..=26).contains(&code) {
            return Some(format!("ctrl+alt+{}", char_string(i64::from(code) + 96)));
        }
        let key = char_string(i64::from(code));
        if code.is_ascii_lowercase_u16() || (48..=57).contains(&code) || is_symbol_key(&key) {
            return Some(format!("alt+{key}"));
        }
    }

    let nav = match data {
        "\x1b[A" => Some("up"),
        "\x1b[B" => Some("down"),
        "\x1b[C" => Some("right"),
        "\x1b[D" => Some("left"),
        "\x1b[H" | "\x1bOH" => Some("home"),
        "\x1b[F" | "\x1bOF" => Some("end"),
        "\x1b[3~" => Some("delete"),
        "\x1b[5~" => Some("pageUp"),
        "\x1b[6~" => Some("pageDown"),
        _ => None,
    };
    if let Some(id) = nav {
        return Some(id.into());
    }

    if units.len() == 1 {
        let code = units[0];
        if (1..=26).contains(&code) {
            return Some(format!("ctrl+{}", char_string(i64::from(code) + 96)));
        }
        if (32..=126).contains(&code) {
            return Some(data.to_string());
        }
    }
    None
}

trait AsciiLowerU16 {
    fn is_ascii_lowercase_u16(&self) -> bool;
}

impl AsciiLowerU16 for u16 {
    fn is_ascii_lowercase_u16(&self) -> bool {
        (97..=122).contains(self)
    }
}

const KITTY_PRINTABLE_ALLOWED_MODIFIERS: i64 = SHIFT | LOCK_MASK;

/// Decode a Kitty CSI-u sequence into a printable character (plain or Shift-modified keys only).
pub fn decode_kitty_printable(data: &str) -> Option<String> {
    let m = KITTY_CSI_U_REGEX.captures(data)?;
    let codepoint = parse_int(&m[1]);
    let shifted_key = m
        .get(2)
        .filter(|g| !g.as_str().is_empty())
        .map(|g| parse_int(g.as_str()));
    let mod_value = m.get(4).map_or(1, |g| parse_int(g.as_str()));
    let modifier = mod_value - 1;

    if (modifier & !KITTY_PRINTABLE_ALLOWED_MODIFIERS) != 0 {
        return None;
    }
    if modifier & (ALT | CTRL) != 0 {
        return None;
    }
    let mut effective = codepoint;
    if modifier & SHIFT != 0
        && let Some(shifted) = shifted_key
    {
        effective = shifted;
    }
    effective = normalize_kitty_functional_codepoint(effective);
    if effective < 32 {
        return None;
    }
    from_code_point(effective)
}

/// `String.fromCodePoint`, except lone surrogates (valid in JS strings) are unrepresentable in Rust.
fn from_code_point(cp: i64) -> Option<String> {
    u32::try_from(cp)
        .ok()
        .and_then(char::from_u32)
        .map(String::from)
}

fn decode_modify_other_keys_printable(data: &str) -> Option<String> {
    let parsed = parse_modify_other_keys_sequence(data)?;
    let modifier = parsed.modifier & !LOCK_MASK;
    if (modifier & !SHIFT) != 0 {
        return None;
    }
    if parsed.codepoint < 32 {
        return None;
    }
    from_code_point(parsed.codepoint)
}

/// Decode a printable key from Kitty CSI-u or xterm modifyOtherKeys input.
pub fn decode_printable_key(data: &str) -> Option<String> {
    decode_kitty_printable(data).or_else(|| decode_modify_other_keys_printable(data))
}

#[cfg(test)]
#[path = "keys_tests.rs"]
mod tests;
