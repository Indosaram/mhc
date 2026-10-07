//! Port of the pinned picomatch `lib/parse.js` (picomatch 4.0.5, MIT).
//!
//! The parser is ported method-for-method so an upstream diff translates
//! mechanically. Every JS string is treated as a sequence of UTF-16 code units:
//! the pattern is mapped through `utils::js_units` before it is tokenized (and
//! `fastpaths` does the same), so `input[i]`, `.length`, `.slice()` and the
//! `=== 1` tests of the pinned extglob analysis all see UTF-16 units even when
//! the pattern contains astral characters.

use super::constants::{
    DEFAULT_MAX_EXTGLOB_RECURSION, ExtglobChars, MAX_LENGTH, extglob_chars, glob_chars, posix_regex_source,
    replacement,
};
use super::utils::{escape_regex, has_regex_chars, js_length, js_unit_order, js_units, remove_prefix, unit_value};
use super::{Options, PicomatchError};

#[derive(Clone, Debug, Default)]
struct Token {
    kind: String,
    value: String,
    output: Option<String>,
    prev: Option<usize>,
    star: bool,
    extglob: bool,
    posix: bool,
    suffix: Option<String>,
}

#[derive(Clone, Debug, Default)]
struct BraceToken {
    output_index: usize,
    tokens_index: usize,
    dots: bool,
    comma: bool,
}

#[derive(Clone, Debug, Default)]
struct ExtglobToken {
    kind: String,
    open: String,
    close: String,
    inner: String,
    prev: usize,
    parens: i64,
    output: String,
    start_index: isize,
    tokens_index: usize,
}

#[derive(Clone, Debug, Default)]
struct Analysis {
    risky: bool,
    safe_output: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct ParseState {
    pub input: String,
    pub output: String,
    pub prefix: String,
    pub negated: bool,
    pub negated_extglob: bool,
    pub globstar: bool,
}

fn drop_chars(value: &mut String, count: usize) {
    if count == 0 {
        return;
    }
    let keep = value.chars().count().saturating_sub(count);
    let truncated: String = value.chars().take(keep).collect();
    *value = truncated;
}

fn prefix_chars(value: &str, count: usize) -> String {
    value.chars().take(count).collect()
}

fn rfind_char(value: &str, ch: char) -> Option<usize> {
    value.chars().enumerate().filter(|(_, c)| *c == ch).map(|(i, _)| i).next_back()
}

fn non_special_chars_len(value: &str) -> usize {
    value
        .chars()
        .take_while(|ch| !matches!(ch, '@' | '!' | '[' | ']' | '.' | ',' | '$' | '*' | '+' | '?' | '^' | '{' | '}' | '(' | ')' | '|' | '\\' | '/'))
        .count()
}

fn is_word_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

fn split_top_level(input: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut bracket = 0i64;
    let mut paren = 0i64;
    let mut quote = 0i64;
    let mut value = String::new();
    let mut escaped = false;
    for ch in input.chars() {
        if escaped {
            value.push(ch);
            escaped = false;
            continue;
        }
        if ch == '\\' {
            value.push(ch);
            escaped = true;
            continue;
        }
        if ch == '"' {
            quote = if quote == 1 { 0 } else { 1 };
            value.push(ch);
            continue;
        }
        if quote == 0 {
            if ch == '[' {
                bracket += 1;
            } else if ch == ']' && bracket > 0 {
                bracket -= 1;
            } else if bracket == 0 {
                if ch == '(' {
                    paren += 1;
                } else if ch == ')' && paren > 0 {
                    paren -= 1;
                } else if ch == '|' && paren == 0 {
                    parts.push(value.clone());
                    value.clear();
                    continue;
                }
            }
        }
        value.push(ch);
    }
    parts.push(value);
    parts
}

fn is_plain_branch(branch: &str) -> bool {
    let mut escaped = false;
    for ch in branch.chars() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if matches!(ch, '?' | '*' | '+' | '@' | '!' | '(' | ')' | '[' | ']' | '{' | '}') {
            return false;
        }
    }
    true
}

fn normalize_simple_branch(branch: &str) -> Option<String> {
    let mut value = branch.trim().to_string();
    let mut changed = true;
    while changed {
        changed = false;
        if is_wrapped_at_group(&value) {
            value = value.chars().skip(2).take(value.chars().count().saturating_sub(3)).collect();
            changed = true;
        }
    }
    if !is_plain_branch(&value) {
        return None;
    }
    Some(unescape_single(&value))
}

fn is_wrapped_at_group(value: &str) -> bool {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() < 4 || chars[0] != '@' || chars[1] != '(' || chars[chars.len() - 1] != ')' {
        return false;
    }
    let inner = &chars[2..chars.len() - 1];
    !inner.is_empty()
        && inner
            .iter()
            .all(|ch| !matches!(ch, '\\' | '(' | ')' | '[' | ']' | '{' | '}' | '|'))
}

fn unescape_single(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    let mut out = String::new();
    let mut index = 0;
    while index < chars.len() {
        if chars[index] == '\\' && index + 1 < chars.len() {
            out.push(chars[index + 1]);
            index += 2;
            continue;
        }
        out.push(chars[index]);
        index += 1;
    }
    out
}

fn has_repeated_char_prefix_overlap(branches: &[String]) -> bool {
    let values: Vec<String> = branches.iter().filter_map(|branch| normalize_simple_branch(branch)).collect();
    for i in 0..values.len() {
        for j in i + 1..values.len() {
            let a = &values[i];
            let b = &values[j];
            let Some(ch) = a.chars().next() else { continue };
            let repeated_a = a.chars().all(|c| c == ch);
            let repeated_b = b.chars().all(|c| c == ch);
            if !repeated_a || !repeated_b {
                continue;
            }
            if a == b || a.starts_with(b.as_str()) || b.starts_with(a.as_str()) {
                return true;
            }
        }
    }
    false
}

struct RepeatedExtglob {
    kind: char,
    body: String,
    end: usize,
}

fn parse_repeated_extglob(pattern: &str, require_end: bool) -> Option<RepeatedExtglob> {
    let chars: Vec<char> = pattern.chars().collect();
    if !matches!(chars.first(), Some('+' | '*')) || chars.get(1) != Some(&'(') {
        return None;
    }
    let mut bracket = 0i64;
    let mut paren = 0i64;
    let mut quote = 0i64;
    let mut escaped = false;
    let mut index = 1;
    while index < chars.len() {
        let ch = chars[index];
        if escaped {
            escaped = false;
            index += 1;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            index += 1;
            continue;
        }
        if ch == '"' {
            quote = if quote == 1 { 0 } else { 1 };
            index += 1;
            continue;
        }
        if quote == 1 {
            index += 1;
            continue;
        }
        if ch == '[' {
            bracket += 1;
            index += 1;
            continue;
        }
        if ch == ']' && bracket > 0 {
            bracket -= 1;
            index += 1;
            continue;
        }
        if bracket > 0 {
            index += 1;
            continue;
        }
        if ch == '(' {
            paren += 1;
            index += 1;
            continue;
        }
        if ch == ')' {
            paren -= 1;
            if paren == 0 {
                if require_end && index != chars.len() - 1 {
                    return None;
                }
                return Some(RepeatedExtglob {
                    kind: chars[0],
                    body: chars[2..index].iter().collect(),
                    end: index,
                });
            }
        }
        index += 1;
    }
    None
}

