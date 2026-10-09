use fancy_regex::Regex as FancyRegex;
use regex::Regex;
use std::sync::LazyLock;

static INJECTED_BLOCK: LazyLock<FancyRegex> = LazyLock::new(|| {
    FancyRegex::new(r"(?i)<(omo-[a-z0-9-]+-pointer|ultrawork-mode|omo-ultrawork-reminder)>[\s\S]*?</\1>")
        .expect("valid injected block regex")
});

static INLINE_CODE: LazyLock<FancyRegex> = LazyLock::new(|| {
    FancyRegex::new(r"(?<!`)(`+)(?!`)[\s\S]*?(?<!`)\1(?!`)")
        .expect("valid inline code regex")
});

static FENCE_START: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^ {0,3}(`{3,}|~{3,})[^\n]*(?:\n|$)")
        .expect("valid fence start regex")
});

/// Preserve offsets for the ultrawork /skill: argument check. Non-whitespace masking also prevents
/// a skill name from being fabricated across removed text, such as "ulw `not a request` loop".
pub fn mask(region: &str) -> String {
    let mut out = String::with_capacity(region.len());
    for c in region.chars() {
        if matches!(c, '\r' | '\n') {
            out.push(c);
        } else {
            out.push('\0');
        }
    }
    out
}

pub fn strip_quoted_regions(text: &str) -> String {
    let mut visible = match INJECTED_BLOCK.replace_all(text, |caps: &fancy_regex::Captures| {
        mask(caps.get(0).map_or("", |m| m.as_str()))
    }) {
        std::borrow::Cow::Borrowed(s) => s.to_string(),
        std::borrow::Cow::Owned(s) => s,
    };

    let mut start_pos = 0;
    while start_pos < visible.len() {
        let Some(opening) = FENCE_START.find_at(&visible, start_pos) else {
            break;
        };

        let full_opening_str = opening.as_str();
        let trimmed_leading = full_opening_str.trim_start_matches(' ');
        let fence_char = match trimmed_leading.chars().next() {
            Some(c @ ('`' | '~')) => c,
            _ => {
                start_pos = opening.end();
                continue;
            }
        };
        let fence_len = trimmed_leading.chars().take_while(|&c| c == fence_char).count();

        let fence_end_pattern = format!(
            r"(?m)^ {{0,3}}{}{{{},}}[ \t]*\r?$",
            regex::escape(&fence_char.to_string()),
            fence_len
        );

        let end_idx = if let Ok(fence_end_re) = Regex::new(&fence_end_pattern) {
            if let Some(closing) = fence_end_re.find_at(&visible, opening.end()) {
                closing.end()
            } else {
                visible.len()
            }
        } else {
            visible.len()
        };

        let opening_start = opening.start();
        let masked = mask(&visible[opening_start..end_idx]);
        let masked_len = masked.len();
        visible.replace_range(opening_start..end_idx, &masked);
        start_pos = opening_start + masked_len;
    }

    match INLINE_CODE.replace_all(&visible, |caps: &fancy_regex::Captures| {
        mask(caps.get(0).map_or("", |m| m.as_str()))
    }) {
        std::borrow::Cow::Borrowed(s) => s.to_string(),
        std::borrow::Cow::Owned(s) => s,
    }
}
