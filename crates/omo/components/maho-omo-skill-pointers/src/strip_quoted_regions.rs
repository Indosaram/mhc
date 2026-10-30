use fancy_regex::Regex as FancyRegex;
use regex::Regex;
use std::sync::LazyLock;

static INJECTED_BLOCK: LazyLock<FancyRegex> = LazyLock::new(|| {
    FancyRegex::new(r"(?i)<(omo-[a-z0-9-]+-(?:pointer|mode)|ultrawork-mode|omo-ultrawork-reminder)>[\s\S]*?</\1>")
        .expect("valid injected block regex")
});

static INLINE_CODE: LazyLock<FancyRegex> = LazyLock::new(|| {
    FancyRegex::new(r"(?<!`)(`+)(?!`)[\s\S]*?(?<!`)\1(?!`)").expect("valid inline code regex")
});

static RELAY_HEADER: LazyLock<Regex> = LazyLock::new(|| {
    match Regex::new(
        r"^\s*(?:\[(?:[A-Z][A-Z0-9_-]{2,}|[A-Za-z0-9_.:@#-]+(?: via [A-Za-z0-9_.:@#-]+)* -> [A-Za-z0-9_.:@#-]+[^\]\n]{0,160})\]|[A-Z][A-Za-z0-9_.-]{0,30} \([^()\s]{1,40}\)(?:, | to )[A-Za-z0-9_.-]{1,40}:)",
    ) {
        Ok(regex) => regex,
        Err(error) => panic!("invalid relay header regex: {error}"),
    }
});
static BLOCK_QUOTE_LINE: LazyLock<Regex> =
    LazyLock::new(|| match Regex::new(r"(?m)^ {0,3}>[^\n]*") {
        Ok(regex) => regex,
        Err(error) => panic!("invalid block quote regex: {error}"),
    });
static DOUBLE_QUOTED: LazyLock<Regex> =
    LazyLock::new(
        || match Regex::new("\"[^\"\n]*\"|\u{201C}[^\u{201D}\n]*\u{201D}") {
            Ok(regex) => regex,
            Err(error) => panic!("invalid double quote regex: {error}"),
        },
    );
static SINGLE_QUOTED: LazyLock<FancyRegex> = LazyLock::new(|| {
    FancyRegex::new(r"(?<![A-Za-z0-9_'])'[^'\n]{1,200}'(?![A-Za-z0-9_])|‘[^’\n]{1,200}’")
        .expect("valid single quote regex")
});
static URL_SPAN: LazyLock<FancyRegex> = LazyLock::new(|| {
    match FancyRegex::new(r#"(?i)(?<![a-z0-9+.-])[a-z][a-z0-9+.-]*://[^\s<>"'`]+"#) {
        Ok(regex) => regex,
        Err(error) => panic!("invalid URL span regex: {error}"),
    }
});
static NEGATED_INVOCATION: LazyLock<FancyRegex> = LazyLock::new(|| {
    match FancyRegex::new(
        "(?i)(?<![A-Za-z0-9_])(?:do\\s+not|don['\u{2019}]t|dont|never)\\s+(?:load|use|run|invoke|start|trigger|arm|enable)(?![A-Za-z0-9_])[^.;:!?\\n]*?(?=[.;:!?\\n]|\\s(?:but|then|instead|however)\\b|\\s[\u{2014}\u{2013}-]\\s|$)",
    ) {
        Ok(regex) => regex,
        Err(error) => panic!("invalid negated invocation regex: {error}"),
    }
});

pub const NOT_AFTER_IDENTIFIER: &str = r"(?<![A-Za-z0-9][-_.]|[/\\])";
pub const NOT_BEFORE_PATH: &str = r"(?![-_.]?[A-Za-z0-9]*[/\\])";
pub const NOT_INTO_IDENTIFIER: &str = r"(?![-_](?!(?:loop|plan|research|execute|mass)(?![-_.]?[A-Za-z0-9]))[A-Za-z0-9]|\.[A-Za-z0-9])";

static FENCE_START: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^ {0,3}(`{3,}|~{3,})[^\n]*(?:\n|$)").expect("valid fence start regex")
});

/// Preserve offsets for the ultrawork /skill: argument check. Non-whitespace masking also prevents
/// a skill name from being fabricated across removed text, such as "ulw `not a request` loop".
pub fn mask(region: &str) -> String {
    let mut out = String::with_capacity(region.len());
    for c in region.chars() {
        if matches!(c, '\r' | '\n') {
            out.push(c);
        } else {
            // One NUL per UTF-8 byte keeps byte offsets aligned with the original text.
            for _ in 0..c.len_utf8() {
                out.push('\0');
            }
        }
    }
    out
}

pub fn strip_quoted_spans(text: &str) -> String {
    if RELAY_HEADER.is_match(text) {
        return mask(text);
    }
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
        let fence_len = trimmed_leading
            .chars()
            .take_while(|&c| c == fence_char)
            .count();

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

    let visible = INLINE_CODE.replace_all(&visible, |caps: &fancy_regex::Captures| {
        mask(caps.get(0).map_or("", |m| m.as_str()))
    });
    let visible =
        BLOCK_QUOTE_LINE.replace_all(&visible, |caps: &regex::Captures<'_>| mask(&caps[0]));
    let visible = DOUBLE_QUOTED.replace_all(&visible, |caps: &regex::Captures<'_>| mask(&caps[0]));
    let visible = SINGLE_QUOTED.replace_all(&visible, |caps: &fancy_regex::Captures<'_>| {
        mask(caps.get(0).map_or("", |m| m.as_str()))
    });
    URL_SPAN
        .replace_all(&visible, |caps: &fancy_regex::Captures<'_>| {
            mask(caps.get(0).map_or("", |m| m.as_str()))
        })
        .into_owned()
}

pub fn mask_negated_invocations(text: &str) -> String {
    NEGATED_INVOCATION
        .replace_all(text, |caps: &fancy_regex::Captures<'_>| {
            mask(caps.get(0).map_or("", |m| m.as_str()))
        })
        .into_owned()
}

pub fn strip_quoted_regions(text: &str) -> String {
    mask_negated_invocations(&strip_quoted_spans(text))
}
