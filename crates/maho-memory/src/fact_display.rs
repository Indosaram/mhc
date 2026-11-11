//! How a fact is shown to a reader: its text with the provenance suffix trimmed, and the receipt rendered
//! back as one sentence.
//!
//! Spec: `docs/cleanroom/specs/memory-fact-display.md`. The stored line keeps its full provenance; only the
//! display drops it, because the same fact read in a note and read in the memory index must agree on the
//! words while the note stays an honest record of where they came from.

use crate::records::Fact;

/// UTF-16 units, because the original limit applied to a JS string; counting `chars()` would let an astral
/// character through as one unit and exceed the stored budget.
pub const QUOTE_DISPLAY_UNITS: usize = 300;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimReceipt {
    User {
        user_quote: String,
        approves: String,
    },
    /// `downgraded` marks a suggestion that claimed to be the user's but had no matching words.
    Agent {
        downgraded: bool,
    },
    Inferred,
}

impl ClaimReceipt {
    /// Returning `None` is deliberate: a malformed receipt makes the display fall back to the plain fact text
    /// rather than inventing a provenance claim.
    pub fn parse(value: &serde_json::Value) -> Option<Self> {
        let object = value.as_object()?;
        if object
            .get("downgraded")
            .is_some_and(|flag| flag != &serde_json::Value::Bool(true))
        {
            return None;
        }
        match object.get("source")?.as_str()? {
            "user" => {
                let user_quote = object.get("userQuote")?.as_str()?.to_owned();
                let approves = object.get("approves")?.as_str()?.to_owned();
                if user_quote.trim().is_empty() || !is_proposal_id(&approves) {
                    return None;
                }
                Some(Self::User {
                    user_quote,
                    approves,
                })
            }
            "agent" => Some(Self::Agent {
                downgraded: object.contains_key("downgraded"),
            }),
            "inferred" => Some(Self::Inferred),
            _ => None,
        }
    }

    /// The sentence shown after `(source: …)`.
    pub fn render(&self) -> String {
        match self {
            Self::User {
                user_quote,
                approves,
            } => {
                let collapsed = user_quote.split_whitespace().collect::<Vec<_>>().join(" ");
                let quote = truncate_utf16(&collapsed, QUOTE_DISPLAY_UNITS);
                format!(
                    "user replied \"{quote}\" to proposal {approves}; a record, not the user's approval"
                )
            }
            Self::Agent { downgraded: true } => {
                "agent suggestion, not a user decision (claimed as the user's, but no matching user words)"
                    .to_owned()
            }
            Self::Agent { downgraded: false } => {
                "agent suggestion, not a user decision".to_owned()
            }
            Self::Inferred => "inferred, not a user decision".to_owned(),
        }
    }
}

