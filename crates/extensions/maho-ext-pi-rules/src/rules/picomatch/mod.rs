//! Port of the pinned picomatch package (4.0.5, MIT): `lib/constants.js`,
//! `lib/utils.js`, `lib/scan.js`, `lib/parse.js` and the `lib/picomatch.js`
//! glue (`makeRe` / `compileRe` / `toRegex` / `test`).
//!
//! The rules matcher consumes this through `compile` + `Compiled::is_match`,
//! which is exactly `picomatch(pattern, { bash: true, dot: true })`.

pub mod constants;
pub mod parse;
pub mod scan;
pub mod utils;

use fancy_regex::Regex;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaxExtglobRecursion {
    Disabled,
    Depth(i64),
}

impl Default for MaxExtglobRecursion {
    fn default() -> Self {
        Self::Depth(constants::DEFAULT_MAX_EXTGLOB_RECURSION)
    }
}

impl MaxExtglobRecursion {
    #[must_use]
    pub fn is_disabled(&self) -> bool {
        matches!(self, Self::Disabled)
    }

    #[must_use]
    pub fn depth(&self) -> i64 {
        match self {
            Self::Depth(value) => *value,
            Self::Disabled => constants::DEFAULT_MAX_EXTGLOB_RECURSION,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Options {
    pub dot: bool,
    pub bash: bool,
    pub capture: bool,
    pub contains: bool,
    pub windows: bool,
    pub noext: Option<bool>,
    pub noextglob: Option<bool>,
    pub nonegate: bool,
    pub noglobstar: bool,
    pub nobrace: bool,
    pub nobracket: bool,
    pub posix: Option<bool>,
    pub strict_slashes: Option<bool>,
    pub strict_brackets: bool,
    pub literal_brackets: Option<bool>,
    pub unescape: bool,
    pub keep_quotes: bool,
    pub regex: Option<bool>,
    pub fastpaths: Option<bool>,
    pub max_length: Option<usize>,
    pub prepend: Option<String>,
    pub max_extglob_recursion: MaxExtglobRecursion,
}


impl Options {
    #[must_use]
    pub fn bash_dot() -> Self {
        Self { bash: true, dot: true, ..Self::default() }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PicomatchError {
    Type(String),
    Syntax(String),
}

impl std::fmt::Display for PicomatchError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Type(message) | Self::Syntax(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for PicomatchError {}

#[derive(Debug)]
pub struct Compiled {
    regex: Regex,
    glob: String,
    negated: bool,
}

impl Compiled {
    #[must_use]
    pub fn is_match(&self, input: &str) -> bool {
        if input.is_empty() {
            return false;
        }
        if input == self.glob {
            return true;
        }
        self.regex.is_match(&utils::js_units(input)).unwrap_or(false)
    }

    #[must_use]
    pub fn glob(&self) -> &str {
        &self.glob
    }

    #[must_use]
    pub fn negated(&self) -> bool {
        self.negated
    }
}

/// Translate a pinned-picomatch regex source into a Rust regex source.
///
/// The source is already in the JS UTF-16 code-unit domain (`utils::js_units`),
/// so `.` becomes the JS `.` character class, and every backslash escape is
/// rewritten to what JS (Annex B, no `u` flag) actually means, because
/// `regex-syntax` gives several of them a different meaning or rejects them:
///
/// * `\d \D \s \S \w \W` and `\b \B` agree and are kept verbatim;
/// * the shared control escapes `\f \n \r \t \v` are emitted as `\x{..}`;
/// * `\xHH` is kept and `\uHHHH` becomes `\u{HHHH}`;
/// * a legacy octal escape (`\0`..`\377`) is emitted as `\x{..}`;
/// * every other `\` + ASCII character -- `\a`, `\z`, `\A`, `\e`, `\p`, `\8`,
///   `\<`, `\>` -- is a JS identity escape and is emitted as the literal
///   character (`\a` is the bell and `\z`/`\A` are text anchors in
///   `regex-syntax`, `\<`/`\>` are word-boundary assertions, and `\0`..`\9` are
///   unsupported backreferences);
/// * a `\` in front of a UTF-16 unit character (a mapped surrogate unit) is
///   dropped, because the Rust engine rejects `\` before a non-ASCII character.
fn js_regex_expression(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    let mut result = String::new();
    let mut class = false;
    let mut index = 0;
    while index < chars.len() {
        let ch = chars[index];
        match ch {
            '\\' => {
                let (emitted, consumed) = js_escape(&chars, index);
                result.push_str(&emitted);
                index += consumed;
            }
            '[' => {
                if class { result.push('\\'); }
                class = true;
                result.push(ch);
                index += 1;
            }
            ']' => {
                if !class { result.push('\\'); }
                class = false;
                result.push(ch);
                index += 1;
            }
            '.' if !class => {
                result.push_str("[^\\n\\r\\u{2028}\\u{2029}]");
                index += 1;
            }
            '&' | '~' if class => {
                result.push_str(&format!("\\x{{{:X}}}", u32::from(ch)));
                index += 1;
            }
            _ => {
                result.push(ch);
                index += 1;
            }
        }
    }
    result
}

/// Translate the JS escape at `chars[start]` (`\`) into the Rust regex source
/// that matches what JS matches, returning the source and the number of pattern
/// characters consumed.
fn js_escape(chars: &[char], start: usize) -> (String, usize) {
    let Some(&escaped) = chars.get(start + 1) else {
        return ("\\".to_string(), 1);
    };
    if !escaped.is_ascii() {
        return (escaped.to_string(), 2);
    }
    match escaped {
        'd' | 'D' | 's' | 'S' | 'w' | 'W' | 'b' | 'B' => (format!("\\{escaped}"), 2),
        'f' => ("\\x{0C}".to_string(), 2),
        'n' => ("\\x{0A}".to_string(), 2),
        'r' => ("\\x{0D}".to_string(), 2),
        't' => ("\\x{09}".to_string(), 2),
        'v' => ("\\x{0B}".to_string(), 2),
        'c' => match chars.get(start + 2) {
            Some(&letter) if letter.is_ascii_alphabetic() => {
                let code = u32::from(letter.to_ascii_uppercase()) - u32::from('A') + 1;
                (format!("\\x{code:02X}"), 3)
            }
            _ => ("c".to_string(), 2),
        },
        'x' => match (chars.get(start + 2), chars.get(start + 3)) {
            (Some(&high), Some(&low)) if high.is_ascii_hexdigit() && low.is_ascii_hexdigit() => {
                (format!("\\x{high}{low}"), 4)
            }
            _ => ("x".to_string(), 2),
        },
        'u' => {
            // Brace escapes are generated mapped-unit regex source, not raw JS escapes.
            if chars.get(start + 2) == Some(&'{') {
                let mut end = start + 3;
                while end < chars.len() && chars[end] != '}' { end += 1; }
                if end < chars.len() {
                    let body: String = chars[start + 2..=end].iter().collect();
                    return (format!("\\u{body}"), end - start + 1);
                }
            }
            let hex: Option<String> = chars
                .get(start + 2..start + 6)
                .filter(|digits| digits.iter().all(char::is_ascii_hexdigit))
                .map(|digits| digits.iter().collect());
            match hex {
                Some(digits) => (format!("\\u{{{digits}}}"), 6),
                None => ("u".to_string(), 2),
            }
        }
        '0'..='7' => {
            let (digits, consumed) = legacy_octal(chars, start);
            let value = u32::from_str_radix(&digits, 8).unwrap_or(0);
            (format!("\\x{{{value:02X}}}"), consumed)
        }
        '8' | '9' | '<' | '>' => (escaped.to_string(), 2),
        _ if escaped.is_ascii_alphanumeric() => (escaped.to_string(), 2),
        _ => (format!("\\{escaped}"), 2),
    }
}

/// JS `LegacyOctalEscapeSequence` (Annex B): up to three octal digits (at most
/// `\377`), or `\0` alone as NUL. Returns the octal digits and the number of
/// pattern characters they span, including the backslash.
fn legacy_octal(chars: &[char], start: usize) -> (String, usize) {
    let first = chars[start + 1];
    let second = chars.get(start + 2).copied().filter(|ch| ('0'..='7').contains(ch));
    let third = chars.get(start + 3).copied().filter(|ch| ('0'..='7').contains(ch));
    let digits = if first == '0' {
        match (second, third) {
            (Some(second), Some(third)) => format!("0{second}{third}"),
            (Some(second), None) => format!("0{second}"),
            (None, _) => "0".to_string(),
        }
    } else {
        match (second, third) {
            (Some(second), Some(third)) if first <= '3' => format!("{first}{second}{third}"),
            (Some(second), _) => format!("{first}{second}"),
            (None, _) => first.to_string(),
        }
    };
    let consumed = 1 + digits.chars().count();
    (digits, consumed)
}

fn to_regex(source: &str) -> Regex {
    let translated = js_regex_expression(source);
    match Regex::new(&translated) {
        Ok(regex) => regex,
        Err(_) => Regex::new("$^").unwrap_or_else(|_| unreachable!("constant never-match regex compiles")),
    }
}

pub fn compile(pattern: &str, options: &Options) -> Result<Compiled, PicomatchError> {
    if pattern.is_empty() {
        return Err(PicomatchError::Type("Expected pattern to be a non-empty string".into()));
    }

    let first = pattern.chars().next();
    let mut fastpath_output = None;
    if options.fastpaths != Some(false) && matches!(first, Some('.') | Some('*')) {
        fastpath_output = parse::fastpaths(pattern, options);
    }

    let (source, negated) = match fastpath_output {
        Some(source) => (source, false),
        None => {
            let state = parse::parse(pattern, options)?;
            (state.output.clone(), state.negated)
        }
    };

    let (prepend, append) = if options.contains { ("", "") } else { ("^", "$") };
    let mut regex_source = format!("{prepend}(?:{source}){append}");
    if negated {
        regex_source = format!("^(?!{regex_source}).*$");
    }

    Ok(Compiled { regex: to_regex(&regex_source), glob: pattern.to_string(), negated })
}

#[must_use]
pub fn is_match(pattern: &str, input: &str, options: &Options) -> bool {
    compile(pattern, options).is_ok_and(|compiled| compiled.is_match(input))
}