fn build_char_class_star(chars: &[String]) -> String {
    let source = if chars.len() == 1 {
        escape_regex(&chars[0])
    } else {
        format!("[{}]", chars.iter().map(|ch| escape_regex(ch)).collect::<String>())
    };
    format!("{source}*")
}

fn get_star_extglob_sequence_chars(pattern: &str) -> Option<Vec<String>> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut index = 0;
    let mut collected = Vec::new();
    while index < chars.len() {
        let slice: String = chars[index..].iter().collect();
        let matched = parse_repeated_extglob(&slice, false)?;
        if matched.kind != '*' {
            return None;
        }
        let branches: Vec<String> = split_top_level(&matched.body).into_iter().map(|branch| branch.trim().to_string()).collect();
        if branches.len() != 1 {
            return None;
        }
        let branch = normalize_simple_branch(&branches[0])?;
        if branch.chars().count() != 1 {
            return None;
        }
        collected.push(branch);
        index += matched.end + 1;
    }
    if collected.is_empty() {
        return None;
    }
    Some(collected)
}

fn repeated_extglob_recursion(pattern: &str) -> i64 {
    let mut depth = 0i64;
    let mut value = pattern.trim().to_string();
    let mut matched = parse_repeated_extglob(&value, true);
    while let Some(current) = matched {
        depth += 1;
        value = current.body.trim().to_string();
        matched = parse_repeated_extglob(&value, true);
    }
    depth
}

fn analyze_repeated_extglob(body: &str, options: &Options) -> Analysis {
    if options.max_extglob_recursion.is_disabled() {
        return Analysis::default();
    }
    let max = options.max_extglob_recursion.depth();
    let branches: Vec<String> = split_top_level(body).into_iter().map(|branch| branch.trim().to_string()).collect();
    if branches.len() > 1
        && (branches.iter().any(String::is_empty)
            || branches.iter().any(|branch| !branch.is_empty() && branch.chars().all(|ch| ch == '*' || ch == '?'))
            || has_repeated_char_prefix_overlap(&branches))
    {
        return Analysis { risky: true, safe_output: None };
    }

    let mut safe_chars: Vec<String> = Vec::new();
    let mut saw_star_sequence = false;
    let mut combinable = true;
    for branch in &branches {
        if let Some(chars) = get_star_extglob_sequence_chars(branch) {
            saw_star_sequence = true;
            safe_chars.extend(chars);
            continue;
        }
        if let Some(literal) = normalize_simple_branch(branch)
            && literal.chars().count() == 1
        {
            safe_chars.push(literal);
            continue;
        }
        combinable = false;
        if repeated_extglob_recursion(branch) > max {
            return Analysis { risky: true, safe_output: None };
        }
    }

    if saw_star_sequence {
        if combinable {
            let mut unique: Vec<String> = Vec::new();
            for ch in safe_chars {
                if !unique.contains(&ch) {
                    unique.push(ch);
                }
            }
            return Analysis { risky: true, safe_output: Some(build_char_class_star(&unique)) };
        }
        return Analysis { risky: true, safe_output: None };
    }
    Analysis::default()
}

/// `expandRange(args)` from the pinned `parse.js`.
///
/// `args.sort()` uses `Array#sort`'s default comparator -- lexicographic UTF-16
/// code-unit order -- not the Rust `str`/`char` scalar-value order, which
/// disagrees as soon as an endpoint contains a surrogate unit. The class body is
/// the pinned `[${args.join('-')}]` template; because the port matches in the
/// mapped unit domain (a lone surrogate unit `u` is `0x10000 + u`), a unit range
/// is emitted as the equivalent mapped-domain union so the Rust engine sees
/// exactly the units JS sees.
fn expand_range(args: &[String]) -> String {
    let mut sorted = args.to_vec();
    sorted.sort_by_key(|value| js_unit_order(value));
    match translate_unit_class(&sorted.join("-")) {
        UnitClass::Body(body) => format!("[{body}]"),
        UnitClass::OutOfOrder => escape_range_fallback(&sorted),
        UnitClass::Unsupported => {
            let value = format!("[{}]", sorted.join("-"));
            if fancy_regex::Regex::new(&value).is_ok() { value } else { escape_range_fallback(&sorted) }
        }
    }
}

/// `args.map(v => utils.escapeRegex(v)).join('..')` -- the pinned fallback when
/// `new RegExp(value)` throws.
fn escape_range_fallback(sorted: &[String]) -> String {
    sorted.iter().map(|entry| escape_regex(entry)).collect::<Vec<_>>().join("..")
}

/// The outcome of rewriting a JS class body into the mapped unit domain.
enum UnitClass {
    /// A class body rewritten into the mapped unit domain.
    Body(String),
    /// A range whose endpoints are out of order -- the pinned `new RegExp` throws.
    OutOfOrder,
    /// A body containing an atom that is not a single UTF-16 unit.
    Unsupported,
}

/// Rewrite a JS class body (the pinned `[${args.join('-')}]` interior) into the
/// mapped unit domain, mirroring `new RegExp`'s acceptance.
fn translate_unit_class(body: &str) -> UnitClass {
    let chars: Vec<char> = body.chars().collect();
    let Some(atoms) = class_atoms(&chars) else {
        return UnitClass::Unsupported;
    };
    let mut out = String::new();
    let mut index = 0;
    while index < atoms.len() {
        let is_range = index + 2 < atoms.len() && atoms[index + 1].dash && !atoms[index].dash && !atoms[index + 2].dash;
        if is_range {
            if atoms[index].value > atoms[index + 2].value {
                return UnitClass::OutOfOrder;
            }
            out.push_str(&map_unit_range(atoms[index].value, atoms[index + 2].value));
            index += 3;
            continue;
        }
        out.push_str(&format!("\\u{{{:X}}}", atoms[index].value));
        index += 1;
    }
    UnitClass::Body(out)
}

/// One class atom: the UTF-16 unit it stands for, how many source characters it
/// spans, and whether it is a raw (unescaped) `-`.
struct ClassAtom {
    value: u32,
    len: usize,
    dash: bool,
}

