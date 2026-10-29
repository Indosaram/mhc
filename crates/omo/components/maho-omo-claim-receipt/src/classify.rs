//! Port of `claim-receipt/classify.ts`: find success claims in an answer's prose.
//!
//! Patterns run over text whose code, quotes and relayed reports are already masked to NUL, so
//! `[^.\n\0]` keeps a match inside one visible clause. Korean never uses `\b`: it does not fire
//! after Hangul.

use std::sync::LazyLock;

use fancy_regex::Regex as FancyRegex;
use regex::Regex;

/// The kind of work a claim says was done.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ClaimKind {
    Test,
    Typecheck,
    Lint,
    Build,
    Run,
    Exists,
    Done,
}

impl ClaimKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Test => "test",
            Self::Typecheck => "typecheck",
            Self::Lint => "lint",
            Self::Build => "build",
            Self::Run => "run",
            Self::Exists => "exists",
            Self::Done => "done",
        }
    }
}

/// How firmly the answer states the claim.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClaimStrength {
    Asserted,
    Hedged,
}

impl ClaimStrength {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Asserted => "asserted",
            Self::Hedged => "hedged",
        }
    }
}

/// One success claim found in the answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Claim {
    pub kind: ClaimKind,
    pub strength: ClaimStrength,
    pub admitted: bool,
    pub reported: bool,
    pub sentence: String,
    pub rule: &'static str,
}

struct ClaimRule {
    id: &'static str,
    kind: ClaimKind,
    pattern: &'static str,
}