/// 16 lowercase hex characters: the proposal id the receipt names.
fn is_proposal_id(value: &str) -> bool {
    value.len() == 16
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn truncate_utf16(value: &str, units: usize) -> String {
    if value.encode_utf16().count() <= units {
        return value.to_owned();
    }
    let kept: Vec<u16> = value.encode_utf16().take(units).collect();
    format!("{}...", String::from_utf16_lossy(&kept))
}

/// The suffix is recognised in the shapes the writer produces, so a legacy line written before the current
/// receipt format still reads as plain text instead of showing a parenthetical to the reader.
pub fn strip_receipt_suffix(content: &str) -> String {
    let mut text = content;
    while let Some(start) = find_last_source_suffix(text) {
        text = &text[..start];
    }
    text.trim().to_owned()
}

fn find_last_source_suffix(text: &str) -> Option<usize> {
    let trimmed = text.trim_end();
    let close = trimmed.strip_suffix(')')?;
    let open = close.rfind("(source: ")?;
    let body = &close[open + "(source: ".len()..];
    is_known_source_body(body).then_some(open)
}

/// Anything else is the fact's own prose, because a fact may legitimately mention a source in parentheses.
fn is_known_source_body(body: &str) -> bool {
    let body = body.trim();
    if body.starts_with("user said \"") || body.starts_with("user replied \"") {
        return quoted_speech_ends_cleanly(body)
            || body.contains("; a record, not the user's approval");
    }
    if body.starts_with("agent suggestion")
        || body.starts_with("inferred")
        || body.starts_with("mixed")
    {
        return true;
    }
    false
}

fn quoted_speech_ends_cleanly(body: &str) -> bool {
    body.ends_with('"') || body.ends_with("approving proposal")
}

/// What a reader sees for one fact, with the receipt appended when the fact carries one.
pub fn fact_display_text(fact: &Fact) -> String {
    let text = strip_receipt_suffix(&fact.text);
    match fact.source.as_deref().and_then(parse_source_body) {
        Some(receipt) => format!("{text} (source: {})", receipt.render()),
        None => text,
    }
}

/// The stored parenthetical is already a rendered sentence, so it is shown as-is when it is one the writer
/// could have produced. Re-rendering it would mean parsing prose.
fn parse_source_body(body: &str) -> Option<ClaimReceipt> {
    let body = body.trim();
    if body.starts_with("agent suggestion, not a user decision") {
        return Some(ClaimReceipt::Agent {
            downgraded: body.contains("claimed as the user's"),
        });
    }
    if body == "inferred, not a user decision" {
        return Some(ClaimReceipt::Inferred);
    }
    if body.starts_with("user replied \"") && body.contains("; a record, not the user's approval") {
        let quote_start = body.find('"')? + 1;
        let quote_end = body.rfind("\" to proposal ")?;
        let approves = body[quote_end + "\" to proposal ".len()..]
            .split(';')
            .next()?
            .trim();
        if !is_proposal_id(approves) {
            return None;
        }
        return Some(ClaimReceipt::User {
            user_quote: body[quote_start..quote_end].to_owned(),
            approves: approves.to_owned(),
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fact(date: &str, text: &str, source: Option<&str>) -> Fact {
        Fact {
            date: date.to_owned(),
            text: text.to_owned(),
            source: source.map(str::to_owned),
        }
    }

    #[test]
    fn a_fact_without_provenance_is_its_text() {
        assert_eq!(
            fact_display_text(&fact("2026-11-01", "likes tea", None)),
            "likes tea"
        );
    }

    #[test]
    fn a_user_receipt_is_rendered_with_the_quote_and_proposal() {
        let value = serde_json::json!({
            "source": "user",
            "userQuote": "i like tea",
            "approves": "0123456789abcdef"
        });
        let receipt = ClaimReceipt::parse(&value).expect("parses");
        assert_eq!(
            receipt.render(),
            "user replied \"i like tea\" to proposal 0123456789abcdef; a record, not the user's approval"
        );
    }

    #[test]
    fn a_user_receipt_with_a_short_or_non_hex_proposal_is_refused() {
        for approves in ["0123", "0123456789abcdeZ", ""] {
            let value = serde_json::json!({
                "source": "user",
                "userQuote": "q",
                "approves": approves
            });
            assert!(
                ClaimReceipt::parse(&value).is_none(),
                "approves: {approves}"
            );
        }
    }

    #[test]
    fn a_user_receipt_with_an_empty_quote_is_refused() {
        let value = serde_json::json!({
            "source": "user",
            "userQuote": "   ",
            "approves": "0123456789abcdef"
        });
        assert!(ClaimReceipt::parse(&value).is_none());
    }

    #[test]
    fn a_downgraded_agent_receipt_says_so() {
        let value = serde_json::json!({ "source": "agent", "downgraded": true });
        let receipt = ClaimReceipt::parse(&value).expect("parses");
        assert!(receipt.render().contains("claimed as the user's"));
    }

    #[test]
    fn an_agent_receipt_is_never_a_user_decision() {
        let value = serde_json::json!({ "source": "agent" });
        let receipt = ClaimReceipt::parse(&value).expect("parses");
        assert_eq!(receipt.render(), "agent suggestion, not a user decision");
    }

    #[test]
    fn a_false_downgraded_flag_is_malformed() {
        let value = serde_json::json!({ "source": "agent", "downgraded": false });
        assert!(ClaimReceipt::parse(&value).is_none());
    }

    #[test]
    fn an_unknown_source_is_refused() {
        assert!(ClaimReceipt::parse(&serde_json::json!({ "source": "guess" })).is_none());
        assert!(ClaimReceipt::parse(&serde_json::json!({})).is_none());
        assert!(ClaimReceipt::parse(&serde_json::json!("agent")).is_none());
    }

    #[test]
    fn a_long_quote_is_cut_to_the_utf16_budget() {
        let long = "a".repeat(400);
        let value = serde_json::json!({
            "source": "user",
            "userQuote": long,
            "approves": "0123456789abcdef"
        });
        let rendered = ClaimReceipt::parse(&value).expect("parses").render();
        assert!(
            rendered.contains(&format!("{}...\"", "a".repeat(300))),
            "{rendered}"
        );
        assert!(!rendered.contains(&"a".repeat(301)), "{rendered}");
    }

    #[test]
    fn the_quote_budget_counts_utf16_units_not_chars() {
        // 200 astral characters are 400 UTF-16 units, so they must be cut at 300 units = 150 chars.
        let long = "\u{1F600}".repeat(200);
        let value = serde_json::json!({
            "source": "user",
            "userQuote": long,
            "approves": "0123456789abcdef"
        });
        let rendered = ClaimReceipt::parse(&value).expect("parses").render();
        let quoted = rendered
            .split('"')
            .nth(1)
            .expect("quote is delimited")
            .trim_end_matches("...");
        assert_eq!(quoted.encode_utf16().count(), 300);
    }

    #[test]
    fn a_quote_at_exactly_the_budget_is_not_truncated() {
        let exact = "a".repeat(300);
        let value = serde_json::json!({
            "source": "user",
            "userQuote": exact,
            "approves": "0123456789abcdef"
        });
        let rendered = ClaimReceipt::parse(&value).expect("parses").render();
        assert!(!rendered.contains("..."), "{rendered}");
    }

    #[test]
    fn quote_whitespace_is_collapsed_before_rendering() {
        let value = serde_json::json!({
            "source": "user",
            "userQuote": "  i   like\n tea ",
            "approves": "0123456789abcdef"
        });
        let rendered = ClaimReceipt::parse(&value).expect("parses").render();
        assert!(rendered.contains("\"i like tea\""), "{rendered}");
    }

    #[test]
    fn a_stored_user_source_is_rendered_from_its_sentence() {
        let stored = "user replied \"i like tea\" to proposal 0123456789abcdef; a record, not the user's approval";
        let displayed = fact_display_text(&fact("2026-11-01", "likes tea", Some(stored)));
        assert_eq!(
            displayed,
            "likes tea (source: user replied \"i like tea\" to proposal 0123456789abcdef; a record, not the user's approval)"
        );
    }

    #[test]
    fn a_stored_agent_source_keeps_its_downgrade_note() {
        let stored = "agent suggestion, not a user decision (claimed as the user's, but no matching user words)";
        let displayed = fact_display_text(&fact("2026-11-01", "likes tea", Some(stored)));
        assert!(displayed.contains("claimed as the user's"), "{displayed}");
    }

    #[test]
    fn a_stored_inferred_source_is_shown() {
        let displayed = fact_display_text(&fact(
            "2026-11-01",
            "likes tea",
            Some("inferred, not a user decision"),
        ));
        assert_eq!(
            displayed,
            "likes tea (source: inferred, not a user decision)"
        );
    }

    #[test]
    fn an_unrecognised_source_is_dropped_rather_than_shown_raw() {
        let displayed = fact_display_text(&fact("2026-11-01", "likes tea", Some("mystery")));
        assert_eq!(displayed, "likes tea");
    }

    #[test]
    fn a_legacy_suffix_inside_the_text_is_stripped() {
        let displayed = fact_display_text(&fact(
            "2026-11-01",
            "likes tea (source: user said \"i like tea\")",
            None,
        ));
        assert_eq!(displayed, "likes tea");
    }

    #[test]
    fn a_legacy_agent_suffix_is_stripped() {
        let displayed = fact_display_text(&fact(
            "2026-11-01",
            "likes tea (source: agent suggestion)",
            None,
        ));
        assert_eq!(displayed, "likes tea");
    }

    #[test]
    fn prose_that_merely_mentions_a_source_is_kept() {
        let text = "the record cites (source: an internal document)";
        assert_eq!(strip_receipt_suffix(text), text);
    }

    #[test]
    fn an_unterminated_parenthetical_is_kept() {
        let text = "likes tea (source: user said \"i like tea\"";
        assert_eq!(strip_receipt_suffix(text), text);
    }

    #[test]
    fn stripping_is_repeatable_for_stacked_suffixes() {
        assert_eq!(
            strip_receipt_suffix("likes tea (source: inferred) (source: inferred)"),
            "likes tea"
        );
    }

    #[test]
    fn an_empty_text_stays_empty() {
        assert_eq!(strip_receipt_suffix(""), "");
    }

    #[test]
    fn multi_byte_text_survives_stripping() {
        assert_eq!(
            strip_receipt_suffix("한국어를 좋아함 (source: inferred)"),
            "한국어를 좋아함"
        );
    }
}