/// Split a JS class body into atoms, or `None` for a shape the mapped-domain
/// rewrite cannot express as single units (a JS class escape such as `\d`).
fn class_atoms(chars: &[char]) -> Option<Vec<ClassAtom>> {
    let mut atoms = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        let ch = chars[index];
        if ch != '\\' {
            atoms.push(ClassAtom { value: unit_value(ch), len: 1, dash: ch == '-' });
            index += 1;
            continue;
        }
        let escaped = *chars.get(index + 1)?;
        let (value, len) = match escaped {
            'd' | 'D' | 's' | 'S' | 'w' | 'W' | 'B' => return None,
            'f' => (0x0C, 2),
            'n' => (0x0A, 2),
            'r' => (0x0D, 2),
            't' => (0x09, 2),
            'v' => (0x0B, 2),
            'b' => (0x08, 2),
            '0'..='7' => {
                let (digits, len) = octal_digits(chars, index);
                (u32::from_str_radix(&digits, 8).unwrap_or(0), len)
            }
            'x' => match (chars.get(index + 2), chars.get(index + 3)) {
                (Some(&high), Some(&low)) if high.is_ascii_hexdigit() && low.is_ascii_hexdigit() => {
                    (u32::from_str_radix(&format!("{high}{low}"), 16).unwrap_or(0), 4)
                }
                _ => (u32::from('x'), 2),
            },
            'u' => match chars.get(index + 2..index + 6) {
                Some(digits) if digits.iter().all(char::is_ascii_hexdigit) => {
                    (u32::from_str_radix(&digits.iter().collect::<String>(), 16).unwrap_or(0), 6)
                }
                _ => (u32::from('u'), 2),
            },
            _ => (u32::from(escaped), 2),
        };
        atoms.push(ClassAtom { value, len, dash: false });
        index += len;
    }
    Some(atoms)
}

/// JS `LegacyOctalEscapeSequence` (Annex B) digit collection.
fn octal_digits(chars: &[char], start: usize) -> (String, usize) {
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

/// A JS class range `low-high` (both UTF-16 units) as a mapped-domain class body.
fn map_unit_range(low: u32, high: u32) -> String {
    let mut out = String::new();
    push_unit_piece(&mut out, low, high.min(0xD7FF), false);
    push_unit_piece(&mut out, low.max(0xD800), high.min(0xDFFF), true);
    push_unit_piece(&mut out, low.max(0xE000), high, false);
    out
}

/// Append one contiguous piece of a unit range; surrogate units map to
/// `0x10000 + unit` in the port's domain.
fn push_unit_piece(out: &mut String, from: u32, to: u32, mapped: bool) {
    if from > to {
        return;
    }
    let map = |unit: u32| if mapped { 0x1_0000 + unit } else { unit };
    if from == to {
        out.push_str(&format!("\\u{{{:X}}}", map(from)));
    } else {
        out.push_str(&format!("\\u{{{:X}}}-\\u{{{:X}}}", map(from), map(to)));
    }
}

fn fastpath_replace(input: &str, glob: &super::constants::GlobChars, star: &str, qmark_no_dot: &str) -> (String, bool) {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::with_capacity(input.len());
    let mut backslashes = false;
    let mut index = 0;
    while index < chars.len() {
        let match_offset = index;
        let mut matched: Option<(String, String, String, String)> = None;
        for esc_len in [1usize, 0] {
            if esc_len == 1 && chars[index] != '\\' {
                continue;
            }
            let start = index + esc_len;
            let Some(first) = chars.get(start).copied() else { continue };
            if is_word_char(first) {
                continue;
            }
            let mut end = start + 1;
            while chars.get(end) == Some(&first) {
                end += 1;
            }
            let esc: String = if esc_len == 1 { "\\".into() } else { String::new() };
            let group: String = chars[start..end].iter().collect();
            let rest: String = chars[start + 1..end].iter().collect();
            matched = Some((esc, group, first.to_string(), rest));
            index = end;
            break;
        }
        let Some((esc, group, first, rest)) = matched else {
            out.push(chars[index]);
            index += 1;
            continue;
        };
        let whole = format!("{esc}{group}");
        let replacement = match first.as_str() {
            "\\" => {
                backslashes = true;
                whole.clone()
            }
            "?" => {
                let tail = if rest.is_empty() { String::new() } else { glob.qmark.repeat(rest.chars().count()) };
                if !esc.is_empty() {
                    format!("{esc}?{tail}")
                } else if match_offset == 0 {
                    format!("{qmark_no_dot}{tail}")
                } else {
                    glob.qmark.repeat(group.chars().count())
                }
            }
            "." => glob.dot_literal.repeat(group.chars().count()),
            "*" => {
                if !esc.is_empty() {
                    format!("{esc}*{}", if rest.is_empty() { String::new() } else { star.to_string() })
                } else {
                    star.to_string()
                }
            }
            _ => {
                if !esc.is_empty() {
                    whole.clone()
                } else {
                    format!("\\{whole}")
                }
            }
        };
        out.push_str(&replacement);
    }
    (out, backslashes)
}

impl Options {
    fn effective_noextglob(&self) -> bool {
        self.noext.unwrap_or_else(|| self.noextglob.unwrap_or(false))
    }
}

struct Parser<'a> {
    original_input: String,
    input: Vec<char>,
    options: &'a Options,
    glob: super::constants::GlobChars,
    capture: String,
    star: String,
    index: isize,
    start: usize,
    output: String,
    prefix: String,
    backtrack: bool,
    negated: bool,
    negated_extglob: bool,
    brackets: i64,
    braces: i64,
    parens: i64,
    quotes: i64,
    globstar: bool,
    tokens: Vec<Token>,
    extglobs: Vec<ExtglobToken>,
    brace_stack: Vec<BraceToken>,
    stack: Vec<String>,
    prev: usize,
    len: usize,
}

impl<'a> Parser<'a> {
    fn new(input: &str, options: &'a Options) -> Result<Self, PicomatchError> {
        let replaced = replacement(input).unwrap_or(input).to_string();
        let max = options.max_length.map_or(MAX_LENGTH, |value| value.min(MAX_LENGTH));
        let unit_length = js_length(&replaced);
        if unit_length > max {
            return Err(PicomatchError::Syntax(format!(
                "Input length: {unit_length}, exceeds maximum allowed length: {max}"
            )));
        }
        let glob = glob_chars(options.windows);
        let capture = if options.capture { String::new() } else { "?:".into() };
        let star = if options.bash {
            globstar_source(&glob, &capture, options.dot)
        } else {
            glob.star.clone()
        };
        let star = if options.capture { format!("({star})") } else { star };
        let bos = Token { kind: "bos".into(), value: String::new(), output: Some(options.prepend.clone().unwrap_or_default()), ..Token::default() };
        let mut prefix = None;
        let stripped = remove_prefix(&js_units(&replaced), &mut prefix);
        let chars: Vec<char> = stripped.chars().collect();
        let len = chars.len();
        Ok(Self {
            original_input: replaced,
            input: chars,
            options,
            glob,
            capture,
            star,
            index: -1,
            start: 0,
            output: String::new(),
            prefix: prefix.unwrap_or_default(),
            backtrack: false,
            negated: false,
            negated_extglob: false,
            brackets: 0,
            braces: 0,
            parens: 0,
            quotes: 0,
            globstar: false,
            tokens: vec![bos],
            extglobs: Vec::new(),
            brace_stack: Vec::new(),
            stack: Vec::new(),
            prev: 0,
            len,
        })
    }

    fn eos(&self) -> bool {
        self.index == self.len as isize - 1
    }

    fn peek(&self, n: isize) -> String {
        let target = self.index + n;
        if target < 0 {
            return String::new();
        }
        self.input.get(target as usize).map_or_else(String::new, |ch| ch.to_string())
    }

    fn advance(&mut self) -> String {
        self.index += 1;
        if self.index < 0 {
            return String::new();
        }
        self.input.get(self.index as usize).map_or_else(String::new, |ch| ch.to_string())
    }