static CLAIM_RULES: &[ClaimRule] = &[
    ClaimRule {
        id: "test.zero-fail",
        kind: ClaimKind::Test,
        pattern: r"(?i)(?<![0-9])0\s*(?:failed|failures?|fail)\b",
    },
    ClaimRule {
        id: "test.ko-zero-fail",
        kind: ClaimKind::Test,
        pattern: r"실패\s*:?\s*0(?![0-9])",
    },
    ClaimRule {
        id: "test.n-of-n",
        kind: ClaimKind::Test,
        pattern: r"(?i)(?<![\w.])(\d+)\s*/\s*\1\s*(?:pass(?:ed|ing|es)?|통과)",
    },
    ClaimRule {
        id: "test.count-passed",
        kind: ClaimKind::Test,
        pattern: r"(?i)(?<![\w.])[1-9]\d*\s+pass(?:ed|ing)?\b",
    },
    ClaimRule {
        id: "test.tests-pass",
        kind: ClaimKind::Test,
        pattern: r"(?i)\btests?\b[^.\n\x00]{0,30}?\b(?:pass(?:es|ed|ing)?|(?:are|is|stays?) green)\b",
    },
    ClaimRule {
        id: "test.suite-pass",
        kind: ClaimKind::Test,
        pattern: r"(?i)\b(?:suite|ci|checks)\b[^.\n\x00]{0,20}?\b(?:pass(?:es|ed)?|green|succeed(?:s|ed)?)\b",
    },
    ClaimRule {
        id: "test.should-pass",
        kind: ClaimKind::Test,
        pattern: r"(?i)\b(?:should|will|would)\s+(?:now\s+|still\s+|all\s+)?pass\b",
    },
    ClaimRule {
        id: "test.ko-tests",
        kind: ClaimKind::Test,
        pattern: r"테스트[^.\n\x00]{0,15}통과",
    },
    ClaimRule {
        id: "test.ko-all",
        kind: ClaimKind::Test,
        pattern: r"(?:모두|전부|다)\s*통과",
    },
    ClaimRule {
        id: "test.ko-passed",
        kind: ClaimKind::Test,
        pattern: r"통과(?:했|합니다|해요|함|됐|된다|될|할)",
    },
    ClaimRule {
        id: "test.ko-count",
        kind: ClaimKind::Test,
        pattern: r"(?<![0-9])[1-9]\d*\s*(?:개|건)?\s*통과",
    },
    ClaimRule {
        id: "typecheck.clean",
        kind: ClaimKind::Typecheck,
        pattern: r"(?i)\b(?:typecheck(?:s|ing)?|type ?check(?:s|ing)?|tsc|tsgo|types)\b[^.\n\x00]{0,20}?\b(?:pass(?:es|ed)?|clean|green|succeed(?:s|ed)?|ok)\b",
    },
    ClaimRule {
        id: "typecheck.no-errors",
        kind: ClaimKind::Typecheck,
        pattern: r"(?i)\bno (?:type|ts|typescript) errors?\b",
    },
    ClaimRule {
        id: "typecheck.ko",
        kind: ClaimKind::Typecheck,
        pattern: r"타입\s*(?:체크|검사)[^.\n\x00]{0,12}(?:통과|성공|깨끗)|타입\s*(?:에러|오류)\s*(?:없|0)",
    },
    ClaimRule {
        id: "lint.clean",
        kind: ClaimKind::Lint,
        pattern: r"(?i)\blint(?:er|ing)?\b[^.\n\x00]{0,20}?\b(?:pass(?:es|ed)?|clean|green)\b|린트[^.\n\x00]{0,12}(?:통과|성공|깨끗)",
    },
    ClaimRule {
        id: "build.ok",
        kind: ClaimKind::Build,
        pattern: r"(?i)\bbuilds?\b[^.\n\x00]{0,15}?\b(?:pass(?:es|ed)?|succeed(?:s|ed)?|green|works|ok)\b",
    },
    ClaimRule {
        id: "build.ko",
        kind: ClaimKind::Build,
        pattern: r"빌드[^.\n\x00]{0,12}(?:성공|통과|됩니다|됐|돼요)",
    },
    ClaimRule {
        id: "run.works",
        kind: ClaimKind::Run,
        pattern: r"(?i)\b(?:works|is working|runs) (?:as expected|correctly|end to end|fine)\b|(?:정상(?:적으로)?|잘)\s*(?:동작|작동)",
    },
    ClaimRule {
        id: "exists.already",
        kind: ClaimKind::Exists,
        pattern: r"(?i)\balready\s+(?:exists?|implemented|in place|supported|wired up|present)\b",
    },
    ClaimRule {
        id: "exists.ko",
        kind: ClaimKind::Exists,
        pattern: r"이미\s*(?:있|존재|구현(?:되어|돼|됐|되었|했|해)|되어|돼|지원(?:되|돼|해|합)|반영(?:되|돼|됐|했)|들어)",
    },
    ClaimRule {
        id: "done.leading",
        kind: ClaimKind::Done,
        pattern: r"(?i)^(?:done|completed|finished|fixed|implemented|verified|confirmed|all set)\b",
    },
    ClaimRule {
        id: "done.ko-leading",
        kind: ClaimKind::Done,
        pattern: r"^(?:완료|끝|구현 완료|수정 완료|확인 완료)",
    },
    ClaimRule {
        id: "done.ko",
        kind: ClaimKind::Done,
        pattern: r"완료(?:했|됐|되었|입니다|됨)|끝났(?:어요|습니다|다)",
    },
];

static CLAIM_PATTERNS: LazyLock<Vec<FancyRegex>> = LazyLock::new(|| {
    CLAIM_RULES
        .iter()
        .map(|rule| FancyRegex::new(rule.pattern).expect("valid claim rule regex"))
        .collect()
});

fn pattern(source: &str) -> FancyRegex {
    FancyRegex::new(source).expect("valid claim regex")
}

static NONZERO_FAILURES: LazyLock<FancyRegex> = LazyLock::new(|| {
    pattern(r"(?i)(?<![0-9])[1-9]\d*\s*(?:failed|failures?|fail)\b|실패\s*:?\s*[1-9]|(?<![0-9])[1-9]\d*\s*(?:건|개)?\s*실패")
});
static EN_NEGATION: LazyLock<FancyRegex> =
    LazyLock::new(|| pattern(r"(?i)\b(?:not|never|cannot|no longer)\b|n't\b"));
