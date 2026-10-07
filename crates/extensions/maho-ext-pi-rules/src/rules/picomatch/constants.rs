//! Port of the pinned picomatch `lib/constants.js` (picomatch 4.0.5, MIT).
//!
//! Every value mirrors the pinned source so the Rust grammar and the JS
//! grammar share one table of record. The rules matcher only ever compiles
//! with `bash: true, dot: true` on POSIX-normalized paths, but both platform
//! tables are ported because the pinned `globChars(win32)` builds the Windows
//! table structurally from the POSIX one.

/// `MAX_LENGTH` — maximum pattern length in UTF-16 code units.
pub const MAX_LENGTH: usize = 1024 * 64;

/// `DEFAULT_MAX_EXTGLOB_RECURSION`.
pub const DEFAULT_MAX_EXTGLOB_RECURSION: i64 = 0;

/// Platform glob-regex character table (`POSIX_CHARS` / `WINDOWS_CHARS`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GlobChars {
    pub dot_literal: String,
    pub plus_literal: String,
    pub qmark_literal: String,
    pub slash_literal: String,
    pub one_char: String,
    pub qmark: String,
    pub end_anchor: String,
    pub dots_slash: String,
    pub no_dot: String,
    pub no_dots: String,
    pub no_dot_slash: String,
    pub no_dots_slash: String,
    pub qmark_no_dot: String,
    pub star: String,
    pub start_anchor: String,
    pub sep: char,
}

/// `constants.globChars(win32)`.
#[must_use]
pub fn glob_chars(win32: bool) -> GlobChars {
    if win32 {
        GlobChars {
            dot_literal: "\\.".into(),
            plus_literal: "\\+".into(),
            qmark_literal: "\\?".into(),
            slash_literal: "[\\\\/]".into(),
            one_char: "(?=.)".into(),
            qmark: "[^\\\\/]".into(),
            end_anchor: "(?:[\\\\/]|$)".into(),
            dots_slash: "\\.{1,2}(?:[\\\\/]|$)".into(),
            no_dot: "(?!\\.)".into(),
            no_dots: "(?!(?:^|[\\\\/])\\.{1,2}(?:[\\\\/]|$))".into(),
            no_dot_slash: "(?!\\.{0,1}(?:[\\\\/]|$))".into(),
            no_dots_slash: "(?!\\.{1,2}(?:[\\\\/]|$))".into(),
            qmark_no_dot: "[^.\\\\/]".into(),
            star: "[^\\\\/]*?".into(),
            start_anchor: "(?:^|[\\\\/])".into(),
            sep: '\\',
        }
    } else {
        GlobChars {
            dot_literal: "\\.".into(),
            plus_literal: "\\+".into(),
            qmark_literal: "\\?".into(),
            slash_literal: "\\/".into(),
            one_char: "(?=.)".into(),
            qmark: "[^/]".into(),
            end_anchor: "(?:\\/|$)".into(),
            dots_slash: "\\.{1,2}(?:\\/|$)".into(),
            no_dot: "(?!\\.)".into(),
            no_dots: "(?!(?:^|\\/)\\.{1,2}(?:\\/|$))".into(),
            no_dot_slash: "(?!\\.{0,1}(?:\\/|$))".into(),
            no_dots_slash: "(?!\\.{1,2}(?:\\/|$))".into(),
            qmark_no_dot: "[^./]".into(),
            star: "[^/]*?".into(),
            start_anchor: "(?:^|\\/)".into(),
            sep: '/',
        }
    }
}

/// `constants.POSIX_REGEX_SOURCE` — POSIX character-class bodies.
#[must_use]
pub fn posix_regex_source(name: &str) -> Option<&'static str> {
    Some(match name {
        "alnum" => "a-zA-Z0-9",
        "alpha" => "a-zA-Z",
        "ascii" => "\\x00-\\x7F",
        "blank" => " \\t",
        "cntrl" => "\\x00-\\x1F\\x7F",
        "digit" => "0-9",
        "graph" => "\\x21-\\x7E",
        "lower" => "a-z",
        "print" => "\\x20-\\x7E ",
        "punct" => "\\-!\"#$%&'()\\*+,./:;<=>?@[\\]^_`{|}~",
        "space" => " \\t\\r\\n\\x0B\\x0C",
        "upper" => "A-Z",
        "word" => "A-Za-z0-9_",
        "xdigit" => "A-Fa-f0-9",
        _ => return None,
    })
}