    fn remaining(&self) -> String {
        let start = (self.index + 1).max(0) as usize;
        if start >= self.input.len() {
            return String::new();
        }
        self.input[start..].iter().collect()
    }

    fn consume(&mut self, _value: &str, num: isize) {
        self.index += num;
    }

    fn append(&mut self, output: Option<&str>, value: &str) {
        match output {
            Some(text) => self.output.push_str(text),
            None => self.output.push_str(value),
        }
    }

    fn globstar(&self) -> String {
        globstar_source(&self.glob, &self.capture, self.options.dot)
    }

    fn increment(&mut self, kind: &str) {
        match kind {
            "parens" => self.parens += 1,
            "brackets" => self.brackets += 1,
            "braces" => self.braces += 1,
            _ => self.quotes += 1,
        }
        self.stack.push(kind.to_string());
    }

    fn decrement(&mut self, kind: &str) {
        match kind {
            "parens" => self.parens -= 1,
            "brackets" => self.brackets -= 1,
            "braces" => self.braces -= 1,
            _ => self.quotes -= 1,
        }
        self.stack.pop();
    }

    fn negate(&mut self) -> bool {
        let mut count = 1;
        while self.peek(1) == "!" && (self.peek(2) != "(" || self.peek(3) == "?") {
            self.advance();
            self.start += 1;
            count += 1;
        }
        if count % 2 == 0 {
            return false;
        }
        self.negated = true;
        self.start += 1;
        true
    }

    fn push(&mut self, mut tok: Token) {
        if self.tokens[self.prev].kind == "globstar" {
            let is_brace = self.braces > 0 && (tok.kind == "comma" || tok.kind == "brace");
            let is_extglob = tok.extglob || (!self.extglobs.is_empty() && (tok.kind == "pipe" || tok.kind == "paren"));
            if tok.kind != "slash" && tok.kind != "paren" && !is_brace && !is_extglob {
                let prev_output_len = self.tokens[self.prev].output.as_deref().unwrap_or("").chars().count();
                drop_chars(&mut self.output, prev_output_len);
                let star = self.star.clone();
                let prev = &mut self.tokens[self.prev];
                prev.kind = "star".into();
                prev.value = "*".into();
                prev.output = Some(star.clone());
                self.output.push_str(&star);
            }
        }

        if !self.extglobs.is_empty() && tok.kind != "paren" {
            let last = self.extglobs.len() - 1;
            self.extglobs[last].inner.push_str(&tok.value);
        }

        if !tok.value.is_empty() || tok.output.is_some() {
            self.append(tok.output.as_deref(), &tok.value);
        }

        if self.tokens[self.prev].kind == "text" && tok.kind == "text" {
            let prev = &mut self.tokens[self.prev];
            let base = prev.output.clone().unwrap_or_else(|| prev.value.clone());
            prev.output = Some(format!("{base}{}", tok.value));
            prev.value.push_str(&tok.value);
            return;
        }

        tok.prev = Some(self.prev);
        self.tokens.push(tok);
        self.prev = self.tokens.len() - 1;
    }

    fn extglob_open(&mut self, kind: &str, value: &str) {
        let table = extglob_chars(value.chars().next().unwrap_or(' '), &self.glob).unwrap_or(ExtglobChars { open: String::new(), close: String::new() });
        let token = ExtglobToken {
            kind: kind.to_string(),
            open: table.open,
            close: table.close,
            inner: String::new(),
            prev: self.prev,
            parens: self.parens,
            output: self.output.clone(),
            start_index: self.index,
            tokens_index: self.tokens.len(),
        };
        let output = format!("{}{}", if self.options.capture { "(" } else { "" }, token.open);
        self.increment("parens");
        let first_output = if self.output.is_empty() { Some(self.glob.one_char.clone()) } else { Some(String::new()) };
        self.push(Token { kind: kind.to_string(), value: value.to_string(), output: first_output, ..Token::default() });
        let advance = self.advance();
        self.push(Token { kind: "paren".into(), extglob: true, value: advance, output: Some(output), ..Token::default() });
        self.extglobs.push(token);
    }

    fn extglob_close(&mut self, token: ExtglobToken, close_value: &str) {
        let start = token.start_index.max(0) as usize;
        let end = (self.index.max(0) as usize + 1).min(self.input.len());
        let literal: String = self.input[start.min(self.input.len())..end].iter().collect();
        let body: String = self.input[(start + 2).min(self.input.len())..(self.index.max(0) as usize).min(self.input.len())].iter().collect();
        let analysis = analyze_repeated_extglob(&body, self.options);

        if (token.kind == "plus" || token.kind == "star") && analysis.risky {
            let safe_output = analysis.safe_output.as_ref().map(|safe| {
                let prefix = if token.output.is_empty() { self.glob.one_char.clone() } else { String::new() };
                let inner = if self.options.capture { format!("({safe})") } else { safe.clone() };
                format!("{prefix}{inner}")
            });
            let open_output = safe_output.unwrap_or_else(|| escape_regex(&literal));
            if let Some(open) = self.tokens.get_mut(token.tokens_index) {
                open.kind = "text".into();
                open.value = literal;
                open.output = Some(open_output.clone());
            }
            let mut i = token.tokens_index + 1;
            while i < self.tokens.len() {
                self.tokens[i].value.clear();
                self.tokens[i].output = Some(String::new());
                self.tokens[i].suffix = None;
                i += 1;
            }
            self.output = format!("{}{open_output}", token.output);
            self.backtrack = true;
            self.push(Token { kind: "paren".into(), extglob: true, value: close_value.to_string(), output: Some(String::new()), ..Token::default() });
            self.decrement("parens");
            return;
        }

        let mut output = format!("{}{}", token.close, if self.options.capture { ")" } else { "" });

        if token.kind == "negate" {
            let mut extglob_star = self.star.clone();
            if token.inner.chars().count() > 1 && token.inner.contains('/') {
                extglob_star = self.globstar();
            }
            let remaining = self.remaining();
            let all_close = !remaining.is_empty() && remaining.chars().all(|ch| ch == ')');
            if extglob_star != self.star || self.eos() || all_close {
                output = format!(")$)){extglob_star}");
            }
            if token.inner.contains('*')
                && is_dot_suffix(&remaining)
            {
                let sub = Options { fastpaths: Some(false), ..self.options.clone() };
                if let Ok(parsed) = parse(&remaining, &sub) {
                    output = format!("){}){extglob_star})", parsed.output);
                }
            }
            if token.prev == 0 {
                self.negated_extglob = true;
            }
        }

        self.push(Token { kind: "paren".into(), extglob: true, value: close_value.to_string(), output: Some(output), ..Token::default() });
        self.decrement("parens");
    }

