//! Model reference grammar and reasoning helpers (spec model-selection §2).

/// Reasoning levels in increasing order (`auto` is separate).
pub const LEVEL_ORDER: [&str; 7] = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];

fn level_of(token: &str) -> Option<&'static str> {
    let lower = token.trim().to_ascii_lowercase();
    if lower == "auto" {
        return Some("auto");
    }
    LEVEL_ORDER.iter().copied().find(|level| *level == lower)
}

/// Normalised reasoning: trimmed, lower-cased; empty → `None`; `none` → `off`; a known level or
/// `auto` → that level; anything else is passed through (lower-cased).
pub fn normalize_reasoning(value: &str) -> Option<String> {
    let lower = value.trim().to_ascii_lowercase();
    match lower.as_str() {
        "" => None,
        "none" => Some("off".into()),
        _ => Some(lower),
    }
}

/// Whether a (normalised) reasoning value is a known level or `auto`.
pub fn is_known_reasoning(value: &str) -> bool {
    level_of(value).is_some()
}

/// The highest allowed level not above `requested` (in [`LEVEL_ORDER`]); `None` when nothing
/// qualifies or `requested` is not a level.
pub fn clamp_reasoning<'a>(requested: &str, allowed: &[&'a str]) -> Option<&'a str> {
    let requested = normalize_reasoning(requested)?;
    let rank = LEVEL_ORDER.iter().position(|level| *level == requested)?;
    allowed
        .iter()
        .filter_map(|candidate| {
            let position = LEVEL_ORDER
                .iter()
                .position(|level| level.eq_ignore_ascii_case(candidate.trim()))?;
            (position <= rank).then_some((position, *candidate))
        })
        .max_by_key(|(position, _)| *position)
        .map(|(_, candidate)| candidate)
}

/// A trimmed model string, or `None` when empty or absent.
pub fn normalize_model(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
}

/// Replaces every `.` followed by a digit with `-` (`gpt-5.4` → `gpt-5-4`).
pub fn normalize_model_id(id: &str) -> String {
    let chars: Vec<char> = id.chars().collect();
    chars
        .iter()
        .enumerate()
        .map(|(i, c)| {
            if *c == '.' && chars.get(i + 1).is_some_and(char::is_ascii_digit) {
                '-'
            } else {
                *c
            }
        })
        .collect()
}

/// Splits a `:<level>` suffix. The suffix counts only when the part before the last `:` is
/// non-empty; `:max` counts only on provider-qualified references unless `allow_max` is set.
pub fn split_reasoning_suffix(reference: &str, allow_max: bool) -> (String, Option<String>) {
    if let Some(colon) = reference.rfind(':') {
        let (base, token) = (&reference[..colon], &reference[colon + 1..]);
        if !base.trim().is_empty()
            && let Some(level) = level_of(token)
            && (level != "max" || allow_max || base.contains('/'))
        {
            return (base.trim().to_owned(), Some(level.to_owned()));
        }
    }
    (reference.trim().to_owned(), None)
}

fn is_word_token(token: &str) -> bool {
    let mut chars = token.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Splits a variant off a delegated-task model: a trailing `(token)` is taken as written (even
/// when it is not a known level), then a `:level` suffix (allowing `:max`), then a trailing
/// space-separated word (lower-cased).
pub fn parse_variant(reference: &str) -> (String, Option<String>) {
    let text = reference.trim();
    if let Some(stripped) = text.strip_suffix(')')
        && let Some(open) = stripped.rfind('(')
    {
        let base = stripped[..open].trim();
        let token = stripped[open + 1..].trim();
        if !base.is_empty() && !token.is_empty() {
            return (base.to_owned(), Some(token.to_ascii_lowercase()));
        }
    }
    let (base, level) = split_reasoning_suffix(text, true);
    if level.is_some() {
        return (base, level);
    }
    if let Some(space) = text.rfind(char::is_whitespace) {
        let (base, token) = (text[..space].trim(), &text[space + 1..]);
        if !base.is_empty() && is_word_token(token) {
            return (base.to_owned(), Some(token.to_ascii_lowercase()));
        }
    }
    (text.to_owned(), None)
}

/// A parsed `provider/model[:variant]` string.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedModel {
    pub provider: String,
    pub model_id: String,
    pub variant: Option<String>,
}

impl ParsedModel {
    pub fn qualified(&self) -> String {
        format!("{}/{}", self.provider, self.model_id)
    }
}

/// Parses `provider/model[:variant]`; requires a non-empty provider and model around the first
/// `/`.
pub fn parse_model_string(reference: &str) -> Option<ParsedModel> {
    let (base, variant) = parse_variant(reference);
    let (provider, model_id) = base.split_once('/')?;
    let (provider, model_id) = (provider.trim(), model_id.trim());
    if provider.is_empty() || model_id.is_empty() {
        return None;
    }
    Some(ParsedModel {
        provider: provider.to_owned(),
        model_id: model_id.to_owned(),
        variant,
    })
}

/// The provider of a reference (text before the first `/`), if qualified.
pub fn provider_of(reference: &str) -> Option<&str> {
    reference
        .split_once('/')
        .map(|(provider, _)| provider)
        .filter(|p| !p.is_empty())
}