/// `constants.REPLACEMENTS` — whole-pattern rewrites that reduce parse time.
#[must_use]
pub fn replacement(input: &str) -> Option<&'static str> {
    match input {
        "***" => Some("*"),
        "**/**" => Some("**"),
        "**/**/**" => Some("**"),
        _ => None,
    }
}

/// `constants.extglobChars(chars)` — extglob open/close regex fragments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtglobChars {
    pub open: String,
    pub close: String,
}

/// `constants.extglobChars(chars)` keyed by the extglob prefix character.
#[must_use]
pub fn extglob_chars(value: char, chars: &GlobChars) -> Option<ExtglobChars> {
    Some(match value {
        '!' => ExtglobChars { open: "(?:(?!(?:".into(), close: format!(")){})", chars.star) },
        '?' => ExtglobChars { open: "(?:".into(), close: ")?".into() },
        '+' => ExtglobChars { open: "(?:".into(), close: ")+".into() },
        '*' => ExtglobChars { open: "(?:".into(), close: ")*".into() },
        '@' => ExtglobChars { open: "(?:".into(), close: ")".into() },
        _ => return None,
    })
}

/// Character codes from the pinned `constants.js`, used by the scanner and parser.
pub mod codes {
    pub const CHAR_0: u32 = 48;
    pub const CHAR_9: u32 = 57;
    pub const CHAR_UPPERCASE_A: u32 = 65;
    pub const CHAR_LOWERCASE_A: u32 = 97;
    pub const CHAR_UPPERCASE_Z: u32 = 90;
    pub const CHAR_LOWERCASE_Z: u32 = 122;
    pub const CHAR_LEFT_PARENTHESES: u32 = 40;
    pub const CHAR_RIGHT_PARENTHESES: u32 = 41;
    pub const CHAR_ASTERISK: u32 = 42;
    pub const CHAR_AMPERSAND: u32 = 38;
    pub const CHAR_AT: u32 = 64;
    pub const CHAR_BACKWARD_SLASH: u32 = 92;
    pub const CHAR_CARRIAGE_RETURN: u32 = 13;
    pub const CHAR_CIRCUMFLEX_ACCENT: u32 = 94;
    pub const CHAR_COLON: u32 = 58;
    pub const CHAR_COMMA: u32 = 44;
    pub const CHAR_DOT: u32 = 46;
    pub const CHAR_DOUBLE_QUOTE: u32 = 34;
    pub const CHAR_EQUAL: u32 = 61;
    pub const CHAR_EXCLAMATION_MARK: u32 = 33;
    pub const CHAR_FORM_FEED: u32 = 12;
    pub const CHAR_FORWARD_SLASH: u32 = 47;
    pub const CHAR_GRAVE_ACCENT: u32 = 96;
    pub const CHAR_HASH: u32 = 35;
    pub const CHAR_HYPHEN_MINUS: u32 = 45;
    pub const CHAR_LEFT_ANGLE_BRACKET: u32 = 60;
    pub const CHAR_LEFT_CURLY_BRACE: u32 = 123;
    pub const CHAR_LEFT_SQUARE_BRACKET: u32 = 91;
    pub const CHAR_LINE_FEED: u32 = 10;
    pub const CHAR_NO_BREAK_SPACE: u32 = 160;
    pub const CHAR_PERCENT: u32 = 37;
    pub const CHAR_PLUS: u32 = 43;
    pub const CHAR_QUESTION_MARK: u32 = 63;
    pub const CHAR_RIGHT_ANGLE_BRACKET: u32 = 62;
    pub const CHAR_RIGHT_CURLY_BRACE: u32 = 125;
    pub const CHAR_RIGHT_SQUARE_BRACKET: u32 = 93;
    pub const CHAR_SEMICOLON: u32 = 59;
    pub const CHAR_SINGLE_QUOTE: u32 = 39;
    pub const CHAR_SPACE: u32 = 32;
    pub const CHAR_TAB: u32 = 9;
    pub const CHAR_UNDERSCORE: u32 = 95;
    pub const CHAR_VERTICAL_LINE: u32 = 124;
    pub const CHAR_ZERO_WIDTH_NOBREAK_SPACE: u32 = 65279;
}