    fn run_fastpath(&mut self) -> Option<()> {
        if self.options.fastpaths == Some(false) {
            return None;
        }
        let input: String = self.input.iter().collect();
        let starts_special = input.starts_with('*') || input.starts_with('!');
        let has_group = input.chars().any(|ch| matches!(ch, '/' | '(' | ')' | '[' | ']' | '{' | '}' | '"'));
        if starts_special || has_group {
            return None;
        }
        let qmark_no_dot = if self.options.dot { self.glob.qmark.clone() } else { self.glob.qmark_no_dot.clone() };
        let (mut output, backslashes) = fastpath_replace(&input, &self.glob, &self.star, &qmark_no_dot);
        if backslashes {
            if self.options.unescape {
                output = output.replace('\\', "");
            } else {
                output = collapse_backslash_runs(&output);
            }
        }
        if output == input && self.options.contains {
            self.output = input;
            return Some(());
        }
        let negated = self.negated;
        self.output = super::utils::wrap_output(&output, negated, self.options.contains);
        Some(())
    }

    fn run(mut self) -> Result<ParseState, PicomatchError> {
        if self.run_fastpath().is_some() {
            return Ok(self.finish());
        }
        self.output.clear();

        while !self.eos() {
            let mut value = self.advance();

            if value == "\u{0}" {
                continue;
            }

            if value == "\\" {
                let next = self.peek(1);
                if next == "/" && !self.options.bash {
                    continue;
                }
                if next == "." || next == ";" {
                    continue;
                }
                if next.is_empty() {
                    value.push('\\');
                    self.push(Token { kind: "text".into(), value, ..Token::default() });
                    continue;
                }
                let remaining = self.remaining();
                let slash_run = remaining.chars().take_while(|ch| *ch == '\\').count();
                if slash_run > 2 {
                    self.index += slash_run as isize;
                    if slash_run % 2 != 0 {
                        value.push('\\');
                    }
                }
                if self.options.unescape {
                    value = self.advance();
                } else {
                    let next = self.advance();
                    value.push_str(&next);
                }
                if self.brackets == 0 {
                    self.push(Token { kind: "text".into(), value, ..Token::default() });
                    continue;
                }
            }

            if self.brackets > 0 && (value != "]" || self.tokens[self.prev].value == "[" || self.tokens[self.prev].value == "[^") {
                if self.options.posix != Some(false) && value == ":" {
                    let prev_value = self.tokens[self.prev].value.clone();
                    let inner: String = prev_value.chars().skip(1).collect();
                    if inner.contains('[') {
                        self.tokens[self.prev].posix = true;
                        if inner.contains(':')
                            && let Some(idx) = rfind_char(&prev_value, '[')
                        {
                            let pre = prefix_chars(&prev_value, idx);
                            let rest: String = prev_value.chars().skip(idx + 2).collect();
                            if let Some(posix) = posix_regex_source(&rest) {
                                self.tokens[self.prev].value = format!("{pre}{posix}");
                                self.backtrack = true;
                                self.advance();
                                if self.tokens[0].output.as_deref().unwrap_or("").is_empty() && self.prev == 1 {
                                    self.tokens[0].output = Some(self.glob.one_char.clone());
                                }
                                continue;
                            }
                        }
                    }
                }
                if (value == "[" && self.peek(1) != ":") || (value == "-" && self.peek(1) == "]") {
                    value = format!("\\{value}");
                }
                if value == "]" && (self.tokens[self.prev].value == "[" || self.tokens[self.prev].value == "[^") {
                    value = format!("\\{value}");
                }
                if self.options.posix == Some(true) && value == "!" && self.tokens[self.prev].value == "[" {
                    value = "^".into();
                }
                self.tokens[self.prev].value.push_str(&value);
                self.append(Some(&value), &value);
                continue;
            }

            if self.quotes == 1 && value != "\"" {
                value = escape_regex(&value);
                self.tokens[self.prev].value.push_str(&value);
                self.append(Some(&value), &value);
                continue;
            }

            if value == "\"" {
                self.quotes = if self.quotes == 1 { 0 } else { 1 };
                if self.options.keep_quotes {
                    self.push(Token { kind: "text".into(), value, ..Token::default() });
                }
                continue;
            }

            if value == "(" {
                self.increment("parens");
                self.push(Token { kind: "paren".into(), value, ..Token::default() });
                continue;
            }

            if value == ")" {
                if self.parens == 0 && self.options.strict_brackets {
                    return Err(PicomatchError::Syntax(syntax_error("opening", '(')));
                }
                let extglob_parens = self.extglobs.last().map(|token| token.parens);
                if let Some(extglob_parens) = extglob_parens
                    && self.parens == extglob_parens + 1
                    && let Some(token) = self.extglobs.pop()
                {
                    self.extglob_close(token, &value);
                    continue;
                }
                let output = if self.parens != 0 { ")".to_string() } else { "\\)".to_string() };
                self.push(Token { kind: "paren".into(), value, output: Some(output), ..Token::default() });
                self.decrement("parens");
                continue;
            }

            if value == "[" {
                if self.options.nobracket || !self.remaining().contains(']') {
                    if !self.options.nobracket && self.options.strict_brackets {
                        return Err(PicomatchError::Syntax(syntax_error("closing", ']')));
                    }
                    value = "\\[".into();
                } else {
                    self.increment("brackets");
                }
                self.push(Token { kind: "bracket".into(), value, ..Token::default() });
                continue;
            }

            if value == "]" {
                if self.options.nobracket || (self.tokens[self.prev].kind == "bracket" && self.tokens[self.prev].value.chars().count() == 1) {
                    self.push(Token { kind: "text".into(), value: value.clone(), output: Some(format!("\\{value}")), ..Token::default() });
                    continue;
                }
                if self.brackets == 0 {
                    if self.options.strict_brackets {
                        return Err(PicomatchError::Syntax(syntax_error("opening", '[')));
                    }
                    self.push(Token { kind: "text".into(), value: value.clone(), output: Some(format!("\\{value}")), ..Token::default() });
                    continue;
                }
                self.decrement("brackets");
                let prev_value: String = self.tokens[self.prev].value.chars().skip(1).collect();
                if !self.tokens[self.prev].posix && prev_value.starts_with('^') && !prev_value.contains('/') {
                    value = format!("/{value}");
                }
                self.tokens[self.prev].value.push_str(&value);
                self.append(Some(&value), &value);

                if self.options.literal_brackets == Some(false) || has_regex_chars(&prev_value) {
                    continue;
                }
                let escaped = escape_regex(&self.tokens[self.prev].value.clone());
                drop_chars(&mut self.output, self.tokens[self.prev].value.chars().count());
                if self.options.literal_brackets == Some(true) {
                    self.output.push_str(&escaped);
                    self.tokens[self.prev].value = escaped;
                    continue;
                }
                let combined = format!("({}{escaped}|{})", self.capture, self.tokens[self.prev].value);
                self.tokens[self.prev].value = combined.clone();
                self.output.push_str(&combined);
                continue;
            }

            if value == "{" && !self.options.nobrace {
                self.increment("braces");
                let open = BraceToken {
                    output_index: self.output.chars().count(),
                    tokens_index: self.tokens.len(),
                    ..BraceToken::default()
                };
                self.brace_stack.push(open);
                self.push(Token { kind: "brace".into(), value, output: Some("(".into()), ..Token::default() });
                continue;
            }

            if value == "}" {
                let brace = self.brace_stack.last().cloned();
                if self.options.nobrace || brace.is_none() {
                    self.push(Token { kind: "text".into(), value: value.clone(), output: Some(value), ..Token::default() });
                    continue;
                }
                let brace = brace.unwrap_or_default();
                let mut output = ")".to_string();
                if brace.dots {
                    let mut range: Vec<String> = Vec::new();
                    let snapshot = self.tokens.clone();
                    for i in (0..snapshot.len()).rev() {
                        self.tokens.pop();
                        if snapshot[i].kind == "brace" {
                            break;
                        }
                        if snapshot[i].kind != "dots" {
                            range.insert(0, snapshot[i].value.clone());
                        }
                    }
                    output = expand_range(&range);
                    self.backtrack = true;
                }
                if !brace.comma && !brace.dots {
                    let out = prefix_chars(&self.output, brace.output_index);
                    if let Some(entry) = self.tokens.get_mut(brace.tokens_index) {
                        entry.value = "\\{".into();
                        entry.output = Some("\\{".into());
                    }
                    let toks = self.tokens[brace.tokens_index.min(self.tokens.len())..].to_vec();
                    output = "\\}".to_string();
                    value = "\\}".to_string();
                    self.output = out;
                    for token in &toks {
                        self.output.push_str(token.output.as_deref().unwrap_or(&token.value));
                    }
                }
                self.push(Token { kind: "brace".into(), value, output: Some(output), ..Token::default() });
                self.decrement("braces");
                self.brace_stack.pop();
                continue;
            }

            if value == "|" {
                self.push(Token { kind: "text".into(), value, ..Token::default() });
                continue;
            }

            if value == "," {
                let mut output = value.clone();
                if let Some(brace) = self.brace_stack.last_mut()
                    && self.stack.last().map(String::as_str) == Some("braces")
                {
                    brace.comma = true;
                    output = "|".into();
                }
                self.push(Token { kind: "comma".into(), value, output: Some(output), ..Token::default() });
                continue;
            }

            if value == "/" {
                if self.tokens[self.prev].kind == "dot" && self.index == self.start as isize + 1 {
                    self.start = (self.index + 1).max(0) as usize;
                    self.output.clear();
                    self.tokens.pop();
                    self.prev = 0;
                    continue;
                }
                self.push(Token { kind: "slash".into(), value, output: Some(self.glob.slash_literal.clone()), ..Token::default() });
                continue;
            }

            if value == "." {
                if self.braces > 0 && self.tokens[self.prev].kind == "dot" {
                    if self.tokens[self.prev].value == "." {
                        self.tokens[self.prev].output = Some(self.glob.dot_literal.clone());
                    }
                    let prev = &mut self.tokens[self.prev];
                    prev.kind = "dots".into();
                    let appended = format!("{}{value}", prev.output.clone().unwrap_or_default());
                    prev.output = Some(appended);
                    prev.value.push_str(&value);
                    if let Some(brace) = self.brace_stack.last_mut() {
                        brace.dots = true;
                    }
                    continue;
                }
                if self.braces + self.parens == 0 && self.tokens[self.prev].kind != "bos" && self.tokens[self.prev].kind != "slash" {
                    self.push(Token { kind: "text".into(), value, output: Some(self.glob.dot_literal.clone()), ..Token::default() });
                    continue;
                }
                self.push(Token { kind: "dot".into(), value, output: Some(self.glob.dot_literal.clone()), ..Token::default() });
                continue;
            }

            if value == "?" {
                let is_group = self.tokens[self.prev].value == "(";
                if !is_group && !self.options.effective_noextglob() && self.peek(1) == "(" && self.peek(2) != "?" {
                    self.extglob_open("qmark", &value);
                    continue;
                }
                if self.tokens[self.prev].kind == "paren" {
                    let next = self.peek(1);
                    let mut output = value.clone();
                    let remaining = self.remaining();
                    let next_special = next.chars().next().is_some_and(|ch| matches!(ch, '!' | '=' | '<' | ':'));
                    let angle_ok = next == "<" && !has_angle_group(&remaining);
                    if (self.tokens[self.prev].value == "(" && !next_special) || angle_ok {
                        output = format!("\\{value}");
                    }
                    self.push(Token { kind: "text".into(), value, output: Some(output), ..Token::default() });
                    continue;
                }
                let output = if !self.options.dot && (self.tokens[self.prev].kind == "slash" || self.tokens[self.prev].kind == "bos") {
                    self.glob.qmark_no_dot.clone()
                } else {
                    self.glob.qmark.clone()
                };
                self.push(Token { kind: "qmark".into(), value, output: Some(output), ..Token::default() });
                continue;
            }

            if value == "!" {
                if !self.options.effective_noextglob() && self.peek(1) == "(" {
                    let peek2 = self.peek(2);
                    let peek3 = self.peek(3);
                    let peek3_special = peek3.chars().next().is_some_and(|ch| matches!(ch, '!' | '=' | '<' | ':'));
                    if peek2 != "?" || !peek3_special {
                        self.extglob_open("negate", &value);
                        continue;
                    }
                }
                if !self.options.nonegate && self.index == 0 {
                    self.negate();
                    continue;
                }
            }

            if value == "+" {
                if !self.options.effective_noextglob() && self.peek(1) == "(" && self.peek(2) != "?" {
                    self.extglob_open("plus", &value);
                    continue;
                }
                if self.tokens[self.prev].value == "(" || self.options.regex == Some(false) {
                    self.push(Token { kind: "plus".into(), value, output: Some(self.glob.plus_literal.clone()), ..Token::default() });
                    continue;
                }
                if matches!(self.tokens[self.prev].kind.as_str(), "bracket" | "paren" | "brace") || self.parens > 0 {
                    self.push(Token { kind: "plus".into(), value, ..Token::default() });
                    continue;
                }
                self.push(Token { kind: "plus".into(), value: self.glob.plus_literal.clone(), ..Token::default() });
                continue;
            }

            if value == "@" {
                if !self.options.effective_noextglob() && self.peek(1) == "(" && self.peek(2) != "?" {
                    self.push(Token { kind: "at".into(), extglob: true, value, output: Some(String::new()), ..Token::default() });
                    continue;
                }
                self.push(Token { kind: "text".into(), value, ..Token::default() });
                continue;
            }

            if value != "*" {
                if value == "$" || value == "^" {
                    value = format!("\\{value}");
                }
                let remaining = self.remaining();
                let matched_len = non_special_chars_len(&remaining);
                if matched_len > 0 {
                    let matched: String = remaining.chars().take(matched_len).collect();
                    value.push_str(&matched);
                    self.index += matched_len as isize;
                }
                self.push(Token { kind: "text".into(), value, ..Token::default() });
                continue;
            }

            if self.tokens[self.prev].kind == "globstar" || self.tokens[self.prev].star {
                let prev = &mut self.tokens[self.prev];
                prev.kind = "star".into();
                prev.star = true;
                prev.value.push_str(&value);
                prev.output = Some(self.star.clone());
                self.backtrack = true;
                self.globstar = true;
                self.consume(&value, 0);
                continue;
            }

            let rest = self.remaining();
            if !self.options.effective_noextglob() && starts_group(&rest) {
                self.extglob_open("star", &value);
                continue;
            }

            if self.tokens[self.prev].kind == "star" {
                if self.options.noglobstar {
                    self.consume(&value, 0);
                    continue;
                }
                let prior = self.tokens[self.prev].prev.unwrap_or(0);
                let before = self.tokens[prior].prev;
                let is_start = self.tokens[prior].kind == "slash" || self.tokens[prior].kind == "bos";
                let after_star = before.is_some_and(|idx| self.tokens[idx].kind == "star" || self.tokens[idx].kind == "globstar");

                if self.options.bash && (!is_start || (rest.chars().next().is_some() && rest.chars().next() != Some('/'))) {
                    self.push(Token { kind: "star".into(), value, output: Some(String::new()), ..Token::default() });
                    continue;
                }

                let is_brace = self.braces > 0 && (self.tokens[prior].kind == "comma" || self.tokens[prior].kind == "brace");
                let is_extglob = !self.extglobs.is_empty() && (self.tokens[prior].kind == "pipe" || self.tokens[prior].kind == "paren");
                if !is_start && self.tokens[prior].kind != "paren" && !is_brace && !is_extglob {
                    self.push(Token { kind: "star".into(), value, output: Some(String::new()), ..Token::default() });
                    continue;
                }

                let mut rest = rest;
                while rest.chars().take(3).collect::<String>() == "/**" {
                    let after = self.input.get((self.index + 4).max(0) as usize).copied();
                    if after.is_some() && after != Some('/') {
                        break;
                    }
                    rest = rest.chars().skip(3).collect();
                    self.consume("/**", 3);
                }

                if self.tokens[prior].kind == "bos" && self.eos() {
                    let globstar = self.globstar();
                    let prev = &mut self.tokens[self.prev];
                    prev.kind = "globstar".into();
                    prev.value.push_str(&value);
                    prev.output = Some(globstar.clone());
                    self.output = globstar;
                    self.globstar = true;
                    self.consume(&value, 0);
                    continue;
                }

                if self.tokens[prior].kind == "slash" && self.tokens[self.tokens[prior].prev.unwrap_or(0)].kind != "bos" && !after_star && self.eos() {
                    let prior_output_len = self.tokens[prior].output.as_deref().unwrap_or("").chars().count();
                    let prev_output_len = self.tokens[self.prev].output.as_deref().unwrap_or("").chars().count();
                    drop_chars(&mut self.output, prior_output_len + prev_output_len);
                    let prior_output = format!("(?:{}", self.tokens[prior].output.as_deref().unwrap_or(""));
                    self.tokens[prior].output = Some(prior_output);
                    let globstar = self.globstar();
                    let tail = if self.options.strict_slashes == Some(true) { ")".to_string() } else { "|$)".to_string() };
                    let prev = &mut self.tokens[self.prev];
                    prev.kind = "globstar".into();
                    prev.output = Some(format!("{globstar}{tail}"));
                    prev.value.push_str(&value);
                    self.globstar = true;
                    let prior_output = self.tokens[prior].output.clone().unwrap_or_default();
                    let prev_output = self.tokens[self.prev].output.clone().unwrap_or_default();
                    self.output.push_str(&prior_output);
                    self.output.push_str(&prev_output);
                    self.consume(&value, 0);
                    continue;
                }

                if self.tokens[prior].kind == "slash" && self.tokens[self.tokens[prior].prev.unwrap_or(0)].kind != "bos" && rest.starts_with('/') {
                    let end = if rest.chars().count() > 1 { "|$" } else { "" };
                    let prior_output_len = self.tokens[prior].output.as_deref().unwrap_or("").chars().count();
                    let prev_output_len = self.tokens[self.prev].output.as_deref().unwrap_or("").chars().count();
                    drop_chars(&mut self.output, prior_output_len + prev_output_len);
                    let prior_output = format!("(?:{}", self.tokens[prior].output.as_deref().unwrap_or(""));
                    self.tokens[prior].output = Some(prior_output);
                    let globstar = self.globstar();
                    let slash = self.glob.slash_literal.clone();
                    let prev = &mut self.tokens[self.prev];
                    prev.kind = "globstar".into();
                    prev.output = Some(format!("{globstar}{slash}|{slash}{end})"));
                    prev.value.push_str(&value);
                    self.globstar = true;
                    let prior_output = self.tokens[prior].output.clone().unwrap_or_default();
                    let prev_output = self.tokens[self.prev].output.clone().unwrap_or_default();
                    self.output.push_str(&prior_output);
                    self.output.push_str(&prev_output);
                    let advance = self.advance();
                    self.consume(&format!("{value}{advance}"), 0);
                    self.push(Token { kind: "slash".into(), value: "/".into(), output: Some(String::new()), ..Token::default() });
                    continue;
                }

                if self.tokens[prior].kind == "bos" && rest.starts_with('/') {
                    let globstar = self.globstar();
                    let slash = self.glob.slash_literal.clone();
                    let prev = &mut self.tokens[self.prev];
                    prev.kind = "globstar".into();
                    prev.value.push_str(&value);
                    prev.output = Some(format!("(?:^|{slash}|{globstar}{slash})"));
                    let output = prev.output.clone().unwrap_or_default();
                    self.output = output;
                    self.globstar = true;
                    let advance = self.advance();
                    self.consume(&format!("{value}{advance}"), 0);
                    self.push(Token { kind: "slash".into(), value: "/".into(), output: Some(String::new()), ..Token::default() });
                    continue;
                }

                let prev_output_len = self.tokens[self.prev].output.as_deref().unwrap_or("").chars().count();
                drop_chars(&mut self.output, prev_output_len);
                let globstar = self.globstar();
                let prev = &mut self.tokens[self.prev];
                prev.kind = "globstar".into();
                prev.output = Some(globstar.clone());
                prev.value.push_str(&value);
                self.output.push_str(&globstar);
                self.globstar = true;
                self.consume(&value, 0);
                continue;
            }

            let mut token = Token { kind: "star".into(), value: value.clone(), output: Some(self.star.clone()), ..Token::default() };

            if self.options.bash {
                token.output = Some(".*?".into());
                if self.tokens[self.prev].kind == "bos" || self.tokens[self.prev].kind == "slash" {
                    let nodot = if self.options.dot { String::new() } else { self.glob.no_dot.clone() };
                    token.output = Some(format!("{nodot}.*?"));
                }
                self.push(token);
                continue;
            }

            if matches!(self.tokens[self.prev].kind.as_str(), "bracket" | "paren") && self.options.regex == Some(true) {
                token.output = Some(value.clone());
                self.push(token);
                continue;
            }

            if self.index == self.start as isize || self.tokens[self.prev].kind == "slash" || self.tokens[self.prev].kind == "dot" {
                let prev_kind = self.tokens[self.prev].kind.clone();
                let addition = if prev_kind == "dot" {
                    self.glob.no_dot_slash.clone()
                } else if self.options.dot {
                    self.glob.no_dots_slash.clone()
                } else {
                    self.glob.no_dot.clone()
                };
                self.output.push_str(&addition);
                let prev = &mut self.tokens[self.prev];
                let appended = format!("{}{addition}", prev.output.clone().unwrap_or_default());
                prev.output = Some(appended);
                if self.peek(1) != "*" {
                    let one = self.glob.one_char.clone();
                    self.output.push_str(&one);
                    let prev = &mut self.tokens[self.prev];
                    let appended = format!("{}{one}", prev.output.clone().unwrap_or_default());
                    prev.output = Some(appended);
                }
            }

            self.push(token);
        }

        while self.brackets > 0 {
            if self.options.strict_brackets {
                return Err(PicomatchError::Syntax(syntax_error("closing", ']')));
            }
            let escaped = super::utils::escape_last(&self.output, '[', None);
            self.output = escaped;
            self.decrement("brackets");
        }
        while self.parens > 0 {
            if self.options.strict_brackets {
                return Err(PicomatchError::Syntax(syntax_error("closing", '(')));
            }
            let escaped = super::utils::escape_last(&self.output, '(', None);
            self.output = escaped;
            self.decrement("parens");
        }
        while self.braces > 0 {
            if self.options.strict_brackets {
                return Err(PicomatchError::Syntax(syntax_error("closing", '}')));
            }
            let escaped = super::utils::escape_last(&self.output, '{', None);
            self.output = escaped;
            self.decrement("braces");
        }

        if self.options.strict_slashes != Some(true) && (self.tokens[self.prev].kind == "star" || self.tokens[self.prev].kind == "bracket") {
            let slash = self.glob.slash_literal.clone();
            self.push(Token { kind: "maybe_slash".into(), value: String::new(), output: Some(format!("{slash}?")), ..Token::default() });
        }

        if self.backtrack {
            self.output.clear();
            let tokens = self.tokens.clone();
            for token in &tokens {
                self.output.push_str(token.output.as_deref().unwrap_or(&token.value));
                if let Some(suffix) = &token.suffix {
                    self.output.push_str(suffix);
                }
            }
        }

        Ok(self.finish())
    }