static EN_FAILURE_AFTER: LazyLock<FancyRegex> =
    LazyLock::new(|| pattern(r"(?i)\b(?:fail(?:s|ed|ing|ures?)?|broken|red)\b"));
static KO_NEGATION: LazyLock<FancyRegex> = LazyLock::new(|| {
    pattern(r"않|못|아니|(?:^|\s)안\s|미(?:통과|완료|구현|검증|반영)|불통과|실패(?:하|했|해|함|입니다|예요|중)|(?:에러|오류)가\s*(?:나|발생)")
});
static EN_ADMISSION: LazyLock<FancyRegex> = LazyLock::new(|| {
    pattern(r"(?i)\b(?:not (?:yet )?(?:been )?(?:run|ran|verified|tested|checked|executed)|(?:haven't|have not|hasn't|has not|didn't|did not|couldn't|could not)\s+(?:yet\s+)?(?:run|verify|test|check|execute|re-?run)|untested|unverified|without (?:running|testing|verifying))\b")
});
static KO_ADMISSION: LazyLock<FancyRegex> = LazyLock::new(|| {
    pattern(r"아직\s*(?:안|못)|안\s*(?:돌렸|돌려|돌림|해\s*봤|실행)|못\s*(?:돌렸|돌려|돌림|해\s*봤|실행)|돌리지\s*(?:않|못)|실행(?:하지|은)?\s*(?:않|못|안)|미검증|미확인|(?:검증|확인)(?:하지|은|도)?\s*(?:전|안|못)|(?:검증|확인)되지\s*않")
});
static HEDGE: LazyLock<FancyRegex> = LazyLock::new(|| {
    pattern(r"(?i)\b(?:should|would|ought to|expected to|likely|probably|presumably|i think|i believe|i assume|might)\b|거예요|겁니다|거야|것\s*같|거\s*같|듯|아마|예상|(?:할|될|일)\s*(?:거|것)")
});
static HANDOFF_LINE: LazyLock<FancyRegex> =
    LazyLock::new(|| pattern(r"(?i)^(?:Ask|Now|Next|wanted):"));
static CONDITIONAL: LazyLock<FancyRegex> = LazyLock::new(|| {
    pattern(r"(?i)\b(?:if|once|when|whenever|whether|until|unless|as soon as|as each)\b|(?:하|되|으|이|라)면(?=[\s,.]|$)|(?:할|될)\s*때")
});
static REPORTED_RESULT: LazyLock<FancyRegex> = LazyLock::new(|| {
    pattern(r"(?i)(?<![\w.])\d+\s*(?:/\s*\d+|pass(?:ed|ing)?|tests?|fail(?:ed|ures?)?)\b|(?<![\w.])\d+\s+(?:of\s+\d+|(?:[A-Za-z-]+\s+){1,2}tests?)\b|(?<![\w.])\d+\s*(?:개|건)|\bexit(?:s|ed)?\s*(?:code\s*)?0\b|\b[A-Z_]*EXIT=0\b")
});
static LEADING_MARKER: LazyLock<FancyRegex> = LazyLock::new(|| {
    pattern("^[\\s\\x00]*(?:[-*\\u{2022}]|\\d+[.)]|[\\u{2705}\\u{2714}\\u{2611}]\\u{fe0f}?)?\\s*")
});

