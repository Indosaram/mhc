//! Pattern preflight (spec §4): common mistakes caught before the program is started.

use crate::error::{
    Phase, REWRITE_METAVARIABLE_KIND_MISMATCH, REWRITE_UNBOUND_METAVARIABLE,
    PATTERN_HINT_REJECTED, ToolError,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// How a finding is treated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    /// Rejects unless `force` is set.
    Hint,
    /// Rejects even with `force` (rewrite metavariable errors).
    Always,
    /// Never rejects; goes to the success warnings.
    Warning,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    pub code: &'static str,
    pub severity: Severity,
    pub message: String,
}

impl Finding {
    fn hint(message: impl Into<String>) -> Self {
        Self {
            code: PATTERN_HINT_REJECTED,
            severity: Severity::Hint,
            message: message.into(),
        }
    }
}

/// One metavariable occurrence: `$X` (dollars 1), `$$X` (2) or `$$$X` (3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetaUse {
    pub dollars: usize,
    pub name: String,
}

fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Every `$`-run in `text`, with the name that follows it.
pub fn meta_uses(text: &str) -> Vec<MetaUse> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '$' {
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && chars[i] == '$' {
            i += 1;
        }
        let dollars = i - start;
        let name_start = i;
        while i < chars.len() && is_name_char(chars[i]) {
            i += 1;
        }
        out.push(MetaUse {
            dollars,
            name: chars[name_start..i].iter().collect(),
        });
    }
    out
}

fn check_metavariables(pattern: &str, findings: &mut Vec<Finding>) {
    for usage in meta_uses(pattern) {
        if usage.dollars == 2 {
            findings.push(Finding::hint(format!(
                "`$${}` has exactly two dollar signs; use `$` for one node or `$$$` for several.",
                usage.name
            )));
        } else if (usage.dollars == 1 || usage.dollars == 3)
            && usage.name.chars().any(|c| c.is_ascii_lowercase())
        {
            findings.push(Finding::hint(format!(
                "Metavariable `{}{}` must be upper case (letters A-Z, digits, underscore); `$_` is the wildcard.",
                "$".repeat(usage.dollars),
                usage.name
            )));
        }
    }
}

fn check_regex_syntax(pattern: &str, findings: &mut Vec<Finding>) {
    let chars: Vec<char> = pattern.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if c == '\\'
            && let Some(next) = chars.get(i + 1)
            && "dDwWsSbB".contains(*next)
        {
            findings.push(Finding::hint(format!(
                "`\\{next}` is regular-expression syntax; patterns are code, not regular expressions."
            )));
        }
        if c == '.'
            && matches!(chars.get(i + 1), Some('*' | '+'))
            && !follows_metavariable(&chars, i)
        {
            findings.push(Finding::hint(
                "`.*` / `.+` is regular-expression syntax; use `$_` or `$$$` to match anything.",
            ));
        }
        if c == '['
            && let Some(close) = chars[i + 1..].iter().position(|&x| x == ']')
        {
            let inner = &chars[i + 1..i + 1 + close];
            let has_range = inner.windows(3).any(|w| {
                w[1] == '-'
                    && w[0] <= w[2]
                    && ((w[0].is_ascii_lowercase() && w[2].is_ascii_lowercase())
                        || (w[0].is_ascii_uppercase() && w[2].is_ascii_uppercase())
                        || (w[0].is_ascii_digit() && w[2].is_ascii_digit()))
            });
            if has_range && !inner.iter().any(|c| c.is_whitespace()) {
                findings.push(Finding::hint(
                    "A bracketed character range such as `[a-z]` is regular-expression syntax.",
                ));
            }
        }
    }
}

/// True when the char before the `.` at `dot` ends a `$NAME` metavariable.
fn follows_metavariable(chars: &[char], dot: usize) -> bool {
    let mut j = dot;
    while j > 0 && is_name_char(chars[j - 1]) {
        j -= 1;
    }
    j > 0 && chars[j - 1] == '$'
}

fn check_heads(pattern: &str, language: &str, findings: &mut Vec<Finding>) {
    let text = pattern.trim();
    let has_body = text.contains('{');
    let bodiless = |prefixes: &[&str]| {
        let words: Vec<&str> = text.split_whitespace().collect();
        let first = words
            .iter()
            .find(|word| !matches!(**word, "pub" | "async" | "export" | "default" | "const" | "unsafe"));
        first.is_some_and(|word| prefixes.contains(word)) && !has_body
    };
    match language {
        "rust" => {
            if bodiless(&["fn"]) && !text.ends_with(';') {
                findings.push(Finding::hint(
                    "A function head without a body cannot parse alone; include the `{ ... }` body.",
                ));
            }
        }
        "go" => {
            if bodiless(&["func"]) {
                findings.push(Finding::hint(
                    "A function head without a body cannot parse alone; include the `{ ... }` body.",
                ));
            }
        }
        "javascript" | "typescript" | "tsx" => {
            if bodiless(&["function", "function*", "class"]) {
                findings.push(Finding::hint(
                    "A function or class head without a body cannot parse alone; include the `{ ... }` body.",
                ));
            }
        }
        "python" => {
            let first = text.split_whitespace().next().unwrap_or_default();
            if matches!(first, "def" | "class" | "async") && text.ends_with(':') {
                findings.push(Finding::hint(
                    "A python head ending in a colon has no body and cannot parse alone; drop the colon or add the body.",
                ));
            }
        }
        _ => {}
    }
}

