//! Port of senpi `packages/coding-agent/src/core/reserved-keywords.ts`.
//!
//! OmO's reserved prompt keywords (`ulw`, `mass ulw`, `ulw plan`, `ulw loop`, `ulw research`).
//! `ulw`/`ultrawork` and the mass aliases are painted exactly where the ultrawork engine arms: their
//! spans come from [`maho_ultrawork::visible_keyword_spans`], so painting and arming share one
//! masking pass and one word-boundary rule. The skill keywords (`ulw plan`, `ulw loop`,
//! `ulw research`) keep the patterns below: case-insensitive, spaces or hyphens between words, each
//! name only as a word of its own. Overlapping mentions all match. For the skill keywords, injected
//! blocks, fenced/inline code, relay headers, block quotes, double quotes, URLs and negated
//! invocations are quoted material, never a request.
//!
//! Offsets are UTF-8 byte offsets into the searched text (TS reports UTF-16 code units); masking keeps
//! every byte offset, so spans index the original text.

use std::sync::LazyLock;

use fancy_regex::Regex as FancyRegex;
use regex::Regex;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReservedKeyword {
    Ultrawork,
    MassUlw,
    UlwPlan,
    UlwLoop,
    UlwResearch,
}

impl ReservedKeyword {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ultrawork => "ultrawork",
            Self::MassUlw => "mass-ulw",
            Self::UlwPlan => "ulw-plan",
            Self::UlwLoop => "ulw-loop",
            Self::UlwResearch => "ulw-research",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReservedKeywordSpan {
    pub keyword: ReservedKeyword,
    /// Byte offsets into the searched text, end exclusive.
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReservedKeywordRun {
    pub start: usize,
    pub end: usize,
}

const MASS_ULW_ALIAS: &str = r"(?:mass[\s_-]*ulw|ulw[\s_-]*mass|mulw|meth)";

// A keyword glued to a preceding identifier segment names something else; so does one that opens a path.
const NOT_AFTER_IDENTIFIER: &str = r"(?<![A-Za-z0-9][-_.]|[/\\])";
const NOT_BEFORE_PATH: &str = r"(?![-_.]?[A-Za-z0-9]*[/\\])";
// The name must end its own word; a chained skill word may follow if it, too, ends there.
const NOT_INTO_IDENTIFIER: &str = r"(?![-_](?!(?:loop|plan|research|execute|mass)(?![-_.]?[A-Za-z0-9]))[A-Za-z0-9]|\.[A-Za-z0-9])";
// JS `\b` without the `u` flag is ASCII-only, and every keyword starts and ends on a word character.
const WORD_START: &str = r"(?<![A-Za-z0-9_])";
const WORD_END: &str = r"(?![A-Za-z0-9_])";

fn fancy(pattern: &str) -> FancyRegex {
    FancyRegex::new(pattern).unwrap_or_else(|e| panic!("reserved keyword regex {pattern}: {e}"))
}

fn keyword_pattern(body: &str) -> FancyRegex {
    fancy(&format!(
        "(?i){NOT_AFTER_IDENTIFIER}{WORD_START}{body}{WORD_END}{NOT_INTO_IDENTIFIER}{NOT_BEFORE_PATH}"
    ))
}

static RESERVED_KEYWORD_PATTERNS: LazyLock<Vec<(ReservedKeyword, FancyRegex)>> =
    LazyLock::new(|| {
        vec![
            (ReservedKeyword::UlwPlan, keyword_pattern(r"ulw[\s-]*plan")),
            (ReservedKeyword::UlwLoop, keyword_pattern(r"ulw[\s-]*loop")),
            (
                ReservedKeyword::UlwResearch,
                keyword_pattern(&format!(r"(?:ulw|{MASS_ULW_ALIAS})[\s-]*research")),
            ),
        ]
    });

/// Cheap pre-check: every reserved keyword contains one of these stems.
static RESERVED_KEYWORD_STEM: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)ulw|ultrawork|mulw|meth").unwrap_or_else(|e| panic!("stem: {e}"))
});

static INJECTED_BLOCK: LazyLock<FancyRegex> = LazyLock::new(|| {
    fancy(
        r"(?i)<(omo-[a-z0-9-]+-pointer|ultrawork-mode|omo-ultrawork-(?:reminder|directive|mass))>[\s\S]*?</\1>",
    )
});
static INLINE_CODE: LazyLock<FancyRegex> =
    LazyLock::new(|| fancy(r"(?<!`)(`+)(?!`)[\s\S]*?(?<!`)\1(?!`)"));