// The quote-region scanner of `strip-quoted-regions.ts`, re-implemented here so masking keeps BYTE
// offsets: the answer's own offsets index the original text (`span.start..span.end`), and a mask
// that replaced a multi-byte character with one NUL would shift every later index.
static RELAY_HEADER: LazyLock<FancyRegex> = LazyLock::new(|| {
    pattern(r"^\s*(?:\[(?:[A-Z][A-Z0-9_-]{2,}|[\w.:@#-]+(?: via [\w.:@#-]+)* -> [\w.:@#-]+[^\]\n]{0,160})\]|[A-Z][\w.-]{0,30} \([^()\s]{1,40}\)(?:, | to )[\w.-]{1,40}:)")
});
static INJECTED_BLOCK: LazyLock<FancyRegex> = LazyLock::new(|| {
    pattern(r"(?i)<(omo-[a-z0-9-]+-(?:pointer|mode)|ultrawork-mode|omo-ultrawork-reminder)>[\s\S]*?</\1>")
});
static INLINE_CODE: LazyLock<FancyRegex> =
    LazyLock::new(|| pattern(r"(?<!`)(`+)(?!`)[\s\S]*?(?<!`)\1(?!`)"));
static BLOCK_QUOTE_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^ {0,3}>[^\n]*").expect("valid block quote line regex")
});
static DOUBLE_QUOTED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new("\"[^\"\n]*\"|\u{201C}[^\u{201D}\n]*\u{201D}").expect("valid double quoted regex")
});
static SINGLE_QUOTED: LazyLock<FancyRegex> = LazyLock::new(|| {
    pattern("(?<![\\w'])'[^'\\n]{1,200}'(?![\\w])|\u{2018}[^\u{2019}\\n]{1,200}\u{2019}")
});
static URL_SPAN: LazyLock<FancyRegex> = LazyLock::new(|| {
    pattern(r#"(?i)(?<![a-z0-9+.-])[a-z][a-z0-9+.-]*://[^\s<>"'`]+"#)
});
static FENCE_START: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^ {0,3}(`{3,}|~{3,})[^\n]*(?:\n|$)").expect("valid fence start regex")
});
static EXTRA_QUOTES: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new("\u{2018}[^\u{2019}\n]*\u{2019}|[\u{300C}\u{300E}][^\u{300D}\u{300F}\n]*[\u{300D}\u{300F}]")
        .expect("valid extra quotes regex")
});

const SELF_NEGATING_RULES: [&str; 4] = [
    "test.zero-fail",
    "test.ko-zero-fail",
    "typecheck.no-errors",
    "typecheck.ko",
];

fn mask_region(region: &str) -> String {
    let mut masked = String::with_capacity(region.len());
    for character in region.chars() {
        if character == '\r' || character == '\n' {
            masked.push(character);
        } else {
            for _ in 0..character.len_utf8() {
                masked.push('\0');
            }
        }
    }
    masked
}

fn strip_quoted_spans(text: &str) -> String {
    if RELAY_HEADER.is_match(text).unwrap_or(false) {
        return mask_region(text);
    }
    let mut visible = match INJECTED_BLOCK.replace_all(text, |captures: &fancy_regex::Captures| {
        mask_region(captures.get(0).map_or("", |matched| matched.as_str()))
    }) {
        std::borrow::Cow::Borrowed(borrowed) => borrowed.to_owned(),
        std::borrow::Cow::Owned(owned) => owned,
    };

    let mut start_pos = 0usize;
    while start_pos < visible.len() {
        let Some(opening) = FENCE_START.find_at(&visible, start_pos) else {
            break;
        };
        let opening_str = opening.as_str();
        let trimmed = opening_str.trim_start_matches(' ');
        let Some(fence_char) = trimmed.chars().next().filter(|c| *c == '`' || *c == '~') else {
            start_pos = opening.end();
            continue;
        };
        let fence_len = trimmed.chars().take_while(|c| *c == fence_char).count();
        let fence_end_pattern = format!(
            r"(?m)^ {{0,3}}{}{{{},}}[ \t]*\r?$",
            regex::escape(&fence_char.to_string()),
            fence_len
        );
        let end_index = match Regex::new(&fence_end_pattern) {
            Ok(fence_end) => fence_end
                .find_at(&visible, opening.end())
                .map_or(visible.len(), |closing| closing.end()),
            Err(_) => visible.len(),
        };
        let opening_start = opening.start();
        let masked = mask_region(&visible[opening_start..end_index]);
        let masked_len = masked.len();
        visible.replace_range(opening_start..end_index, &masked);
        start_pos = opening_start + masked_len;
    }

    let visible = INLINE_CODE.replace_all(&visible, |captures: &fancy_regex::Captures| {
        mask_region(captures.get(0).map_or("", |matched| matched.as_str()))
    });
    let visible = BLOCK_QUOTE_LINE.replace_all(&visible, |captures: &regex::Captures<'_>| {
        mask_region(&captures[0])
    });
    let visible = DOUBLE_QUOTED.replace_all(&visible, |captures: &regex::Captures<'_>| {
        mask_region(&captures[0])
    });
    let visible = SINGLE_QUOTED.replace_all(&visible, |captures: &fancy_regex::Captures| {
        mask_region(captures.get(0).map_or("", |matched| matched.as_str()))
    });
    URL_SPAN
        .replace_all(&visible, |captures: &fancy_regex::Captures| {
            mask_region(captures.get(0).map_or("", |matched| matched.as_str()))
        })
        .into_owned()
}

