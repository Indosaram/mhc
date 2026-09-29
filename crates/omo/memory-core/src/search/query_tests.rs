use pretty_assertions::assert_eq;
use serde_json::json;

use super::*;

#[test]
fn test_normalize_text_when_mixed_case_and_padded_whitespace_then_it_lowercases_collapses_runs_and_trims()
 {
    let raw = "  ALPHA \n\t Beta   GAMMA  ";
    let normalized = normalize_text(raw);
    assert_eq!(normalized, "alpha beta gamma");
}

#[test]
fn test_parse_query_when_bare_terms_and_double_quoted_phrase_then_terms_and_phrases_are_separated()
{
    let query = "foo \"bar baz\" qux";
    let parsed = parse_query(query);
    assert_eq!(parsed.terms, vec!["foo", "qux"]);
    assert_eq!(parsed.phrases, vec!["bar baz"]);
}

#[test]
fn test_parse_query_when_repeated_whitespace_between_terms_then_empty_fragments_are_dropped() {
    let query = "  alpha    beta \n gamma  ";
    let parsed = parse_query(query);
    assert_eq!(parsed.terms, vec!["alpha", "beta", "gamma"]);
    assert_eq!(parsed.phrases, Vec::<String>::new());
}

#[test]
fn test_parse_query_when_unclosed_quote_then_falls_back_to_raw_whitespace_terms_keeping_quote_character()
 {
    let query = "foo \"bar baz";
    let parsed = parse_query(query);
    assert_eq!(parsed.terms, vec!["foo", "\"bar", "baz"]);
    assert_eq!(parsed.phrases, Vec::<String>::new());
}

#[test]
fn test_parse_query_when_empty_quotes_and_blank_input_then_nothing_is_produced() {
    let empty_phrase = "\"\"";
    let parsed = parse_query(empty_phrase);
    let blank = parse_query("   ");

    assert_eq!(parsed.terms, Vec::<String>::new());
    assert_eq!(parsed.phrases, Vec::<String>::new());
    assert_eq!(blank.terms, Vec::<String>::new());
    assert_eq!(blank.phrases, Vec::<String>::new());
}

#[test]
fn test_parse_query_when_mixed_case_input_then_case_is_preserved_for_match_time_normalization() {
    let query = "Foo \"Bar Baz\"";
    let parsed = parse_query(query);
    assert_eq!(parsed.terms, vec!["Foo"]);
    assert_eq!(parsed.phrases, vec!["Bar Baz"]);
}

#[test]
fn test_match_score_when_one_term_then_score_is_first_index_plus_short_term_bonus() {
    let text = "alpha beta gamma";
    let score = match_score(text, &parse_query("beta"));
    assert_eq!(score, Some((6 + 46) as f64));
}

#[test]
fn test_match_score_when_two_terms_then_per_term_contributions_sum() {
    let text = "alpha beta gamma";
    let score = match_score(text, &parse_query("alpha gamma"));
    assert_eq!(score, Some((45 + (11 + 45)) as f64));
}

#[test]
fn test_match_score_when_term_of_50_chars_or_more_then_length_bonus_floors_at_zero() {
    let long = "a".repeat(55);
    let score = match_score(&long, &parse_query(&long));
    assert_eq!(score, Some(0.0));
}

#[test]
fn test_match_score_when_quoted_phrase_then_phrase_contributes_tenth_of_first_index() {
    let text = "alpha beta gamma";
    let score = match_score(text, &parse_query("\"beta gamma\"")).expect("score");
    assert!((score - 0.6).abs() < 1e-9);
}

#[test]
fn test_match_score_when_phrase_and_term_then_contributions_combine() {
    let text = "alpha beta gamma";
    let score = match_score(text, &parse_query("alpha \"beta gamma\"")).expect("score");
    assert!((score - (0.0 + 45.0 + 6.0 * 0.1)).abs() < 1e-9);
}

#[test]
fn test_match_score_when_term_is_absent_then_match_is_rejected() {
    let text = "alpha beta gamma";
    let score = match_score(text, &parse_query("alpha delta"));
    assert_eq!(score, None);
}