// A relayed message opens with a sender header: all of it is someone else's text.
static RELAY_HEADER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^\s*(?:\[(?:[A-Z][A-Z0-9_-]{2,}|[A-Za-z0-9_.-]+ -> [A-Za-z0-9_.-]+)\]|[A-Z][A-Za-z0-9_.-]{0,30} \([^()\s]{1,40}\)(?:, | to )[A-Za-z0-9_.-]{1,40}:)",
    )
    .unwrap_or_else(|e| panic!("relay header: {e}"))
});
static BLOCK_QUOTE_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^ {0,3}>[^\n]*").unwrap_or_else(|e| panic!("block quote: {e}"))
});
static DOUBLE_QUOTED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new("\"[^\"\\n]*\"|\u{201C}[^\u{201D}\\n]*\u{201D}")
        .unwrap_or_else(|e| panic!("double quoted: {e}"))
});
static URL_SPAN: LazyLock<FancyRegex> =
    LazyLock::new(|| fancy(r#"(?i)(?<![a-z0-9+.-])[a-z][a-z0-9+.-]*://[^\s<>"'`]+"#));
// A keyword named only to forbid it is masked up to the next clause break.
static NEGATED_INVOCATION: LazyLock<FancyRegex> = LazyLock::new(|| {
    fancy(
        "(?i)(?<![A-Za-z0-9_])(?:do\\s+not|don['\u{2019}]t|dont|never)\\s+(?:load|use|run|invoke|start|trigger|arm|enable)(?![A-Za-z0-9_])[^.;:!?\\n]*?(?=[.;:!?\\n]|\\s(?:but|then|instead|however)(?![A-Za-z0-9_])|\\s[\u{2014}\u{2013}-]\\s|$)",
    )
});
static FENCE_START: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^ {0,3}(`{3,}|~{3,})[^\n]*(?:\n|$)")
        .unwrap_or_else(|e| panic!("fence start: {e}"))
});

/// Replaces every character but line breaks with NUL bytes, keeping each byte offset.
fn mask_range(text: &str, start: usize, end: usize) -> String {
    let mut masked = String::with_capacity(text.len());
    masked.push_str(&text[..start]);
    for character in text[start..end].chars() {
        if character == '\r' || character == '\n' {
            masked.push(character);
        } else {
            masked.extend(std::iter::repeat_n('\0', character.len_utf8()));
        }
    }
    masked.push_str(&text[end..]);
    masked
}

fn mask_ranges(text: String, ranges: Vec<(usize, usize)>) -> String {
    ranges
        .into_iter()
        .fold(text, |masked, (start, end)| mask_range(&masked, start, end))
}

fn mask_regex(text: String, pattern: &Regex) -> String {
    let ranges = pattern
        .find_iter(&text)
        .map(|m| (m.start(), m.end()))
        .collect();
    mask_ranges(text, ranges)
}

fn mask_fancy(text: String, pattern: &FancyRegex) -> String {
    let ranges = pattern
        .find_iter(&text)
        .filter_map(Result::ok)
        .map(|m| (m.start(), m.end()))
        .collect();
    mask_ranges(text, ranges)
}

fn mask_fences(mut visible: String) -> String {
    let mut position = 0;
    while position <= visible.len() {
        let Some(opening) = FENCE_START.captures_at(&visible, position) else {
            break;
        };
        let (Some(whole), Some(fence)) = (opening.get(0), opening.get(1)) else {
            break;
        };
        let (open_start, open_end) = (whole.start(), whole.end());
        let fence = fence.as_str();
        let fence_char = regex::escape(&fence[..1]);
        let closing = Regex::new(&format!(
            r"(?m)^ {{0,3}}{fence_char}{{{},}}[ \t]*\r?$",
            fence.len()
        ))
        .ok()
        .and_then(|pattern| pattern.find_at(&visible, open_end).map(|m| m.end()));
        let end = closing.unwrap_or(visible.len());
        visible = mask_range(&visible, open_start, end);
        if end == open_start {
            position = end + 1;
        } else {
            position = end;
        }
    }
    visible
}

/// A skill keyword needs the word `plan`, `loop` or `research`, matched with the same Unicode case
/// folding as the fancy patterns these guard, so they can never hide a match such a pattern would
/// have found (`(?i)` folds U+017F to `s`, which `to_lowercase` does not).
static SKILL_WORD: LazyLock<Vec<(ReservedKeyword, Regex)>> = LazyLock::new(|| {
    vec![
        (ReservedKeyword::UlwPlan, word_regex("plan")),
        (ReservedKeyword::UlwLoop, word_regex("loop")),
        (ReservedKeyword::UlwResearch, word_regex("research")),
    ]
});

fn word_regex(word: &str) -> Regex {
    Regex::new(&format!("(?i){word}")).unwrap_or_else(|e| panic!("skill word gate: {e}"))
}

/// A skill keyword needs the word `plan`, `loop` or `research`. Linear, and folded the same way as
/// the fancy patterns it guards, so it can never hide a match they would have found.
static SKILL_KEYWORD_GATE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new("(?i)plan|loop|research").unwrap_or_else(|e| panic!("skill gate: {e}"))
});
/// Every branch of [`NEGATED_INVOCATION`] opens with one of these; linear, same folding.
static NEGATION_GATE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new("(?i)do\\s+not|don['\u{2019}]t|dont|never")
        .unwrap_or_else(|e| panic!("negation gate: {e}"))
});

/// Quoted, injected, relayed and negated regions are masked (offsets kept) before matching.
///
/// A pass runs only when the text carries a substring its pattern requires. Masking replaces
/// characters but never introduces them, so a substring missing from the original text is missing
/// from every later intermediate too: skipping the pass is observably a no-op. These passes are
/// fancy-regex backtracking over the whole buffer and dominate this function, while a typical
/// draft trips none of them.
fn mask_quoted_regions(text: &str) -> String {
    if RELAY_HEADER.is_match(text) {
        return mask_range(text, 0, text.len());
    }
    let visible = if text.contains('<') {
        mask_fancy(text.to_owned(), &INJECTED_BLOCK)
    } else {
        text.to_owned()
    };
    let visible = if text.contains("```") || text.contains("~~~") {
        mask_fences(visible)
    } else {
        visible
    };
    let visible = if text.contains('`') {
        mask_fancy(visible, &INLINE_CODE)
    } else {
        visible
    };
    let visible = if text.contains('>') {
        mask_regex(visible, &BLOCK_QUOTE_LINE)
    } else {
        visible
    };
    let visible = if text.contains('"') || text.contains('\u{201C}') {
        mask_regex(visible, &DOUBLE_QUOTED)
    } else {
        visible
    };
    let visible = if text.contains("://") {
        mask_fancy(visible, &URL_SPAN)
    } else {
        visible
    };
    if NEGATION_GATE.is_match(text) {
        mask_fancy(visible, &NEGATED_INVOCATION)
    } else {
        visible
    }
}

/// Every reserved-keyword mention outside quoted regions, in text order.
pub fn find_reserved_keyword_spans(text: &str) -> Vec<ReservedKeywordSpan> {
    if !RESERVED_KEYWORD_STEM.is_match(text) {
        return Vec::new();
    }
    let mut spans: Vec<ReservedKeywordSpan> = maho_ultrawork::visible_keyword_spans(text)
        .into_iter()
        .map(|span| ReservedKeywordSpan {
            keyword: if span.mass {
                ReservedKeyword::MassUlw
            } else {
                ReservedKeyword::Ultrawork
            },
            start: span.start,
            end: span.end,
        })
        .collect();
    // Without `plan`/`loop`/`research` no skill pattern can match, so neither the masking passes
    // nor their scans can change the outcome; a draft naming only `ulw` stops here.
    if SKILL_KEYWORD_GATE.is_match(text) {
        let visible = mask_quoted_regions(text);
        for (keyword, pattern) in RESERVED_KEYWORD_PATTERNS.iter() {
            // Presence check only, in the guarded pattern's own folding; a hit still goes through the
            // full pattern below.
            let word_may_match = SKILL_WORD
                .iter()
                .find(|(candidate, _)| candidate == keyword)
                .is_some_and(|(_, word)| word.is_match(&visible));
            if !word_may_match {
                continue;
            }
            for found in pattern.find_iter(&visible).filter_map(Result::ok) {
                spans.push(ReservedKeywordSpan {
                    keyword: *keyword,
                    start: found.start(),
                    end: found.end(),
                });
            }
        }
    }
    spans.sort_by(|left, right| left.start.cmp(&right.start).then(right.end.cmp(&left.end)));
    spans
}

/// Overlapping mentions ("mass ulw research" is three) merge into one painted run, as on the desktop.
pub fn find_reserved_keyword_runs(text: &str) -> Vec<ReservedKeywordRun> {
    let mut runs: Vec<ReservedKeywordRun> = Vec::new();
    for span in find_reserved_keyword_spans(text) {
        match runs.last_mut() {
            Some(previous) if span.start <= previous.end => {
                previous.end = previous.end.max(span.end)
            }
            _ => runs.push(ReservedKeywordRun {
                start: span.start,
                end: span.end,
            }),
        }
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keywords_of(text: &str) -> Vec<&'static str> {
        let mut seen = Vec::new();
        for span in find_reserved_keyword_spans(text) {
            if !seen.contains(&span.keyword.as_str()) {
                seen.push(span.keyword.as_str());
            }
        }
        seen
    }

    fn runs_of(text: &str) -> Vec<&str> {
        find_reserved_keyword_runs(text)
            .iter()
            .map(|run| &text[run.start..run.end])
            .collect()
    }

    #[test]
    fn recognises_every_keyword_and_alias_the_engine_arms_on() {
        assert_eq!(keywords_of("ulw fix the login bug"), vec!["ultrawork"]);
        assert_eq!(keywords_of("please ULTRAWORK this"), vec!["ultrawork"]);
        for alias in [
            "mass ulw", "mass-ulw", "mass_ulw", "massulw", "ulw mass", "mulw", "meth",
        ] {
            assert!(
                keywords_of(&format!("{alias} the migration")).contains(&"mass-ulw"),
                "{alias}"
            );
        }
        assert!(keywords_of("ulw plan the refactor").contains(&"ulw-plan"));
        assert!(keywords_of("ulw-loop until green").contains(&"ulw-loop"));
        assert_eq!(keywords_of("ulwresearch the market"), vec!["ulw-research"]);
        assert_eq!(
            keywords_of("mulw research the market"),
            vec!["ulw-research", "mass-ulw"]
        );
    }

    #[test]
    fn needs_a_whole_word() {
        assert!(keywords_of("ulwfoo and ulw_helper and methane").is_empty());
        assert_eq!(keywords_of("ulw\u{d574}\u{c918}"), vec!["ultrawork"]);
    }

    #[test]
    fn ignores_keywords_inside_inline_code_fenced_code_and_injected_blocks() {
        assert!(keywords_of("rename `ulw` to something else").is_empty());
        assert!(keywords_of("```\nulw research\n```\nplain text").is_empty());
        assert!(keywords_of("<ultrawork-mode>ulw</ultrawork-mode> plain").is_empty());
        assert!(
            keywords_of("<omo-ultrawork-directive>ulw</omo-ultrawork-directive> plain").is_empty()
        );
        assert_eq!(runs_of("`ulw` then ulw loop"), vec!["ulw loop"]);
    }

    #[test]
    fn paints_nothing_the_engine_would_not_arm() {
        for text in [
            "https://x.com/ulw-research",
            "open ./ulw/plan.md",
            "see .omo/ulw/state",
            "branch feat/tui-ulw-rainbow",
            "edit ulw-plan.md",
            "the mass-ulw-refactor branch",
            "the senpi-ulw-loop helper",
            "bump v1.ulw",
            "he said \"ulw\" twice",
            "> ulw quoted",
            "Do not load mass-ulw or ulw-research.",
            "[REPORT] ulw done",
        ] {
            assert!(runs_of(text).is_empty(), "{text}");
        }
    }

    #[test]
    fn still_paints_the_request_around_masked_material() {
        assert_eq!(
            runs_of("read https://x.com/a then ulw the fix"),
            vec!["ulw"]
        );
        assert_eq!(runs_of("Do not use tmux, but ulw the fix"), vec!["ulw"]);
        assert_eq!(runs_of("mass ulw-loop the queue"), vec!["mass ulw-loop"]);
        assert_eq!(runs_of("Ulw-Research the market"), vec!["Ulw-Research"]);
    }

    // Intended divergence from upstream (no nested ultrawork span): docs/divergences.md ULW-8.
    #[test]
    fn returns_overlapping_spans_over_the_original_text() {
        let text = "go: mass ulw research";
        let spans: Vec<(&str, &str)> = find_reserved_keyword_spans(text)
            .iter()
            .map(|span| (span.keyword.as_str(), &text[span.start..span.end]))
            .collect();
        assert_eq!(
            spans,
            vec![
                ("ulw-research", "mass ulw research"),
                ("mass-ulw", "mass ulw")
            ]
        );
        assert!(find_reserved_keyword_spans("nothing reserved here").is_empty());
    }

    #[test]
    fn merges_overlapping_mentions_into_one_painted_run() {
        assert_eq!(
            runs_of("mass ulw research the docs, then ulw"),
            vec!["mass ulw research", "ulw"]
        );
        assert_eq!(runs_of("ulw fix the bug"), vec!["ulw"]);
        assert!(runs_of("fix the bug").is_empty());
    }

    /// The masking passes are skipped when the text cannot trip them; skipping must never change
    /// what is painted. Each case puts a keyword where a pass would have masked it or kept it.
    #[test]
    fn gated_masking_passes_match_an_ungated_pipeline() {
        for text in [
            "ulw go",
            "ulw plan the migration",
            "ulw loop on the failing test",
            "ulw research the field",
            "mass ulw research",
            "plan the migration",
            "loop over it",
            "research this",
            "```\nulw in a fence\n```\nulw go",
            "~~unterminated fence\nulw go",
            "`ulw` inline then ulw go",
            "\"quoted ulw\" then ulw go",
            "\u{201C}quoted ulw\u{201D} then ulw plan",
            "> quoted ulw\nulw go",
            "see https://x.dev/ulw then ulw go",
            "do not load ulw now. ulw go",
            "never run ulw-loop",
            "don't use ulw; ulw plan instead",
            "<ultrawork-mode>ulw inside</ultrawork-mode> then ulw go",
            "[alice -> bob] please ulw",
            "From: alice\nTo: bob\nulw go",
            "PLAN in caps",
            "ULW PLAN in caps",
            // `(?i)` folds U+017F to `s`, which `to_lowercase` alone does not: the presence gate must
            // use the guarded pattern's own folding or this spelling stops being painted.
            "ulw-re\u{17f}earch",
            "ulw lo\u{17f}p",
            "ulw pla\u{17f}n",
            // Negations must be recognized through a curly apostrophe too.
            "don\u{2019}t use ulw",
            "doesn\u{2019}t enable ulw",
            // A skill keyword inside each masked region, with a visible one outside it: these are the
            // only cases that can catch a masking pass being skipped when it was still needed.
            "`ulw plan` then ulw loop",
            "```\nulw plan\n```\nulw loop",
            "~~~\nulw research\n~~~\nulw plan",
            "> ulw plan\nulw loop",
            "\"ulw plan\" then ulw loop",
            "https://x.dev/ulw-research then ulw plan",
            "<ultrawork-mode>ulw plan</ultrawork-mode> then ulw loop",
            "\u{d55c}\u{ae00} ulw plan \u{d14c}\u{c2a4}\u{d2b8}",
            "nothing reserved here",
        ] {
            assert_eq!(
                find_reserved_keyword_spans(text),
                reference_reserved_keyword_spans(text),
                "{text:?}"
            );
        }
    }

    /// Every pass applied, no gate: the behavior the gates must reproduce exactly.
    fn reference_reserved_keyword_spans(text: &str) -> Vec<ReservedKeywordSpan> {
        let mut spans: Vec<ReservedKeywordSpan> = maho_ultrawork::visible_keyword_spans(text)
            .into_iter()
            .map(|span| ReservedKeywordSpan {
                keyword: if span.mass {
                    ReservedKeyword::MassUlw
                } else {
                    ReservedKeyword::Ultrawork
                },
                start: span.start,
                end: span.end,
            })
            .collect();
        if RESERVED_KEYWORD_STEM.is_match(text) {
            let visible = reference_mask_quoted_regions(text);
            for (keyword, pattern) in RESERVED_KEYWORD_PATTERNS.iter() {
                for found in pattern.find_iter(&visible).filter_map(Result::ok) {
                    spans.push(ReservedKeywordSpan {
                        keyword: *keyword,
                        start: found.start(),
                        end: found.end(),
                    });
                }
            }
        }
        spans.sort_by(|left, right| left.start.cmp(&right.start).then(right.end.cmp(&left.end)));
        spans
    }

    fn reference_mask_quoted_regions(text: &str) -> String {
        if RELAY_HEADER.is_match(text) {
            return mask_range(text, 0, text.len());
        }
        let visible = mask_fancy(text.to_owned(), &INJECTED_BLOCK);
        let visible = mask_fences(visible);
        let visible = mask_fancy(visible, &INLINE_CODE);
        let visible = mask_regex(visible, &BLOCK_QUOTE_LINE);
        let visible = mask_regex(visible, &DOUBLE_QUOTED);
        let visible = mask_fancy(visible, &URL_SPAN);
        mask_fancy(visible, &NEGATED_INVOCATION)
    }
}