    fn finish(&self) -> ParseState {
        ParseState {
            input: self.original_input.clone(),
            output: self.output.clone(),
            prefix: self.prefix.clone(),
            negated: self.negated,
            negated_extglob: self.negated_extglob,
            globstar: self.globstar,
        }
    }
}

fn globstar_source(glob: &super::constants::GlobChars, capture: &str, dot: bool) -> String {
    let dots = if dot { glob.dots_slash.clone() } else { glob.dot_literal.clone() };
    format!("({capture}(?:(?!{}{dots}).)*?)", glob.start_anchor)
}

fn collapse_backslash_runs(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    let mut out = String::new();
    let mut index = 0;
    while index < chars.len() {
        if chars[index] == '\\' {
            let mut end = index;
            while end < chars.len() && chars[end] == '\\' {
                end += 1;
            }
            let run = end - index;
            if run % 2 == 0 {
                out.push_str("\\\\");
            } else {
                out.push('\\');
            }
            index = end;
            continue;
        }
        out.push(chars[index]);
        index += 1;
    }
    out
}

fn starts_group(rest: &str) -> bool {
    let mut chars = rest.chars();
    chars.next() == Some('(') && chars.next().is_some_and(|ch| ch != '?')
}

fn is_dot_suffix(rest: &str) -> bool {
    let chars: Vec<char> = rest.chars().collect();
    if chars.len() < 2 || chars[0] != '.' {
        return false;
    }
    chars[1..].iter().all(|ch| !matches!(ch, '\\' | '/' | '.'))
}