#[test]
fn test_match_score_when_phrase_words_out_of_order_then_phrase_does_not_match() {
    let text = "alpha beta gamma";
    let score = match_score(text, &parse_query("\"gamma beta\""));
    assert_eq!(score, None);
}

#[test]
fn test_match_score_when_mixed_case_and_padded_text_then_matching_is_case_insensitive() {
    let text = "   ALPHA \n  Beta  ";
    let score = match_score(text, &parse_query("BETA"));
    assert_eq!(score, Some((6 + 46) as f64));
}

#[test]
fn test_match_score_when_unclosed_quote_query_then_retained_quote_character_must_appear_literally()
{
    let with_quote = "say foo \"bar now";
    let without_quote = "say foo bar now";

    let matched = match_score(with_quote, &parse_query("foo \"bar"));
    let rejected = match_score(without_quote, &parse_query("foo \"bar"));

    assert_eq!(matched, Some((4 + 47 + (8 + 46)) as f64));
    assert_eq!(rejected, None);
}

#[test]
fn test_match_score_when_empty_text_then_nothing_matches() {
    let text = "   ";
    let score = match_score(text, &parse_query("alpha"));
    assert_eq!(score, None);
}

#[test]
fn test_searchable_text_when_every_searchable_field_present_then_all_parts_are_joined() {
    let document = SearchDocument {
        id: "m1".to_string(),
        conversation_id: "c1".to_string(),
        date: None,
        message_type: Some("assistant_message".to_string()),
        content: Some(json!([
            { "text": "visible answer" },
            { "text": "second block" }
        ])),
        reasoning: Some("hidden thought".to_string()),
        summary: Some("compacted summary".to_string()),
        tool_calls: Some(vec![SearchToolCall {
            name: Some("bash".to_string()),
            arguments: Some("{\"cmd\":\"ls\"}".to_string()),
        }]),
        tool_return: Some(json!({ "stdout": "listing" })),
        func_response: None,
    };

    let text = searchable_text(&document);
    let expected = [
        "assistant_message",
        "visible answer\nsecond block",
        "hidden thought",
        "compacted summary",
        "bash",
        "{\"cmd\":\"ls\"}",
        "{\"stdout\":\"listing\"}",
    ]
    .join("\n");

    assert_eq!(text, expected);
}

#[test]
fn test_searchable_text_when_string_content_and_no_optional_fields_then_empty_parts_are_dropped() {
    let document = SearchDocument {
        id: "m2".to_string(),
        conversation_id: "c1".to_string(),
        date: None,
        message_type: Some("user_message".to_string()),
        content: Some(json!("plain text")),
        reasoning: None,
        summary: None,
        tool_calls: None,
        tool_return: None,
        func_response: None,
    };

    let text = searchable_text(&document);
    assert_eq!(text, "user_message\nplain text");
}

#[test]
fn test_date_in_range_when_inclusive_bounds_and_message_on_boundary_then_stays_eligible() {
    let start = "2026-01-01T00:00:00.000Z";
    let end = "2026-01-31T23:59:59.000Z";

    let on_start = date_in_range(Some(start), Some(start), Some(end));
    let on_end = date_in_range(Some(end), Some(start), Some(end));
    let outside = date_in_range(Some("2026-02-01T00:00:00.000Z"), Some(start), Some(end));

    assert_eq!(on_start, true);
    assert_eq!(on_end, true);
    assert_eq!(outside, false);
}

#[test]
fn test_date_in_range_when_missing_or_unparseable_message_date_then_stays_eligible() {
    let start = "2026-01-01T00:00:00.000Z";

    let missing = date_in_range(None, Some(start), None);
    let invalid = date_in_range(Some("not-a-date"), Some(start), None);

    assert_eq!(missing, true);
    assert_eq!(invalid, true);
}

#[test]
fn test_date_in_range_when_unparseable_bounds_then_filter_is_ignored() {
    let created = "2026-01-15T00:00:00.000Z";

    let bad_start = date_in_range(Some(created), Some("nonsense"), None);
    let bad_end = date_in_range(Some(created), None, Some("nonsense"));
    let no_bounds = date_in_range(Some(created), None, None);

    assert_eq!(bad_start, true);
    assert_eq!(bad_end, true);
    assert_eq!(no_bounds, true);
}
