//! Port of senpi packages/ai/src/cursor/display-name.ts (senpi#3620).
//!
//! Cursor display-name cleanup for catalog identities.

use std::sync::LazyLock;

use regex::Regex;

use crate::cursor::model_capabilities::CursorModelCapability;

const NO_ZDR_MARK: &str = "\u{1}";

static NO_ZDR_RE: LazyLock<Option<Regex>> = LazyLock::new(|| Regex::new(r"\s*\(NO ZDR\)\s*").ok());
static LEVEL_WORDS_RE: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"\s+(Minimal|Low|Medium|High|Extra High|XHigh|Max|None)\b").ok());
static FAST_RE: LazyLock<Option<Regex>> = LazyLock::new(|| Regex::new(r"\s+Fast\b").ok());
static NO_THINKING_RE: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"\s+No Thinking\b").ok());
static THINKING_SUFFIX_RE: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"\s+Thinking\b").ok());
static THINKING_WORD_RE: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"\bThinking\b").ok());
static SIZE_TOKEN_RE: LazyLock<Option<Regex>> = LazyLock::new(|| Regex::new(r"\s+1M\b").ok());

fn replace_all(re: &LazyLock<Option<Regex>>, text: &str, with: &str) -> String {
    match re.as_ref() {
        Some(re) => re.replace_all(text, with).into_owned(),
        None => text.to_owned(),
    }
}

/// The part of a group member the name cleanup reads.
pub struct CursorNamedMember<'a> {
    pub name: &'a str,
    pub level: Option<&'a str>,
}

/// Drops zero-width characters and collapses whitespace in a raw Cursor label.
pub fn sanitize_cursor_display_name(name: &str) -> String {
    let stripped: String = name
        .chars()
        .filter(|c| !matches!(c, '\u{200B}'..='\u{200D}' | '\u{FEFF}'))
        .collect();
    stripped.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Names a grouped identity after its representative variant, without the level, fast and
/// thinking words that the identity's level map and thinking flag now carry.
pub fn clean_cursor_group_name(
    members: &[CursorNamedMember<'_>],
    base_id: &str,
    thinking_mode: Option<bool>,
) -> String {
    let representative = ["high", "medium", "low"]
        .iter()
        .find_map(|level| members.iter().find(|m| m.level == Some(*level)))
        .or_else(|| members.first());
    let Some(representative) = representative else {
        return base_id.to_owned();
    };
    let mut name = sanitize_cursor_display_name(representative.name);
    name = replace_all(&NO_ZDR_RE, &name, &format!(" {NO_ZDR_MARK}"))
        .trim()
        .to_owned();
    name = replace_all(&LEVEL_WORDS_RE, &name, "");
    name = replace_all(&FAST_RE, &name, "");
    if thinking_mode != Some(true) {
        name = replace_all(&NO_THINKING_RE, &name, "");
        name = replace_all(&THINKING_SUFFIX_RE, &name, "");
    } else if !THINKING_WORD_RE
        .as_ref()
        .is_some_and(|re| re.is_match(&name))
    {
        let marked = format!(" {NO_ZDR_MARK}");
        name = if name.contains(NO_ZDR_MARK) {
            name.replacen(&marked, &format!(" Thinking {NO_ZDR_MARK}"), 1)
        } else {
            format!("{name} Thinking")
        };
    }
    let name = sanitize_cursor_display_name(&name.replace(NO_ZDR_MARK, "(NO ZDR)"));
    if name.is_empty() {
        base_id.to_owned()
    } else {
        name
    }
}

/// A size token may stay in a display name only while the served window backs it.
pub fn drop_unserved_size_token(
    name: String,
    capability: Option<&CursorModelCapability>,
) -> String {
    match capability {
        Some(cap) if cap.evidence == "served-checkpoint" && cap.window < 1_000_000.0 => {
            sanitize_cursor_display_name(&replace_all(&SIZE_TOKEN_RE, &name, ""))
        }
        _ => name,
    }
}