fn has_angle_group(remaining: &str) -> bool {
    let chars: Vec<char> = remaining.chars().collect();
    for index in 0..chars.len() {
        if chars[index] != '<' {
            continue;
        }
        if let Some(next) = chars.get(index + 1) {
            if matches!(next, '!' | '=') {
                return true;
            }
            let mut end = index + 1;
            while end < chars.len() && (chars[end].is_ascii_alphanumeric() || chars[end] == '_') {
                end += 1;
            }
            if end > index + 1 && chars.get(end) == Some(&'>') {
                return true;
            }
        }
    }
    false
}

fn syntax_error(kind: &str, ch: char) -> String {
    format!("Missing {kind}: \"{ch}\" - use \"\\\\{ch}\" to match literal characters")
}

#[must_use]
pub fn fastpaths(input: &str, options: &Options) -> Option<String> {
    let max = options.max_length.map_or(MAX_LENGTH, |value| value.min(MAX_LENGTH));
    if js_length(input) > max {
        return None;
    }
    let input = replacement(input).unwrap_or(input);
    let mapped = js_units(input);
    let input = mapped.as_str();
    let glob = glob_chars(options.windows);
    let nodot = if options.dot { glob.no_dots.clone() } else { glob.no_dot.clone() };
    let slash_dot = if options.dot { glob.no_dots_slash.clone() } else { glob.no_dot_slash.clone() };
    let capture = if options.capture { String::new() } else { "?:".into() };
    let mut star = if options.bash { ".*?".to_string() } else { glob.star.clone() };
    if options.capture {
        star = format!("({star})");
    }

    fn create(str_value: &str, glob: &super::constants::GlobChars, nodot: &str, slash_dot: &str, star: &str, capture: &str, options: &Options) -> Option<String> {
        let globstar = |options: &Options| {
            if options.noglobstar {
                return star.to_string();
            }
            let dots = if options.dot { glob.dots_slash.clone() } else { glob.dot_literal.clone() };
            format!("({capture}(?:(?!{}{dots}).)*?)", glob.start_anchor)
        };
        match str_value {
            "*" => Some(format!("{nodot}{}{star}", glob.one_char)),
            ".*" => Some(format!("{}{}{star}", glob.dot_literal, glob.one_char)),
            "*.*" => Some(format!("{nodot}{star}{}{}{star}", glob.dot_literal, glob.one_char)),
            "*/*" => Some(format!("{nodot}{star}{}{}{slash_dot}{star}", glob.slash_literal, glob.one_char)),
            "**" => Some(format!("{nodot}{}", globstar(options))),
            "**/*" => Some(format!("(?:{nodot}{}{})?{slash_dot}{}{star}", globstar(options), glob.slash_literal, glob.one_char)),
            "**/*.*" => Some(format!("(?:{nodot}{}{})?{slash_dot}{star}{}{}{star}", globstar(options), glob.slash_literal, glob.dot_literal, glob.one_char)),
            "**/.*" => Some(format!("(?:{nodot}{}{})?{}{}{star}", globstar(options), glob.slash_literal, glob.dot_literal, glob.one_char)),
            _ => {
                let (head, ext) = match str_value.rfind('.') {
                    Some(index) => (&str_value[..index], &str_value[index + 1..]),
                    None => return None,
                };
                if ext.is_empty() || !ext.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_') {
                    return None;
                }
                let source = create(head, glob, nodot, slash_dot, star, capture, options)?;
                Some(format!("{source}{}{ext}", glob.dot_literal))
            }
        }
    }

    let mut prefix = None;
    let output = remove_prefix(input, &mut prefix);
    let mut source = create(&output, &glob, &nodot, &slash_dot, &star, &capture, options)?;
    if options.strict_slashes != Some(true) {
        source.push_str(&format!("{}?", glob.slash_literal));
    }
    Some(source)
}

#[must_use]
pub fn parse(input: &str, options: &Options) -> Result<ParseState, PicomatchError> {
    Parser::new(input, options)?.run()
}