#[must_use]
pub fn mask_quoted_prose(text: &str) -> String {
    let spans = strip_quoted_spans(text);
    EXTRA_QUOTES
        .replace_all(&spans, |captures: &regex::Captures<'_>| mask_region(&captures[0]))
        .into_owned()
}

/// A sentence's byte range inside the masked text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

#[must_use]
pub fn split_sentences(masked: &str) -> Vec<Span> {
    let characters: Vec<(usize, char)> = masked.char_indices().collect();
    let mut spans: Vec<Span> = Vec::new();
    let mut start = 0usize;
    for (position, (index, character)) in characters.iter().enumerate() {
        let next = characters.get(position + 1).map(|(_, next)| *next);
        let terminal = matches!(character, '.' | '!' | '?' | '\u{3002}')
            && next.is_none_or(char::is_whitespace);
        if *character != '\n' && !terminal {
            continue;
        }
        let end = if terminal {
            index + character.len_utf8()
        } else {
            *index
        };
        if has_visible(&masked[start..end]) {
            spans.push(Span { start, end });
        }
        start = end;
    }
    if has_visible(&masked[start..]) {
        spans.push(Span {
            start,
            end: masked.len(),
        });
    }
    spans
}

fn has_visible(text: &str) -> bool {
    text.chars()
        .any(|character| character != '\0' && !character.is_whitespace())
}

fn blank(text: &str, start: usize, end: usize) -> String {
    let mut blanked = String::with_capacity(text.len());
    blanked.push_str(&text[..start]);
    blanked.push_str(&" ".repeat(end - start));
    blanked.push_str(&text[end..]);
    blanked
}

struct FoundClaim {
    rule: &'static ClaimRule,
    index: usize,
    length: usize,
}

fn find_claim(sentence: &str) -> Option<FoundClaim> {
    let lead = LEADING_MARKER
        .find(sentence)
        .ok()
        .flatten()
        .map_or(0, |matched| matched.end());
    let body = sentence.get(lead..).unwrap_or("").trim_end();
    if HANDOFF_LINE.is_match(body).unwrap_or(false) {
        return None;
    }
    let tests_excluded = NONZERO_FAILURES.is_match(body).unwrap_or(false);
    for (position, rule) in CLAIM_RULES.iter().enumerate() {
        if tests_excluded && rule.kind == ClaimKind::Test {
            continue;
        }
        let Some(pattern) = CLAIM_PATTERNS.get(position) else {
            continue;
        };
        let Ok(Some(matched)) = pattern.find(body) else {
            continue;
        };
        return Some(FoundClaim {
            rule,
            index: lead + matched.start(),
            length: matched.end() - matched.start(),
        });
    }
    None
}

fn is_negated(sentence: &str, rule_id: &str, claim_start: usize, claim_end: usize) -> bool {
    let rest = if SELF_NEGATING_RULES.contains(&rule_id) {
        blank(sentence, claim_start, claim_end)
    } else {
        sentence.to_owned()
    };
    if EN_NEGATION.is_match(&rest).unwrap_or(false) || KO_NEGATION.is_match(&rest).unwrap_or(false) {
        return true;
    }
    EN_FAILURE_AFTER
        .is_match(rest.get(claim_end..).unwrap_or(""))
        .unwrap_or(false)
}

fn is_admission(text: &str) -> bool {
    EN_ADMISSION.is_match(text).unwrap_or(false) || KO_ADMISSION.is_match(text).unwrap_or(false)
}

#[must_use]
pub fn classify_claims(text: &str) -> Vec<Claim> {
    let masked = mask_quoted_prose(text);
    let spans = split_sentences(&masked);
    let mut claims: Vec<Claim> = Vec::new();
    for (position, span) in spans.iter().enumerate() {
        let Some(sentence) = masked.get(span.start..span.end) else {
            continue;
        };
        let Some(found) = find_claim(sentence) else {
            continue;
        };
        if CONDITIONAL.is_match(sentence).unwrap_or(false) {
            continue;
        }
        let from = position.saturating_sub(1);
        let to = (position + 2).min(spans.len());
        let neighbours = spans
            .get(from..to)
            .unwrap_or(&[])
            .iter()
            .map(|neighbour| masked.get(neighbour.start..neighbour.end).unwrap_or(""))
            .collect::<Vec<_>>()
            .join(" ");
        let admitted = is_admission(&neighbours);
        if !admitted && is_negated(sentence, found.rule.id, found.index, found.index + found.length)
        {
            continue;
        }
        claims.push(Claim {
            kind: found.rule.kind,
            strength: if HEDGE.is_match(sentence).unwrap_or(false) {
                ClaimStrength::Hedged
            } else {
                ClaimStrength::Asserted
            },
            admitted,
            reported: REPORTED_RESULT.is_match(sentence).unwrap_or(false),
            sentence: text.get(span.start..span.end).unwrap_or("").trim().to_owned(),
            rule: found.rule.id,
        });
    }
    claims
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape(text: &str) -> (ClaimKind, ClaimStrength, bool) {
        let claims = classify_claims(text);
        assert_eq!(claims.len(), 1, "one claim for {text:?}");
        let claim = &claims[0];
        (claim.kind, claim.strength, claim.admitted)
    }

    #[test]
    fn english_and_korean_success_claims_are_found() {
        assert_eq!(
            shape("All tests pass."),
            (ClaimKind::Test, ClaimStrength::Asserted, false)
        );
        assert_eq!(
            shape("The build succeeds."),
            (ClaimKind::Build, ClaimStrength::Asserted, false)
        );
        assert_eq!(
            shape("Done."),
            (ClaimKind::Done, ClaimStrength::Asserted, false)
        );
        assert_eq!(
            shape("테스트 모두 통과했어요."),
            (ClaimKind::Test, ClaimStrength::Asserted, false)
        );
        assert_eq!(
            shape("빌드 성공."),
            (ClaimKind::Build, ClaimStrength::Asserted, false)
        );
    }

    #[test]
    fn negations_quotes_and_conditionals_are_not_claims() {
        assert!(classify_claims("Tests do not pass.").is_empty());
        assert!(classify_claims("Run `all tests pass` as the label.").is_empty());
        assert!(classify_claims("> All tests pass.").is_empty());
        assert!(classify_claims("If the suite passes, I send READY to the lead.").is_empty());
        assert!(classify_claims("3 failed, 10 passed.").is_empty());
    }

    #[test]
    fn one_claim_per_claiming_sentence_keeps_its_text() {
        let claims = classify_claims("I fixed the parser.\nAll tests pass.\n빌드도 성공했어요.");
        let shape: Vec<(&'static str, &str)> = claims
            .iter()
            .map(|claim| (claim.kind.as_str(), claim.sentence.as_str()))
            .collect();
        assert_eq!(
            shape,
            vec![("test", "All tests pass."), ("build", "빌드도 성공했어요.")]
        );
    }
}