/// A single `|` between operands outside string literals.
fn has_single_pipe(pattern: &str) -> bool {
    let chars: Vec<char> = pattern.chars().collect();
    let mut quote: Option<char> = None;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if let Some(q) = quote {
            if c == '\\' {
                i += 1;
            } else if c == q {
                quote = None;
            }
        } else if matches!(c, '"' | '\'' | '`') {
            quote = Some(c);
        } else if c == '|' {
            let prev_pipe = i > 0 && chars[i - 1] == '|';
            let next_pipe = chars.get(i + 1) == Some(&'|');
            if prev_pipe || next_pipe {
                // `||`: skip the pair.
                i += if next_pipe { 1 } else { 0 };
            } else {
                let before = chars[..i].iter().rev().find(|c| !c.is_whitespace());
                let after = chars[i + 1..].iter().find(|c| !c.is_whitespace());
                if before.is_some() && after.is_some() {
                    return true;
                }
            }
        }
        i += 1;
    }
    false
}

/// Captures of the pattern: name -> true when captured as multi.
fn captures(pattern: &str) -> BTreeMap<String, bool> {
    let mut map = BTreeMap::new();
    for usage in meta_uses(pattern) {
        if usage.name.is_empty() || usage.name.starts_with('_') {
            continue;
        }
        match usage.dollars {
            1 => {
                map.entry(usage.name).or_insert(false);
            }
            3 => {
                map.insert(usage.name, true);
            }
            _ => {}
        }
    }
    map
}

fn check_rewrite(pattern: &str, rewrite: &str, findings: &mut Vec<Finding>) {
    let captured = captures(pattern);
    for usage in meta_uses(rewrite) {
        if usage.name.is_empty() || usage.name.starts_with('_') {
            continue;
        }
        let multi = match usage.dollars {
            1 => false,
            3 => true,
            _ => continue,
        };
        let sigil = "$".repeat(usage.dollars);
        match captured.get(&usage.name) {
            None => findings.push(Finding {
                code: REWRITE_UNBOUND_METAVARIABLE,
                severity: Severity::Always,
                message: format!(
                    "The replacement uses `{sigil}{}`, which the pattern does not capture.",
                    usage.name
                ),
            }),
            Some(&is_multi) if is_multi != multi => findings.push(Finding {
                code: REWRITE_METAVARIABLE_KIND_MISMATCH,
                severity: Severity::Always,
                message: format!(
                    "The replacement uses `{sigil}{}` as {}, but the pattern captures it as {}.",
                    usage.name,
                    if multi { "multi" } else { "single" },
                    if is_multi { "multi" } else { "single" },
                ),
            }),
            Some(_) => {}
        }
    }
}

/// All findings for a pattern (and replacement when rewriting).
pub fn findings(pattern: &str, language: &str, rewrite: Option<&str>) -> Vec<Finding> {
    let mut out = Vec::new();
    check_metavariables(pattern, &mut out);
    check_regex_syntax(pattern, &mut out);
    check_heads(pattern, language, &mut out);
    if has_single_pipe(pattern) {
        out.push(Finding {
            code: PATTERN_HINT_REJECTED,
            severity: Severity::Warning,
            message: "The pattern has a single `|`; if you meant logical or, write `||`.".into(),
        });
    }
    if let Some(rewrite) = rewrite {
        check_rewrite(pattern, rewrite, &mut out);
    }
    out.dedup();
    out
}

/// Apply the preflight: `Ok` carries the warnings for the success result.
pub fn preflight(
    pattern: &str,
    language: &str,
    rewrite: Option<&str>,
    force: bool,
) -> Result<Vec<String>, ToolError> {
    let all = findings(pattern, language, rewrite);
    let rejecting: Vec<&Finding> = all
        .iter()
        .filter(|f| match f.severity {
            Severity::Always => true,
            Severity::Hint => !force,
            Severity::Warning => false,
        })
        .collect();
    if let Some(first) = rejecting.first() {
        let listed: Vec<Value> = all
            .iter()
            .map(|f| json!({ "code": f.code, "message": f.message, "rejecting": rejecting.contains(&f) }))
            .collect();
        return Err(ToolError::new(first.code, Phase::Preflight, first.message.clone())
            .with_language(language)
            .with_details(json!({ "findings": listed })));
    }
    Ok(all.into_iter().map(|f| f.message).collect())
}
